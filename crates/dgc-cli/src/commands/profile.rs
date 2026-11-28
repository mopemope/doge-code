//! List the daemon's named launch profiles.

use std::process::ExitCode;

use clap::Subcommand;
use dgc_protocol::methods::{PROFILE_LIST, ProfileListResult};

use super::{CliResult, connect};
use crate::output::{escape_json_line, escape_line, provider_name};

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// List available launch profiles.
    Ls,
}

pub async fn run(command: ProfileCommand, json: bool) -> CliResult<ExitCode> {
    match command {
        ProfileCommand::Ls => {
            let mut client = connect().await?;
            let result: ProfileListResult =
                client.call(PROFILE_LIST, &serde_json::json!({})).await?;
            if json {
                println!("{}", escape_json_line(&serde_json::to_string(&result)?));
            } else {
                for profile in result.profiles {
                    let model = escape_line(profile.model.as_deref().unwrap_or("-"));
                    let mode = escape_line(profile.mode.as_deref().unwrap_or("-"));
                    let auto = if profile.auto_approve {
                        "  auto-approve"
                    } else {
                        ""
                    };
                    let name = escape_line(&profile.name);
                    let provider = provider_name(profile.provider);
                    let mcp = if profile.mcp { "  mcp" } else { "" };
                    let notes = profile
                        .notes
                        .as_deref()
                        .map_or_else(String::new, |notes| format!("  {}", escape_line(notes)));
                    println!("{name}  {provider}  {model}  {mode}{auto}{mcp}{notes}");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
