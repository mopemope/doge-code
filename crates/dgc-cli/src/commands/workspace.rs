//! `dgc-next workspace {create|ls|archive}`.

use std::process::ExitCode;

use clap::Subcommand;
use dgc_protocol::ids::{ProjectId, WorkspaceId};
use dgc_protocol::methods::{
    WORKSPACE_ARCHIVE, WORKSPACE_CREATE, WORKSPACE_LIST, WorkspaceArchiveParams,
    WorkspaceCreateParams, WorkspaceListParams,
};
use dgc_protocol::model::{Isolation, Workspace};

use super::{CliResult, connect};
use crate::output;

#[derive(Subcommand)]
pub enum WorkspaceCommand {
    /// Create a workspace: the project directory, or a new git worktree.
    Create {
        /// Project to create the workspace in.
        #[arg(long)]
        project: ProjectId,
        /// Create a dgc-owned git worktree on a new branch instead of using
        /// the project directory.
        #[arg(long, requires = "branch")]
        worktree: bool,
        /// New branch name for the worktree.
        #[arg(long, requires = "worktree")]
        branch: Option<String>,
        /// Commit-ish to branch from (default: HEAD).
        #[arg(long, requires = "worktree")]
        base: Option<String>,
        /// Directory name of the worktree (default: derived from the branch).
        #[arg(long, requires = "worktree")]
        slug: Option<String>,
    },
    /// List active workspaces.
    Ls {
        /// Only workspaces of this project.
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Archive a workspace; an owned worktree and its branch are deleted.
    Archive { id: WorkspaceId },
}

pub async fn run(command: WorkspaceCommand, json: bool) -> CliResult<ExitCode> {
    let mut client = connect().await?;
    match command {
        WorkspaceCommand::Create {
            project,
            worktree,
            branch,
            base,
            slug,
        } => {
            let params = WorkspaceCreateParams {
                project_id: project,
                isolation: if worktree {
                    Isolation::Worktree
                } else {
                    Isolation::Local
                },
                branch,
                base_ref: base,
                slug,
            };
            let workspace: Workspace = client.call(WORKSPACE_CREATE, &params).await?;
            print_workspace(&workspace, json)?;
        }
        WorkspaceCommand::Ls { project } => {
            let result: serde_json::Value = client
                .call(
                    WORKSPACE_LIST,
                    &WorkspaceListParams {
                        project_id: project,
                    },
                )
                .await?;
            if json {
                println!("{result}");
            } else {
                let workspaces: Vec<Workspace> =
                    serde_json::from_value(result["workspaces"].clone())?;
                for workspace in &workspaces {
                    println!("{}", output::workspace_line(workspace));
                }
            }
        }
        WorkspaceCommand::Archive { id } => {
            let workspace: Workspace = client
                .call(WORKSPACE_ARCHIVE, &WorkspaceArchiveParams { id })
                .await?;
            print_workspace(&workspace, json)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn print_workspace(workspace: &Workspace, json: bool) -> CliResult<()> {
    if json {
        println!("{}", serde_json::to_string(workspace)?);
    } else {
        println!("{}", output::workspace_line(workspace));
    }
    Ok(())
}
