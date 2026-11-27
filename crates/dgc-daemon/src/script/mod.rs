//! Run byte-for-byte trusted workspace scripts under daemon supervision.

pub mod file;
mod log;
pub(crate) mod trust;

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dgc_protocol::ids::WorkspaceId;
use dgc_protocol::methods::{
    MAX_SCRIPT_LOG_BYTES, ScriptFileState, ScriptListResult, ScriptLogsResult, ScriptState,
    ScriptStatus, ScriptTrustState,
};
use dgc_protocol::model::{Workspace, WorkspaceState};
use dgc_protocol::rpc::{INTERNAL, INVALID_PARAMS, INVALID_STATE};
use tokio::io::AsyncReadExt;
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::{RwLock, RwLockWriteGuard, watch};
use tokio::time::timeout;

use crate::process::{ChildProcess, ChildSpec, ProcessError};
use crate::store::{Store, StoreError};
use crate::workspace::{WorkspaceError, WorkspaceManager};
use file::{ScriptFile, ScriptFileError, open_cwd, valid_name};
use log::{LogFiles, LogWriter};

pub(crate) const STOP_GRACE: Duration = Duration::from_secs(5);
const LOG_LIMIT: u64 = 8 * 1024 * 1024;
const DRAIN_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    File(#[from] ScriptFileError),
    #[error("{0}")]
    InvalidParams(&'static str),
    #[error("{0}")]
    InvalidState(&'static str),
    #[error("script trust store error")]
    Store(#[from] StoreError),
    #[error("script log I/O error")]
    Log(#[from] io::Error),
    #[error("script process error")]
    Process(#[from] ProcessError),
}

impl ScriptError {
    pub fn code(&self) -> i32 {
        match self {
            Self::Workspace(err) => err.code(),
            Self::File(_) | Self::InvalidParams(_) => INVALID_PARAMS,
            Self::InvalidState(_) => INVALID_STATE,
            Self::Store(_) | Self::Log(_) | Self::Process(_) => INTERNAL,
        }
    }
}

#[derive(Clone, Copy)]
enum Execution {
    Running {
        pid: Option<u32>,
    },
    Exited {
        status: Option<ExitStatus>,
        ended_at: i64,
    },
}

#[derive(Clone)]
struct Run {
    stop: watch::Sender<Option<Duration>>,
    execution: watch::Receiver<Execution>,
    started_at: i64,
    oldest_log_offset: Arc<Mutex<u64>>,
}

impl Run {
    fn running(&self) -> bool {
        matches!(*self.execution.borrow(), Execution::Running { .. })
    }

    fn request_stop(&self, grace: Duration) {
        self.stop.send_if_modified(|value| {
            if value.is_some() {
                return false;
            }
            *value = Some(grace);
            true
        });
    }

    async fn wait(mut self) {
        let _ = self
            .execution
            .wait_for(|state| matches!(state, Execution::Exited { .. }))
            .await;
    }

    fn fill(&self, listed: &mut ScriptStatus) {
        listed.started_at = Some(self.started_at);
        match *self.execution.borrow() {
            Execution::Running { pid } => {
                listed.state = ScriptState::Running;
                listed.pid = pid;
            }
            Execution::Exited { status, ended_at } => {
                listed.state = ScriptState::Exited;
                listed.exit_code = status.and_then(|status| status.code());
                listed.signal = status.and_then(|status| status.signal());
                listed.ended_at = Some(ended_at);
            }
        }
    }
}

type Runs = HashMap<WorkspaceId, HashMap<String, Run>>;
type BeforeSpawn = Box<dyn Fn(&Path) + Send + Sync>;

pub struct ScriptManager {
    state_dir: PathBuf,
    store: Store,
    workspaces: Arc<WorkspaceManager>,
    trust_writes: Mutex<()>,
    runs: Mutex<Runs>,
    // Archive holds the write side through its safety check and commit.
    // The value is true once daemon shutdown disables starts permanently.
    start_gate: RwLock<bool>,
    log_limit: u64,
    before_spawn: Option<BeforeSpawn>,
}

impl ScriptManager {
    pub fn new(store_dir: &Path, workspaces: Arc<WorkspaceManager>) -> Result<Self, StoreError> {
        Ok(Self {
            state_dir: store_dir.to_owned(),
            store: Store::open(store_dir)?,
            workspaces,
            trust_writes: Mutex::new(()),
            runs: Mutex::new(HashMap::new()),
            start_gate: RwLock::new(false),
            log_limit: LOG_LIMIT,
            before_spawn: None,
        })
    }

    /// Integration-test seam. A server built with `new` has no hook.
    #[doc(hidden)]
    pub fn with_before_spawn(mut self, hook: impl Fn(&Path) + Send + Sync + 'static) -> Self {
        self.before_spawn = Some(Box::new(hook));
        self
    }

    /// Integration-test seam for reaching rotation with small payloads.
    #[doc(hidden)]
    pub fn with_log_limit(mut self, limit: u64) -> Self {
        self.log_limit = limit.max(1);
        self
    }

    fn active_workspace(&self, id: &WorkspaceId) -> Result<Workspace, ScriptError> {
        let workspace = self.workspaces.get_workspace(id)?;
        if workspace.state == WorkspaceState::Archived {
            return Err(ScriptError::InvalidState("workspace_archived"));
        }
        Ok(workspace)
    }

    fn runs(&self) -> MutexGuard<'_, Runs> {
        lock(&self.runs)
    }

    pub async fn list(&self, id: &WorkspaceId) -> Result<ScriptListResult, ScriptError> {
        let workspace = self.active_workspace(id)?;
        let project = self.workspaces.get_project(&workspace.project_id)?;
        let history = self.store.get_script_trust(&project.id)?;
        let mut result = ScriptListResult {
            project_name: project.name,
            file: ScriptFileState::Ok,
            error: None,
            trust: history.state(None),
            content: None,
            scripts: Vec::new(),
        };
        let definitions = match ScriptFile::read(&workspace.path) {
            Ok(file) => {
                result.trust = history.state(Some(&file.bytes));
                result.content =
                    Some(
                        String::from_utf8(file.bytes).map_err(|_| ScriptFileError::Invalid {
                            reason: "dgc.json must be UTF-8",
                        })?,
                    );
                file.scripts
            }
            Err(ScriptFileError::Missing) => {
                result.file = ScriptFileState::Missing;
                BTreeMap::new()
            }
            Err(err) => {
                result.file = ScriptFileState::Invalid;
                result.error = Some(err.to_string());
                BTreeMap::new()
            }
        };
        let all_runs = self.runs();
        let runs = all_runs.get(id);
        let mut entries: BTreeMap<_, _> = definitions
            .into_iter()
            .map(|(name, def)| (name, Some(def)))
            .collect();
        if let Some(runs) = runs {
            for name in runs.keys() {
                entries.entry(name.clone()).or_default();
            }
        }
        for (name, def) in entries {
            let in_file = def.is_some();
            let (command, cwd) = def.map(|def| (def.command, def.cwd)).unwrap_or_default();
            let mut status = ScriptStatus {
                name,
                in_file,
                command,
                cwd,
                state: ScriptState::NotRunning,
                pid: None,
                exit_code: None,
                signal: None,
                started_at: None,
                ended_at: None,
            };
            if let Some(run) = runs.and_then(|runs| runs.get(&status.name)) {
                run.fill(&mut status);
            }
            result.scripts.push(status);
        }
        Ok(result)
    }

    pub async fn trust(&self, id: &WorkspaceId, expected_content: &str) -> Result<(), ScriptError> {
        let _gate = self.start_gate.read().await;
        let workspace = self.active_workspace(id)?;
        let _write = lock(&self.trust_writes);
        let file = ScriptFile::read(&workspace.path)
            .map_err(|err| ScriptError::InvalidParams(err.kind()))?;
        if file.bytes != expected_content.as_bytes() {
            return Err(ScriptError::InvalidState("script_file_changed"));
        }
        let mut history = self.store.get_script_trust(&workspace.project_id)?;
        history.remember(expected_content);
        self.store
            .put_script_trust(&workspace.project_id, &history)?;
        if file.scripts.is_empty() {
            tracing::info!(workspace_id = %id, "script trusted");
        }
        for script_name in file.scripts.keys() {
            tracing::info!(workspace_id = %id, script_name, "script trusted");
        }
        Ok(())
    }

    pub async fn start(&self, id: &WorkspaceId, name: &str) -> Result<(), ScriptError> {
        let gate = self.start_gate.read().await;
        if *gate {
            return Err(ScriptError::InvalidState("daemon_shutting_down"));
        }
        let workspace = self.active_workspace(id)?;
        if !valid_name(name) {
            return Err(ScriptError::InvalidParams("unknown script name"));
        }
        let mut all_runs = self.runs();
        let runs = all_runs.entry(id.clone()).or_default();
        let previous = runs.get(name);
        if previous.is_some_and(Run::running) {
            return Err(ScriptError::InvalidState("script_running"));
        }

        let mut file = ScriptFile::read(&workspace.path)?;
        let def = file
            .scripts
            .remove(name)
            .ok_or(ScriptError::InvalidParams("unknown script name"))?;
        let history = self.store.get_script_trust(&workspace.project_id)?;
        if history.state(Some(&file.bytes)) != ScriptTrustState::Trusted {
            return Err(ScriptError::InvalidState("script_untrusted"));
        }
        if let Some(hook) = &self.before_spawn {
            hook(&workspace.path);
        }

        let cwd_fd = open_cwd(&workspace.path, def.cwd.as_deref())?;
        let files = LogFiles::open(&self.state_dir, id, name, true)?
            .ok_or_else(|| io::Error::other("script log directory missing"))?;
        let oldest = previous
            .map(|run| Arc::clone(&run.oldest_log_offset))
            .unwrap_or_default();
        let writer = LogWriter::open(files, self.log_limit, Arc::clone(&oldest))?;
        let env = [
            ("DGC_WORKSPACE_ID", OsString::from(id.as_str())),
            (
                "DGC_WORKSPACE_PATH",
                workspace.path.clone().into_os_string(),
            ),
            ("DGC_SCRIPT_NAME", OsString::from(name)),
        ];
        let spec = ChildSpec {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_owned(), def.command],
            cwd: workspace.path,
        };
        let (process, stdout, stderr) = ChildProcess::spawn_raw(spec, &env, cwd_fd)?;
        let pid = process.pid();
        let (stop, stop_rx) = watch::channel(None);
        let (execution_tx, execution) = watch::channel(Execution::Running { pid });
        runs.insert(
            name.to_owned(),
            Run {
                stop,
                execution,
                started_at: now_unix(),
                oldest_log_offset: oldest,
            },
        );
        tracing::info!(workspace_id = %id, script_name = name, pid, "script started");
        tokio::spawn(supervise(
            (id.clone(), name.to_owned()),
            process,
            stdout,
            stderr,
            writer,
            stop_rx,
            execution_tx,
        ));
        Ok(())
    }

    pub async fn stop(&self, id: &WorkspaceId, name: &str) -> Result<(), ScriptError> {
        let workspace = self.active_workspace(id)?;
        let run = self.runs().get(id).and_then(|runs| runs.get(name)).cloned();
        if let Some(run) = run {
            run.request_stop(STOP_GRACE);
            run.wait().await;
        } else if !ScriptFile::read(&workspace.path)?
            .scripts
            .contains_key(name)
        {
            return Err(ScriptError::InvalidParams("unknown script name"));
        }
        Ok(())
    }

    pub async fn logs(
        &self,
        id: &WorkspaceId,
        name: &str,
        offset: Option<u64>,
        max_bytes: Option<u32>,
    ) -> Result<ScriptLogsResult, ScriptError> {
        self.active_workspace(id)?;
        if !valid_name(name) {
            return Err(ScriptError::InvalidParams("invalid script name"));
        }
        let runs = self.runs();
        let run = runs.get(id).and_then(|runs| runs.get(name));
        let running = run.is_some_and(Run::running);
        let oldest = run.map(|run| lock(&run.oldest_log_offset));
        let max_bytes = max_bytes
            .unwrap_or(MAX_SCRIPT_LOG_BYTES)
            .min(MAX_SCRIPT_LOG_BYTES);
        Ok(match LogFiles::open(&self.state_dir, id, name, false)? {
            Some(files) => files.page(
                oldest.as_deref().copied().unwrap_or_default(),
                offset,
                max_bytes,
                running,
            )?,
            None => ScriptLogsResult {
                data: String::new(),
                start_offset: 0,
                next_offset: 0,
                running,
            },
        })
    }

    pub(crate) async fn exclude_starts(&self) -> RwLockWriteGuard<'_, bool> {
        self.start_gate.write().await
    }

    /// The archive handler holds the exclusive start gate while it stops
    /// scripts and archives the workspace.
    pub async fn stop_workspace(&self, id: &WorkspaceId) {
        let runs = self
            .runs()
            .get(id)
            .into_iter()
            .flat_map(HashMap::values)
            .cloned()
            .collect();
        stop_all(runs, STOP_GRACE).await;
    }

    pub async fn shutdown_all(&self, grace: Duration) {
        *self.start_gate.write().await = true;
        let runs = self
            .runs()
            .values()
            .flat_map(HashMap::values)
            .cloned()
            .collect();
        stop_all(runs, grace).await;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

async fn stop_all(runs: Vec<Run>, grace: Duration) {
    for run in &runs {
        run.request_stop(grace);
    }
    for run in runs {
        run.wait().await;
    }
}

async fn wait_or_stop(
    workspace_id: &WorkspaceId,
    script_name: &str,
    process: &mut ChildProcess,
    mut stop: watch::Receiver<Option<Duration>>,
) -> Result<ExitStatus, ProcessError> {
    let grace = async {
        stop.wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|value| *value)
            .unwrap_or_default()
    };
    tokio::select! {
        status = process.wait() => status,
        grace = grace => {
            tracing::info!(%workspace_id, script_name, pid = process.pid(), "script stopping");
            process.terminate(grace).await
        }
    }
}

async fn supervise(
    (workspace_id, script_name): (WorkspaceId, String),
    mut process: ChildProcess,
    stdout: ChildStdout,
    stderr: ChildStderr,
    writer: LogWriter,
    stop: watch::Receiver<Option<Duration>>,
    execution: watch::Sender<Execution>,
) {
    let pid = process.pid();
    let pump = copy_to_log(stdout, stderr, writer);
    tokio::pin!(pump);
    let status = {
        let wait = wait_or_stop(&workspace_id, &script_name, &mut process, stop);
        tokio::pin!(wait);
        tokio::select! {
            status = &mut wait => {
                // wait/terminate kills the group after reaping its leader. The
                // remaining bytes are readable until EOF, unless a process outside
                // that group retained a pipe.
                match timeout(DRAIN_GRACE, &mut pump).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%workspace_id, script_name, error_kind = ?error.kind(), "script log copy failed"),
                    Err(_) => tracing::warn!(%workspace_id, script_name, error_kind = "timeout", "script output pipe did not close"),
                }
                Some(status)
            }
            copied = &mut pump => match copied {
                Ok(()) => Some(wait.await),
                Err(error) => {
                    tracing::warn!(%workspace_id, script_name, error_kind = ?error.kind(), "script log copy failed");
                    None
                }
            }
        }
    };
    let status = match status {
        Some(status) => status,
        None => process.terminate(Duration::ZERO).await,
    };
    // If a wait failed, Drop still owns and kills the group before waiters wake.
    drop(process);
    if status.is_err() {
        tracing::warn!(%workspace_id, script_name, error_kind = "process", "script process wait failed");
    }
    let status = status.ok();
    tracing::info!(
        %workspace_id,
        script_name,
        pid,
        exit_code = status.and_then(|status| status.code()),
        signal = status.and_then(|status| status.signal()),
        "script exited"
    );
    execution.send_replace(Execution::Exited {
        status,
        ended_at: now_unix(),
    });
}

async fn copy_to_log(
    mut stdout: ChildStdout,
    mut stderr: ChildStderr,
    mut writer: LogWriter,
) -> io::Result<()> {
    let mut out_buf = [0; 8192];
    let mut err_buf = [0; 8192];
    let mut out_open = true;
    let mut err_open = true;
    while out_open || err_open {
        tokio::select! {
            count = stdout.read(&mut out_buf), if out_open => {
                let count = count?;
                out_open = count > 0;
                writer.append(&out_buf[..count])?;
            }
            count = stderr.read(&mut err_buf), if err_open => {
                let count = count?;
                err_open = count > 0;
                writer.append(&err_buf[..count])?;
            }
        }
    }
    Ok(())
}

fn now_unix() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}
