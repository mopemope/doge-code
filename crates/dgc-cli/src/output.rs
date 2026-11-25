//! Rendering shared by subcommands.

use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::methods::{DaemonStatus, ProviderStatus, WorkspaceArchiveResult};
use dgc_protocol::model::{
    Agent, AgentState, Decision, Isolation, PendingPermission, PermissionKind, Project,
    ProviderKind, Resolution, SendBehavior, SendOutcome, Workspace, WorkspaceState,
};

/// `text` with every control character except `\n` written as a visible
/// escape (`\r`, `\t`, `\x1b`, `\u{9b}`), and Unicode bidirectional
/// overrides as `\u{202e}` etc., so daemon-supplied text (agent output,
/// permission summaries, errors) cannot move the cursor, erase or recolor
/// what the terminal shows, or reorder it.
pub fn escape_text(text: &str) -> String {
    escape(text, true)
}

/// Like [`escape_text`] but also escapes `\n`: for fields that must stay on
/// one line (a newline there could imitate another list row).
pub fn escape_line(text: &str) -> String {
    escape(text, false)
}

fn escape(text: &str, keep_newline: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' if keep_newline => out.push('\n'),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if c.is_control() || is_bidi_control(c) => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Unicode bidirectional formatting characters (embeddings, overrides,
/// isolates and marks) that can visually reorder a line.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// One compact JSON line.
pub fn daemon_status_json(status: &DaemonStatus) -> serde_json::Result<String> {
    serde_json::to_string(status)
}

pub fn daemon_status_text(status: &DaemonStatus) -> String {
    let mut text = format!(
        "pid: {}\nprotocol_version: {}\nstarted_at: {}\nPATH: {}",
        status.pid,
        status.protocol_version,
        escape_line(&status.started_at),
        escape_line(&status.path_env)
    );
    for provider in &status.providers {
        text.push('\n');
        text.push_str(&provider_line(provider));
    }
    text
}

/// `claude: <path> (<version> (tested|untested))` or `codex: not found`.
fn provider_line(status: &ProviderStatus) -> String {
    let name = provider_name(status.kind);
    let Some(path) = &status.path else {
        return format!("{name}: not found");
    };
    let version = escape_line(status.version.as_deref().unwrap_or("unknown version"));
    let tested = if status.tested { "tested" } else { "untested" };
    format!(
        "{name}: {} ({version} ({tested}))",
        escape_line(&path.display().to_string())
    )
}

pub fn provider_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "claude",
        ProviderKind::Codex => "codex",
    }
}

pub fn state_name(state: AgentState) -> &'static str {
    match state {
        AgentState::Starting => "starting",
        AgentState::Idle => "idle",
        AgentState::Running => "running",
        AgentState::WaitingPermission => "waiting_permission",
        AgentState::Stopped => "stopped",
        AgentState::Failed => "failed",
        AgentState::Archived => "archived",
    }
}

fn behavior_name(behavior: SendBehavior) -> &'static str {
    match behavior {
        SendBehavior::Queue => "queue",
        SendBehavior::Steer => "steer",
        SendBehavior::Interrupt => "interrupt",
    }
}

pub fn outcome_name(outcome: SendOutcome) -> &'static str {
    match outcome {
        SendOutcome::Sent => "sent",
        SendOutcome::Queued => "queued",
        SendOutcome::Steered => "steered",
        SendOutcome::InterruptedAndSent => "interrupted_and_sent",
    }
}

fn kind_name(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Command => "command",
        PermissionKind::FileChange => "file_change",
        PermissionKind::Tool => "tool",
        PermissionKind::Other => "other",
    }
}

fn resolution_name(resolution: Resolution) -> &'static str {
    match resolution {
        Resolution::Decided(Decision::Allow) => "allow",
        Resolution::Decided(Decision::AllowForSession) => "allow_for_session",
        Resolution::Decided(Decision::Deny) => "deny",
        Resolution::Cancelled => "cancelled",
    }
}

/// `<id>  <workspace>  <provider>  <state>  <title>[  auto-approve]`
pub fn agent_line(agent: &Agent) -> String {
    let mut line = format!(
        "{}  {}  {}  {}  {}",
        agent.id,
        agent.workspace_id,
        provider_name(agent.provider),
        state_name(agent.state),
        escape_line(&agent.title)
    );
    if agent.auto_approve {
        line.push_str("  auto-approve");
    }
    line
}

/// `key: value` lines for `show`.
pub fn agent_details(agent: &Agent) -> String {
    let optional =
        |value: &Option<String>| value.as_deref().map_or_else(|| "-".to_owned(), escape_line);
    let mut text = format!(
        "id: {}\nworkspace: {}\ntitle: {}\nprovider: {}\nmodel: {}\nmode: {}\n\
         auto_approve: {}\nstate: {}\nprovider_session: {}\ncreated_at: {}",
        agent.id,
        agent.workspace_id,
        escape_line(&agent.title),
        provider_name(agent.provider),
        optional(&agent.model),
        optional(&agent.mode),
        agent.auto_approve,
        state_name(agent.state),
        optional(&agent.provider_session_id),
        escape_line(&agent.created_at),
    );
    if let Some(error) = &agent.last_error {
        text.push_str(&format!("\nlast_error: {}", escape_line(error)));
    }
    text
}

/// `<permission id>  <agent id>  <kind>  <summary>`
pub fn permission_line(permission: &PendingPermission) -> String {
    format!(
        "{}  {}  {}  {}",
        permission.id,
        permission.agent_id,
        kind_name(permission.kind),
        escape_line(&permission.summary)
    )
}

/// One event as `[seq] <description>`. Continuation lines of multi-line text
/// are indented by four spaces, so every event starts with `[`.
pub fn event_line(event: &AgentEvent) -> String {
    let body = match &event.kind {
        EventKind::TurnStarted => "turn started".to_owned(),
        EventKind::AssistantText { text, final_: true } => format!("assistant: {text}"),
        EventKind::AssistantText {
            text,
            final_: false,
        } => {
            format!("assistant (partial): {text}")
        }
        EventKind::ToolCall { name, summary } => format!("tool: {name} {summary}"),
        EventKind::ToolResult {
            summary,
            is_error: false,
        } => format!("tool result: {summary}"),
        EventKind::ToolResult {
            summary,
            is_error: true,
        } => format!("tool error: {summary}"),
        EventKind::PermissionRequested { permission } => format!(
            "permission requested {} ({}): {}",
            permission.id,
            kind_name(permission.kind),
            permission.summary
        ),
        EventKind::PermissionResolved { id, resolution } => {
            format!("permission resolved {id}: {}", resolution_name(*resolution))
        }
        EventKind::TurnCompleted { .. } => "turn completed".to_owned(),
        EventKind::RateLimited { detail } => format!("rate limited: {detail}"),
        EventKind::Error { message } => format!("error: {message}"),
        EventKind::StateChanged { state } => format!("state: {}", state_name(*state)),
        EventKind::UserMessage { text, behavior } => {
            format!("user ({}): {text}", behavior_name(*behavior))
        }
    };
    format!(
        "[{}] {}",
        event.seq,
        escape_text(&body).replace('\n', "\n    ")
    )
}

/// `<id>  <name>  <path>`
pub fn project_line(project: &Project) -> String {
    format!(
        "{}  {}  {}",
        project.id,
        escape_line(&project.name),
        escape_line(&project.path.display().to_string())
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
        escape_line(workspace.branch.as_deref().unwrap_or("-")),
        escape_line(&workspace.path.display().to_string())
    )
}

/// The archived workspace's line, then for a worktree what stayed on disk:
/// `worktree kept on disk: <path>` and/or `branch kept: <branch>`.
pub fn archive_text(result: &WorkspaceArchiveResult) -> String {
    let workspace = &result.workspace;
    let mut text = workspace_line(workspace);
    if workspace.isolation == Isolation::Worktree {
        if !result.worktree_removed && workspace.path.exists() {
            text.push_str(&format!(
                "\nworktree kept on disk: {}",
                escape_line(&workspace.path.display().to_string())
            ));
        }
        if result.branch_kept
            && let Some(branch) = &workspace.branch
        {
            text.push_str(&format!("\nbranch kept: {}", escape_line(branch)));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dgc_protocol::ids::{ProjectId, WorkspaceId};

    use super::*;

    #[test]
    fn event_lines_are_prefixed_and_indent_continuations() {
        let event = AgentEvent {
            agent_id: dgc_protocol::ids::AgentId::generate(),
            seq: 7,
            at: String::new(),
            kind: EventKind::AssistantText {
                text: "a\nb".into(),
                final_: true,
            },
        };
        assert_eq!(event_line(&event), "[7] assistant: a\n    b");
        let state = AgentEvent {
            kind: EventKind::StateChanged {
                state: AgentState::WaitingPermission,
            },
            ..event
        };
        assert_eq!(event_line(&state), "[7] state: waiting_permission");
    }

    #[test]
    fn permission_line_escapes_terminal_control_sequences() {
        let permission = PendingPermission::new(
            dgc_protocol::ids::PermissionId::generate(),
            dgc_protocol::ids::AgentId::generate(),
            PermissionKind::Command,
            "Bash: \rrm -rf ~\x1b[2K\nperm_fake  line\u{7f}\u{9b}\u{202e}".to_owned(),
            serde_json::Value::Null,
            String::new(),
        );
        let line = permission_line(&permission);
        assert!(
            line.ends_with("Bash: \\rrm -rf ~\\x1b[2K\\nperm_fake  line\\x7f\\u{9b}\\u{202e}"),
            "{line:?}"
        );
        assert!(!line.chars().any(char::is_control), "{line:?}");
    }

    #[test]
    fn event_lines_escape_controls_but_keep_newlines() {
        let event = AgentEvent {
            agent_id: dgc_protocol::ids::AgentId::generate(),
            seq: 3,
            at: String::new(),
            kind: EventKind::AssistantText {
                text: "ok\x1b[31m\tred\r\nnext".into(),
                final_: true,
            },
        };
        assert_eq!(
            event_line(&event),
            "[3] assistant: ok\\x1b[31m\\tred\\r\n    next"
        );
        let error = AgentEvent {
            kind: EventKind::Error {
                message: "bad\x07".into(),
            },
            ..event
        };
        assert_eq!(event_line(&error), "[3] error: bad\\x07");
    }

    #[test]
    fn single_line_fields_escape_newlines_too() {
        let agent = Agent {
            id: dgc_protocol::ids::AgentId::generate(),
            workspace_id: WorkspaceId::generate(),
            title: "t\nfake\x1b".into(),
            provider: ProviderKind::Claude,
            model: None,
            mode: None,
            auto_approve: false,
            provider_session_id: None,
            state: AgentState::Idle,
            parent_id: None,
            labels: Default::default(),
            created_at: String::new(),
            last_error: Some("e\r".into()),
        };
        assert!(agent_line(&agent).ends_with("idle  t\\nfake\\x1b"));
        let details = agent_details(&agent);
        assert!(details.contains("\ntitle: t\\nfake\\x1b\n"), "{details:?}");
        assert!(details.ends_with("\nlast_error: e\\r"), "{details:?}");
    }

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
