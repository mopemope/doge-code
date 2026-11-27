use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dgc_protocol::ids::{ScheduleId, WorkspaceId};
use dgc_protocol::methods::{
    AgentCreateParams, ScheduleCreateParams, ScheduleKind, ScheduleListEntry, ScheduleTargetParams,
};
use dgc_protocol::model::{
    AgentState, ProviderKind, RunOutcome, Schedule, ScheduleRun, ScheduleState, ScheduleTarget,
    WorkspaceState,
};
use dgc_protocol::rpc::{INTERNAL, INVALID_PARAMS, INVALID_STATE, NOT_FOUND};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock, RwLockWriteGuard, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{MissedTickBehavior, interval};

use super::cron::CronExpr;
use super::localtime::LocalClock;
use crate::agent::{AgentError, AgentManager, CreateOptions};
use crate::provider;
use crate::store::{Store, StoreError};
use crate::time::format_rfc3339;
use crate::workspace::{WorkspaceError, WorkspaceManager};

const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_FIELD_BYTES: usize = 256;
const MAX_SCHEDULES: usize = 256;
const MAX_RECENT_RUNS: usize = 20;
const MISSED_AFTER_SECS: i64 = 120;
const TICK_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("schedule not found")]
    NotFound,
    #[error("{0}")]
    InvalidParams(String),
    #[error("{0}")]
    InvalidState(&'static str),
    #[error("could not access schedule state")]
    Store(#[from] StoreError),
    #[error("could not inspect schedule target")]
    Target,
    #[error("could not finish schedule run")]
    Task,
}

impl ScheduleError {
    pub fn code(&self) -> i32 {
        match self {
            Self::NotFound => NOT_FOUND,
            Self::InvalidParams(_) => INVALID_PARAMS,
            Self::InvalidState(_) => INVALID_STATE,
            Self::Store(_) | Self::Target | Self::Task => INTERNAL,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Store(err) => err.kind(),
            Self::NotFound => "not_found",
            Self::InvalidParams(_) => "invalid_params",
            Self::InvalidState(_) => "invalid_state",
            Self::Target => "target",
            Self::Task => "task",
        }
    }
}

enum TargetStatus {
    Available(Option<AgentState>),
    Unavailable(&'static str),
}

struct Entry {
    schedule: Option<Schedule>,
    // A run cannot be rolled back if the provider accepted it. Retry its
    // write before attempting another run in this daemon.
    dirty: bool,
}

type Entries = BTreeMap<ScheduleId, Arc<Mutex<Entry>>>;

/// Owns schedule mutations and serializes work per schedule.
#[derive(Clone)]
pub struct ScheduleManager {
    store: Arc<Store>,
    agents: Arc<AgentManager>,
    workspaces: Arc<WorkspaceManager>,
    clock: Arc<dyn LocalClock>,
    entries: Arc<RwLock<Entries>>,
    run_gate: Arc<RwLock<()>>,
}

impl ScheduleManager {
    pub fn new(
        store: Arc<Store>,
        agents: Arc<AgentManager>,
        workspaces: Arc<WorkspaceManager>,
        clock: Arc<dyn LocalClock>,
    ) -> Result<Self, StoreError> {
        let entries = store
            .list_schedules()?
            .into_iter()
            .map(|schedule| (schedule.id.clone(), new_entry(schedule)))
            .collect();
        Ok(Self {
            store,
            agents,
            workspaces,
            clock,
            entries: Arc::new(RwLock::new(entries)),
            run_gate: Arc::new(RwLock::new(())),
        })
    }

    pub async fn create(&self, params: ScheduleCreateParams) -> Result<Schedule, ScheduleError> {
        self.create_at(params, now_unix()).await
    }

    pub async fn create_at(
        &self,
        params: ScheduleCreateParams,
        now: i64,
    ) -> Result<Schedule, ScheduleError> {
        if params.prompt.trim().is_empty() || params.prompt.len() > MAX_PROMPT_BYTES {
            return Err(invalid("prompt must be nonempty and at most 64 KiB"));
        }
        if params.max_runs == Some(0) {
            return Err(invalid("max_runs must be positive"));
        }
        if params.expires_at.is_some_and(|expires| expires <= now) {
            return Err(invalid("expires_at must be in the future"));
        }
        validate_field_len("name", params.name.as_deref().unwrap_or_default())?;
        validate_field_len("cron", &params.cron)?;
        let next_run_at = Some(self.next_run(&params.cron, now)?);
        let _gate = self.run_gate.read().await;
        let target = self.resolve_target(params.target)?;
        if let TargetStatus::Unavailable(reason) = self.target_status(&target)? {
            return Err(invalid(reason));
        }
        let mut entries = self.entries.write().await;
        if entries.len() >= MAX_SCHEDULES {
            return Err(invalid("at most 256 schedules can be stored"));
        }
        let schedule = Schedule {
            id: ScheduleId::generate(),
            name: params.name,
            target,
            prompt: params.prompt,
            cron: params.cron,
            state: ScheduleState::Active,
            end_reason: None,
            max_runs: params.max_runs,
            runs: 0,
            expires_at: params.expires_at,
            next_run_at,
            last_agent_id: None,
            recent_runs: Vec::new(),
            created_at: format_rfc3339(now.max(0) as u128 * 1000),
        };
        self.store.put_schedule(&schedule)?;
        entries.insert(schedule.id.clone(), new_entry(schedule.clone()));
        Ok(schedule)
    }

    pub fn get(&self, id: &ScheduleId) -> Result<Schedule, ScheduleError> {
        self.store.get_schedule(id)?.ok_or(ScheduleError::NotFound)
    }

    pub fn list(
        &self,
        kind: Option<ScheduleKind>,
    ) -> Result<Vec<ScheduleListEntry>, ScheduleError> {
        Ok(self
            .store
            .list_schedules()?
            .into_iter()
            .filter(|schedule| {
                kind.is_none_or(|kind| {
                    matches!(
                        (kind, &schedule.target),
                        (ScheduleKind::NewAgent, ScheduleTarget::NewAgent { .. })
                            | (ScheduleKind::Agent, ScheduleTarget::Agent { .. })
                    )
                })
            })
            .map(|schedule| ScheduleListEntry {
                id: schedule.id,
                name: schedule.name,
                target: schedule.target,
                // Cron separators can expand to six bytes each in JSON.
                // A single space per separator keeps the whole list bounded.
                cron: schedule
                    .cron
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                state: schedule.state,
                next_run_at: schedule.next_run_at,
                max_runs: schedule.max_runs,
                runs: schedule.runs,
            })
            .collect())
    }

    pub async fn pause(&self, id: &ScheduleId) -> Result<Schedule, ScheduleError> {
        let entry = self.entry(id).await?;
        let mut entry = entry.lock().await;
        let mut schedule = entry.schedule.clone().ok_or(ScheduleError::NotFound)?;
        if schedule.state == ScheduleState::Ended {
            return Err(ScheduleError::InvalidState("ended"));
        }
        schedule.state = ScheduleState::Paused;
        self.store.put_schedule(&schedule)?;
        entry.schedule = Some(schedule.clone());
        entry.dirty = false;
        Ok(schedule)
    }

    pub async fn resume(&self, id: &ScheduleId) -> Result<Schedule, ScheduleError> {
        self.resume_at(id, now_unix()).await
    }

    pub async fn resume_at(&self, id: &ScheduleId, now: i64) -> Result<Schedule, ScheduleError> {
        let entry = self.entry(id).await?;
        let mut entry = entry.lock().await;
        let mut schedule = entry.schedule.clone().ok_or(ScheduleError::NotFound)?;
        match schedule.state {
            ScheduleState::Ended => return Err(ScheduleError::InvalidState("ended")),
            ScheduleState::Active => return Err(ScheduleError::InvalidState("active")),
            ScheduleState::Paused => {}
        }
        if let Some(reason) = limit_reached(&schedule, now) {
            end(&mut schedule, reason);
        } else {
            schedule.next_run_at = Some(self.next_run(&schedule.cron, now)?);
            schedule.state = ScheduleState::Active;
        }
        self.store.put_schedule(&schedule)?;
        let ended = schedule.state == ScheduleState::Ended;
        entry.schedule = Some(schedule.clone());
        entry.dirty = false;
        if ended {
            return Err(ScheduleError::InvalidState("ended"));
        }
        Ok(schedule)
    }

    pub async fn remove(&self, id: &ScheduleId) -> Result<(), ScheduleError> {
        let entry = self.entry(id).await?;
        let mut entry = entry.lock().await;
        if entry.schedule.is_none() {
            return Err(ScheduleError::NotFound);
        }
        self.store.remove_schedule(id)?;
        entry.schedule = None;
        self.entries.write().await.remove(id);
        Ok(())
    }

    pub async fn run(
        &self,
        id: ScheduleId,
        shutdown: watch::Receiver<bool>,
    ) -> Result<ScheduleRun, ScheduleError> {
        // The connection may disappear while the provider is handling its
        // request. Keep the operation alive until its result is persisted.
        let manager = self.clone();
        tokio::spawn(async move { manager.run_inner(&id, None, Some(&shutdown)).await })
            .await
            .map_err(|_| ScheduleError::Task)?
    }

    pub async fn run_at(&self, id: &ScheduleId, now: i64) -> Result<ScheduleRun, ScheduleError> {
        self.run_inner(id, Some(now), None).await
    }

    async fn run_inner(
        &self,
        id: &ScheduleId,
        now: Option<i64>,
        shutdown: Option<&watch::Receiver<bool>>,
    ) -> Result<ScheduleRun, ScheduleError> {
        let entry = self.entry(id).await?;
        let mut entry = entry.lock().await;
        let _gate = self.run_gate.read().await;
        if shutdown.is_some_and(stopping) {
            return Err(ScheduleError::InvalidState("daemon_shutting_down"));
        }
        self.flush(&mut entry)?;
        let schedule = entry.schedule.as_mut().ok_or(ScheduleError::NotFound)?;
        let now = now.unwrap_or_else(now_unix);
        if schedule.state == ScheduleState::Ended {
            return Err(ScheduleError::InvalidState("ended"));
        }
        if let Some(reason) = limit_reached(schedule, now) {
            end(schedule, reason);
            self.persist(&mut entry)?;
            return Err(ScheduleError::InvalidState("ended"));
        }
        let outcome = self
            .try_run(schedule, self.target_status(&schedule.target)?, true)
            .await?;
        let run = record(schedule, now, now, outcome);
        self.persist(&mut entry)?;
        Ok(run)
    }

    /// Prevent a new schedule run during workspace archival and wait for any
    /// in-flight runs. Shutdown uses the same gate to drain manual runs.
    pub(crate) async fn exclude_runs(&self) -> RwLockWriteGuard<'_, ()> {
        self.run_gate.write().await
    }

    pub async fn tick(&self, now: i64) {
        let mut runs = JoinSet::new();
        self.start_tick(now, None, &mut runs).await;
        drain(&mut runs).await;
    }

    pub fn spawn_loop(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut timer = interval(TICK_INTERVAL);
            timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut runs = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    () = wait_shutdown(&mut shutdown) => break,
                    finished = runs.join_next(), if !runs.is_empty() => log_join(finished),
                    _ = timer.tick() => {
                        self.start_tick(now_unix(), Some(&shutdown), &mut runs).await;
                    }
                }
            }
            drain(&mut runs).await;
        })
    }

    async fn start_tick(
        &self,
        now: i64,
        shutdown: Option<&watch::Receiver<bool>>,
        runs: &mut JoinSet<()>,
    ) {
        let entries = self.entries.read().await;
        for (id, entry) in entries.iter() {
            // The timer keeps checking other schedules during slow provider
            // calls. This entry is already covered by its in-flight operation.
            let Ok(entry) = Arc::clone(entry).try_lock_owned() else {
                continue;
            };
            let manager = self.clone();
            let id = id.clone();
            let shutdown = shutdown.cloned();
            runs.spawn(async move {
                if let Err(err) = manager.tick_entry(now, shutdown.as_ref(), entry).await {
                    tracing::warn!(schedule_id = %id, error_kind = err.kind(), "schedule tick failed");
                }
            });
        }
    }

    async fn tick_entry(
        &self,
        now: i64,
        shutdown: Option<&watch::Receiver<bool>>,
        mut entry: OwnedMutexGuard<Entry>,
    ) -> Result<(), ScheduleError> {
        let _gate = self.run_gate.read().await;
        if shutdown.is_some_and(stopping) {
            return Ok(());
        }
        self.flush(&mut entry)?;
        let Some(schedule) = entry.schedule.as_mut() else {
            return Ok(());
        };
        if schedule.state == ScheduleState::Ended {
            return Ok(());
        }
        if let Some(reason) = limit_reached(schedule, now) {
            end(schedule, reason);
            return self.persist(&mut entry);
        }
        let target = self.target_status(&schedule.target)?;
        if let TargetStatus::Unavailable(reason) = target {
            end(schedule, reason);
            return self.persist(&mut entry);
        }
        if schedule.state != ScheduleState::Active {
            return Ok(());
        }
        let Some(due) = schedule.next_run_at else {
            end(schedule, "no_next_run");
            return self.persist(&mut entry);
        };
        if due > now {
            // A backward clock adjustment can put an earlier cron match back
            // in the future. Reschedule without dispatching a run this tick.
            if let Ok(next) = self.next_run(&schedule.cron, now)
                && next < due
            {
                schedule.next_run_at = Some(next);
                return self.persist(&mut entry);
            }
            return Ok(());
        }
        schedule.next_run_at = self.next_run(&schedule.cron, now).ok();
        let outcome = if now.saturating_sub(due) > MISSED_AFTER_SECS {
            RunOutcome::Missed
        } else {
            // Consume the due time on disk before sending anything to a
            // provider, so a restart cannot repeat this timed occurrence.
            if let Err(err) = self.store.put_schedule(schedule) {
                // No side effect started. A later tick may retry the write.
                schedule.next_run_at = Some(due);
                return Err(err.into());
            }
            self.try_run(schedule, target, false).await?
        };
        record(schedule, due, now, outcome);
        if schedule.state == ScheduleState::Active && schedule.next_run_at.is_none() {
            end(schedule, "no_next_run");
        }
        self.persist(&mut entry)
    }

    async fn entry(&self, id: &ScheduleId) -> Result<Arc<Mutex<Entry>>, ScheduleError> {
        self.entries
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or(ScheduleError::NotFound)
    }

    fn next_run(&self, cron: &str, now: i64) -> Result<i64, ScheduleError> {
        CronExpr::parse(cron)
            .map_err(|_| invalid("invalid cron expression"))?
            .next_after(now, self.clock.as_ref())
            .ok_or_else(|| invalid("cron has no occurrence within five years"))
    }

    fn resolve_target(
        &self,
        target: ScheduleTargetParams,
    ) -> Result<ScheduleTarget, ScheduleError> {
        let (workspace_id, provider, profile, model, mode) = match target {
            ScheduleTargetParams::NewAgent {
                workspace_id,
                provider,
                profile,
                model,
                mode,
            } => (workspace_id, provider, profile, model, mode),
            ScheduleTargetParams::Agent { agent_id } => {
                return Ok(ScheduleTarget::Agent { agent_id });
            }
        };
        let mut params = agent_params(workspace_id, provider, profile, model, mode);
        let provider = self
            .agents
            .resolve_profile(&mut params)
            .map_err(|err| invalid(err.to_string()))?;
        validate_field_len("model", params.model.as_deref().unwrap_or_default())?;
        validate_field_len("mode", params.mode.as_deref().unwrap_or_default())?;
        // Fix the safe default explicitly as well as profile overrides.
        // Otherwise a provider setting could supply a different mode at launch.
        params.mode.get_or_insert_with(|| match provider {
            ProviderKind::Claude => "manual".to_owned(),
            ProviderKind::Codex => "workspace-write".to_owned(),
        });
        let auto_approve =
            provider::validate_mode(provider, params.mode.as_deref()).map_err(invalid)?;
        Ok(ScheduleTarget::NewAgent {
            workspace_id: params.workspace_id,
            provider,
            model: params.model,
            mode: params.mode,
            auto_approve,
        })
    }

    fn target_status(&self, target: &ScheduleTarget) -> Result<TargetStatus, ScheduleError> {
        let (workspace_id, agent_state) = match target {
            ScheduleTarget::NewAgent { workspace_id, .. } => (workspace_id.clone(), None),
            ScheduleTarget::Agent { agent_id } => match self.agents.get(agent_id) {
                Ok(agent) => (agent.workspace_id, Some(agent.state)),
                Err(AgentError::NotFound(_)) => {
                    return Ok(TargetStatus::Unavailable("agent_archived"));
                }
                Err(_) => return Err(ScheduleError::Target),
            },
        };
        match self.workspaces.get_workspace(&workspace_id) {
            Ok(workspace) if workspace.state == WorkspaceState::Active => {}
            Ok(_) | Err(WorkspaceError::NotFound(_)) => {
                return Ok(TargetStatus::Unavailable("workspace_unavailable"));
            }
            Err(_) => return Err(ScheduleError::Target),
        }
        if agent_state == Some(AgentState::Archived) {
            Ok(TargetStatus::Unavailable("agent_archived"))
        } else {
            Ok(TargetStatus::Available(agent_state))
        }
    }

    async fn try_run(
        &self,
        schedule: &mut Schedule,
        status: TargetStatus,
        manual: bool,
    ) -> Result<RunOutcome, ScheduleError> {
        let state = match status {
            TargetStatus::Available(state) => state,
            TargetStatus::Unavailable(reason) => {
                end(schedule, reason);
                return Ok(skipped(reason));
            }
        };
        match &schedule.target {
            ScheduleTarget::NewAgent {
                workspace_id,
                provider,
                model,
                mode,
                ..
            } => {
                if let Some(id) = &schedule.last_agent_id {
                    match self.agents.get(id) {
                        Ok(agent) if busy(agent.state) => {
                            if manual {
                                return Err(ScheduleError::InvalidState("previous_run_active"));
                            }
                            return Ok(skipped("previous_run_active"));
                        }
                        Ok(_) | Err(AgentError::NotFound(_)) => {}
                        Err(err) => return Ok(agent_failure(&err)),
                    }
                }
                if !self.agents.provider_available(*provider) {
                    return Ok(failed("provider_unavailable"));
                }
                let first_line = schedule.prompt.lines().next().unwrap_or_default();
                let title = match &schedule.name {
                    Some(name) => format!("{name}: {first_line}"),
                    None => first_line.to_owned(),
                };
                let params = AgentCreateParams {
                    title,
                    initial_prompt: schedule.prompt.clone(),
                    labels: BTreeMap::from([("schedule".to_owned(), schedule.id.to_string())]),
                    ..agent_params(
                        workspace_id.clone(),
                        Some(*provider),
                        None,
                        model.clone(),
                        mode.clone(),
                    )
                };
                match self
                    .agents
                    .create_with(
                        params,
                        CreateOptions {
                            stop_when_idle: true,
                        },
                    )
                    .await
                {
                    Ok(agent) => Ok(RunOutcome::Started { agent_id: agent.id }),
                    Err(err) => Ok(agent_failure(&err)),
                }
            }
            ScheduleTarget::Agent { agent_id } => {
                let outcome = if state == Some(AgentState::Idle) {
                    match self
                        .agents
                        .send_if_idle(agent_id, schedule.prompt.clone())
                        .await
                    {
                        Ok(outcome) => RunOutcome::Sent {
                            agent_id: agent_id.clone(),
                            outcome,
                        },
                        Err(AgentError::InvalidState { state }) => match state.as_str() {
                            "running" | "starting" | "waiting_permission" => skipped("agent_busy"),
                            "stopped" | "failed" => skipped("agent_not_running"),
                            "archived" => {
                                end(schedule, "agent_archived");
                                skipped("agent_archived")
                            }
                            _ => failed("invalid_state"),
                        },
                        Err(AgentError::NotFound(_)) => {
                            end(schedule, "agent_archived");
                            skipped("agent_archived")
                        }
                        Err(err) => agent_failure(&err),
                    }
                } else if state.is_some_and(busy) {
                    skipped("agent_busy")
                } else {
                    skipped("agent_not_running")
                };
                Ok(outcome)
            }
        }
    }

    fn persist(&self, entry: &mut Entry) -> Result<(), ScheduleError> {
        entry.dirty = true;
        self.flush(entry)
    }

    fn flush(&self, entry: &mut Entry) -> Result<(), ScheduleError> {
        if entry.dirty {
            if let Some(schedule) = &entry.schedule {
                self.store.put_schedule(schedule)?;
            }
            entry.dirty = false;
        }
        Ok(())
    }
}

fn new_entry(schedule: Schedule) -> Arc<Mutex<Entry>> {
    Arc::new(Mutex::new(Entry {
        schedule: Some(schedule),
        dirty: false,
    }))
}

fn agent_params(
    workspace_id: WorkspaceId,
    provider: Option<ProviderKind>,
    profile: Option<String>,
    model: Option<String>,
    mode: Option<String>,
) -> AgentCreateParams {
    AgentCreateParams {
        workspace_id,
        provider,
        profile,
        model,
        mode,
        title: String::new(),
        initial_prompt: String::new(),
        labels: BTreeMap::new(),
        parent_id: None,
    }
}

fn record(
    schedule: &mut Schedule,
    scheduled_for: i64,
    now: i64,
    outcome: RunOutcome,
) -> ScheduleRun {
    match &outcome {
        RunOutcome::Started { agent_id } | RunOutcome::Sent { agent_id, .. } => {
            schedule.runs = schedule.runs.saturating_add(1);
            schedule.last_agent_id = Some(agent_id.clone());
            if let Some(reason) = limit_reached(schedule, now) {
                end(schedule, reason);
            }
        }
        _ => {}
    }
    let run = ScheduleRun {
        scheduled_for,
        at: now,
        outcome,
    };
    schedule.recent_runs.push(run.clone());
    let excess = schedule.recent_runs.len().saturating_sub(MAX_RECENT_RUNS);
    schedule.recent_runs.drain(..excess);
    run
}

fn limit_reached(schedule: &Schedule, now: i64) -> Option<&'static str> {
    if schedule.max_runs.is_some_and(|max| schedule.runs >= max) {
        Some("max_runs")
    } else if schedule.expires_at.is_some_and(|expiry| now >= expiry) {
        Some("expired")
    } else {
        None
    }
}

fn end(schedule: &mut Schedule, reason: &'static str) {
    schedule.state = ScheduleState::Ended;
    schedule.end_reason = Some(reason.to_owned());
    schedule.next_run_at = None;
}

fn busy(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Starting | AgentState::Running | AgentState::WaitingPermission
    )
}

fn validate_field_len(field: &str, value: &str) -> Result<(), ScheduleError> {
    if value.len() > MAX_FIELD_BYTES {
        return Err(invalid(format!(
            "schedule {field} must be at most {MAX_FIELD_BYTES} bytes"
        )));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> ScheduleError {
    ScheduleError::InvalidParams(message.into())
}

fn skipped(reason: &'static str) -> RunOutcome {
    RunOutcome::Skipped {
        reason: reason.to_owned(),
    }
}

fn agent_failure(err: &AgentError) -> RunOutcome {
    let kind = match err {
        AgentError::Provider(_) => "provider",
        AgentError::Storage(_) => "storage",
        AgentError::InvalidParams(_) => "invalid_params",
        AgentError::NotFound(_) => "not_found",
        _ => "invalid_state",
    };
    failed(kind)
}

fn failed(kind: &'static str) -> RunOutcome {
    RunOutcome::Failed {
        error_kind: kind.to_owned(),
    }
}

async fn wait_shutdown(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}

fn stopping(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow() || shutdown.has_changed().is_err()
}

fn now_unix() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn log_join(result: Option<Result<(), tokio::task::JoinError>>) {
    if result.is_some_and(|result| result.is_err()) {
        tracing::warn!(error_kind = "task", "schedule worker stopped unexpectedly");
    }
}

async fn drain(runs: &mut JoinSet<()>) {
    while let Some(result) = runs.join_next().await {
        log_join(Some(result));
    }
}
