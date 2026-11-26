//! `dgc-next permit {ls|allow|deny}`: answer agents' permission requests.

use std::process::ExitCode;

use clap::Subcommand;
use dgc_protocol::ids::{AgentId, PermissionId};
use dgc_protocol::methods::{
    PERMISSION_LIST, PERMISSION_RESPOND, PermissionListParams, PermissionRespondParams,
};
use dgc_protocol::model::{Decision, PendingPermission};
use serde_json::Value;

use super::agent::print_done;
use super::{CliResult, connect};
use crate::output;

#[derive(Subcommand)]
pub enum PermitCommand {
    /// List pending permission requests, oldest first.
    Ls {
        /// Only requests of this agent.
        #[arg(long)]
        agent: Option<AgentId>,
    },
    /// Approve a request.
    Allow {
        id: PermissionId,
        /// Approve for the rest of the provider session where the provider
        /// supports it.
        #[arg(long)]
        session: bool,
    },
    /// Refuse a request.
    Deny { id: PermissionId },
}

pub async fn run(command: PermitCommand, json: bool) -> CliResult<ExitCode> {
    let mut client = connect().await?;
    let (id, decision, word) = match command {
        PermitCommand::Ls { agent } => {
            let result: Value = client
                .call(PERMISSION_LIST, &PermissionListParams { agent_id: agent })
                .await?;
            if json {
                println!("{result}");
            } else {
                let pending: Vec<PendingPermission> =
                    serde_json::from_value(result["permissions"].clone())?;
                for permission in &pending {
                    println!("{}", output::permission_line(permission));
                }
            }
            return Ok(ExitCode::SUCCESS);
        }
        PermitCommand::Allow { id, session: false } => (id, Decision::Allow, "allowed"),
        PermitCommand::Allow { id, session: true } => {
            (id, Decision::AllowForSession, "allowed for the session")
        }
        PermitCommand::Deny { id } => (id, Decision::Deny, "denied"),
    };
    let result: Value = client
        .call(
            PERMISSION_RESPOND,
            &PermissionRespondParams { id, decision },
        )
        .await?;
    print_done(json, &result, word);
    Ok(ExitCode::SUCCESS)
}
