//! Rendering shared by subcommands.

use dgc_protocol::methods::DaemonStatus;
use dgc_protocol::model::{Isolation, Project, Workspace, WorkspaceState};

/// One compact JSON line.
pub fn daemon_status_json(status: &DaemonStatus) -> serde_json::Result<String> {
    serde_json::to_string(status)
}

pub fn daemon_status_text(status: &DaemonStatus) -> String {
    format!(
        "pid: {}\nprotocol_version: {}\nstarted_at: {}\nPATH: {}",
        status.pid, status.protocol_version, status.started_at, status.path_env
    )
}

/// `<id>  <name>  <path>`
pub fn project_line(project: &Project) -> String {
    format!(
        "{}  {}  {}",
        project.id,
        project.name,
        project.path.display()
    )
}

/// `<id>  <project>  <isolation>  <state>  <branch or ->  <path>`
pub fn workspace_line(workspace: &Workspace) -> String {
    let isolation = match workspace.isolation {
        Isolation::Local => "local",
        Isolation::Worktree => "worktree",
    };
    let state = match workspace.state {
        WorkspaceState::Active => "active",
        WorkspaceState::Archived => "archived",
    };
    format!(
        "{}  {}  {}  {}  {}  {}",
        workspace.id,
        workspace.project_id,
        isolation,
        state,
        workspace.branch.as_deref().unwrap_or("-"),
        workspace.path.display()
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dgc_protocol::ids::{ProjectId, WorkspaceId};

    use super::*;

    #[test]
    fn workspace_line_shows_branch_or_dash() {
        let mut ws = Workspace {
            id: WorkspaceId::generate(),
            project_id: ProjectId::generate(),
            isolation: Isolation::Worktree,
            path: PathBuf::from("/w/a"),
            branch: Some("feat/x".into()),
            base_ref: None,
            state: WorkspaceState::Active,
            owned: true,
            created_at: String::new(),
        };
        let line = workspace_line(&ws);
        assert!(line.ends_with("worktree  active  feat/x  /w/a"), "{line}");
        ws.branch = None;
        ws.isolation = Isolation::Local;
        ws.state = WorkspaceState::Archived;
        assert!(workspace_line(&ws).ends_with("local  archived  -  /w/a"));
    }
}
