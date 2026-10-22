//! Catalog payloads stay private; the shared UI transport carries only JobId.
use super::core::TuiExecutor;
use crate::{
    features::{
        model_selection::ModelPickerState,
        openai_subscription::ProviderKind,
        openrouter::catalog::{self, CatalogClient, CatalogError, CatalogPolicy, CatalogResult},
    },
    jobs::{JobId, JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess},
    tui::state::TuiApp,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub(crate) struct CatalogDelivery {
    pub generation: u64,
    pub provider: ProviderKind,
    pub result: Result<CatalogResult, CatalogError>,
}
#[derive(Clone, Default)]
pub(crate) struct CatalogResultStore(Arc<Mutex<HashMap<JobId, CatalogDelivery>>>);
impl CatalogResultStore {
    pub fn stage(&self, id: JobId, delivery: CatalogDelivery) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, delivery);
    }
    pub fn take(&self, id: JobId) -> Option<CatalogDelivery> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id)
    }
    pub fn discard(&self, id: JobId) {
        self.take(id);
    }
}
impl TuiExecutor {
    pub(crate) fn cancel_model_catalog(&mut self, ui: &mut TuiApp) {
        if let Some(picker) = ui.model_picker.as_mut() {
            picker.generation = picker.generation.wrapping_add(1);
            if let Some(id) = picker.catalog_job.take() {
                self.jobs.cancel(id);
                self.catalog_results.discard(id);
            }
        }
    }
    pub(crate) fn start_model_catalog(
        &mut self,
        ui: &mut TuiApp,
        policy: CatalogPolicy,
    ) -> anyhow::Result<()> {
        self.cancel_model_catalog(ui);
        let Some(picker) = ui.model_picker.as_mut() else {
            return Ok(());
        };
        if picker.provider != ProviderKind::Openrouter {
            picker.error = Some("Go/Zen use a fixed offline catalog".into());
            return Ok(());
        }
        let generation = picker.generation;
        let client = match CatalogClient::new(catalog::default_cache_path()) {
            Ok(client) => client,
            Err(error) => {
                picker.error = Some(error.to_string());
                picker.state = if picker.entries.is_empty() {
                    ModelPickerState::FailedWithoutSnapshot
                } else {
                    ModelPickerState::ReadyStale
                };
                return Ok(());
            }
        };
        // Read-only cache preview is immediate. Network is always managed below.
        if picker.entries.is_empty()
            && let Some(path) = catalog::default_cache_path()
            && let Ok(Some(cached)) = catalog::read_cached(&path, chrono::Utc::now())
        {
            let fresh = cached.freshness == catalog::CatalogFreshness::Fresh;
            picker.apply_catalog(generation, Ok(cached));
            if matches!(policy, CatalogPolicy::PreferCache) && fresh {
                ui.dirty = true;
                return Ok(());
            }
        }
        picker.state = if picker.entries.is_empty() {
            ModelPickerState::LoadingWithoutSnapshot
        } else {
            ModelPickerState::RefreshingWithSnapshot
        };
        let store = self.catalog_results.clone();
        if self.ui_tx.is_none() {
            self.set_ui_tx(ui.sender());
        }
        let spawned = self.jobs.spawn(
            JobSpec::new(
                JobKind::ModelCatalog,
                JobScope::Background,
                WorkspaceAccess::None,
                "OpenRouter model catalog",
            ),
            move |ctx| async move {
                let result = client.load(policy, ctx.cancellation_token()).await;
                if ctx.cancellation.is_cancelled() {
                    return JobRunOutcome::Cancelled;
                }
                store.stage(
                    ctx.id,
                    CatalogDelivery {
                        generation,
                        provider: ProviderKind::Openrouter,
                        result,
                    },
                );
                JobRunOutcome::Completed
            },
        );
        let Some(picker) = ui.model_picker.as_mut() else {
            if let Ok(id) = spawned {
                self.jobs.cancel(id);
                self.catalog_results.discard(id);
            }
            return Ok(());
        };
        match spawned {
            Ok(id) => picker.catalog_job = Some(id),
            Err(error) => {
                picker.error = Some(error.to_string());
                picker.state = if picker.entries.is_empty() {
                    ModelPickerState::FailedWithoutSnapshot
                } else {
                    ModelPickerState::ReadyStale
                };
            }
        }
        ui.dirty = true;
        Ok(())
    }
    pub(crate) fn finish_model_catalog(
        &mut self,
        ui: &mut TuiApp,
        id: JobId,
    ) -> anyhow::Result<bool> {
        let Some(job) = self.jobs.get_snapshot(id) else {
            self.catalog_results.discard(id);
            return Ok(false);
        };
        if job.kind != JobKind::ModelCatalog {
            return Ok(false);
        }
        if !job.status.is_terminal() {
            return Ok(true);
        }
        let delivery = self.catalog_results.take(id);
        if let Some(picker) = ui
            .model_picker
            .as_mut()
            .filter(|picker| picker.catalog_job == Some(id))
        {
            picker.catalog_job = None;
            if let Some(delivery) = delivery.filter(|delivery| delivery.provider == picker.provider)
            {
                picker.apply_catalog(delivery.generation, delivery.result);
            } else {
                picker.error = Some("Catalog update cancelled; Ctrl+R to retry".into());
                picker.state = if picker.entries.is_empty() {
                    ModelPickerState::FailedWithoutSnapshot
                } else {
                    ModelPickerState::ReadyStale
                };
            }
            ui.dirty = true;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::model_selection::ModelPicker;
    use crate::tui::commands::model_selection_tests::{fixture, synthetic_catalog};
    #[tokio::test]
    async fn openrouter_catalog_job_consumes_once_after_terminal() {
        let (mut executor, mut ui, _dir) = fixture(false);
        ui.model_picker = Some(ModelPicker::new(ProviderKind::Openrouter, "").unwrap());
        ui.status = crate::tui::state::Status::Running;
        let gate = Arc::new(tokio::sync::Notify::new());
        let wait = gate.clone();
        let store = executor.catalog_results.clone();
        let id = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::ModelCatalog,
                    JobScope::Background,
                    WorkspaceAccess::None,
                    "fixture",
                ),
                move |ctx| async move {
                    store.stage(
                        ctx.id,
                        CatalogDelivery {
                            generation: 0,
                            provider: ProviderKind::Openrouter,
                            result: Ok(synthetic_catalog()),
                        },
                    );
                    wait.notified().await;
                    JobRunOutcome::Completed
                },
            )
            .unwrap();
        ui.model_picker.as_mut().unwrap().catalog_job = Some(id);
        tokio::task::yield_now().await;
        assert!(executor.finish_model_catalog(&mut ui, id).unwrap());
        assert!(!ui.model_picker.as_ref().unwrap().can_select());
        assert_eq!(
            executor.jobs.get_snapshot(id).unwrap().scope,
            JobScope::Background
        );
        assert_eq!(
            executor.jobs.get_snapshot(id).unwrap().workspace_access,
            WorkspaceAccess::None
        );
        gate.notify_one();
        for _ in 0..100 {
            if executor.jobs.get_snapshot(id).unwrap().status.is_terminal() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert_eq!(executor.jobs.active_count(), 0);
        assert!(executor.finish_model_catalog(&mut ui, id).unwrap());
        assert!(ui.model_picker.as_ref().unwrap().can_select());
        assert!(executor.catalog_results.take(id).is_none());
        assert_eq!(ui.status, crate::tui::state::Status::Running);
        assert!(executor.finish_model_catalog(&mut ui, id).unwrap());
    }
    #[tokio::test]
    async fn openrouter_picker_cancel_discards_late_completion() {
        let (mut executor, mut ui, _dir) = fixture(false);
        ui.model_picker = Some(ModelPicker::new(ProviderKind::Openrouter, "").unwrap());
        let id = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::ModelCatalog,
                    JobScope::Background,
                    WorkspaceAccess::None,
                    "fixture",
                ),
                |ctx| async move {
                    ctx.cancellation.cancelled().await;
                    JobRunOutcome::Cancelled
                },
            )
            .unwrap();
        ui.model_picker.as_mut().unwrap().catalog_job = Some(id);
        executor.cancel_model_catalog(&mut ui);
        ui.model_picker = Some(ModelPicker::new(ProviderKind::Openrouter, "new").unwrap());
        executor.catalog_results.stage(
            id,
            CatalogDelivery {
                generation: 0,
                provider: ProviderKind::Openrouter,
                result: Ok(synthetic_catalog()),
            },
        );
        for _ in 0..100 {
            if executor.jobs.get_snapshot(id).unwrap().status.is_terminal() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        executor.finish_model_catalog(&mut ui, id).unwrap();
        assert!(executor.catalog_results.take(id).is_none());
        assert_eq!(ui.model_picker.as_ref().unwrap().query, "new");
        assert!(ui.model_picker.as_ref().unwrap().entries.is_empty());
        assert!(ui.model_picker.as_ref().unwrap().error.is_none());
    }
}

#[cfg(test)]
mod modal_cancel_tests {
    use super::*;
    use crate::features::model_selection::ModelPicker;
    use crate::tui::commands::model_selection_tests::{fixture, synthetic_catalog};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    #[tokio::test]
    async fn openrouter_model_picker_cancel_keys_and_provider_change_discard_late_payload() {
        for (code, modifiers) in [
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('g'), KeyModifiers::CONTROL),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::BackTab, KeyModifiers::SHIFT),
        ] {
            let (executor, mut ui, _dir) = fixture(false);
            let jobs = executor.jobs.clone();
            let store = executor.catalog_results.clone();
            let id = jobs
                .spawn(
                    JobSpec::new(
                        JobKind::ModelCatalog,
                        JobScope::Background,
                        WorkspaceAccess::None,
                        "cancel fixture",
                    ),
                    |ctx| async move {
                        ctx.cancellation.cancelled().await;
                        JobRunOutcome::Cancelled
                    },
                )
                .unwrap();
            let mut picker = ModelPicker::new(ProviderKind::Openrouter, "").unwrap();
            picker.catalog_job = Some(id);
            ui.model_picker = Some(picker);
            ui.handler = Some(Box::new(executor));
            crate::tui::event_handlers::handle_model_picker_key(
                &mut ui,
                KeyEvent::new(code, modifiers),
            )
            .unwrap();
            for _ in 0..100 {
                if jobs.get_snapshot(id).unwrap().status.is_terminal() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            assert!(jobs.get_snapshot(id).unwrap().status.is_terminal());
            store.stage(
                id,
                CatalogDelivery {
                    generation: 0,
                    provider: ProviderKind::Openrouter,
                    result: Ok(synthetic_catalog()),
                },
            );
            ui.model_picker =
                Some(ModelPicker::new(ProviderKind::Openrouter, "replacement").unwrap());
            let mut handler = ui.handler.take().unwrap();
            handler.handle_job_completed(&id.to_string(), &mut ui);
            ui.handler = Some(handler);
            assert!(store.take(id).is_none());
            assert!(ui.model_picker.as_ref().unwrap().entries.is_empty());
            assert_eq!(ui.model_picker.as_ref().unwrap().query, "replacement");
        }
    }
}
