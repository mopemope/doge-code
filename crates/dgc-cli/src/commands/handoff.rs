//! Preview or start a continuation of an existing agent's work.

use std::process::ExitCode;

use clap::Args;
use dgc_protocol::ids::AgentId;
use dgc_protocol::methods::{AGENT_HANDOFF, AgentHandoffParams, AgentHandoffResult};

use super::agent::{Provider, follow_until_settled, warn_auto_approve};
use super::{CliResult, connect};
use crate::output;

#[derive(Args)]
pub struct HandoffCommand {
    id: AgentId,
    /// Target provider (defaults to the other provider without a profile).
    #[arg(long, value_enum)]
    to: Option<Provider>,
    /// Named launch profile from the daemon's config.
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    mode: Option<String>,
    /// Print the handoff prompt without starting an agent.
    #[arg(long)]
    dry_run: bool,
    /// Stream the new agent's events until it settles.
    #[arg(long)]
    follow: bool,
}

pub async fn run(command: HandoffCommand, json: bool) -> CliResult<ExitCode> {
    let mut client = connect().await?;
    let params = AgentHandoffParams {
        agent_id: command.id,
        provider: command.to.map(Into::into),
        profile: command.profile,
        model: command.model,
        mode: command.mode,
        dry_run: command.dry_run,
    };
    let result: AgentHandoffResult = client.call(AGENT_HANDOFF, &params).await?;
    if let Some(agent) = &result.agent {
        warn_auto_approve(agent);
    }
    if json {
        println!(
            "{}",
            output::escape_json_line(&serde_json::to_string(&result)?)
        );
    } else if let Some(agent) = &result.agent {
        println!("{}", agent.id);
    } else {
        print!("{}", output::escape_text(&result.prompt));
    }
    if command.follow
        && let Some(agent) = result.agent
    {
        drop(client);
        return follow_until_settled(&agent.id, json).await;
    }
    Ok(ExitCode::SUCCESS)
}
