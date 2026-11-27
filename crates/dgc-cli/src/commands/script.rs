//! List, trust and run workspace scripts through the daemon.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Subcommand;
use dgc_client::{Client, ClientError};
use dgc_protocol::ids::WorkspaceId;
use dgc_protocol::methods::{
    SCRIPT_LIST, SCRIPT_LOGS, SCRIPT_START, SCRIPT_STOP, SCRIPT_TRUST, ScriptFileState,
    ScriptListParams, ScriptListResult, ScriptLogsParams, ScriptLogsResult, ScriptNameParams,
    ScriptTrustParams, ScriptTrustState, WORKSPACE_LIST, WorkspaceListParams,
};
use dgc_protocol::model::Workspace;
use dgc_protocol::rpc::INVALID_STATE;
use serde_json::{Value, json};

use super::{CliResult, connect};
use crate::output;

const FOLLOW_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Subcommand)]
pub enum ScriptCommand {
    /// List script definitions, trust and execution state.
    Ls {
        #[arg(long, value_name = "WS")]
        workspace: WorkspaceId,
    },
    /// Review and trust the current dgc.json for its project.
    Trust {
        #[arg(long, value_name = "WS")]
        workspace: WorkspaceId,
        /// Explicitly trust the displayed configuration without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Start a script from a trusted dgc.json.
    Start {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "WS")]
        workspace: WorkspaceId,
    },
    /// Stop a script in this workspace.
    Stop {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "WS")]
        workspace: WorkspaceId,
    },
    /// Read the script's combined stdout and stderr.
    Logs {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "WS")]
        workspace: WorkspaceId,
        /// Follow the retained log until the script exits and the log is drained.
        #[arg(long)]
        follow: bool,
    },
}

pub async fn run(command: ScriptCommand, json: bool) -> CliResult<ExitCode> {
    if json && !matches!(command, ScriptCommand::Ls { .. }) {
        return Err("use --json with `dgc script ls`".into());
    }
    let mut client = connect().await?;
    match command {
        ScriptCommand::Ls { workspace } => list(&mut client, workspace, json).await?,
        ScriptCommand::Trust { workspace, yes } => trust(&mut client, workspace, yes).await?,
        ScriptCommand::Start { name, workspace } => {
            let params = ScriptNameParams {
                workspace_id: workspace,
                name,
            };
            match client.call::<_, Value>(SCRIPT_START, &params).await {
                Ok(_) => println!("started {}", output::escape_line(&params.name)),
                Err(error) if invalid_state(&error, "script_untrusted") => {
                    return Err(format!(
                        "script_untrusted. Run `dgc script trust --workspace {}` to review dgc.json",
                        params.workspace_id
                    )
                    .into());
                }
                Err(error) => return Err(error.into()),
            }
        }
        ScriptCommand::Stop { name, workspace } => {
            let params = ScriptNameParams {
                workspace_id: workspace,
                name,
            };
            let _: Value = client.call(SCRIPT_STOP, &params).await?;
            println!("stopped {}", output::escape_line(&params.name));
        }
        ScriptCommand::Logs {
            name,
            workspace,
            follow,
        } => {
            logs(&mut client, workspace, name, follow).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn list(client: &mut Client, workspace_id: WorkspaceId, json: bool) -> CliResult<()> {
    let list: ScriptListResult = client
        .call(SCRIPT_LIST, &ScriptListParams { workspace_id })
        .await?;
    if json {
        let result = json!({
            "project_name": list.project_name,
            "file": list.file,
            "error": list.error,
            "trust": list.trust,
            "scripts": list.scripts,
        });
        println!("{}", output::escape_json_line(&result.to_string()));
    } else {
        let trust = match list.trust {
            ScriptTrustState::Trusted => "trusted",
            ScriptTrustState::Untrusted => "untrusted",
            ScriptTrustState::Changed => "changed",
        };
        println!("trust: {trust}");
        if list.file != ScriptFileState::Ok {
            println!("{}", file_problem(&list));
        }
        for script in &list.scripts {
            println!("{}", output::script_line(script));
        }
    }
    Ok(())
}

async fn trust(client: &mut Client, workspace_id: WorkspaceId, yes: bool) -> CliResult<()> {
    let list: ScriptListResult = client
        .call(
            SCRIPT_LIST,
            &ScriptListParams {
                workspace_id: workspace_id.clone(),
            },
        )
        .await?;
    let Some(expected_content) = list.content else {
        return Err(file_problem(&list).into());
    };
    let path = workspace_path(client, &workspace_id)
        .await?
        .join("dgc.json");
    let project = output::escape_line(&list.project_name);
    println!(
        "Project {project}: {}",
        output::escape_line(&path.to_string_lossy())
    );
    let mut count = 0;
    for script in list.scripts.iter().filter(|script| script.in_file) {
        println!("  {}", output::script_definition(script));
        count += 1;
    }

    if !yes {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return Err("trust requires terminal input. Use --yes to confirm explicitly".into());
        }
        print!("Trust these {count} scripts for {project}? [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        stdin.read_line(&mut answer)?;
        let answer = answer.trim();
        if !answer.eq_ignore_ascii_case("y") && !answer.eq_ignore_ascii_case("yes") {
            return Err("trust cancelled".into());
        }
    }

    let params = ScriptTrustParams {
        workspace_id,
        expected_content,
    };
    match client.call::<_, Value>(SCRIPT_TRUST, &params).await {
        Ok(_) => println!("trusted dgc.json for {project}"),
        Err(error) if invalid_state(&error, "script_file_changed") => {
            return Err(format!(
                "dgc.json changed. Run `dgc script trust --workspace {}` again to review the new contents",
                params.workspace_id
            ).into());
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn file_problem(list: &ScriptListResult) -> String {
    let reason = match list.file {
        ScriptFileState::Ok => "snapshot unavailable",
        ScriptFileState::Missing => "missing",
        ScriptFileState::Invalid => list.error.as_deref().unwrap_or("invalid"),
    };
    format!("dgc.json: {}", output::escape_line(reason))
}

async fn workspace_path(client: &mut Client, id: &WorkspaceId) -> CliResult<PathBuf> {
    let mut result: Value = client
        .call(WORKSPACE_LIST, &WorkspaceListParams::default())
        .await?;
    let workspaces: Vec<Workspace> = serde_json::from_value(result["workspaces"].take())?;
    workspaces
        .into_iter()
        .find(|workspace| &workspace.id == id)
        .map(|workspace| workspace.path)
        .ok_or_else(|| format!("workspace {id} is no longer available").into())
}

fn invalid_state(error: &ClientError, reason: &str) -> bool {
    matches!(error, ClientError::Rpc(rpc) if rpc.code == INVALID_STATE && rpc.message == reason)
}

async fn logs(
    client: &mut Client,
    workspace_id: WorkspaceId,
    name: String,
    follow: bool,
) -> CliResult<()> {
    let mut params = ScriptLogsParams {
        workspace_id,
        name,
        offset: follow.then_some(0),
        max_bytes: None,
    };
    let mut at_line_start = true;
    loop {
        let page: ScriptLogsResult = match client.call(SCRIPT_LOGS, &params).await {
            Ok(page) => page,
            Err(error) if follow => {
                return Err(format!(
                    "stopped following script logs because {}",
                    output::escape_line(&error.to_string())
                )
                .into());
            }
            Err(error) => return Err(error.into()),
        };
        if params
            .offset
            .is_some_and(|offset| page.next_offset < offset)
        {
            return Err(
                "stopped following script logs because the log offsets reset. The daemon may have restarted"
                    .into(),
            );
        }
        {
            let mut stdout = io::stdout().lock();
            if params
                .offset
                .is_some_and(|offset| page.start_offset > offset)
            {
                if !at_line_start {
                    writeln!(stdout)?;
                }
                writeln!(stdout, "… output skipped (rotated) …")?;
                at_line_start = true;
            }
            if !page.data.is_empty() {
                write!(stdout, "{}", output::escape_script_log(&page.data))?;
                at_line_start = page.data.ends_with('\n');
            }
            stdout.flush()?;
        }
        if !follow || (!page.running && page.data.is_empty()) {
            return Ok(());
        }
        params.offset = Some(page.next_offset);
        tokio::time::sleep(FOLLOW_INTERVAL).await;
    }
}
