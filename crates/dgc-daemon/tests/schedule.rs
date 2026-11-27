use std::collections::{BTreeMap, HashMap};
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dgc_daemon::agent::AgentManager;
use dgc_daemon::config::{DaemonConfig, ProfileConfig};
use dgc_daemon::paths::DaemonPaths;
use dgc_daemon::provider::ProviderAdapter;
use dgc_daemon::provider::claude::ClaudeAdapter;
use dgc_daemon::schedule::ScheduleManager;
use dgc_daemon::schedule::localtime::{LocalClock, LocalDateTime};
use dgc_daemon::store::Store;
use dgc_daemon::workspace::WorkspaceManager;
use dgc_protocol::ids::{AgentId, ProjectId, WorkspaceId};
use dgc_protocol::methods::{
    AgentCreateParams, AgentSendParams, ScheduleCreateParams, ScheduleListResult,
    ScheduleTargetParams,
};
use dgc_protocol::model::{
    Agent, AgentState, Isolation, ProviderKind, RunOutcome, Schedule, ScheduleState,
    ScheduleTarget, SendBehavior, SendOutcome, Workspace, WorkspaceState,
};
use dgc_protocol::rpc::{INVALID_PARAMS, INVALID_STATE};
use tempfile::{TempDir, tempdir};
use tokio::time::{sleep, timeout};

const NOW: i64 = 1_800_000_015;
const DUE: i64 = 1_800_000_060;
const WAIT: Duration = Duration::from_secs(5);
const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude/scenarios/schedule"
);

struct Utc;

impl LocalClock for Utc {
    fn to_local(&self, unix: i64) -> LocalDateTime {
        let mut tm = MaybeUninit::uninit();
        // SAFETY: gmtime_r initializes tm on success and retains neither pointer.
        let tm = unsafe {
            assert!(!libc::gmtime_r(&unix, tm.as_mut_ptr()).is_null());
            tm.assume_init()
        };
        LocalDateTime {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            offset_secs: 0,
        }
    }

    fn from_local(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Option<i64> {
        // SAFETY: zero initializes all libc tm fields to valid representations.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = y - 1900;
        tm.tm_mon = mo as i32 - 1;
        tm.tm_mday = d as i32;
        tm.tm_hour = h as i32;
        tm.tm_min = mi as i32;
        // SAFETY: timegm reads and normalizes this initialized tm.
        let unix = unsafe { libc::timegm(&mut tm) };
        let actual = self.to_local(unix);
        ((
            actual.year,
            actual.month,
            actual.day,
            actual.hour,
            actual.minute,
        ) == (y, mo, d, h, mi))
            .then_some(unix)
    }
}

struct Harness {
    dir: TempDir,
    paths: DaemonPaths,
    store: Arc<Store>,
    workspace: Workspace,
    agents: Arc<AgentManager>,
    manager: ScheduleManager,
}

impl Harness {
    fn new(scenario: &str, config: DaemonConfig) -> Self {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        let store = Arc::new(Store::open(&paths.state_dir).expect("store"));
        let workspace = Workspace {
            id: WorkspaceId::generate(),
            project_id: ProjectId::generate(),
            isolation: Isolation::Local,
            path: dir.path().to_owned(),
            branch: None,
            base_ref: None,
            state: WorkspaceState::Active,
            owned: false,
            created_at: String::new(),
        };
        store.put_workspace(&workspace).expect("workspace");
        let (agents, manager) = Self::managers(&paths, &store, scenario, config);
        Self {
            dir,
            paths,
            store,
            workspace,
            agents,
            manager,
        }
    }

    fn managers(
        paths: &DaemonPaths,
        store: &Arc<Store>,
        scenario: &str,
        config: DaemonConfig,
    ) -> (Arc<AgentManager>, ScheduleManager) {
        let adapter: Arc<dyn ProviderAdapter> = Arc::new(ClaudeAdapter::with_program(
            PathBuf::from(env!("CARGO_BIN_EXE_dgc-fake-provider")),
            vec![format!("{FIXTURES}/{scenario}.jsonl")],
        ));
        let agents = Arc::new(AgentManager::new(
            Arc::clone(store),
            paths.clone(),
            config,
            HashMap::from([(ProviderKind::Claude, adapter)]),
        ));
        let workspaces = Arc::new(WorkspaceManager::new(
            Arc::clone(store),
            paths.worktrees_dir.clone(),
        ));
        let manager = ScheduleManager::new(
            Arc::clone(store),
            Arc::clone(&agents),
            workspaces,
            Arc::new(Utc),
        )
        .expect("open schedule manager");
        (agents, manager)
    }

    async fn restart(&mut self, scenario: &str, config: DaemonConfig) {
        self.shutdown().await;
        self.store = Arc::new(Store::open(&self.paths.state_dir).expect("reopen store"));
        (self.agents, self.manager) = Self::managers(&self.paths, &self.store, scenario, config);
        self.agents.recover_on_startup().expect("recover agents");
    }

    fn params(&self) -> ScheduleCreateParams {
        ScheduleCreateParams {
            name: Some("Nightly".into()),
            target: ScheduleTargetParams::NewAgent {
                workspace_id: self.workspace.id.clone(),
                provider: Some(ProviderKind::Claude),
                profile: None,
                model: None,
                mode: Some("manual".into()),
            },
            prompt: "first line\nsecond line".into(),
            cron: "* * * * *".into(),
            max_runs: None,
            expires_at: None,
        }
    }

    async fn create(&self) -> Schedule {
        self.manager
            .create_at(self.params(), NOW)
            .await
            .expect("create")
    }

    async fn create_agent(&self) -> Agent {
        self.agents
            .create(AgentCreateParams {
                workspace_id: self.workspace.id.clone(),
                title: "heartbeat target".into(),
                provider: Some(ProviderKind::Claude),
                profile: None,
                model: None,
                mode: Some("manual".into()),
                initial_prompt: "first turn".into(),
                labels: BTreeMap::new(),
                parent_id: None,
                mcp: false,
            })
            .await
            .expect("create target")
    }

    async fn heartbeat(&self, id: AgentId) -> Schedule {
        self.manager
            .create_at(
                ScheduleCreateParams {
                    target: ScheduleTargetParams::Agent { agent_id: id },
                    prompt: "heartbeat".into(),
                    ..self.params()
                },
                NOW,
            )
            .await
            .expect("create heartbeat")
    }

    fn read(&self, schedule: &Schedule) -> Schedule {
        self.store
            .get_schedule(&schedule.id)
            .expect("read schedule")
            .expect("stored schedule")
    }

    async fn shutdown(&self) {
        self.agents.shutdown_all(WAIT).await;
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new("complete", DaemonConfig::default())
    }
}

async fn wait_state(agents: &AgentManager, id: &AgentId, state: AgentState) -> Agent {
    timeout(WAIT, async {
        loop {
            let agent = agents.get(id).expect("read agent");
            if agent.state == state {
                return agent;
            }
            assert_ne!(agent.state, AgentState::Failed, "{:?}", agent.last_error);
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("agent did not reach expected state")
}

#[tokio::test]
async fn create_validates_cron_target_and_limits() {
    let h = Harness::new("complete", profile("manual", &"m".repeat(257)));
    let mut archived = h.workspace.clone();
    archived.id = WorkspaceId::generate();
    archived.state = WorkspaceState::Archived;
    h.store.put_workspace(&archived).expect("archive workspace");
    let mut bad_workspace = h.params();
    if let ScheduleTargetParams::NewAgent { workspace_id, .. } = &mut bad_workspace.target {
        *workspace_id = archived.id;
    }
    let agent = h.create_agent().await;
    wait_state(&h.agents, &agent.id, AgentState::Idle).await;
    h.agents.archive(&agent.id).await.expect("archive target");
    for params in [
        ScheduleCreateParams {
            cron: "nope".into(),
            ..h.params()
        },
        ScheduleCreateParams {
            cron: "0 0 31 2 *".into(),
            ..h.params()
        },
        ScheduleCreateParams {
            prompt: "é".repeat(32 * 1024 + 1),
            ..h.params()
        },
        ScheduleCreateParams {
            max_runs: Some(0),
            ..h.params()
        },
        bad_workspace,
        ScheduleCreateParams {
            target: ScheduleTargetParams::Agent {
                agent_id: AgentId::generate(),
            },
            ..h.params()
        },
        ScheduleCreateParams {
            target: ScheduleTargetParams::Agent { agent_id: agent.id },
            ..h.params()
        },
    ] {
        assert_eq!(
            h.manager
                .create_at(params, NOW)
                .await
                .expect_err("invalid schedule")
                .code(),
            INVALID_PARAMS
        );
    }
    let mut oversized_model = h.params();
    let mut oversized_mode = h.params();
    let mut oversized_profile_model = h.params();
    if let ScheduleTargetParams::NewAgent { model, .. } = &mut oversized_model.target {
        *model = Some("é".repeat(129));
    }
    if let ScheduleTargetParams::NewAgent { mode, .. } = &mut oversized_mode.target {
        *mode = Some("m".repeat(257));
    }
    if let ScheduleTargetParams::NewAgent { profile, .. } = &mut oversized_profile_model.target {
        *profile = Some("automation".into());
    }
    let oversized_fields = [
        (
            "name",
            ScheduleCreateParams {
                name: Some("é".repeat(129)),
                ..h.params()
            },
        ),
        (
            "cron",
            ScheduleCreateParams {
                cron: format!("{}* * * * *", " ".repeat(248)),
                ..h.params()
            },
        ),
        ("model", oversized_model),
        ("mode", oversized_mode),
        ("model", oversized_profile_model),
    ];
    let mut wrong_errors = Vec::new();
    for (field, params) in oversized_fields {
        match h.manager.create_at(params, NOW).await {
            Err(err) if err.code() == INVALID_PARAMS && err.to_string().contains("256") => {}
            Err(err) => wrong_errors.push(format!("{field}: {err}")),
            Ok(_) => wrong_errors.push(format!("{field}: accepted an oversized field")),
        }
    }
    assert!(wrong_errors.is_empty(), "{wrong_errors:?}");
    assert!(h.store.list_schedules().expect("list").is_empty());
    h.manager
        .create_at(
            ScheduleCreateParams {
                prompt: "é".repeat(32 * 1024),
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("64 KiB is allowed");
    for _ in 1..256 {
        h.create().await;
    }
    assert_eq!(
        h.manager
            .create_at(h.params(), NOW)
            .await
            .expect_err("257th schedule")
            .code(),
        INVALID_PARAMS
    );
}

fn profile(mode: &str, model: &str) -> DaemonConfig {
    DaemonConfig {
        profiles: BTreeMap::from([(
            "automation".into(),
            ProfileConfig {
                provider: ProviderKind::Claude,
                model: Some(model.into()),
                mode: Some(mode.into()),
                notes: None,
                mcp: Some(true),
            },
        )]),
        ..DaemonConfig::default()
    }
}

#[tokio::test]
async fn schedule_snapshots_profile_at_creation() {
    let mut h = Harness::new("complete", profile("manual", "haiku"));
    let schedule = h
        .manager
        .create_at(
            ScheduleCreateParams {
                target: ScheduleTargetParams::NewAgent {
                    workspace_id: h.workspace.id.clone(),
                    provider: None,
                    profile: Some("automation".into()),
                    model: None,
                    mode: None,
                },
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("create from profile");
    assert_eq!(
        schedule.target,
        ScheduleTarget::NewAgent {
            workspace_id: h.workspace.id.clone(),
            provider: ProviderKind::Claude,
            model: Some("haiku".into()),
            mode: Some("manual".into()),
            auto_approve: false,
        }
    );
    // Loading this profile is safe: only the previously saved manual schedule runs.
    h.restart("complete", profile("bypassPermissions", "opus"))
        .await;
    h.manager.tick(DUE).await;
    let id = h.read(&schedule).last_agent_id.expect("launched agent");
    let agent = wait_state(&h.agents, &id, AgentState::Stopped).await;
    assert_eq!(agent.mode.as_deref(), Some("manual"));
    assert_eq!(agent.model.as_deref(), Some("haiku"));
    assert!(!agent.auto_approve);
    assert!(!agent.mcp);
    assert_eq!(agent.depth, 0);
    assert!(agent.parent_id.is_none());
}

#[tokio::test]
async fn due_schedule_starts_agent_with_label_and_title() {
    let h = Harness::default();
    let schedule = h.create().await;
    h.manager.tick(DUE - 1).await;
    assert!(h.agents.list().expect("agents").is_empty());
    h.manager.tick(DUE).await;
    let stored = h.read(&schedule);
    let id = stored.last_agent_id.expect("launched agent");
    assert_eq!(stored.runs, 1);
    assert_eq!(stored.next_run_at, Some(DUE + 60));
    assert_eq!(stored.recent_runs[0].scheduled_for, DUE);
    assert_eq!(
        stored.recent_runs[0].outcome,
        RunOutcome::Started {
            agent_id: id.clone()
        }
    );
    let agent = wait_state(&h.agents, &id, AgentState::Stopped).await;
    assert_eq!(agent.title, "Nightly: first line");
    assert_eq!(
        agent.labels,
        BTreeMap::from([("schedule".into(), schedule.id.to_string())])
    );
    assert!(agent.stop_when_idle);
}

#[tokio::test]
async fn scheduled_agent_stops_when_idle_and_can_resume() {
    let mut h = Harness::default();
    let schedule = h.create().await;
    h.manager.tick(DUE).await;
    let id = h.read(&schedule).last_agent_id.expect("launched agent");
    let agent = wait_state(&h.agents, &id, AgentState::Stopped).await;
    assert_eq!(
        agent.provider_session_id.as_deref(),
        Some("schedule-session")
    );
    // Use a separate fixture for the next process, which expects --resume.
    h.restart("resume", DaemonConfig::default()).await;
    let resumed = h.agents.resume(&id).await.expect("resume");
    assert_eq!(resumed.state, AgentState::Starting);
    let idle = wait_state(&h.agents, &id, AgentState::Idle).await;
    assert!(!idle.stop_when_idle);
    assert!(!resumed.stop_when_idle);
    let sent = h
        .agents
        .send(AgentSendParams {
            agent_id: id.clone(),
            text: "follow-up".into(),
            behavior: SendBehavior::Queue,
        })
        .await
        .expect("resumed agent accepts input");
    assert_eq!(sent, SendOutcome::Sent);
    wait_state(&h.agents, &id, AgentState::Idle).await;
    h.shutdown().await;
    assert!(!h.agents.get(&id).expect("persisted agent").stop_when_idle);

    let h = Harness::new("permission", DaemonConfig::default());
    let schedule = h.create().await;
    h.manager.tick(DUE).await;
    let id = h.read(&schedule).last_agent_id.expect("launched agent");
    wait_state(&h.agents, &id, AgentState::WaitingPermission).await;
    h.agents
        .interrupt(&id)
        .await
        .expect("interrupt scheduled agent");
    wait_state(&h.agents, &id, AgentState::Stopped).await;
    assert!(h.agents.pending_permissions(Some(id)).is_empty());
}

#[tokio::test]
async fn schedule_skips_while_previous_run_active() {
    let h = Harness::new("permission", DaemonConfig::default());
    let schedule = h.create().await;
    h.manager.tick(DUE).await;
    let id = h.read(&schedule).last_agent_id.expect("launched agent");
    wait_state(&h.agents, &id, AgentState::WaitingPermission).await;
    let error = h
        .manager
        .run_at(&schedule.id, DUE + 1)
        .await
        .expect_err("previous run active");
    assert_eq!(error.code(), INVALID_STATE);
    assert!(error.to_string().contains("previous_run_active"));
    h.manager.tick(DUE + 60).await;
    let stored = h.read(&schedule);
    assert_eq!(stored.recent_runs.len(), 2);
    assert_eq!(
        stored.recent_runs[1].outcome,
        RunOutcome::Skipped {
            reason: "previous_run_active".into()
        }
    );
    assert_eq!(stored.runs, 1);
    assert_eq!(h.agents.list().expect("agents").len(), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn overdue_runs_are_recorded_missed_not_run() {
    let h = Harness::default();
    let schedule = h.create().await;
    h.manager.tick(DUE + 180).await;
    let stored = h.read(&schedule);
    assert_eq!(stored.runs, 0);
    assert_eq!(stored.recent_runs.len(), 1);
    assert_eq!(stored.recent_runs[0].outcome, RunOutcome::Missed);
    assert_eq!(stored.recent_runs[0].scheduled_for, DUE);
    assert_eq!(stored.recent_runs[0].at, DUE + 180);
    assert_eq!(stored.next_run_at, Some(DUE + 240));
    assert!(h.agents.list().expect("list").is_empty());
}

#[tokio::test]
async fn schedule_tick_recomputes_next_run_when_clock_moves_backwards() {
    let h = Harness::default();
    let schedule = h
        .manager
        .create_at(h.params(), NOW + 3_600)
        .await
        .expect("create while clock is ahead");
    assert_eq!(schedule.next_run_at, Some(DUE + 3_600));
    h.manager.tick(NOW).await;
    let stored = h.read(&schedule);
    assert_eq!(stored.next_run_at, Some(DUE));
    assert_eq!(stored.state, ScheduleState::Active);
    assert_eq!(stored.runs, 0);
    assert!(stored.recent_runs.is_empty());
    assert!(h.agents.list().expect("list agents").is_empty());

    h.manager.tick(NOW + 1).await;
    assert_eq!(h.read(&schedule), stored);
}

#[tokio::test]
async fn missed_after_restart_records_once() {
    let mut h = Harness::default();
    let schedule = h.create().await;
    std::fs::write(h.dir.path().join("state/schedules/corrupt.json"), "{").expect("corrupt file");
    h.restart("complete", DaemonConfig::default()).await;
    let later = DUE + 3 * 24 * 60 * 60;
    h.manager.tick(later).await;
    h.manager.tick(later).await;
    let stored = h.read(&schedule);
    assert_eq!(stored.recent_runs.len(), 1);
    assert_eq!(stored.recent_runs[0].outcome, RunOutcome::Missed);
    assert_eq!(stored.next_run_at, Some(later + 60));
    assert!(h.agents.list().expect("list").is_empty());
}

#[tokio::test]
async fn max_runs_and_expiry_end_the_schedule() {
    let h = Harness::default();
    let limited = h
        .manager
        .create_at(
            ScheduleCreateParams {
                max_runs: Some(1),
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("limited");
    let expired = h
        .manager
        .create_at(
            ScheduleCreateParams {
                expires_at: Some(NOW + 10),
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("expiring");
    h.manager.tick(DUE - 1).await;
    assert_eq!(h.read(&expired).state, ScheduleState::Ended);
    assert_eq!(h.read(&expired).end_reason.as_deref(), Some("expired"));
    h.manager.tick(DUE).await;
    let limited = h.read(&limited);
    assert_eq!(limited.state, ScheduleState::Ended);
    assert_eq!(limited.end_reason.as_deref(), Some("max_runs"));
    assert_eq!(limited.runs, 1);
    h.manager.tick(DUE + 60).await;
    assert_eq!(h.agents.list().expect("only one agent").len(), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn manual_run_counts_and_keeps_next_run() {
    let h = Harness::default();
    let schedule = h.create().await;
    let run = h
        .manager
        .run_at(&schedule.id, NOW + 1)
        .await
        .expect("run now");
    assert_eq!(run.scheduled_for, NOW + 1);
    let stored = h.read(&schedule);
    assert_eq!(stored.recent_runs, vec![run]);
    assert_eq!(stored.next_run_at, schedule.next_run_at);
    assert_eq!(stored.runs, 1);
    assert_eq!(stored.state, ScheduleState::Active);
    h.shutdown().await;
}

#[tokio::test]
async fn manual_run_reaching_max_runs_clears_next_run() {
    let h = Harness::default();
    let schedule = h
        .manager
        .create_at(
            ScheduleCreateParams {
                max_runs: Some(1),
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("create");
    h.manager
        .run_at(&schedule.id, NOW + 1)
        .await
        .expect("manual run");
    h.shutdown().await;
    let stored = h.read(&schedule);
    assert_eq!(stored.state, ScheduleState::Ended);
    assert_eq!(stored.end_reason.as_deref(), Some("max_runs"));
    assert_eq!(stored.runs, 1);
    assert_eq!(stored.next_run_at, None);
}

#[tokio::test]
async fn manual_run_ending_an_expired_or_unavailable_schedule_clears_next_run() {
    let h = Harness::default();
    let expired = h
        .manager
        .create_at(
            ScheduleCreateParams {
                expires_at: Some(NOW + 1),
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("create expiring schedule");
    let unavailable = h.create().await;
    let mut workspace = h.workspace.clone();
    workspace.state = WorkspaceState::Archived;
    h.store
        .put_workspace(&workspace)
        .expect("archive workspace");

    let err = h
        .manager
        .run_at(&expired.id, NOW + 1)
        .await
        .expect_err("expired");
    assert_eq!(err.code(), INVALID_STATE);
    h.manager
        .run_at(&unavailable.id, NOW + 1)
        .await
        .expect("record unavailable result");
    for (schedule, reason) in [(expired, "expired"), (unavailable, "workspace_unavailable")] {
        let stored = h.read(&schedule);
        assert_eq!(stored.state, ScheduleState::Ended);
        assert_eq!(stored.end_reason.as_deref(), Some(reason));
        assert_eq!(stored.next_run_at, None, "{reason}");
    }
}

#[tokio::test]
async fn pause_resume_recomputes_next_run() {
    let h = Harness::default();
    let schedule = h.create().await;
    h.manager.pause(&schedule.id).await.expect("pause");
    h.manager.tick(DUE + 180).await;
    assert!(h.read(&schedule).recent_runs.is_empty());
    let resumed = h
        .manager
        .resume_at(&schedule.id, DUE + 180)
        .await
        .expect("resume");
    assert_eq!(resumed.state, ScheduleState::Active);
    assert_eq!(resumed.next_run_at, Some(DUE + 240));
    assert!(h.agents.list().expect("agents").is_empty());
}

#[tokio::test]
async fn heartbeat_sends_only_when_idle() {
    let h = Harness::new("heartbeat", DaemonConfig::default());
    let agent = h.create_agent().await;
    let heartbeat = h.heartbeat(agent.id.clone()).await;
    wait_state(&h.agents, &agent.id, AgentState::Idle).await;
    h.manager.tick(DUE).await;
    wait_state(&h.agents, &agent.id, AgentState::Running).await;
    h.manager.tick(DUE + 60).await;
    h.shutdown().await;
    h.manager.tick(DUE + 120).await;
    let stored = h.read(&heartbeat);
    let outcomes: Vec<_> = stored
        .recent_runs
        .into_iter()
        .map(|run| run.outcome)
        .collect();
    assert_eq!(
        outcomes,
        [
            RunOutcome::Sent {
                agent_id: agent.id,
                outcome: SendOutcome::Sent
            },
            RunOutcome::Skipped {
                reason: "agent_busy".into()
            },
            RunOutcome::Skipped {
                reason: "agent_not_running".into()
            },
        ]
    );
    assert_eq!(stored.runs, 1);
    assert_eq!(stored.state, ScheduleState::Active);
}

#[tokio::test]
async fn heartbeat_ends_when_agent_archived() {
    let h = Harness::default();
    let agent = h.create_agent().await;
    wait_state(&h.agents, &agent.id, AgentState::Idle).await;
    let heartbeat = h.heartbeat(agent.id.clone()).await;
    h.agents.archive(&agent.id).await.expect("archive");
    h.manager.tick(DUE).await;
    let stored = h.read(&heartbeat);
    assert_eq!(stored.state, ScheduleState::Ended);
    assert_eq!(stored.end_reason.as_deref(), Some("agent_archived"));
    assert_eq!(stored.runs, 0);
}

#[tokio::test]
async fn schedule_ends_when_workspace_archived() {
    let h = Harness::default();
    let agent = h.create_agent().await;
    wait_state(&h.agents, &agent.id, AgentState::Idle).await;
    let heartbeat = h.heartbeat(agent.id).await;
    let schedule = h.create().await;
    let mut workspace = h.workspace.clone();
    workspace.state = WorkspaceState::Archived;
    h.store
        .put_workspace(&workspace)
        .expect("archive workspace");
    h.manager.tick(DUE).await;
    h.shutdown().await;
    for existing in [schedule, heartbeat] {
        let stored = h.read(&existing);
        assert_eq!(stored.state, ScheduleState::Ended);
        assert_eq!(stored.end_reason.as_deref(), Some("workspace_unavailable"));
        assert_eq!(stored.runs, 0);
    }
}

#[tokio::test]
async fn recent_runs_capped_at_20() {
    let h = Harness::default();
    let schedule = h.create().await;
    for index in 0..25 {
        h.manager.tick(DUE + 180 + index * 240).await;
    }
    let stored = h.read(&schedule);
    assert_eq!(stored.recent_runs.len(), 20);
    assert_eq!(stored.recent_runs[0].at, DUE + 180 + 5 * 240);
    assert_eq!(stored.recent_runs[19].at, DUE + 180 + 24 * 240);
    assert!(h.agents.list().expect("list").is_empty());
}

#[tokio::test]
async fn unavailable_provider_records_only_error_kind() {
    let h = Harness::default();
    let schedule = h
        .manager
        .create_at(
            ScheduleCreateParams {
                target: ScheduleTargetParams::NewAgent {
                    workspace_id: h.workspace.id.clone(),
                    provider: Some(ProviderKind::Codex),
                    profile: None,
                    model: None,
                    mode: None,
                },
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("schedule can be saved without the provider installed");
    h.manager.tick(DUE).await;
    let stored = h.read(&schedule);
    assert_eq!(
        stored.recent_runs[0].outcome,
        RunOutcome::Failed {
            error_kind: "provider_unavailable".into()
        }
    );
    assert_eq!(stored.runs, 0);
    assert_eq!(stored.state, ScheduleState::Active);
    assert_eq!(stored.next_run_at, Some(DUE + 60));
}

#[tokio::test]
async fn failed_provider_launch_does_not_count_toward_max_runs() {
    let h = Harness::new("fail_start", DaemonConfig::default());
    let schedule = h
        .manager
        .create_at(
            ScheduleCreateParams {
                max_runs: Some(1),
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("create");
    h.manager.tick(DUE).await;
    let stored = h.read(&schedule);
    assert_eq!(
        stored.recent_runs[0].outcome,
        RunOutcome::Failed {
            error_kind: "provider".into()
        }
    );
    assert_eq!(stored.runs, 0);
    assert_eq!(stored.state, ScheduleState::Active);
    assert_eq!(
        h.agents.list().expect("agent records")[0].state,
        AgentState::Failed
    );
}

#[tokio::test]
async fn schedule_defaults_to_manual_provider_policy() {
    let h = Harness::default();
    let schedule = h
        .manager
        .create_at(
            ScheduleCreateParams {
                target: ScheduleTargetParams::NewAgent {
                    workspace_id: h.workspace.id.clone(),
                    provider: Some(ProviderKind::Claude),
                    profile: None,
                    model: None,
                    mode: None,
                },
                ..h.params()
            },
            NOW,
        )
        .await
        .expect("create");
    let ScheduleTarget::NewAgent {
        mode, auto_approve, ..
    } = schedule.target
    else {
        panic!("expected a new-agent schedule");
    };
    assert_eq!(mode.as_deref(), Some("manual"));
    assert!(!auto_approve);
}

#[tokio::test]
async fn timed_run_persists_next_run_before_starting_the_provider() {
    let h = Harness::default();
    let schedule = h.create().await;
    let observed = h.dir.path().join("observed.json");
    let schedule_file = h
        .paths
        .state_dir
        .join("schedules")
        .join(format!("{}.json", schedule.id));
    let script = h.dir.path().join("snapshot.sh");
    std::fs::write(
        &script,
        "cp \"$1\" \"$2\" || exit 1\nshift 2\nexec \"$@\"\n",
    )
    .expect("write snapshot wrapper");
    let adapter: Arc<dyn ProviderAdapter> = Arc::new(ClaudeAdapter::with_program(
        PathBuf::from("/bin/sh"),
        vec![
            script.to_string_lossy().into_owned(),
            schedule_file.to_string_lossy().into_owned(),
            observed.to_string_lossy().into_owned(),
            env!("CARGO_BIN_EXE_dgc-fake-provider").into(),
            format!("{FIXTURES}/complete.jsonl"),
        ],
    ));
    let agents = Arc::new(AgentManager::new(
        Arc::clone(&h.store),
        h.paths.clone(),
        DaemonConfig::default(),
        HashMap::from([(ProviderKind::Claude, adapter)]),
    ));
    let workspaces = Arc::new(WorkspaceManager::new(
        Arc::clone(&h.store),
        h.paths.worktrees_dir.clone(),
    ));
    let manager = ScheduleManager::new(
        Arc::clone(&h.store),
        Arc::clone(&agents),
        workspaces,
        Arc::new(Utc),
    )
    .expect("open schedule manager");
    manager.tick(DUE).await;
    agents.shutdown_all(WAIT).await;

    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&observed).expect("snapshot from provider launch"))
            .expect("decode snapshot");
    assert_eq!(snapshot["data"]["next_run_at"], DUE + 60);
    assert_eq!(snapshot["data"]["recent_runs"], serde_json::json!([]));
    let stored = h.read(&schedule);
    assert_eq!(stored.runs, 1);
    assert!(matches!(
        stored.recent_runs[0].outcome,
        RunOutcome::Started { .. }
    ));
}

#[tokio::test]
async fn timed_run_does_not_start_if_next_run_cannot_be_saved() {
    let h = Harness::default();
    let schedule = h.create().await;
    let file = h
        .paths
        .state_dir
        .join("schedules")
        .join(format!("{}.json", schedule.id));
    let backup = h.dir.path().join("schedule-backup.json");
    std::fs::rename(&file, &backup).expect("move original schedule");
    std::os::unix::fs::symlink(&backup, &file).expect("block writes");
    h.manager.tick(DUE).await;
    let started = h.agents.list().expect("list agents");
    h.shutdown().await;
    assert!(
        started.is_empty(),
        "an unwritable occurrence was dispatched"
    );

    std::fs::rename(&backup, &file).expect("restore original schedule");
    h.manager.tick(DUE).await;
    h.shutdown().await;
    assert_eq!(h.read(&schedule).runs, 1);
}

#[tokio::test]
async fn timed_run_missed_grace_boundary() {
    for (delay, expected_runs) in [(120, 1), (121, 0)] {
        let h = Harness::default();
        let schedule = h.create().await;
        h.manager.tick(DUE + delay).await;
        h.shutdown().await;
        let stored = h.read(&schedule);
        assert_eq!(stored.runs, expected_runs, "delay {delay}");
        let [run] = stored.recent_runs.as_slice() else {
            panic!("expected one run record at delay {delay}");
        };
        assert_eq!(run.scheduled_for, DUE);
        assert_eq!(run.at, DUE + delay);
        if expected_runs == 1 {
            assert!(matches!(run.outcome, RunOutcome::Started { .. }));
        } else {
            assert_eq!(run.outcome, RunOutcome::Missed);
            assert!(h.agents.list().expect("list agents").is_empty());
        }
    }
}

#[tokio::test]
async fn heartbeat_skips_while_waiting_for_permission() {
    let h = Harness::new("permission", DaemonConfig::default());
    let agent = h.create_agent().await;
    let heartbeat = h.heartbeat(agent.id.clone()).await;
    wait_state(&h.agents, &agent.id, AgentState::WaitingPermission).await;
    h.manager.tick(DUE).await;
    assert_eq!(
        h.agents.get(&agent.id).expect("agent").state,
        AgentState::WaitingPermission
    );
    assert_eq!(
        h.agents.pending_permissions(Some(agent.id.clone())).len(),
        1
    );
    h.agents
        .interrupt(&agent.id)
        .await
        .expect("cancel test turn");
    h.shutdown().await;
    let stored = h.read(&heartbeat);
    assert_eq!(stored.runs, 0);
    assert_eq!(
        stored.recent_runs[0].outcome,
        RunOutcome::Skipped {
            reason: "agent_busy".into()
        }
    );
}

#[tokio::test]
async fn schedule_list_fits_the_wire_at_the_field_limits() {
    let h = Harness::default();
    let mut params = h.params();
    params.name = Some("\0".repeat(256));
    params.cron = format!("{}* * * * *", "\u{b}".repeat(247));
    if let ScheduleTargetParams::NewAgent { model, .. } = &mut params.target {
        *model = Some("\0".repeat(256));
    }
    for _ in 0..256 {
        h.manager
            .create_at(params.clone(), NOW)
            .await
            .expect("256-byte metadata is accepted");
    }
    let response = ScheduleListResult {
        schedules: h.manager.list(None).expect("list schedules"),
    };
    let wire = serde_json::to_vec(&dgc_protocol::rpc::Response::ok(
        1,
        serde_json::to_value(response).expect("encode result"),
    ))
    .expect("encode wire message");
    assert!(
        wire.len() <= dgc_protocol::MAX_MESSAGE_BYTES,
        "list uses {} bytes",
        wire.len()
    );
}
