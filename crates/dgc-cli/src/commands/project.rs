//! `dgc-next project {add|ls|rm}`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;
use dgc_protocol::ids::ProjectId;
use dgc_protocol::methods::{
    PROJECT_ADD, PROJECT_LIST, PROJECT_REMOVE, ProjectAddParams, ProjectRemoveParams,
};
use dgc_protocol::model::Project;

use super::{CliResult, connect};
use crate::output;

#[derive(Subcommand)]
pub enum ProjectCommand {
    /// Register a directory as a project (default: the current directory).
    Add {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// List registered projects.
    Ls,
    /// Unregister a project that has no active workspaces.
    Rm { id: ProjectId },
}

pub async fn run(command: ProjectCommand, json: bool) -> CliResult<ExitCode> {
    let mut client = connect().await?;
    match command {
        ProjectCommand::Add { path } => {
            // The daemon's working directory is not ours, so send an absolute path.
            let path = std::path::absolute(&path)?;
            let project: Project = client.call(PROJECT_ADD, &ProjectAddParams { path }).await?;
            if json {
                println!("{}", serde_json::to_string(&project)?);
            } else {
                println!("{}", output::project_line(&project));
            }
        }
        ProjectCommand::Ls => {
            let result: serde_json::Value =
                client.call(PROJECT_LIST, &serde_json::json!({})).await?;
            if json {
                println!("{result}");
            } else {
                let projects: Vec<Project> = serde_json::from_value(result["projects"].clone())?;
                for project in &projects {
                    println!("{}", output::project_line(project));
                }
            }
        }
        ProjectCommand::Rm { id } => {
            let _: serde_json::Value = client
                .call(PROJECT_REMOVE, &ProjectRemoveParams { id: id.clone() })
                .await?;
            if json {
                println!("{}", serde_json::json!({ "removed": id }));
            } else {
                println!("removed {id}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
