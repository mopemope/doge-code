use super::Executor;
use crate::{
    config::AppConfig,
    features::{openai_subscription::ProviderKind, opencode},
    llm::OpenAIClient,
    session::{SessionData, SessionStore, data::SessionModelSelection},
};

fn config(root: &std::path::Path, provider: ProviderKind) -> AppConfig {
    AppConfig {
        project_root: root.into(),
        provider,
        model: "gpt-6-luna".into(),
        base_url: opencode::default_base(provider).unwrap().into(),
        api_key: Some("SYNTHETIC_LOCAL_ONLY".into()),
        no_repomap: true,
        ..Default::default()
    }
}

fn seed(cfg: &AppConfig, binding: bool) -> (SessionStore, String, Vec<u8>) {
    let store = SessionStore::new(cfg.project_root.join(".doge/sessions")).unwrap();
    let mut session = SessionData::new();
    session.model_selection = Some(SessionModelSelection {
        provider: cfg.provider,
        model: "glm-5.3".into(),
    });
    if binding {
        let selected = AppConfig {
            model: "glm-5.3".into(),
            ..cfg.clone()
        };
        session.inference_binding = Some(
            OpenAIClient::from_config(&selected)
                .unwrap()
                .unwrap()
                .inference_binding(&selected.model)
                .unwrap(),
        );
    }
    store.save(&session).unwrap();
    let bytes = std::fs::read(store.session_dir(&session.meta.id).join("session.json")).unwrap();
    (store, session.meta.id, bytes)
}

#[tokio::test]
async fn cli_session_selection_new_and_empty_latest_save_model_then_resume() {
    for provider in [ProviderKind::OpencodeGo, ProviderKind::OpencodeZen] {
        for latest in [false, true] {
            for key in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let mut cfg = config(temp.path(), provider);
                cfg.model = "glm-5.3".into();
                cfg.resume = latest.then(|| "latest".into());
                cfg.api_key = key.then(|| "SYNTHETIC_LOCAL_ONLY".into());
                let executor = Executor::new(cfg.clone()).await.unwrap();
                let store = SessionStore::new(temp.path().join(".doge/sessions")).unwrap();
                let summaries = store.list_with_stats().unwrap();
                assert_eq!(summaries.len(), 1);
                let id = summaries[0].meta.id.clone();
                let saved = store.load(&id).unwrap();
                let selection = saved
                    .model_selection
                    .as_ref()
                    .expect("new CLI selection is recorded");
                assert_eq!(selection.provider, provider);
                assert_eq!(selection.model, "glm-5.3");
                assert!(saved.inference_binding.is_none());
                assert!(saved.conversation.is_empty());
                let before = std::fs::read(store.session_dir(&id).join("session.json")).unwrap();
                assert!(!String::from_utf8_lossy(&before).contains("SYNTHETIC_LOCAL_ONLY"));
                drop(executor);
                cfg.model = "gpt-6-luna".into();
                cfg.resume = Some(if latest {
                    "latest".into()
                } else {
                    id[..8].into()
                });
                let resumed = Executor::new(cfg).await.unwrap();
                assert_eq!(resumed.cfg.model, "glm-5.3");
                assert_eq!(resumed.tools.config.model, "glm-5.3");
                assert_eq!(resumed.client.is_some(), key);
                assert_eq!(
                    std::fs::read(store.session_dir(&id).join("session.json")).unwrap(),
                    before
                );
                assert_eq!(store.list_with_stats().unwrap().len(), 1);
            }
        }
    }
}

#[tokio::test]
async fn cli_session_selection_new_bound_checkpoint_resumes_after_startup_model_change() {
    for provider in [ProviderKind::OpencodeGo, ProviderKind::OpencodeZen] {
        let temp = tempfile::tempdir().unwrap();
        let mut cfg = config(temp.path(), provider);
        cfg.model = "glm-5.3".into();
        let executor = Executor::new(cfg.clone()).await.unwrap();
        let client = executor.client.as_ref().unwrap();
        let binding = client.inference_binding(&cfg.model).unwrap();
        let manager = executor
            .tools
            .get_session_manager_wrapper()
            .get_session_manager()
            .as_ref()
            .unwrap();
        let id = {
            let mut manager = manager.lock().unwrap();
            manager.bind_inference(binding.clone()).unwrap();
            manager.current_session_id().unwrap()
        };
        assert_eq!(client.usage_snapshot().attempts, 0);
        drop(executor);
        cfg.model = "gpt-6-luna".into();
        cfg.resume = Some(id.clone());
        cfg.api_key = None;
        let resumed = Executor::new(cfg).await.unwrap();
        assert_eq!(resumed.cfg.model, "glm-5.3");
        let store = SessionStore::new(temp.path().join(".doge/sessions")).unwrap();
        let saved = store.load(&id).unwrap();
        assert_eq!(saved.model_selection.unwrap().model, "glm-5.3");
        assert_eq!(saved.inference_binding.as_deref(), Some(binding.as_str()));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cli_session_selection_create_failure_preserves_existing_checkpoint_and_retries() {
    use std::os::unix::fs::PermissionsExt;
    for provider in [ProviderKind::OpencodeGo, ProviderKind::OpencodeZen] {
        let temp = tempfile::tempdir().unwrap();
        let mut cfg = config(temp.path(), provider);
        cfg.model = "glm-5.3".into();
        let (store, existing, before) = seed(&cfg, false);
        let root = temp.path().join(".doge/sessions");
        let mode = std::fs::metadata(&root).unwrap().permissions();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = Executor::new(cfg.clone()).await;
        std::fs::set_permissions(&root, mode).unwrap();
        assert!(result.is_err(), "failed save must not publish an executor");
        assert_eq!(store.list_with_stats().unwrap().len(), 1);
        assert_eq!(
            std::fs::read(store.session_dir(&existing).join("session.json")).unwrap(),
            before
        );
        let executor = Executor::new(cfg).await.unwrap();
        assert_eq!(store.list_with_stats().unwrap().len(), 2);
        assert_eq!(
            executor.client.as_ref().unwrap().usage_snapshot().attempts,
            0
        );
        assert_eq!(
            std::fs::read(store.session_dir(&existing).join("session.json")).unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn cli_session_selection_generic_provider_keeps_legacy_creation() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = AppConfig {
        project_root: temp.path().into(),
        api_key: None,
        ..Default::default()
    };
    let executor = Executor::new(cfg).await.unwrap();
    assert!(executor.client.is_none());
    let store = SessionStore::new(temp.path().join(".doge/sessions")).unwrap();
    let id = &store.list_with_stats().unwrap()[0].meta.id;
    assert!(store.load(id).unwrap().model_selection.is_none());
}

#[tokio::test]
async fn cli_resume_model_restores_selection_for_id_latest_with_and_without_binding() {
    for provider in [ProviderKind::OpencodeGo, ProviderKind::OpencodeZen] {
        for latest in [false, true] {
            for binding in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let mut cfg = config(temp.path(), provider);
                let (store, id, bytes) = seed(&cfg, binding);
                cfg.resume = Some(if latest {
                    "latest".into()
                } else {
                    id[..8].into()
                });
                let executor = Executor::new(cfg).await.unwrap();
                assert_eq!(executor.cfg.model, "glm-5.3");
                assert_eq!(executor.tools.config.model, "glm-5.3");
                let client = executor.client.as_ref().unwrap();
                assert_eq!(client.provider, provider);
                assert_eq!(client.opencode_session, id);
                assert_eq!(client.usage_snapshot().attempts, 0);
                assert_eq!(client.api_key, "SYNTHETIC_LOCAL_ONLY");
                assert_eq!(
                    executor.cfg.base_url,
                    opencode::default_base(provider).unwrap()
                );
                assert_eq!(store.list_with_stats().unwrap().len(), 1);
                assert_eq!(
                    std::fs::read(store.session_dir(&id).join("session.json")).unwrap(),
                    bytes
                );
            }
        }
    }
}

#[tokio::test]
async fn cli_resume_model_rejects_invalid_selection_and_binding_even_without_key() {
    for key in [false, true] {
        for kind in [
            "provider",
            "unknown",
            "unsupported",
            "binding",
            "legacy-binding",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut cfg = config(temp.path(), ProviderKind::OpencodeZen);
            let (store, id, _) = seed(&cfg, false);
            let mut data = store.load(&id).unwrap();
            match kind {
                "provider" => {
                    data.model_selection.as_mut().unwrap().provider = ProviderKind::OpencodeGo
                }
                "unknown" => data.model_selection.as_mut().unwrap().model = "unknown-model".into(),
                "unsupported" => {
                    data.model_selection.as_mut().unwrap().model = "claude-opus-4-7".into()
                }
                "binding" => data.inference_binding = Some("another-binding".into()),
                "legacy-binding" => {
                    data.model_selection = None;
                    data.inference_binding = Some("another-binding".into());
                }
                _ => unreachable!(),
            }
            store.save(&data).unwrap();
            let bytes = std::fs::read(store.session_dir(&id).join("session.json")).unwrap();
            cfg.api_key = key.then(|| "SYNTHETIC_LOCAL_ONLY".into());
            cfg.resume = Some(id.clone());
            assert!(Executor::new(cfg).await.is_err(), "kind={kind}, key={key}");
            assert_eq!(
                std::fs::read(store.session_dir(&id).join("session.json")).unwrap(),
                bytes
            );
            assert!(store.try_lease(&id).is_ok());
        }
    }
}

#[tokio::test]
async fn cli_resume_model_legacy_preserves_startup_and_keyless_selection_is_restored() {
    for legacy in [false, true] {
        for binding in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let mut cfg = config(temp.path(), ProviderKind::OpencodeGo);
            let (store, id, _) = seed(&cfg, binding);
            if legacy {
                let mut data = store.load(&id).unwrap();
                data.model_selection = None;
                if binding {
                    data.inference_binding = Some(
                        OpenAIClient::from_config(&cfg)
                            .unwrap()
                            .unwrap()
                            .inference_binding(&cfg.model)
                            .unwrap(),
                    );
                }
                store.save(&data).unwrap();
            }
            cfg.api_key = None;
            cfg.resume = Some("latest".into());
            let before = std::fs::read(store.session_dir(&id).join("session.json")).unwrap();
            let executor = Executor::new(cfg).await.unwrap();
            assert_eq!(
                executor.cfg.model,
                if legacy { "gpt-6-luna" } else { "glm-5.3" }
            );
            assert!(executor.client.is_none());
            assert_eq!(
                std::fs::read(store.session_dir(&id).join("session.json")).unwrap(),
                before
            );
        }
    }
}

#[tokio::test]
async fn cli_resume_model_saved_selection_is_checked_before_startup_client() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(temp.path(), ProviderKind::OpencodeGo);
    let (_, id, _) = seed(&cfg, false);
    // This would fail client construction if the startup model were used.
    cfg.model = "unsupported-startup-model".into();
    cfg.resume = Some(id);
    let executor = Executor::new(cfg).await.unwrap();
    assert_eq!(executor.cfg.model, "glm-5.3");
    assert_eq!(
        executor.client.as_ref().unwrap().usage_snapshot().attempts,
        0
    );
}

#[tokio::test]
async fn cli_resume_model_binding_save_failure_sends_no_request_and_is_recoverable() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(temp.path(), ProviderKind::OpencodeGo);
    let (store, id, before) = seed(&cfg, false);
    cfg.resume = Some(id.clone());
    let mut executor = Executor::new(cfg).await.unwrap();
    let root = temp.path().join(".doge/sessions");
    let backup = temp.path().join("preserved-sessions");
    std::fs::rename(&root, &backup).unwrap();
    std::fs::write(&root, "not a directory").unwrap();
    let result = executor.run("LOCAL FIXTURE ONLY", true).await;
    assert!(result.is_err());
    assert_eq!(
        executor.client.as_ref().unwrap().usage_snapshot().attempts,
        0
    );
    assert_eq!(executor.cfg.model, "glm-5.3");
    assert_eq!(
        std::fs::read(backup.join(&id).join("session.json")).unwrap(),
        before
    );
    std::fs::remove_file(&root).unwrap();
    std::fs::rename(&backup, &root).unwrap();
    executor.flush_session().unwrap();
    let saved = store.load(&id).unwrap();
    assert_eq!(saved.model_selection.unwrap().model, "glm-5.3");
    assert!(saved.inference_binding.unwrap().ends_with(":glm-5.3"));
}

#[tokio::test]
async fn cli_resume_model_rejects_cross_provider_before_auth_and_preserves_disk() {
    for latest in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let cfg = config(temp.path(), ProviderKind::OpencodeGo);
        let (store, id, bytes) = seed(&cfg, false);
        let mut wrong = AppConfig {
            provider: ProviderKind::Openai,
            resume: Some(if latest { "latest".into() } else { id.clone() }),
            ..cfg
        };
        // OpenAI initialization would read real credentials; provider validation
        // must reject this target before that code is entered.
        wrong.api_key = None;
        let error = match Executor::new(wrong).await {
            Ok(_) => panic!("cross-provider session was accepted"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("another provider"));
        assert_eq!(
            std::fs::read(store.session_dir(&id).join("session.json")).unwrap(),
            bytes
        );
        assert_eq!(store.list_with_stats().unwrap().len(), 1);
        assert!(
            store.try_lease(&id).is_ok(),
            "failed resume must release target lease"
        );
    }
}
