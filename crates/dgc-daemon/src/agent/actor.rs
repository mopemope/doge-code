//! Per-agent actor: owns one live agent's provider session and queue, and
//! applies commands and provider events to the agent's state.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dgc_protocol::event::EventKind;
use dgc_protocol::ids::{AgentId, PermissionId};
use dgc_protocol::model::{
    Agent, AgentState, Decision, PendingPermission, PermissionKind, Resolution, SendBehavior,
    SendOutcome,
};
use dgc_protocol::{chunk_text, truncate_summary};
use serde_json::Value;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, warn};

use super::{
    AgentError, PendingEntry, ProviderTimeouts, QueueItem, SESSION_SHUTDOWN_GRACE, Shared, lock,
    provider, save_queue,
};
use crate::provider::{
    AgentSpec, BoxFut, ProviderAdapter, ProviderError, ProviderEvent, ProviderSession,
};
use crate::time::now_rfc3339;

const COMMAND_CHANNEL_CAPACITY: usize = 64;

/// A request from the manager to a live agent's actor.
pub(super) enum Command {
    Send {
        text: String,
        behavior: SendBehavior,
        only_idle: bool,
        reply: oneshot::Sender<Result<SendOutcome, AgentError>>,
    },
    Interrupt {
        reply: oneshot::Sender<Result<(), AgentError>>,
    },
    RespondPermission {
        id: PermissionId,
        decision: Decision,
        reply: oneshot::Sender<Result<(), AgentError>>,
    },
    Archive {
        reply: oneshot::Sender<Result<Agent, AgentError>>,
    },
    Shutdown {
        grace: Duration,
        reply: oneshot::Sender<()>,
    },
}

/// Add a live agent to `live` and return its command receiver.
pub(super) fn register(
    live: &mut HashMap<AgentId, mpsc::Sender<Command>>,
    id: &AgentId,
) -> mpsc::Receiver<Command> {
    let (sender, receiver) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    live.insert(id.clone(), sender);
    receiver
}

pub(super) enum Start {
    Create { prompt: String },
    Resume,
}

enum Flow {
    Continue,
    Stop,
}

/// Await a provider call for at most `limit`.
async fn bounded<T>(
    limit: Duration,
    call: &'static str,
    future: BoxFut<'_, Result<T, ProviderError>>,
) -> Result<T, ProviderError> {
    timeout(limit, future)
        .await
        .unwrap_or(Err(ProviderError::TimedOut(call)))
}

enum Input {
    Command(Option<Command>),
    Event(Option<ProviderEvent>),
}

/// What an actor starts from.
pub(super) struct Launch {
    pub(super) adapter: Arc<dyn ProviderAdapter>,
    pub(super) agent: Agent,
    /// Workspace path.
    pub(super) cwd: PathBuf,
    pub(super) start: Start,
    pub(super) started: Option<oneshot::Sender<Result<(), AgentError>>>,
    pub(super) queue: VecDeque<QueueItem>,
    pub(super) timeouts: ProviderTimeouts,
}

/// Start the provider session, send the first message, then serve commands
/// and provider events until the agent stops being live.
pub(super) async fn run_actor(
    shared: Arc<Shared>,
    launch: Launch,
    commands: mpsc::Receiver<Command>,
) {
    let Launch {
        adapter,
        mut agent,
        cwd,
        start,
        started,
        mut queue,
        timeouts,
    } = launch;
    let resume_session_id = match start {
        Start::Resume => agent.provider_session_id.clone(),
        Start::Create { .. } => None,
    };
    let spec = AgentSpec {
        agent_id: agent.id.clone(),
        cwd,
        model: agent.model.clone(),
        mode: agent.mode.clone(),
        resume_session_id,
    };
    let queue_path = shared.queue_path(&agent.id);
    let session = match bounded(timeouts.start, "start", adapter.start(spec)).await {
        Ok(session) => session,
        Err(err) => {
            if let Start::Create { prompt } = start {
                queue.push_front(QueueItem {
                    text: prompt,
                    behavior: SendBehavior::Queue,
                });
                if save_queue(&queue_path, &queue).is_err() {
                    warn!(agent_id = %agent.id, "could not persist initial prompt in agent queue");
                }
            }
            let message = truncate_summary(&err.to_string());
            shared.emit_or_warn(
                &agent.id,
                EventKind::Error {
                    message: message.clone(),
                },
            );
            agent.last_error = Some(message);
            shared.finish(&mut agent, AgentState::Failed);
            if let Some(started) = started {
                let _ = started.send(Err(provider(err)));
            }
            return;
        }
    };
    let mut actor = Actor {
        shared,
        agent,
        session,
        queue,
        queue_path,
        timeouts,
        deferred_exit: None,
        wedged: None,
    };
    let begun = match start {
        Start::Create { prompt } => {
            bounded(timeouts.call, "send_user", actor.session.send_user(&prompt))
                .await
                .map(|()| true)
        }
        Start::Resume => actor.send_next_queued().await,
    };
    match begun {
        Ok(true) => actor.set_state(AgentState::Running),
        Ok(false) => actor.set_state(AgentState::Idle),
        Err(err) => {
            actor.fail(err.to_string()).await;
            if let Some(started) = started {
                let _ = started.send(Err(provider(err)));
            }
            return;
        }
    }
    if let Some(started) = started {
        let _ = started.send(Ok(()));
    }
    actor.run(commands).await;
}

struct Actor {
    shared: Arc<Shared>,
    agent: Agent,
    session: Box<dyn ProviderSession>,
    queue: VecDeque<QueueItem>,
    queue_path: PathBuf,
    timeouts: ProviderTimeouts,
    /// An exit seen while draining events after an interrupt; handled by
    /// the main loop before anything else.
    deferred_exit: Option<(bool, String)>,
    /// Message of a provider call that timed out; the main loop fails the
    /// agent once the current command has been answered.
    wedged: Option<String>,
}

impl Actor {
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        loop {
            if let Some((success, stderr_tail)) = self.deferred_exit.take() {
                self.exited(success, stderr_tail).await;
                break;
            }
            if self.agent.stop_when_idle
                && self.agent.state == AgentState::Idle
                && self.queue.is_empty()
                && commands.is_empty()
                && !self.has_pending()
            {
                self.shutdown_session(SESSION_SHUTDOWN_GRACE).await;
                self.shared.finish(&mut self.agent, AgentState::Stopped);
                break;
            }
            // Biased toward provider events: events already buffered are
            // applied before a command that arrived after them.
            let input = tokio::select! {
                biased;
                event = self.session.events().recv() => Input::Event(event),
                command = commands.recv() => Input::Command(command),
            };
            let mut flow = match input {
                Input::Command(Some(command)) => self.handle_command(command).await,
                // Unreachable while `Shared::live` holds the sender; stop quietly.
                Input::Command(None) => {
                    self.shutdown_session(SESSION_SHUTDOWN_GRACE).await;
                    Flow::Stop
                }
                Input::Event(Some(event)) => self.handle_event(event).await,
                Input::Event(None) => self.exited(false, String::new()).await,
            };
            if matches!(flow, Flow::Continue)
                && let Some(message) = self.wedged.take()
            {
                flow = self.fail(message).await;
            }
            if matches!(flow, Flow::Stop) {
                break;
            }
        }
    }

    /// Remember a timed-out call so the main loop fails the agent.
    fn note<T>(&mut self, result: Result<T, ProviderError>) -> Result<T, ProviderError> {
        if let Err(err @ ProviderError::TimedOut(_)) = &result {
            self.wedged = Some(err.to_string());
        }
        result
    }

    fn set_state(&mut self, state: AgentState) {
        if self.agent.state == state {
            return;
        }
        if let Err(err) = self.shared.store_state(&mut self.agent, state) {
            warn!(agent_id = %self.agent.id, ?state, error = %err, "could not store agent state");
        }
        debug!(agent_id = %self.agent.id, ?state, "agent state changed");
    }

    /// Record a message merged into the running turn.
    fn record_user(&self, text: String, behavior: SendBehavior) {
        if let Err(err) = self.shared.emit_user(&self.agent.id, &text, behavior) {
            warn!(agent_id = %self.agent.id, error = %err, "could not record agent event");
        }
    }

    /// Record a message that starts a turn: the message, then `TurnStarted`.
    fn record_turn_start(&self, text: String, behavior: SendBehavior) {
        if let Err(err) = self.shared.emit_turn_start(&self.agent.id, &text, behavior) {
            warn!(agent_id = %self.agent.id, error = %err, "could not record agent event");
        }
    }

    fn has_pending(&self) -> bool {
        lock(&self.shared.permissions)
            .values()
            .any(|entry| entry.permission.agent_id == self.agent.id)
    }

    /// Resolve every pending permission of this agent as cancelled.
    fn cancel_pending(&mut self) {
        let mut cancelled = Vec::new();
        lock(&self.shared.permissions).retain(|id, entry| {
            let mine = entry.permission.agent_id == self.agent.id;
            if mine {
                cancelled.push(id.clone());
            }
            !mine
        });
        cancelled.sort();
        for id in cancelled {
            self.shared.emit_or_warn(
                &self.agent.id,
                EventKind::PermissionResolved {
                    id,
                    resolution: Resolution::Cancelled,
                },
            );
        }
    }

    async fn shutdown_session(&mut self, grace: Duration) {
        match timeout(grace, self.session.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => warn!(agent_id = %self.agent.id, "provider shutdown failed"),
            Err(_) => warn!(agent_id = %self.agent.id, "provider shutdown timed out"),
        }
    }

    /// Mark the agent failed with `message` (also recorded as an `error`
    /// event). The queue stays persisted for a later resume.
    async fn fail(&mut self, message: String) -> Flow {
        let message = truncate_summary(&message);
        self.shared.emit_or_warn(
            &self.agent.id,
            EventKind::Error {
                message: message.clone(),
            },
        );
        self.shutdown_session(SESSION_SHUTDOWN_GRACE).await;
        self.cancel_pending();
        self.agent.last_error = Some(message);
        self.shared.finish(&mut self.agent, AgentState::Failed);
        Flow::Stop
    }

    /// Send the head of the queue as a new turn. `Ok(false)` when empty; the
    /// item stays queued if sending fails.
    async fn send_next_queued(&mut self) -> Result<bool, ProviderError> {
        let Some(item) = self.queue.front().cloned() else {
            return Ok(false);
        };
        bounded(
            self.timeouts.call,
            "send_user",
            self.session.send_user(&item.text),
        )
        .await?;
        self.queue.pop_front();
        if let Err(err) = save_queue(&self.queue_path, &self.queue) {
            warn!(agent_id = %self.agent.id, error = %err, "could not persist agent queue");
        }
        self.record_turn_start(item.text, item.behavior);
        Ok(true)
    }

    /// Drop every queued message (an interrupt cancels what was lined up
    /// behind the interrupted turn): persist the empty queue, then record one
    /// `QueueDiscarded` event per message (split like `UserMessage`).
    fn discard_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        let discarded = std::mem::take(&mut self.queue);
        if let Err(err) = save_queue(&self.queue_path, &self.queue) {
            warn!(agent_id = %self.agent.id, error = %err, "could not persist agent queue");
        }
        debug!(agent_id = %self.agent.id, count = discarded.len(), "discarded queued messages");
        for item in discarded {
            for text in chunk_text(&item.text) {
                self.shared.emit_or_warn(
                    &self.agent.id,
                    EventKind::QueueDiscarded {
                        text,
                        behavior: item.behavior,
                    },
                );
            }
        }
    }

    async fn handle_command(&mut self, command: Command) -> Flow {
        match command {
            Command::Send {
                text,
                behavior,
                only_idle,
                reply,
            } => {
                let result = if only_idle && self.agent.state != AgentState::Idle {
                    Err(AgentError::state(self.agent.state))
                } else {
                    self.send(text, behavior).await
                };
                let _ = reply.send(result);
                Flow::Continue
            }
            Command::Interrupt { reply } => {
                let _ = reply.send(self.interrupt().await);
                Flow::Continue
            }
            Command::RespondPermission {
                id,
                decision,
                reply,
            } => {
                let _ = reply.send(self.respond_permission(id, decision).await);
                Flow::Continue
            }
            Command::Archive { reply } => {
                self.shutdown_session(SESSION_SHUTDOWN_GRACE).await;
                self.cancel_pending();
                self.shared.finish(&mut self.agent, AgentState::Archived);
                let _ = reply.send(Ok(self.agent.clone()));
                Flow::Stop
            }
            Command::Shutdown { grace, reply } => {
                self.shutdown_session(grace).await;
                self.cancel_pending();
                self.shared.finish(&mut self.agent, AgentState::Stopped);
                let _ = reply.send(());
                Flow::Stop
            }
        }
    }

    async fn send(
        &mut self,
        text: String,
        behavior: SendBehavior,
    ) -> Result<SendOutcome, AgentError> {
        match self.agent.state {
            AgentState::Idle => {
                let sent = bounded(
                    self.timeouts.call,
                    "send_user",
                    self.session.send_user(&text),
                )
                .await;
                self.note(sent).map_err(provider)?;
                self.record_turn_start(text, behavior);
                self.set_state(AgentState::Running);
                Ok(SendOutcome::Sent)
            }
            AgentState::Running | AgentState::WaitingPermission => match behavior {
                SendBehavior::Queue => {
                    self.queue.push_back(QueueItem { text, behavior });
                    if let Err(err) = save_queue(&self.queue_path, &self.queue) {
                        self.queue.pop_back();
                        return Err(err);
                    }
                    Ok(SendOutcome::Queued)
                }
                SendBehavior::Steer => {
                    let steered =
                        bounded(self.timeouts.call, "steer", self.session.steer(&text)).await;
                    if self.note(steered).map_err(provider)? {
                        self.record_user(text, behavior);
                        Ok(SendOutcome::Steered)
                    } else {
                        self.interrupt_and_send(text, behavior).await
                    }
                }
                SendBehavior::Interrupt => self.interrupt_and_send(text, behavior).await,
            },
            state => Err(AgentError::state(state)),
        }
    }

    async fn interrupt_and_send(
        &mut self,
        text: String,
        behavior: SendBehavior,
    ) -> Result<SendOutcome, AgentError> {
        self.interrupt_session().await?;
        self.cancel_pending();
        self.discard_queue();
        if self.deferred_exit.is_some() {
            self.set_state(AgentState::Idle);
            return Err(provider(ProviderError::Exited));
        }
        let sent = bounded(
            self.timeouts.call,
            "send_user",
            self.session.send_user(&text),
        )
        .await;
        if let Err(err) = self.note(sent) {
            self.set_state(AgentState::Idle);
            return Err(provider(err));
        }
        self.record_turn_start(text, behavior);
        self.set_state(AgentState::Running);
        Ok(SendOutcome::InterruptedAndSent)
    }

    /// Interrupt the provider, then apply what it emitted before the
    /// interrupt returned. Turn-ending and permission events in that buffer
    /// belong to the interrupted turn: `TurnCompleted` is recorded without a
    /// transition and permission requests are dropped. An exit is deferred to
    /// the main loop.
    async fn interrupt_session(&mut self) -> Result<(), AgentError> {
        let interrupted = bounded(self.timeouts.call, "interrupt", self.session.interrupt()).await;
        self.note(interrupted).map_err(provider)?;
        loop {
            match self.session.events().try_recv() {
                Ok(ProviderEvent::TurnCompleted { usage }) => self
                    .shared
                    .emit_or_warn(&self.agent.id, EventKind::TurnCompleted { usage }),
                Ok(ProviderEvent::PermissionRequested { .. }) => {
                    debug!(agent_id = %self.agent.id, "dropped permission request of interrupted turn");
                }
                Ok(ProviderEvent::SessionStarted { session_id }) => {
                    self.session_started(session_id)
                }
                Ok(ProviderEvent::Event(kind)) => self.shared.emit_or_warn(&self.agent.id, kind),
                Ok(ProviderEvent::Exited {
                    success,
                    stderr_tail,
                }) => {
                    self.deferred_exit = Some((success, stderr_tail));
                    return Ok(());
                }
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    self.deferred_exit = Some((false, String::new()));
                    return Ok(());
                }
            }
        }
    }

    async fn interrupt(&mut self) -> Result<(), AgentError> {
        match self.agent.state {
            AgentState::Running | AgentState::WaitingPermission => {
                self.interrupt_session().await?;
                self.cancel_pending();
                self.discard_queue();
                self.set_state(AgentState::Idle);
                Ok(())
            }
            state => Err(AgentError::state(state)),
        }
    }

    async fn respond_permission(
        &mut self,
        id: PermissionId,
        decision: Decision,
    ) -> Result<(), AgentError> {
        let request_id = lock(&self.shared.permissions)
            .get(&id)
            .filter(|entry| entry.permission.agent_id == self.agent.id)
            .map(|entry| entry.provider_request_id.clone())
            .ok_or(AgentError::NotFound("permission"))?;
        let responded = bounded(
            self.timeouts.call,
            "respond_permission",
            self.session.respond_permission(&request_id, decision),
        )
        .await;
        self.note(responded).map_err(provider)?;
        lock(&self.shared.permissions).remove(&id);
        self.shared.emit_or_warn(
            &self.agent.id,
            EventKind::PermissionResolved {
                id,
                resolution: Resolution::Decided(decision),
            },
        );
        if self.agent.state == AgentState::WaitingPermission && !self.has_pending() {
            self.set_state(AgentState::Running);
        }
        Ok(())
    }

    async fn handle_event(&mut self, event: ProviderEvent) -> Flow {
        match event {
            ProviderEvent::SessionStarted { session_id } => {
                self.session_started(session_id);
                Flow::Continue
            }
            ProviderEvent::Event(kind) => {
                self.shared.emit_or_warn(&self.agent.id, kind);
                Flow::Continue
            }
            ProviderEvent::PermissionRequested {
                provider_request_id,
                kind,
                summary,
                raw,
            } => {
                self.request_permission(provider_request_id, kind, summary, raw);
                Flow::Continue
            }
            ProviderEvent::TurnCompleted { usage } => self.turn_completed(usage).await,
            ProviderEvent::Exited {
                success,
                stderr_tail,
            } => self.exited(success, stderr_tail).await,
        }
    }

    fn session_started(&mut self, session_id: String) {
        if self.agent.provider_session_id.as_deref() != Some(session_id.as_str()) {
            self.agent.provider_session_id = Some(session_id);
            if let Err(err) = self.shared.put_agent(&self.agent) {
                warn!(agent_id = %self.agent.id, error = %err, "could not store session id");
            }
        }
    }

    fn request_permission(
        &mut self,
        provider_request_id: String,
        kind: PermissionKind,
        summary: String,
        raw: Value,
    ) {
        let permission = PendingPermission::new(
            PermissionId::generate(),
            self.agent.id.clone(),
            kind,
            summary,
            raw,
            now_rfc3339(),
        );
        lock(&self.shared.permissions).insert(
            permission.id.clone(),
            PendingEntry {
                permission: permission.clone(),
                provider_request_id,
            },
        );
        debug!(agent_id = %self.agent.id, permission_id = %permission.id, "permission requested");
        self.shared.emit_or_warn(
            &self.agent.id,
            EventKind::PermissionRequested { permission },
        );
        self.set_state(AgentState::WaitingPermission);
    }

    async fn turn_completed(&mut self, usage: Option<Value>) -> Flow {
        self.shared
            .emit_or_warn(&self.agent.id, EventKind::TurnCompleted { usage });
        if !matches!(
            self.agent.state,
            AgentState::Running | AgentState::WaitingPermission
        ) {
            return Flow::Continue;
        }
        // The turn is over; whatever it was waiting for no longer applies.
        self.cancel_pending();
        match self.send_next_queued().await {
            Ok(true) => {
                self.set_state(AgentState::Running);
                Flow::Continue
            }
            Ok(false) => {
                self.set_state(AgentState::Idle);
                Flow::Continue
            }
            Err(err) => self.fail(err.to_string()).await,
        }
    }

    /// The provider ended on its own: the agent failed. The message is the
    /// stderr tail when there is one.
    async fn exited(&mut self, success: bool, stderr_tail: String) -> Flow {
        let message = if stderr_tail.trim().is_empty() {
            format!("provider exited unexpectedly (success: {success})")
        } else {
            stderr_tail
        };
        self.fail(message).await
    }
}
