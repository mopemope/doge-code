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
use dgc_protocol::truncate_summary;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, warn};

use super::{
    AgentError, PendingEntry, QueueItem, SESSION_SHUTDOWN_GRACE, Shared, lock, provider, save_queue,
};
use crate::provider::{AgentSpec, ProviderAdapter, ProviderError, ProviderEvent, ProviderSession};
use crate::time::now_rfc3339;

const COMMAND_CHANNEL_CAPACITY: usize = 64;

/// A request from the manager to a live agent's actor.
pub(super) enum Command {
    Send {
        text: String,
        behavior: SendBehavior,
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

enum Input {
    Command(Option<Command>),
    Event(Option<ProviderEvent>),
}

/// Start the provider session, send the first message, then serve commands
/// and provider events until the agent stops being live.
pub(super) async fn run_actor(
    shared: Arc<Shared>,
    adapter: Arc<dyn ProviderAdapter>,
    mut agent: Agent,
    cwd: PathBuf,
    start: Start,
    queue: VecDeque<QueueItem>,
    commands: mpsc::Receiver<Command>,
) {
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
    let session = match adapter.start(spec).await {
        Ok(session) => session,
        Err(err) => {
            let message = truncate_summary(&err.to_string());
            shared.emit_or_warn(
                &agent.id,
                EventKind::Error {
                    message: message.clone(),
                },
            );
            agent.last_error = Some(message);
            shared.finish(&mut agent, AgentState::Failed);
            return;
        }
    };
    let queue_path = shared.queue_path(&agent.id);
    let mut actor = Actor {
        shared,
        agent,
        session,
        queue,
        queue_path,
    };
    let begun = match start {
        Start::Create { prompt } => actor.session.send_user(&prompt).await.map(|()| true),
        Start::Resume => actor.send_next_queued().await,
    };
    match begun {
        Ok(true) => actor.set_state(AgentState::Running),
        Ok(false) => actor.set_state(AgentState::Idle),
        Err(err) => {
            actor.fail(err.to_string()).await;
            return;
        }
    }
    actor.run(commands).await;
}

struct Actor {
    shared: Arc<Shared>,
    agent: Agent,
    session: Box<dyn ProviderSession>,
    queue: VecDeque<QueueItem>,
    queue_path: PathBuf,
}

impl Actor {
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        loop {
            let input = tokio::select! {
                command = commands.recv() => Input::Command(command),
                event = self.session.events().recv() => Input::Event(event),
            };
            let flow = match input {
                Input::Command(Some(command)) => self.handle_command(command).await,
                // Unreachable while `Shared::live` holds the sender; stop quietly.
                Input::Command(None) => {
                    self.shutdown_session(SESSION_SHUTDOWN_GRACE).await;
                    Flow::Stop
                }
                Input::Event(Some(event)) => self.handle_event(event).await,
                Input::Event(None) => self.exited(false, String::new()).await,
            };
            if matches!(flow, Flow::Stop) {
                break;
            }
        }
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

    fn record_user(&self, text: String, behavior: SendBehavior) {
        self.shared
            .emit_or_warn(&self.agent.id, EventKind::UserMessage { text, behavior });
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
        self.session.send_user(&item.text).await?;
        self.queue.pop_front();
        if let Err(err) = save_queue(&self.queue_path, &self.queue) {
            warn!(agent_id = %self.agent.id, error = %err, "could not persist agent queue");
        }
        self.record_user(item.text, item.behavior);
        Ok(true)
    }

    async fn handle_command(&mut self, command: Command) -> Flow {
        match command {
            Command::Send {
                text,
                behavior,
                reply,
            } => {
                let _ = reply.send(self.send(text, behavior).await);
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
                self.session.send_user(&text).await.map_err(provider)?;
                self.record_user(text, behavior);
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
                    if self.session.steer(&text).await.map_err(provider)? {
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
        self.session.interrupt().await.map_err(provider)?;
        self.cancel_pending();
        if let Err(err) = self.session.send_user(&text).await {
            self.set_state(AgentState::Idle);
            return Err(provider(err));
        }
        self.record_user(text, behavior);
        self.set_state(AgentState::Running);
        Ok(SendOutcome::InterruptedAndSent)
    }

    async fn interrupt(&mut self) -> Result<(), AgentError> {
        match self.agent.state {
            AgentState::Running | AgentState::WaitingPermission => {
                self.session.interrupt().await.map_err(provider)?;
                self.cancel_pending();
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
        self.session
            .respond_permission(&request_id, decision)
            .await
            .map_err(provider)?;
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
                if self.agent.provider_session_id.as_deref() != Some(session_id.as_str()) {
                    self.agent.provider_session_id = Some(session_id);
                    if let Err(err) = self.shared.put_agent(&self.agent) {
                        warn!(agent_id = %self.agent.id, error = %err, "could not store session id");
                    }
                }
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
