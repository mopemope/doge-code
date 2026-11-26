//! Daemon I/O for the TUI.
//!
//! [`Backend::start`] spawns one task that owns two connections to the
//! daemon: an RPC connection that runs [`Effect`]s in order (answering with
//! `Snapshot`, `History` and `RpcDone`), and a subscription to every agent's
//! events (forwarded as `AgentEvent`, ending with `Lagged` when the daemon
//! drops it). When either connection fails it reports
//! `Connection(Disconnected)` once, retries every second and reports
//! `Connection(Connected)` when both are back; `update` reloads then.
//!
//! Only the first connection may start the daemon: after a `daemon stop`
//! the TUI waits for someone to start it again instead of undoing the stop.

use std::path::PathBuf;
use std::time::Duration;

use dgc_client::{Client, ClientError};
use dgc_daemon::paths::DaemonPaths;
use dgc_protocol::event::AgentEvent;
use dgc_protocol::ids::AgentId;
use dgc_protocol::methods::{
    AGENT_EVENTS, AGENT_LIST, AgentEventsParams, AgentListParams, MAX_EVENTS_LIMIT, NOTIFY_EVENT,
    NOTIFY_LAGGED, PERMISSION_LIST, PROJECT_LIST, PermissionListParams, SubscribeParams,
    WORKSPACE_LIST, WorkspaceListParams,
};
use dgc_protocol::model::{Agent, PendingPermission, Project, Workspace};
use dgc_protocol::rpc::Notification;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use super::{AppEvent, ConnectionState, Effect, RequestTag};

/// Delay between reconnection attempts.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Events waiting for the UI; a full channel holds the subscription back
/// until the daemon reports it lagged.
const EVENT_CAPACITY: usize = 256;
/// Answer to requests made while nothing is connected.
const NOT_CONNECTED: &str = "not connected to the daemon";

pub struct Backend {
    paths: DaemonPaths,
    exe: PathBuf,
    effects: mpsc::UnboundedReceiver<Effect>,
    events: mpsc::Sender<AppEvent>,
    /// The next connection attempt may start the daemon (the first only).
    may_spawn: bool,
}

/// Why the backend stops serving a connection.
enum Stop {
    /// The UI dropped its end: the backend ends.
    UiGone,
    /// The connection failed; the text is the cause.
    Lost(String),
}

/// How a subscription task ended.
enum SubscriptionEnd {
    /// The daemon dropped it after `lagged`; a `Resubscribe` replaces it.
    Lagged,
    Closed,
    UiGone,
}

/// Both connections to one daemon.
struct Connection {
    rpc: Client,
    /// `None` after a lag, until `Resubscribe`.
    subscription: Option<JoinHandle<SubscriptionEnd>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Dropping the task's receiver also closes its connection.
        if let Some(task) = self.subscription.take() {
            task.abort();
        }
    }
}

impl Backend {
    /// Start talking to the daemon at `paths` (starting it with
    /// `exe daemon run` if nothing answers the first time). Effects go in,
    /// events come out; the backend ends when the effect sender is dropped.
    /// Must be called within a tokio runtime.
    pub fn start(
        paths: DaemonPaths,
        exe: PathBuf,
    ) -> (mpsc::UnboundedSender<Effect>, mpsc::Receiver<AppEvent>) {
        let (effects_tx, effects) = mpsc::unbounded_channel();
        let (events, events_rx) = mpsc::channel(EVENT_CAPACITY);
        let backend = Self {
            paths,
            exe,
            effects,
            events,
            may_spawn: true,
        };
        tokio::spawn(backend.run());
        (effects_tx, events_rx)
    }

    async fn run(mut self) {
        let mut reported = false;
        loop {
            let Ok(mut connection) = self.connect(reported).await else {
                return;
            };
            if self.emit(connected()).await.is_err() {
                return;
            }
            let cause = match self.serve(&mut connection).await {
                Stop::UiGone => return,
                Stop::Lost(cause) => cause,
            };
            drop(connection);
            if self.emit(disconnected(&cause)).await.is_err() {
                return;
            }
            reported = true;
        }
    }

    /// Connect both connections, retrying every second. The first failure
    /// is reported unless the loss already was.
    async fn connect(&mut self, mut reported: bool) -> Result<Connection, Stop> {
        loop {
            match self.try_connect().await {
                Ok(connection) => return Ok(connection),
                Err(err) if !reported => {
                    self.emit(disconnected(&err.to_string())).await?;
                    reported = true;
                }
                Err(_) => {}
            }
            self.wait_disconnected(RETRY_INTERVAL).await?;
        }
    }

    async fn try_connect(&mut self) -> Result<Connection, ClientError> {
        let rpc = if std::mem::take(&mut self.may_spawn) {
            Client::connect_or_spawn(&self.paths, &self.exe).await?
        } else {
            Client::connect(&self.paths).await?
        };
        // Subscribed before `Connected`, so the snapshot `update` asks for
        // then misses nothing.
        let subscription = Some(self.subscribe().await?);
        Ok(Connection { rpc, subscription })
    }

    /// A new connection subscribed to every agent, forwarded by a task.
    async fn subscribe(&self) -> Result<JoinHandle<SubscriptionEnd>, ClientError> {
        let client = Client::connect(&self.paths).await?;
        let notifications = client.subscribe(SubscribeParams::default()).await?;
        Ok(tokio::spawn(forward(notifications, self.events.clone())))
    }

    /// Wait `duration` without a connection, failing requests at once.
    async fn wait_disconnected(&mut self, duration: Duration) -> Result<(), Stop> {
        let deadline = sleep(duration);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => return Ok(()),
                effect = self.effects.recv() => match effect {
                    Some(effect) => self.reject(effect).await?,
                    None => return Err(Stop::UiGone),
                },
            }
        }
    }

    async fn reject(&self, effect: Effect) -> Result<(), Stop> {
        match effect {
            Effect::Call { request, .. } => {
                self.emit(AppEvent::RpcDone {
                    request,
                    result: Err(NOT_CONNECTED.to_owned()),
                })
                .await
            }
            Effect::LoadHistory {
                agent_id,
                generation,
                ..
            } => {
                self.history_failed(agent_id, generation, NOT_CONNECTED.to_owned())
                    .await
            }
            // `Connected` reloads, and a connection comes subscribed.
            Effect::LoadSnapshot | Effect::Resubscribe | Effect::Quit => Ok(()),
        }
    }

    /// Run effects until the UI goes away or a connection fails.
    async fn serve(&mut self, connection: &mut Connection) -> Stop {
        loop {
            let result = tokio::select! {
                effect = self.effects.recv() => match effect {
                    Some(effect) => self.execute(connection, effect).await,
                    None => Err(Stop::UiGone),
                },
                end = subscription_end(&mut connection.subscription) => match end {
                    SubscriptionEnd::Lagged => Ok(()),
                    SubscriptionEnd::Closed => {
                        Err(Stop::Lost("the daemon closed the connection".to_owned()))
                    }
                    SubscriptionEnd::UiGone => Err(Stop::UiGone),
                },
            };
            if let Err(stop) = result {
                return stop;
            }
        }
    }

    async fn execute(&self, connection: &mut Connection, effect: Effect) -> Result<(), Stop> {
        match effect {
            Effect::LoadSnapshot => match snapshot(&mut connection.rpc).await {
                Ok(snapshot) => self.emit(snapshot).await,
                Err(err) => lost(err).map_or(Ok(()), Err),
            },
            Effect::LoadHistory {
                agent_id,
                since_seq,
                generation,
            } => {
                let params = AgentEventsParams {
                    agent_id: agent_id.clone(),
                    since_seq,
                    limit: MAX_EVENTS_LIMIT,
                };
                match events_page(&mut connection.rpc, &params).await {
                    // Pages are cut by size too: only an empty one is the end.
                    Ok(events) => {
                        self.emit(AppEvent::History {
                            agent_id,
                            done: events.is_empty(),
                            events,
                            generation,
                        })
                        .await
                    }
                    Err(err) => {
                        self.history_failed(agent_id, generation, err.to_string())
                            .await?;
                        lost(err).map_or(Ok(()), Err)
                    }
                }
            }
            Effect::Call {
                request,
                method,
                params,
            } => match connection.rpc.call::<_, Value>(method, &params).await {
                Ok(value) => {
                    self.emit(AppEvent::RpcDone {
                        request,
                        result: Ok(value),
                    })
                    .await
                }
                Err(err) => {
                    self.emit(AppEvent::RpcDone {
                        request,
                        result: Err(err.to_string()),
                    })
                    .await?;
                    lost(err).map_or(Ok(()), Err)
                }
            },
            Effect::Resubscribe => {
                if let Some(task) = connection.subscription.take() {
                    task.abort();
                }
                let task = self
                    .subscribe()
                    .await
                    .map_err(|err| Stop::Lost(err.to_string()))?;
                connection.subscription = Some(task);
                Ok(())
            }
            Effect::Quit => Ok(()),
        }
    }

    /// A history load that cannot finish: the empty last page (so the
    /// stream stops loading) and the error.
    async fn history_failed(
        &self,
        agent_id: AgentId,
        generation: u64,
        message: String,
    ) -> Result<(), Stop> {
        self.emit(AppEvent::History {
            agent_id: agent_id.clone(),
            events: Vec::new(),
            done: true,
            generation,
        })
        .await?;
        self.emit(AppEvent::RpcDone {
            request: RequestTag::LoadHistory { agent_id },
            result: Err(message),
        })
        .await
    }

    async fn emit(&self, event: AppEvent) -> Result<(), Stop> {
        self.events.send(event).await.map_err(|_| Stop::UiGone)
    }
}

fn connected() -> AppEvent {
    AppEvent::Connection(ConnectionState::Connected)
}

fn disconnected(cause: &str) -> AppEvent {
    AppEvent::Connection(ConnectionState::Disconnected {
        reason: format!("{cause} — retrying every second (`dgc-next daemon start` starts it)"),
    })
}

/// `Some` when `err` means the connection is unusable; an error answer
/// or an undecodable result leaves it working.
fn lost(err: ClientError) -> Option<Stop> {
    match err {
        ClientError::Rpc(_) | ClientError::Json(_) => None,
        err => Some(Stop::Lost(err.to_string())),
    }
}

/// Resolves when the subscription task ends (never while there is none).
/// Cancel-safe: the task stays in `slot` until it has ended.
async fn subscription_end(slot: &mut Option<JoinHandle<SubscriptionEnd>>) -> SubscriptionEnd {
    let Some(task) = slot else {
        return std::future::pending().await;
    };
    let end = task.await.unwrap_or(SubscriptionEnd::Closed);
    *slot = None;
    end
}

/// Forward a subscription's notifications as events. An event that does not
/// decode is treated like a lag: the UI resubscribes and reloads.
async fn forward(
    mut notifications: mpsc::Receiver<Notification>,
    events: mpsc::Sender<AppEvent>,
) -> SubscriptionEnd {
    while let Some(notification) = notifications.recv().await {
        let event = match notification.method.as_str() {
            NOTIFY_EVENT => match serde_json::from_value::<AgentEvent>(notification.params) {
                Ok(event) => AppEvent::AgentEvent(event),
                Err(_) => AppEvent::Lagged,
            },
            NOTIFY_LAGGED => AppEvent::Lagged,
            _ => continue,
        };
        let lagged = event == AppEvent::Lagged;
        if events.send(event).await.is_err() {
            return SubscriptionEnd::UiGone;
        }
        if lagged {
            return SubscriptionEnd::Lagged;
        }
    }
    SubscriptionEnd::Closed
}

/// Everything the tree shows, from four list calls.
async fn snapshot(client: &mut Client) -> Result<AppEvent, ClientError> {
    let mut projects: Value = client.call(PROJECT_LIST, &json!({})).await?;
    let mut workspaces: Value = client
        .call(WORKSPACE_LIST, &WorkspaceListParams::default())
        .await?;
    let mut agents: Value = client
        .call(
            AGENT_LIST,
            &AgentListParams {
                include_archived: false,
            },
        )
        .await?;
    let mut permissions: Value = client
        .call(PERMISSION_LIST, &PermissionListParams::default())
        .await?;
    let projects: Vec<Project> = serde_json::from_value(projects["projects"].take())?;
    let workspaces: Vec<Workspace> = serde_json::from_value(workspaces["workspaces"].take())?;
    let agents: Vec<Agent> = serde_json::from_value(agents["agents"].take())?;
    let permissions: Vec<PendingPermission> =
        serde_json::from_value(permissions["permissions"].take())?;
    Ok(AppEvent::Snapshot {
        projects,
        workspaces,
        agents,
        permissions,
    })
}

async fn events_page(
    client: &mut Client,
    params: &AgentEventsParams,
) -> Result<Vec<AgentEvent>, ClientError> {
    let mut result: Value = client.call(AGENT_EVENTS, params).await?;
    Ok(serde_json::from_value(result["events"].take())?)
}
