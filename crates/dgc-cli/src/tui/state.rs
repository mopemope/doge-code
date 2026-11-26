//! The TUI model: what the screen shows and what the keys act on.
//!
//! Text stored here that came from the daemon (stream lines, row labels,
//! error messages) is already escaped with `output::escape_*`. Raw protocol
//! values kept for dialogs (`PendingPermission`) must be escaped when drawn.

use std::collections::{HashSet, VecDeque};

use dgc_protocol::event::AgentEvent;
use dgc_protocol::ids::{AgentId, PermissionId, ProjectId, WorkspaceId};
use dgc_protocol::model::{
    Agent, AgentState, Isolation, PendingPermission, Project, ProviderKind, SendBehavior,
    Workspace, WorkspaceState,
};

use super::ConnectionState;
use super::input::LineEditor;
use crate::output::{escape_line, event_line};

/// Most lines the stream keeps; older ones are dropped.
pub const STREAM_CAP: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Input,
    Dialog,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeId {
    Project(ProjectId),
    Workspace(WorkspaceId),
    Agent(AgentId),
}

/// One visible line of the sidebar tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub node: NodeId,
    pub depth: usize,
    /// Escaped: project name, `ws <branch or local>[ (wt)]`, or agent title.
    pub label: String,
    /// `Some(open)` for projects and workspaces, `None` for agents.
    pub expanded: Option<bool>,
    /// Agents only.
    pub agent_state: Option<AgentState>,
    pub provider: Option<ProviderKind>,
    pub auto_approve: bool,
    /// Agents only: a permission request is pending (the `!` mark).
    pub waiting_permission: bool,
}

/// Projects, their active workspaces and the agents in them.
#[derive(Debug, Clone, Default)]
pub struct Tree {
    projects: Vec<Project>,
    workspaces: Vec<Workspace>,
    agents: Vec<Agent>,
    /// Nodes the user folded; everything else is open.
    collapsed: HashSet<NodeId>,
    cursor: Option<NodeId>,
}

impl Tree {
    pub fn cursor(&self) -> Option<&NodeId> {
        self.cursor.as_ref()
    }

    pub fn agent(&self, id: &AgentId) -> Option<&Agent> {
        self.agents.iter().find(|a| &a.id == id)
    }

    pub fn workspace(&self, id: &WorkspaceId) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| &w.id == id)
    }

    pub fn project(&self, id: &ProjectId) -> Option<&Project> {
        self.projects.iter().find(|p| &p.id == id)
    }

    /// Agents that are not archived.
    pub fn agent_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| a.state != AgentState::Archived)
            .count()
    }

    /// Visible rows, top to bottom. `permissions` marks waiting agents.
    pub fn rows(&self, permissions: &[PendingPermission]) -> Vec<Row> {
        let mut rows = Vec::new();
        for project in &self.projects {
            let open = !self
                .collapsed
                .contains(&NodeId::Project(project.id.clone()));
            rows.push(Row {
                node: NodeId::Project(project.id.clone()),
                depth: 0,
                label: escape_line(&project.name),
                expanded: Some(open),
                agent_state: None,
                provider: None,
                auto_approve: false,
                waiting_permission: false,
            });
            if !open {
                continue;
            }
            let workspaces = self
                .workspaces
                .iter()
                .filter(|w| w.project_id == project.id && w.state == WorkspaceState::Active);
            for workspace in workspaces {
                let open = !self
                    .collapsed
                    .contains(&NodeId::Workspace(workspace.id.clone()));
                rows.push(Row {
                    node: NodeId::Workspace(workspace.id.clone()),
                    depth: 1,
                    label: workspace_label(workspace),
                    expanded: Some(open),
                    agent_state: None,
                    provider: None,
                    auto_approve: false,
                    waiting_permission: false,
                });
                if !open {
                    continue;
                }
                let agents = self
                    .agents
                    .iter()
                    .filter(|a| a.workspace_id == workspace.id && a.state != AgentState::Archived);
                for agent in agents {
                    rows.push(Row {
                        node: NodeId::Agent(agent.id.clone()),
                        depth: 2,
                        label: escape_line(&agent.title),
                        expanded: None,
                        agent_state: Some(agent.state),
                        provider: Some(agent.provider),
                        auto_approve: agent.auto_approve,
                        waiting_permission: permissions.iter().any(|p| p.agent_id == agent.id),
                    });
                }
            }
        }
        rows
    }

    /// Agents (not archived) in tree order, folded nodes included.
    pub fn agents_in_order(&self) -> Vec<&Agent> {
        let mut out = Vec::new();
        for project in &self.projects {
            for workspace in self
                .workspaces
                .iter()
                .filter(|w| w.project_id == project.id && w.state == WorkspaceState::Active)
            {
                out.extend(
                    self.agents.iter().filter(|a| {
                        a.workspace_id == workspace.id && a.state != AgentState::Archived
                    }),
                );
            }
        }
        out
    }

    /// Replace the contents with a daemon snapshot, keeping folds and the
    /// cursor (which moves to the nearest surviving row when its node is gone).
    pub fn replace(
        &mut self,
        projects: Vec<Project>,
        workspaces: Vec<Workspace>,
        agents: Vec<Agent>,
    ) {
        self.mutate(|tree| {
            tree.projects = projects;
            tree.workspaces = workspaces
                .into_iter()
                .filter(|w| w.state == WorkspaceState::Active)
                .collect();
            tree.agents = agents
                .into_iter()
                .filter(|a| a.state != AgentState::Archived)
                .collect();
            tree.sort();
        });
    }

    pub fn set_agent_state(&mut self, id: &AgentId, state: AgentState) {
        self.mutate(|tree| {
            if let Some(agent) = tree.agents.iter_mut().find(|a| &a.id == id) {
                agent.state = state;
            }
        });
    }

    /// Add an agent created by this client (no-op when already known).
    pub fn insert_agent(&mut self, agent: Agent) {
        if self.agent(&agent.id).is_some() {
            return;
        }
        self.mutate(|tree| {
            tree.agents.push(agent);
            tree.sort();
        });
    }

    /// Add a workspace created by this client (no-op when already known).
    pub fn insert_workspace(&mut self, workspace: Workspace) {
        if self.workspace(&workspace.id).is_some() || workspace.state != WorkspaceState::Active {
            return;
        }
        self.mutate(|tree| {
            tree.workspaces.push(workspace);
            tree.sort();
        });
    }

    /// Unfold the ancestors of `node` and put the cursor on it.
    pub fn reveal(&mut self, node: &NodeId) {
        match node {
            NodeId::Project(_) => {}
            NodeId::Workspace(id) => {
                if let Some(ws) = self.workspace(id) {
                    let project = NodeId::Project(ws.project_id.clone());
                    self.collapsed.remove(&project);
                }
            }
            NodeId::Agent(id) => {
                let workspace_id = self.agent(id).map(|a| a.workspace_id.clone());
                if let Some(ws) = workspace_id.as_ref().and_then(|w| self.workspace(w)) {
                    let project = NodeId::Project(ws.project_id.clone());
                    self.collapsed.remove(&project);
                }
                if let Some(workspace_id) = workspace_id {
                    self.collapsed.remove(&NodeId::Workspace(workspace_id));
                }
            }
        }
        self.cursor = Some(node.clone());
    }

    pub fn set_cursor(&mut self, node: NodeId) {
        self.cursor = Some(node);
    }

    pub fn collapse(&mut self, node: NodeId) {
        self.collapsed.insert(node);
    }

    pub fn expand(&mut self, node: &NodeId) {
        self.collapsed.remove(node);
    }

    fn sort(&mut self) {
        self.projects
            .sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
        self.workspaces
            .sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        self.agents
            .sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
    }

    /// Run `change`, then repair the cursor: keep it, or move it to the
    /// nearest surviving sibling, else ancestor, else the first row.
    fn mutate(&mut self, change: impl FnOnce(&mut Self)) {
        let old_rows = self.rows(&[]);
        let old_index = self
            .cursor
            .as_ref()
            .and_then(|c| old_rows.iter().position(|r| &r.node == c));
        change(self);
        let new_rows = self.rows(&[]);
        let alive = |node: &NodeId| new_rows.iter().any(|r| &r.node == node);
        if self.cursor.as_ref().is_some_and(alive) {
            return;
        }
        self.cursor = old_index
            .and_then(|index| nearest_survivor(&old_rows, index, &alive))
            .or_else(|| new_rows.first().map(|r| r.node.clone()));
    }
}

fn nearest_survivor(old: &[Row], index: usize, alive: &impl Fn(&NodeId) -> bool) -> Option<NodeId> {
    let depth = old[index].depth;
    for row in &old[index + 1..] {
        if row.depth < depth {
            break;
        }
        if row.depth == depth && alive(&row.node) {
            return Some(row.node.clone());
        }
    }
    for row in old[..index].iter().rev() {
        if row.depth < depth {
            break;
        }
        if row.depth == depth && alive(&row.node) {
            return Some(row.node.clone());
        }
    }
    let mut level = depth;
    for row in old[..index].iter().rev() {
        if row.depth < level {
            if alive(&row.node) {
                return Some(row.node.clone());
            }
            level = row.depth;
        }
    }
    None
}

fn workspace_label(workspace: &Workspace) -> String {
    match workspace.isolation {
        Isolation::Local => "ws local".to_owned(),
        Isolation::Worktree => format!(
            "ws {} (wt)",
            escape_line(workspace.branch.as_deref().unwrap_or("-"))
        ),
    }
}

/// The selected agent's event lines.
///
/// While history is loading (after selecting an agent, a reconnect or a
/// lag), live events are held back and applied once the history is complete,
/// so a live event can never hide older history behind its seq.
#[derive(Debug, Clone, Default)]
pub struct StreamBuffer {
    agent_id: Option<AgentId>,
    lines: VecDeque<String>,
    last_seq: u64,
    /// Lines scrolled up from the tail; 0 follows the tail.
    scroll: usize,
    loading: bool,
    pending: Vec<AgentEvent>,
    /// Identifies the current history load; pages of older loads are stale.
    generation: u64,
}

impl StreamBuffer {
    pub fn agent_id(&self) -> Option<&AgentId> {
        self.agent_id.as_ref()
    }

    pub fn lines(&self) -> &VecDeque<String> {
        &self.lines
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Start over for `agent_id` (empty buffer, history to be loaded).
    pub(super) fn reset(&mut self, agent_id: Option<AgentId>) {
        self.generation += 1;
        self.loading = agent_id.is_some();
        self.agent_id = agent_id;
        self.lines.clear();
        self.last_seq = 0;
        self.scroll = 0;
        self.pending.clear();
    }

    /// Keep the lines and load what is missing after `last_seq`.
    pub(super) fn begin_catch_up(&mut self) {
        self.generation += 1;
        self.loading = self.agent_id.is_some();
    }

    /// Append `event` unless it was already shown. Returns whether it was added.
    fn push(&mut self, event: &AgentEvent) -> bool {
        if event.seq <= self.last_seq {
            return false;
        }
        self.last_seq = event.seq;
        let before = self.lines.len();
        for line in event_line(event).split('\n') {
            self.lines.push_back(line.to_owned());
        }
        let added = self.lines.len() - before;
        while self.lines.len() > STREAM_CAP {
            self.lines.pop_front();
        }
        if self.scroll > 0 {
            // Keep the view where it is while new lines arrive below it.
            self.scroll += added;
            self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
        }
        true
    }

    /// A live event of the selected agent.
    pub(super) fn live(&mut self, event: &AgentEvent) {
        if self.loading {
            if self.pending.len() >= STREAM_CAP {
                // History paging reads the log to its end and will pick
                // these up; do not grow without bound meanwhile.
                self.pending.clear();
            }
            self.pending.push(event.clone());
        } else {
            self.push(event);
        }
    }

    /// Append one history page; returns whether `last_seq` advanced.
    pub(super) fn history_page(&mut self, events: &[AgentEvent]) -> bool {
        let before = self.last_seq;
        for event in events {
            self.push(event);
        }
        self.last_seq > before
    }

    /// History is complete: apply the held live events.
    pub(super) fn finish_history(&mut self) {
        self.loading = false;
        for event in std::mem::take(&mut self.pending) {
            self.push(&event);
        }
    }

    pub(super) fn scroll_up(&mut self, lines: usize) {
        self.scroll = (self.scroll + lines).min(self.lines.len().saturating_sub(1));
    }

    pub(super) fn scroll_down(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentField {
    Provider,
    Model,
    Mode,
    Title,
    Prompt,
}

const AGENT_FIELDS: [AgentField; 5] = [
    AgentField::Provider,
    AgentField::Model,
    AgentField::Mode,
    AgentField::Title,
    AgentField::Prompt,
];

#[derive(Debug, Clone)]
pub struct NewAgentForm {
    pub workspace_id: WorkspaceId,
    pub provider: ProviderKind,
    pub model: LineEditor,
    pub mode: LineEditor,
    pub title: LineEditor,
    pub prompt: LineEditor,
    pub field: AgentField,
    /// Escaped message shown in the dialog.
    pub error: Option<String>,
    /// `agent.create` is in flight.
    pub submitting: bool,
}

impl NewAgentForm {
    pub fn new(workspace_id: WorkspaceId) -> Self {
        Self {
            workspace_id,
            provider: ProviderKind::Claude,
            model: LineEditor::default(),
            mode: LineEditor::default(),
            title: LineEditor::default(),
            prompt: LineEditor::default(),
            field: AgentField::Prompt,
            error: None,
            submitting: false,
        }
    }

    /// The chosen mode makes the provider act without asking (the dialog
    /// shows a red warning).
    pub fn auto_approve_mode(&self) -> bool {
        let mode = self.mode.text().trim();
        match self.provider {
            ProviderKind::Claude => matches!(mode, "bypassPermissions" | "auto" | "acceptEdits"),
            ProviderKind::Codex => mode == "full-access",
        }
    }

    pub fn toggle_provider(&mut self) {
        self.provider = match self.provider {
            ProviderKind::Claude => ProviderKind::Codex,
            ProviderKind::Codex => ProviderKind::Claude,
        };
    }

    pub fn move_field(&mut self, forward: bool) {
        let index = AGENT_FIELDS
            .iter()
            .position(|f| *f == self.field)
            .unwrap_or(0);
        let len = AGENT_FIELDS.len();
        let next = if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        self.field = AGENT_FIELDS[next];
    }

    /// The editor of the focused field (`None` for the provider choice).
    pub fn editor_mut(&mut self) -> Option<&mut LineEditor> {
        match self.field {
            AgentField::Provider => None,
            AgentField::Model => Some(&mut self.model),
            AgentField::Mode => Some(&mut self.mode),
            AgentField::Title => Some(&mut self.title),
            AgentField::Prompt => Some(&mut self.prompt),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceField {
    Branch,
    Base,
}

#[derive(Debug, Clone)]
pub struct NewWorkspaceForm {
    pub project_id: ProjectId,
    pub isolation: Isolation,
    /// Worktree only; required.
    pub branch: LineEditor,
    /// Worktree only; optional.
    pub base: LineEditor,
    pub field: WorkspaceField,
    pub error: Option<String>,
    pub submitting: bool,
}

impl NewWorkspaceForm {
    pub fn new(project_id: ProjectId) -> Self {
        Self {
            project_id,
            isolation: Isolation::Local,
            branch: LineEditor::default(),
            base: LineEditor::default(),
            field: WorkspaceField::Branch,
            error: None,
            submitting: false,
        }
    }

    pub fn toggle_isolation(&mut self) {
        self.isolation = match self.isolation {
            Isolation::Local => Isolation::Worktree,
            Isolation::Worktree => Isolation::Local,
        };
    }

    pub fn move_field(&mut self) {
        self.field = match self.field {
            WorkspaceField::Branch => WorkspaceField::Base,
            WorkspaceField::Base => WorkspaceField::Branch,
        };
    }

    /// The editor of the focused field (`None` for a local workspace).
    pub fn editor_mut(&mut self) -> Option<&mut LineEditor> {
        if self.isolation == Isolation::Local {
            return None;
        }
        match self.field {
            WorkspaceField::Branch => Some(&mut self.branch),
            WorkspaceField::Base => Some(&mut self.base),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Dialog {
    Permission {
        permission: PendingPermission,
        /// `permission.respond` is in flight.
        sending: bool,
        /// Ticks left before a/s/d are accepted ([`PERMISSION_ARM_TICKS`] when
        /// the dialog opens); the dialog shows "keys enabled shortly" until 0.
        ticks_until_armed: u8,
    },
    NewAgent(NewAgentForm),
    NewWorkspace(NewWorkspaceForm),
    ConfirmArchive {
        agent_id: AgentId,
        /// Escaped.
        title: String,
    },
}

impl Dialog {
    /// A permission dialog showing `permission`, its keys not yet enabled.
    pub fn permission(permission: PendingPermission) -> Self {
        Self::Permission {
            permission,
            sending: false,
            ticks_until_armed: PERMISSION_ARM_TICKS,
        }
    }

    /// Whether a/s/d are accepted (always true for other dialogs).
    pub fn permission_armed(&self) -> bool {
        !matches!(self, Self::Permission { ticks_until_armed, .. } if *ticks_until_armed > 0)
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub connection: ConnectionState,
    /// The last error (escaped), shown until the next successful action or
    /// [`ERROR_TICKS`] ticks.
    pub error: Option<String>,
    pub(super) error_ticks: u32,
}

/// Ticks an error stays in the status line.
pub const ERROR_TICKS: u32 = 10;

/// Ticks after a permission dialog opens (or its permission changes) during
/// which a/s/d are ignored, so a key meant for something else cannot answer.
pub const PERMISSION_ARM_TICKS: u8 = 2;

/// Shown (status line and input title) while typing at an agent that waits
/// for a permission answer: the dialog never opens over the input.
pub const PERMISSION_HINT: &str = "\u{26a0} permission waiting \u{2014} Esc then p to review";

/// A permission change seen live while a snapshot was being fetched.
#[derive(Debug, Clone)]
pub(super) enum PermissionChange {
    Added(PendingPermission),
    Resolved(PermissionId),
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub tree: Tree,
    pub focus: Focus,
    pub stream: StreamBuffer,
    pub send_mode: SendBehavior,
    pub input: LineEditor,
    pub dialog: Option<Dialog>,
    pub status: Status,
    /// Every pending permission request, across agents.
    pub permissions: Vec<PendingPermission>,
    /// Terminal size from the last `Resize` (columns, rows).
    pub size: (u16, u16),
    pub(super) focus_before_dialog: Focus,
    /// Permissions the user closed the dialog on; not reopened automatically.
    pub(super) dismissed: HashSet<PermissionId>,
    /// Ticks since an unanswered unknown-agent snapshot request.
    pub(super) snapshot_pending: Option<u32>,
    /// The last sent message, restored into the input if sending fails.
    pub(super) unsent: Option<(AgentId, String)>,
    /// The agent the input was opened on; messages go there, whatever the
    /// tree cursor does afterwards.
    pub(super) input_target: Option<AgentId>,
    /// Permission changes since the pending snapshot was requested, applied
    /// again on top of it (the snapshot may be older than they are).
    pub(super) permission_log: Vec<PermissionChange>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tree: Tree::default(),
            focus: Focus::Tree,
            stream: StreamBuffer::default(),
            send_mode: SendBehavior::Queue,
            input: LineEditor::default(),
            dialog: None,
            status: Status {
                connection: ConnectionState::Reconnecting,
                error: None,
                error_ticks: 0,
            },
            permissions: Vec::new(),
            size: (80, 24),
            focus_before_dialog: Focus::Tree,
            dismissed: HashSet::new(),
            snapshot_pending: None,
            unsent: None,
            input_target: None,
            permission_log: Vec::new(),
        }
    }
}

impl AppState {
    pub fn rows(&self) -> Vec<Row> {
        self.tree.rows(&self.permissions)
    }

    /// Index of the cursor row in [`AppState::rows`].
    pub fn cursor_index(&self) -> Option<usize> {
        let cursor = self.tree.cursor()?;
        self.rows().iter().position(|r| &r.node == cursor)
    }

    /// The agent under the cursor.
    pub fn selected_agent(&self) -> Option<&Agent> {
        match self.tree.cursor()? {
            NodeId::Agent(id) => self
                .tree
                .agent(id)
                .filter(|a| a.state != AgentState::Archived),
            _ => None,
        }
    }

    pub fn selected_agent_id(&self) -> Option<AgentId> {
        self.selected_agent().map(|a| a.id.clone())
    }

    /// The agent the input sends to, while it still exists.
    pub fn input_target(&self) -> Option<&Agent> {
        let id = self.input_target.as_ref()?;
        self.tree
            .agent(id)
            .filter(|a| a.state != AgentState::Archived)
    }

    /// Typing with the input focused but its agent gone (archived or dropped
    /// by a snapshot): sending is refused until the user selects an agent.
    pub fn input_target_missing(&self) -> bool {
        self.focus == Focus::Input && self.input_target().is_none()
    }

    /// The agent whose stream is shown: the input's target while typing,
    /// else the one under the cursor.
    pub fn stream_agent_id(&self) -> Option<AgentId> {
        let focus = if self.focus == Focus::Dialog {
            self.focus_before_dialog
        } else {
            self.focus
        };
        if focus == Focus::Input {
            self.input_target().map(|a| a.id.clone())
        } else {
            self.selected_agent_id()
        }
    }

    /// [`PERMISSION_HINT`] while typing at an agent with a pending permission.
    pub fn permission_hint(&self) -> Option<&'static str> {
        if self.focus != Focus::Input {
            return None;
        }
        let target = self.input_target()?;
        self.permissions
            .iter()
            .any(|p| p.agent_id == target.id)
            .then_some(PERMISSION_HINT)
    }

    pub fn permission_count(&self) -> usize {
        self.permissions.len()
    }

    pub fn agent_count(&self) -> usize {
        self.tree.agent_count()
    }

    /// Lines to move for PageUp / PageDown.
    pub fn page_lines(&self) -> usize {
        usize::from(self.size.1.saturating_sub(5)).max(1)
    }

    pub(super) fn set_error(&mut self, message: String) {
        self.status.error = Some(message);
        self.status.error_ticks = 0;
    }

    pub(super) fn clear_error(&mut self) {
        self.status.error = None;
        self.status.error_ticks = 0;
    }

    /// Show `dialog` and send keys to it.
    pub(super) fn open_dialog(&mut self, dialog: Dialog) {
        if self.focus != Focus::Dialog {
            self.focus_before_dialog = self.focus;
        }
        self.dialog = Some(dialog);
        self.focus = Focus::Dialog;
    }

    pub(super) fn close_dialog(&mut self) {
        self.dialog = None;
        if self.focus == Focus::Dialog {
            self.focus = self.focus_before_dialog;
        }
    }
}

/// First line of `prompt`, cut to 60 characters (the CLI's default title).
pub(super) fn default_title(prompt: &str) -> String {
    prompt
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(60)
        .collect()
}
