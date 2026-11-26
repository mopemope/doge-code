//! The TUI model: what the screen shows and what the keys act on.
//!
//! Text stored here that came from the daemon (stream lines, row labels,
//! error messages) is already escaped with `output::escape_*`. Raw protocol
//! values kept for dialogs (`PendingPermission`) must be escaped when drawn.

use std::collections::{HashMap, HashSet, VecDeque};

use dgc_daemon::provider::validate_mode;
use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::{AgentId, PermissionId, ProjectId, WorkspaceId};
use dgc_protocol::methods::{AgentHandoffParams, ProfileInfo};
use dgc_protocol::model::{
    Agent, AgentState, Isolation, PendingPermission, Project, ProviderKind, SendBehavior,
    Workspace, WorkspaceState,
};

use super::ConnectionState;
use super::input::LineEditor;
use super::layout::permission_layout;
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
    /// The selected agent's initial tail has yet to arrive.
    initial_tail: bool,
    pending: Vec<AgentEvent>,
    /// Highest live sequence dropped from the pending buffer during a load.
    dropped_pending_seq: u64,
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

    pub(super) fn initial_tail(&self) -> bool {
        self.initial_tail
    }

    /// Start over for `agent_id` (empty buffer, history to be loaded).
    pub(super) fn reset(&mut self, agent_id: Option<AgentId>) {
        self.generation += 1;
        self.loading = agent_id.is_some();
        self.initial_tail = self.loading;
        self.agent_id = agent_id;
        self.lines.clear();
        self.last_seq = 0;
        self.scroll = 0;
        self.pending.clear();
        self.dropped_pending_seq = 0;
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
                // Re-read discarded sequences if the current load stops
                // before covering them.
                if let Some(highest) = self.pending.iter().map(|event| event.seq).max() {
                    self.dropped_pending_seq = self.dropped_pending_seq.max(highest);
                }
                self.pending.clear();
            }
            self.pending.push(event.clone());
        } else {
            self.push(event);
        }
    }

    /// Append one history page; returns whether `last_seq` advanced.
    pub(super) fn history_page(&mut self, events: &[AgentEvent]) -> bool {
        self.initial_tail = false;
        let before = self.last_seq;
        for event in events {
            self.push(event);
        }
        self.last_seq > before
    }

    pub(super) fn pending_needs_catch_up(&mut self) -> bool {
        std::mem::take(&mut self.dropped_pending_seq) > self.last_seq
    }

    /// History is complete: apply the held live events.
    pub(super) fn finish_history(&mut self) {
        self.loading = false;
        let mut pending = std::mem::take(&mut self.pending);
        pending.sort_unstable_by_key(|event| event.seq);
        for event in pending {
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
    Profile,
    Provider,
    Model,
    Mode,
    Title,
    Prompt,
}

const AGENT_FIELDS: [AgentField; 6] = [
    AgentField::Profile,
    AgentField::Provider,
    AgentField::Model,
    AgentField::Mode,
    AgentField::Title,
    AgentField::Prompt,
];

#[derive(Debug, Clone)]
pub struct NewAgentForm {
    pub workspace_id: WorkspaceId,
    pub(super) profiles: Vec<ProfileInfo>,
    /// Zero selects "(none)"; the rest are one-based profile indices.
    profile_index: usize,
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
            profiles: Vec::new(),
            profile_index: 0,
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
        let mode = (!mode.is_empty()).then_some(mode);
        matches!(validate_mode(self.provider, mode), Ok(true))
    }

    pub fn selected_profile(&self) -> Option<&ProfileInfo> {
        self.profile_index
            .checked_sub(1)
            .and_then(|i| self.profiles.get(i))
    }

    pub fn cycle_profile(&mut self, forward: bool) {
        if self.profiles.is_empty() {
            return;
        }
        let count = self.profiles.len() + 1;
        self.profile_index = if forward {
            (self.profile_index + 1) % count
        } else {
            (self.profile_index + count - 1) % count
        };
        let (provider, model, mode) =
            self.selected_profile()
                .map_or((ProviderKind::Claude, None, None), |profile| {
                    (
                        profile.provider,
                        profile.model.clone(),
                        profile.mode.clone(),
                    )
                });
        self.provider = provider;
        self.model.set_text(model.as_deref().unwrap_or_default());
        self.mode.set_text(mode.as_deref().unwrap_or_default());
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

    /// The editor of the focused field (`None` for selectors).
    pub fn editor_mut(&mut self) -> Option<&mut LineEditor> {
        match self.field {
            AgentField::Profile | AgentField::Provider => None,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffField {
    Provider,
    Profile,
    Mode,
}

#[derive(Debug, Clone)]
pub struct HandoffForm {
    pub agent_id: AgentId,
    pub provider: ProviderKind,
    pub field: HandoffField,
    pub mode: LineEditor,
    pub profiles: Vec<ProfileInfo>,
    profile_index: usize,
    default_provider: ProviderKind,
    pub preview: Option<String>,
    pub scroll: usize,
    pub ticks_until_armed: u8,
    pub generation: u64,
    pub submitting: bool,
    pub preview_failed: bool,
    pub error: Option<String>,
}

impl HandoffForm {
    pub fn new(agent_id: AgentId, provider: ProviderKind, generation: u64) -> Self {
        Self {
            agent_id,
            provider,
            default_provider: provider,
            field: HandoffField::Provider,
            mode: LineEditor::default(),
            profiles: Vec::new(),
            profile_index: 0,
            preview: None,
            scroll: 0,
            ticks_until_armed: PERMISSION_ARM_TICKS,
            generation,
            submitting: false,
            preview_failed: false,
            error: None,
        }
    }

    pub fn selected_profile(&self) -> Option<&ProfileInfo> {
        self.profile_index
            .checked_sub(1)
            .and_then(|i| self.profiles.get(i))
    }

    pub(super) fn cycle_profile(&mut self, forward: bool) {
        if self.profiles.is_empty() {
            return;
        }
        let count = self.profiles.len() + 1;
        self.profile_index = if forward {
            (self.profile_index + 1) % count
        } else {
            (self.profile_index + count - 1) % count
        };
        let (provider, mode) = self
            .selected_profile()
            .map_or((self.default_provider, None), |profile| {
                (profile.provider, profile.mode.clone())
            });
        self.provider = provider;
        self.mode.set_text(mode.as_deref().unwrap_or_default());
        self.rearm();
    }

    pub(super) fn toggle_provider(&mut self) {
        self.provider = self.provider.other();
        self.profile_index = 0;
        self.mode.set_text("");
        self.rearm();
    }

    pub(super) fn move_field(&mut self, forward: bool) {
        use HandoffField::{Mode, Profile, Provider};
        self.field = match (self.field, forward) {
            (Provider, true) | (Mode, false) => Profile,
            (Profile, true) | (Provider, false) => Mode,
            (Mode, true) | (Profile, false) => Provider,
        };
    }

    pub(super) fn mode_editor(&mut self) -> Option<&mut LineEditor> {
        (self.field == HandoffField::Mode && !self.submitting).then_some(&mut self.mode)
    }

    pub(super) fn rearm(&mut self) {
        self.ticks_until_armed = PERMISSION_ARM_TICKS;
    }

    pub(super) fn mode_value(&self) -> Option<&str> {
        let mode = self.mode.text().trim();
        (!mode.is_empty()).then_some(mode)
    }

    pub fn auto_approve_mode(&self) -> bool {
        matches!(validate_mode(self.provider, self.mode_value()), Ok(true))
    }

    pub(super) fn params(&self, dry_run: bool) -> AgentHandoffParams {
        AgentHandoffParams {
            agent_id: self.agent_id.clone(),
            provider: Some(self.provider),
            // As in NewAgent, expand the displayed settings locally. Clearing
            // the mode field must select the default even with a profile.
            profile: None,
            model: self
                .selected_profile()
                .and_then(|profile| profile.model.clone()),
            mode: self.mode_value().map(str::to_owned),
            dry_run,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct RateLimitState {
    last_limit: Option<(u64, String)>,
    last_start: u64,
    last_completion: u64,
    completed_turn_start: u64,
}

impl RateLimitState {
    pub(super) fn observe(&mut self, event: &AgentEvent) {
        match &event.kind {
            EventKind::RateLimited { detail }
                if self
                    .last_limit
                    .as_ref()
                    .is_none_or(|(seq, _)| *seq < event.seq) =>
            {
                let reason = detail["reason"].as_str().unwrap_or("unknown");
                self.last_limit = Some((event.seq, reason.to_owned()));
            }
            EventKind::TurnStarted => {
                self.last_start = self.last_start.max(event.seq);
                // History can arrive after a newer live completion.
                if event.seq < self.last_completion {
                    self.completed_turn_start = self.completed_turn_start.max(event.seq);
                }
            }
            EventKind::TurnCompleted { .. } if event.seq > self.last_completion => {
                self.last_completion = event.seq;
                if self.last_start < event.seq {
                    self.completed_turn_start = self.completed_turn_start.max(self.last_start);
                }
            }
            _ => {}
        }
    }

    fn reason(&self) -> Option<&str> {
        let (seq, reason) = self.last_limit.as_ref()?;
        (*seq > self.completed_turn_start).then_some(reason)
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
        /// First summary row on screen.
        scroll: usize,
        /// Summary rows `0..seen_rows` have been on screen (contiguously
        /// from the top, at the current terminal size).
        seen_rows: usize,
        /// The whole summary has been on screen: a and s are accepted only
        /// then (d and Esc never need it). Kept in step by
        /// [`AppState::sync_permission_view`].
        seen_end: bool,
    },
    Handoff(HandoffForm),
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
            scroll: 0,
            seen_rows: 0,
            seen_end: false,
        }
    }

    /// Whether a/s/d are accepted (always true for other dialogs).
    pub fn permission_armed(&self) -> bool {
        !matches!(self, Self::Permission { ticks_until_armed, .. } if *ticks_until_armed > 0)
    }
}

/// How the permission dialog's summary scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrollMotion {
    Lines(isize),
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub connection: ConnectionState,
    /// The last error (escaped), shown until the next successful action or
    /// [`ERROR_TICKS`] ticks.
    pub error: Option<String>,
    /// What the last action did (fixed text, e.g. how a send was
    /// delivered); cleared like `error`, and replaced by an error.
    pub notice: Option<String>,
    pub(super) error_ticks: u32,
}

/// Ticks between periodic snapshots while connected (5 s).
pub const SNAPSHOT_REFRESH_TICKS: u32 = 20;

/// Ticks an error or a notice stays in the status line.
pub const ERROR_TICKS: u32 = 10;

/// Ticks after a permission dialog opens (or its permission changes) during
/// which a/s/d are ignored, so a key meant for something else cannot answer.
/// The dialog can open just before a tick, so only two of the three are
/// full 250 ms periods: at least 500 ms.
pub const PERMISSION_ARM_TICKS: u8 = 3;

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
    /// The draft of `input_target`. Other agents' drafts wait in `drafts`.
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
    /// A snapshot has been requested and has yet to arrive.
    pub(super) snapshot_pending: bool,
    /// A subscription lagged while a snapshot was in flight.
    pub(super) snapshot_refresh_again: bool,
    /// Correlates a profile list reply with the form that requested it.
    pub(super) profile_generation: u64,
    pub(super) rate_limits: HashMap<AgentId, RateLimitState>,
    /// Ticks connected with no snapshot pending since the last request: at
    /// [`SNAPSHOT_REFRESH_TICKS`] the tree is reloaded (changes made by
    /// other clients have no live event).
    pub(super) ticks_since_snapshot: u32,
    /// The last sent message, restored into the input if sending fails.
    pub(super) unsent: Option<(AgentId, String)>,
    /// The agent the input was opened on; messages go there, whatever the
    /// tree cursor does afterwards.
    pub(super) input_target: Option<AgentId>,
    /// Non-empty drafts of agents other than `input_target`: text typed for
    /// one agent is never sent to another.
    pub(super) drafts: HashMap<AgentId, LineEditor>,
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
                notice: None,
                error_ticks: 0,
            },
            permissions: Vec::new(),
            size: (80, 24),
            focus_before_dialog: Focus::Tree,
            dismissed: HashSet::new(),
            snapshot_pending: false,
            snapshot_refresh_again: false,
            profile_generation: 0,
            rate_limits: HashMap::new(),
            ticks_since_snapshot: 0,
            unsent: None,
            input_target: None,
            drafts: HashMap::new(),
            permission_log: Vec::new(),
        }
    }
}

impl AppState {
    pub fn rate_limit_reason(&self, id: &AgentId) -> Option<&str> {
        self.rate_limits.get(id)?.reason()
    }

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

    /// Make `agent_id` the owner of the input: the previous owner's text is
    /// kept for it and `agent_id`'s own draft (empty if none) is loaded.
    pub(super) fn bind_draft(&mut self, agent_id: &AgentId) {
        if self.input_target.as_ref() == Some(agent_id) {
            return;
        }
        let next = self.drafts.remove(agent_id).unwrap_or_default();
        let previous = std::mem::replace(&mut self.input, next);
        if let Some(owner) = self.input_target.replace(agent_id.clone())
            && !previous.is_empty()
        {
            self.drafts.insert(owner, previous);
        }
    }

    /// Whether the agent has no unsent text.
    pub fn draft_is_empty(&self, agent_id: &AgentId) -> bool {
        if self.input_target.as_ref() == Some(agent_id) {
            self.input.is_empty()
        } else {
            self.drafts.get(agent_id).is_none_or(|d| d.is_empty())
        }
    }

    /// The editor holding `agent_id`'s draft.
    pub(super) fn draft_mut(&mut self, agent_id: &AgentId) -> &mut LineEditor {
        if self.input_target.as_ref() == Some(agent_id) {
            &mut self.input
        } else {
            self.drafts.entry(agent_id.clone()).or_default()
        }
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
        self.status.notice = None;
        self.status.error_ticks = 0;
    }

    pub(super) fn set_notice(&mut self, message: &str) {
        self.status.error = None;
        self.status.notice = Some(message.to_owned());
        self.status.error_ticks = 0;
    }

    /// Clear the error and the notice.
    pub(super) fn clear_error(&mut self) {
        self.status.error = None;
        self.status.notice = None;
        self.status.error_ticks = 0;
    }

    /// Show `dialog` and send keys to it.
    pub(super) fn open_dialog(&mut self, dialog: Dialog) {
        if self.focus != Focus::Dialog {
            self.focus_before_dialog = self.focus;
        }
        self.dialog = Some(dialog);
        self.focus = Focus::Dialog;
        self.sync_permission_view(true);
    }

    /// Recompute what a permission dialog shows after it opened, was
    /// scrolled or the terminal changed size. `reset` starts reading over
    /// (the rows themselves change when the width does).
    ///
    /// `seen_end` needs every summary row to have been on screen, reached
    /// by scrolling from the top: jumping to the end does not count, since
    /// the rows in between were never visible.
    pub(super) fn sync_permission_view(&mut self, reset: bool) {
        let size = self.size;
        let Some(Dialog::Permission {
            permission,
            scroll,
            seen_rows,
            seen_end,
            ..
        }) = &mut self.dialog
        else {
            return;
        };
        let layout = permission_layout(size, &permission.summary);
        if reset {
            *scroll = 0;
            *seen_rows = 0;
        }
        *scroll = (*scroll).min(layout.max_scroll());
        if layout.visible_rows > 0 && *scroll <= *seen_rows {
            *seen_rows = (*seen_rows).max(*scroll + layout.visible_rows);
        }
        *seen_end = layout.visible_rows > 0 && *seen_rows >= layout.rows.len();
    }

    /// Ignore a/s/d again for [`PERMISSION_ARM_TICKS`] ticks.
    pub(super) fn rearm_permission(&mut self) {
        if let Some(Dialog::Permission {
            ticks_until_armed, ..
        }) = &mut self.dialog
        {
            *ticks_until_armed = PERMISSION_ARM_TICKS;
        }
    }

    /// Scroll the permission dialog's summary.
    pub(super) fn scroll_permission(&mut self, motion: ScrollMotion) {
        let size = self.size;
        let Some(Dialog::Permission {
            permission, scroll, ..
        }) = &mut self.dialog
        else {
            return;
        };
        let layout = permission_layout(size, &permission.summary);
        let page = layout.visible_rows.max(1);
        let max = layout.max_scroll();
        *scroll = match motion {
            ScrollMotion::Lines(delta) => scroll.saturating_add_signed(delta).min(max),
            ScrollMotion::PageUp => scroll.saturating_sub(page),
            ScrollMotion::PageDown => (*scroll + page).min(max),
            ScrollMotion::Home => 0,
            ScrollMotion::End => max,
        };
        self.sync_permission_view(false);
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
