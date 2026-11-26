//! Agent commands: `run`, `ls`, `show`, `send`, `interrupt`, `archive`,
//! `resume`, `logs`.

use std::process::ExitCode;

use clap::{Subcommand, ValueEnum};
use dgc_client::Client;
use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::{AgentId, WorkspaceId};
use dgc_protocol::methods::{
    AGENT_ARCHIVE, AGENT_CREATE, AGENT_EVENTS, AGENT_GET, AGENT_INTERRUPT, AGENT_LIST,
    AGENT_RESUME, AGENT_SEND, AgentCreateParams, AgentEventsParams, AgentIdParams, AgentListParams,
    AgentSendParams, LaggedParams, MAX_EVENTS_LIMIT, NOTIFY_EVENT, NOTIFY_LAGGED, SubscribeParams,
};
use dgc_protocol::model::{Agent, AgentState, ProviderKind, SendBehavior, SendOutcome};
use serde_json::Value;

use super::{CliResult, connect};
use crate::output;

/// Longest default title taken from the prompt, in chars.
const DEFAULT_TITLE_CHARS: usize = 60;

#[derive(Clone, Copy, ValueEnum)]
pub enum Provider {
    Claude,
    Codex,
}

impl From<Provider> for ProviderKind {
    fn from(provider: Provider) -> Self {
        match provider {
            Provider::Claude => ProviderKind::Claude,
            Provider::Codex => ProviderKind::Codex,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Behavior {
    Queue,
    Steer,
    Interrupt,
}

impl From<Behavior> for SendBehavior {
    fn from(behavior: Behavior) -> Self {
        match behavior {
            Behavior::Queue => SendBehavior::Queue,
            Behavior::Steer => SendBehavior::Steer,
            Behavior::Interrupt => SendBehavior::Interrupt,
        }
    }
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Start an agent in a workspace and print its id.
    Run {
        /// Workspace the agent works in.
        #[arg(long)]
        workspace: WorkspaceId,
        #[arg(long, value_enum)]
        provider: Provider,
        /// Provider model name.
        #[arg(long)]
        model: Option<String>,
        /// Provider permission mode (Claude `--permission-mode`; Codex
        /// `read-only`, `workspace-write`, `untrusted`, `full-access`).
        #[arg(long)]
        mode: Option<String>,
        /// Title (default: the first line of the prompt, cut to 60 chars).
        #[arg(long)]
        title: Option<String>,
        /// Stream the agent's events until it is idle, stopped, failed or
        /// archived (exit status 0 only when idle).
        #[arg(long)]
        follow: bool,
        /// The initial prompt.
        prompt: String,
    },
    /// List agents (archived ones only with --all).
    Ls {
        #[arg(long)]
        all: bool,
    },
    /// Show one agent.
    Show { id: AgentId },
    /// Send a message to an agent.
    Send {
        id: AgentId,
        /// What to do when the agent is busy.
        #[arg(long, value_enum, default_value = "queue")]
        behavior: Behavior,
        text: String,
    },
    /// Stop the agent's running turn.
    Interrupt { id: AgentId },
    /// Stop the agent and archive it.
    Archive { id: AgentId },
    /// Restart a stopped or failed agent from its provider session.
    Resume { id: AgentId },
    /// Print the agent's events (with --follow, keep streaming new ones).
    Logs {
        id: AgentId,
        /// Only events after this seq.
        #[arg(long, default_value_t = 0)]
        since: u64,
        #[arg(long)]
        follow: bool,
    },
}

pub async fn run(command: AgentCommand, json: bool) -> CliResult<ExitCode> {
    let mut client = connect().await?;
    match command {
        AgentCommand::Run {
            workspace,
            provider,
            model,
            mode,
            title,
            follow,
            prompt,
        } => {
            let title = title.unwrap_or_else(|| default_title(&prompt));
            let params = AgentCreateParams {
                workspace_id: workspace,
                title,
                provider: provider.into(),
                model,
                mode,
                initial_prompt: prompt,
                labels: Default::default(),
                parent_id: None,
            };
            let agent: Agent = client.call(AGENT_CREATE, &params).await?;
            if agent.auto_approve {
                eprintln!(
                    "warning: agent {} runs in auto-approve mode {}: the provider acts \
                     without asking for permission",
                    agent.id,
                    output::escape_line(agent.mode.as_deref().unwrap_or("-"))
                );
            }
            if json {
                println!("{}", serde_json::to_string(&agent)?);
            } else {
                println!("{}", agent.id);
            }
            if follow {
                drop(client);
                return follow_until_settled(&agent.id, json).await;
            }
        }
        AgentCommand::Ls { all } => {
            let result: Value = client
                .call(
                    AGENT_LIST,
                    &AgentListParams {
                        include_archived: all,
                    },
                )
                .await?;
            if json {
                println!("{result}");
            } else {
                let agents: Vec<Agent> = serde_json::from_value(result["agents"].clone())?;
                for agent in &agents {
                    println!("{}", output::agent_line(agent));
                }
            }
        }
        AgentCommand::Show { id } => {
            let agent: Agent = client.call(AGENT_GET, &AgentIdParams { id }).await?;
            if json {
                println!("{}", serde_json::to_string(&agent)?);
            } else {
                println!("{}", output::agent_details(&agent));
            }
        }
        AgentCommand::Send { id, behavior, text } => {
            let params = AgentSendParams {
                agent_id: id,
                text,
                behavior: behavior.into(),
            };
            let result: Value = client.call(AGENT_SEND, &params).await?;
            if json {
                println!("{result}");
            } else {
                let outcome: SendOutcome = serde_json::from_value(result["outcome"].clone())?;
                println!("{}", output::outcome_name(outcome));
            }
        }
        AgentCommand::Interrupt { id } => {
            let result: Value = client.call(AGENT_INTERRUPT, &AgentIdParams { id }).await?;
            print_done(json, &result, "interrupted");
        }
        AgentCommand::Archive { id } => {
            let agent: Agent = client.call(AGENT_ARCHIVE, &AgentIdParams { id }).await?;
            print_agent(&agent, json)?;
        }
        AgentCommand::Resume { id } => {
            let agent: Agent = client.call(AGENT_RESUME, &AgentIdParams { id }).await?;
            print_agent(&agent, json)?;
        }
        AgentCommand::Logs { id, since, follow } => {
            if follow {
                drop(client);
                follow_events(&id, since, json, |_| None::<()>).await?;
            } else {
                print_history(&mut client, &id, since, json).await?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn default_title(prompt: &str) -> String {
    prompt
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(DEFAULT_TITLE_CHARS)
        .collect()
}

fn print_agent(agent: &Agent, json: bool) -> CliResult<()> {
    if json {
        println!("{}", serde_json::to_string(agent)?);
    } else {
        println!("{}", output::agent_line(agent));
    }
    Ok(())
}

/// `{}`-result commands: the JSON result or a fixed word.
pub fn print_done(json: bool, result: &Value, word: &str) {
    if json {
        println!("{result}");
    } else {
        println!("{word}");
    }
}

fn print_event(event: &AgentEvent, json: bool) -> CliResult<()> {
    if json {
        println!("{}", serde_json::to_string(event)?);
    } else {
        println!("{}", output::event_line(event));
    }
    Ok(())
}

/// Print every stored event after `since`, page by page.
async fn print_history(client: &mut Client, id: &AgentId, since: u64, json: bool) -> CliResult<()> {
    let mut cursor = since;
    loop {
        let params = AgentEventsParams {
            agent_id: id.clone(),
            since_seq: cursor,
            limit: MAX_EVENTS_LIMIT,
        };
        let result: Value = client.call(AGENT_EVENTS, &params).await?;
        let events: Vec<AgentEvent> = serde_json::from_value(result["events"].clone())?;
        // Pages are also cut by size: only an empty page ends the history.
        if events.is_empty() {
            return Ok(());
        }
        for event in &events {
            print_event(event, json)?;
            cursor = event.seq;
        }
    }
}

/// States that end `run --follow`.
fn settled(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Idle | AgentState::Stopped | AgentState::Failed | AgentState::Archived
    )
}

/// Stream the agent's events from the beginning until it settles; the exit
/// status is 0 when it settled idle.
async fn follow_until_settled(id: &AgentId, json: bool) -> CliResult<ExitCode> {
    let state = follow_events(id, 0, json, |event| match event.kind {
        EventKind::StateChanged { state } if settled(state) => Some(state),
        _ => None,
    })
    .await?;
    Ok(match state {
        Some(AgentState::Idle) => ExitCode::SUCCESS,
        Some(_) => ExitCode::FAILURE,
        None => {
            eprintln!("error: the daemon closed the connection before agent {id} settled");
            ExitCode::FAILURE
        }
    })
}

/// Print events after `since` (backfilled by the daemon), then live ones,
/// until `stop` returns a value or the daemon closes the connection. After a
/// `lagged` notice the stream is re-subscribed from the last printed seq.
async fn follow_events<T>(
    id: &AgentId,
    since: u64,
    json: bool,
    stop: impl Fn(&AgentEvent) -> Option<T>,
) -> CliResult<Option<T>> {
    let mut last = since;
    loop {
        let client = connect().await?;
        let params = SubscribeParams {
            agent_id: Some(id.clone()),
            since_seq: Some(last),
        };
        let mut notifications = client.subscribe(params).await?;
        let mut lagged = false;
        while let Some(notification) = notifications.recv().await {
            match notification.method.as_str() {
                NOTIFY_EVENT => {
                    let event: AgentEvent = serde_json::from_value(notification.params)?;
                    if event.agent_id != *id || event.seq <= last {
                        continue;
                    }
                    last = event.seq;
                    print_event(&event, json)?;
                    if let Some(done) = stop(&event) {
                        return Ok(Some(done));
                    }
                }
                NOTIFY_LAGGED => {
                    let notice: LaggedParams = serde_json::from_value(notification.params)?;
                    if notice.agent_id.as_ref().is_none_or(|agent| agent == id) {
                        lagged = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !lagged {
            // The daemon closed the connection (it is shutting down).
            return Ok(None);
        }
    }
}
