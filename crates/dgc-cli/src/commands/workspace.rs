//! `dgc-next workspace {create|ls|archive}`.

use std::process::ExitCode;

use clap::Subcommand;
use dgc_protocol::ids::{ProjectId, WorkspaceId};
use dgc_protocol::methods::{
    WORKSPACE_ARCHIVE, WORKSPACE_CREATE, WORKSPACE_LIST, WorkspaceArchiveParams,
    WorkspaceArchiveResult, WorkspaceCreateParams, WorkspaceListParams,
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
    /// Archive a workspace (and its agents). An owned worktree is deleted
    /// and its branch safely deleted (`git branch -d`); refused while the
    /// worktree has uncommitted changes or commits not in its base.
    Archive {
        id: WorkspaceId,
        /// Delete the worktree and its branch even if that discards
        /// uncommitted changes or commits.
        #[arg(long)]
        force: bool,
    },
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
        WorkspaceCommand::Archive { id, force } => {
            let result: WorkspaceArchiveResult = client
                .call(WORKSPACE_ARCHIVE, &WorkspaceArchiveParams { id, force })
                .await?;
            if json {
                println!("{}", serde_json::to_string(&result)?);
            } else {
                println!("{}", output::archive_text(&result));
            }
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
