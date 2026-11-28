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
        #[arg(long, value_enum, required_unless_present = "profile")]
        provider: Option<Provider>,
        /// Named launch profile from the daemon's config.
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
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
        /// Enable the dgc MCP server for this agent.
        #[arg(long)]
        mcp: bool,
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
        /// Print the newest N stored events, ignoring --since.
        #[arg(long, value_name = "N")]
        tail: Option<u32>,
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
            profile,
            model,
            mode,
            title,
            mcp,
            follow,
            prompt,
        } => {
            let title = title.unwrap_or_else(|| default_title(&prompt));
            let params = AgentCreateParams {
                workspace_id: workspace,
                title,
                provider: provider.map(Into::into),
                profile,
                model,
                mode,
                initial_prompt: prompt,
                labels: Default::default(),
                parent_id: None,
                mcp,
            };
            let agent: Agent = client.call(AGENT_CREATE, &params).await?;
            warn_auto_approve(&agent);
            if json {
                println!(
                    "{}",
                    output::escape_json_line(&serde_json::to_string(&agent)?)
                );
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
                println!("{}", output::escape_json_line(&result.to_string()));
            } else {
                let agents: Vec<Agent> = serde_json::from_value(result["agents"].clone())?;
                for entry in output::agent_tree(&agents) {
                    println!("{}", output::agent_tree_line(&entry));
                }
            }
        }
        AgentCommand::Show { id } => {
            let agent: Agent = client.call(AGENT_GET, &AgentIdParams { id }).await?;
            if json {
                println!(
                    "{}",
                    output::escape_json_line(&serde_json::to_string(&agent)?)
                );
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
        AgentCommand::Logs {
            id,
            since,
            tail,
            follow,
        } => {
            let mut output = FollowOutput {
                json,
                suggested: false,
            };
            let since = match tail {
                Some(n) => print_tail(&mut client, &id, n, follow, &mut output).await?,
                None => since,
            };
            if follow {
                drop(client);
                follow_events(&id, since, output, |_| None::<()>).await?;
            } else if tail.is_none() {
                print_history(&mut client, &id, since, json).await?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn default_title(prompt: &str) -> String {
    prompt
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(DEFAULT_TITLE_CHARS)
        .collect()
}

pub(super) fn warn_auto_approve(agent: &Agent) {
    if agent.auto_approve {
        eprintln!(
            "warning: agent {} runs in auto-approve mode {}: the provider acts \
             without asking for permission",
            agent.id,
            output::escape_line(agent.mode.as_deref().unwrap_or("-"))
        );
    }
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
            tail: None,
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

/// Print one tail page and return its newest sequence for `--follow`.
async fn print_tail(
    client: &mut Client,
    id: &AgentId,
    n: u32,
    follow: bool,
    output: &mut FollowOutput,
) -> CliResult<u64> {
    let params = AgentEventsParams {
        agent_id: id.clone(),
        since_seq: 0,
        limit: MAX_EVENTS_LIMIT,
        // A zero tail still needs the newest sequence to start following.
        tail: Some(if follow { n.max(1) } else { n }),
    };
    let result: Value = client.call(AGENT_EVENTS, &params).await?;
    let events: Vec<AgentEvent> = serde_json::from_value(result["events"].clone())?;
    if n > 0 {
        for event in &events {
            if follow {
                output.print(event)?;
            } else {
                print_event(event, output.json)?;
            }
        }
    }
    Ok(events.last().map_or(0, |event| event.seq))
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
pub(super) async fn follow_until_settled(id: &AgentId, json: bool) -> CliResult<ExitCode> {
    let state = follow_events(
        id,
        0,
        FollowOutput {
            json,
            suggested: false,
        },
        |event| match event.kind {
            EventKind::StateChanged { state } if settled(state) => Some(state),
            _ => None,
        },
    )
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

struct FollowOutput {
    json: bool,
    suggested: bool,
}

impl FollowOutput {
    fn print(&mut self, event: &AgentEvent) -> CliResult<()> {
        print_event(event, self.json)?;
        if !self.json
            && !self.suggested
            && let EventKind::RateLimited { detail } = &event.kind
        {
            let provider = match detail["provider"].as_str() {
                Some("claude") => ProviderKind::Claude,
                Some("codex") => ProviderKind::Codex,
                _ => return Ok(()),
            };
            let reason = output::escape_line(detail["reason"].as_str().unwrap_or("unknown"));
            println!(
                "rate limited by {} ({reason}) — hand off with: dgc handoff {} --to {}",
                output::provider_name(provider),
                event.agent_id,
                output::provider_name(provider.other()),
            );
            self.suggested = true;
        }
        Ok(())
    }
}

/// Print events after `since` (backfilled by the daemon), then live ones,
/// until `stop` returns a value or the daemon closes the connection. After a
/// `lagged` notice the stream is re-subscribed from the last printed seq.
async fn follow_events<T>(
    id: &AgentId,
    since: u64,
    mut output: FollowOutput,
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
                    output.print(&event)?;
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
