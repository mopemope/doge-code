//! `update`: the single place where events change the [`AppState`].

use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::{AgentId, PermissionId, WorkspaceId};
use dgc_protocol::methods::{
    AGENT_ARCHIVE, AGENT_CREATE, AGENT_INTERRUPT, AGENT_RESUME, AGENT_SEND, AgentCreateParams,
    AgentIdParams, AgentSendParams, PERMISSION_RESPOND, PermissionRespondParams, WORKSPACE_CREATE,
    WorkspaceCreateParams,
};
use dgc_protocol::model::{
    Agent, AgentState, Decision, Isolation, PendingPermission, SendBehavior, Workspace,
};

use super::input::{Key, KeyInput};
use super::state::{
    AppState, Dialog, ERROR_TICKS, Focus, NewAgentForm, NewWorkspaceForm, NodeId, PermissionChange,
    ScrollMotion, default_title,
};
use super::{AppEvent, ConnectionState, Effect, RequestTag};
use crate::output::escape_line;

/// Ticks after which an unanswered snapshot request may be repeated.
const SNAPSHOT_RETRY_TICKS: u32 = 20;

/// Apply `event` to `state`; the returned effects are for the runtime to run.
pub fn update(state: &mut AppState, event: AppEvent) -> Vec<Effect> {
    let mut fx = Vec::new();
    match event {
        AppEvent::Key(key) => on_key(state, key, &mut fx),
        AppEvent::Snapshot {
            projects,
            workspaces,
            agents,
            permissions,
        } => {
            state.snapshot_pending = None;
            state.tree.replace(projects, workspaces, agents);
            state.permissions = permissions;
            // The snapshot may predate live permission events: apply them again.
            for change in std::mem::take(&mut state.permission_log) {
                match change {
                    PermissionChange::Added(permission) => add_permission(state, permission),
                    PermissionChange::Resolved(id) => state.permissions.retain(|p| p.id != id),
                }
            }
            let pending = &state.permissions;
            state
                .dismissed
                .retain(|id| pending.iter().any(|p| &p.id == id));
        }
        AppEvent::AgentEvent(event) => on_agent_event(state, event, &mut fx),
        AppEvent::History {
            agent_id,
            events,
            done,
            generation,
        } => on_history(state, &agent_id, &events, done, generation, &mut fx),
        AppEvent::RpcDone { request, result } => on_rpc_done(state, request, result, &mut fx),
        AppEvent::Connection(connection) => on_connection(state, connection, &mut fx),
        AppEvent::Lagged => {
            fx.push(Effect::Resubscribe);
            request_snapshot(state, &mut fx, true);
            catch_up_selected(state, &mut fx);
        }
        AppEvent::Resize { width, height } => {
            if state.size != (width, height) {
                state.size = (width, height);
                // The summary wraps differently now: read it again. Rows
                // can now count as seen before this size was ever drawn,
                // so a key typed before the resize must not answer.
                state.sync_permission_view(true);
                state.rearm_permission();
            }
        }
        AppEvent::Tick => on_tick(state),
    }
    settle(state, &mut fx);
    fx
}

/// Bring the derived state in line after any change: the stream follows the
/// selected agent (the input's target while typing), and a pending permission
/// may open its dialog.
fn settle(state: &mut AppState, fx: &mut Vec<Effect>) {
    // In the tree the input shows the selected agent's own draft.
    if state.focus == Focus::Tree
        && let Some(agent_id) = state.selected_agent_id()
    {
        state.bind_draft(&agent_id);
    }
    let wanted = state.stream_agent_id();
    if state.stream.agent_id() != wanted.as_ref() {
        state.stream.reset(wanted.clone());
        if let Some(agent_id) = wanted {
            fx.push(Effect::LoadHistory {
                agent_id,
                since_seq: 0,
                generation: state.stream.generation(),
            });
        }
    }
    if let Some(Dialog::Permission { permission, .. }) = &state.dialog
        && !state.permissions.iter().any(|p| p.id == permission.id)
    {
        state.close_dialog();
    }
    open_pending_permission(state);
}

/// Open the dialog of the selected agent's next pending permission, but only
/// from the tree with an empty input. It never opens over the input or over
/// another dialog: keys typed there must not answer a request, and even here
/// the dialog ignores a/s/d for its first ticks.
fn open_pending_permission(state: &mut AppState) {
    if state.focus != Focus::Tree {
        return;
    }
    let Some(agent_id) = state.selected_agent_id() else {
        return;
    };
    // Only the selected agent's own draft counts.
    if !state.draft_is_empty(&agent_id) {
        return;
    }
    let next = state
        .permissions
        .iter()
        .find(|p| p.agent_id == agent_id && !state.dismissed.contains(&p.id))
        .cloned();
    if let Some(permission) = next {
        state.open_dialog(Dialog::permission(permission));
    }
}

fn request_snapshot(state: &mut AppState, fx: &mut Vec<Effect>, force: bool) {
    if !force && state.snapshot_pending.is_some() {
        return;
    }
    state.snapshot_pending = Some(0);
    fx.push(Effect::LoadSnapshot);
}

/// Reload the selected agent's events after `last_seq`.
fn catch_up_selected(state: &mut AppState, fx: &mut Vec<Effect>) {
    if let Some(agent_id) = state.stream.agent_id().cloned() {
        state.stream.begin_catch_up();
        fx.push(Effect::LoadHistory {
            agent_id,
            since_seq: state.stream.last_seq(),
            generation: state.stream.generation(),
        });
    }
}

fn on_connection(state: &mut AppState, connection: ConnectionState, fx: &mut Vec<Effect>) {
    let connected = connection == ConnectionState::Connected;
    state.status.connection = match connection {
        ConnectionState::Disconnected { reason } => ConnectionState::Disconnected {
            reason: escape_line(&reason),
        },
        other => other,
    };
    if connected {
        request_snapshot(state, fx, true);
        catch_up_selected(state, fx);
    } else {
        state.snapshot_pending = None;
        state.permission_log.clear();
    }
}

fn on_tick(state: &mut AppState) {
    if let Some(Dialog::Permission {
        ticks_until_armed, ..
    }) = &mut state.dialog
    {
        *ticks_until_armed = ticks_until_armed.saturating_sub(1);
    }
    if state.status.error.is_some() {
        state.status.error_ticks += 1;
        if state.status.error_ticks >= ERROR_TICKS {
            state.clear_error();
        }
    }
    if let Some(ticks) = &mut state.snapshot_pending {
        *ticks += 1;
        if *ticks >= SNAPSHOT_RETRY_TICKS {
            state.snapshot_pending = None;
            state.permission_log.clear();
        }
    }
}

fn on_agent_event(state: &mut AppState, event: AgentEvent, fx: &mut Vec<Effect>) {
    if state.tree.agent(&event.agent_id).is_none() {
        request_snapshot(state, fx, false);
    }
    match &event.kind {
        EventKind::StateChanged { state: agent_state } => {
            state.tree.set_agent_state(&event.agent_id, *agent_state);
        }
        EventKind::PermissionRequested { permission } => {
            if state.snapshot_pending.is_some() {
                state
                    .permission_log
                    .push(PermissionChange::Added(permission.clone()));
            }
            add_permission(state, permission.clone());
        }
        EventKind::PermissionResolved { id, .. } => remove_permission(state, id),
        _ => {}
    }
    if state.stream.agent_id() == Some(&event.agent_id) {
        state.stream.live(&event);
    }
}

fn add_permission(state: &mut AppState, permission: PendingPermission) {
    if !state.permissions.iter().any(|p| p.id == permission.id) {
        state.permissions.push(permission);
    }
}

/// A permission ended (resolved, cancelled or answered by us).
fn remove_permission(state: &mut AppState, id: &PermissionId) {
    if state.snapshot_pending.is_some() {
        state
            .permission_log
            .push(PermissionChange::Resolved(id.clone()));
    }
    state.permissions.retain(|p| &p.id != id);
    state.dismissed.remove(id);
}

fn on_history(
    state: &mut AppState,
    agent_id: &AgentId,
    events: &[AgentEvent],
    done: bool,
    generation: u64,
    fx: &mut Vec<Effect>,
) {
    // A page of an older load (the user moved on, or a reconnect started a
    // new one) must not move `last_seq` past what the current load fetches.
    if state.stream.agent_id() != Some(agent_id) || generation != state.stream.generation() {
        return;
    }
    let advanced = state.stream.history_page(events);
    // An empty page, or one that shows nothing new, ends the paging.
    if done || events.is_empty() || !advanced {
        state.stream.finish_history();
    } else {
        fx.push(Effect::LoadHistory {
            agent_id: agent_id.clone(),
            since_seq: state.stream.last_seq(),
            generation,
        });
    }
}

// ---- RPC results ----

fn on_rpc_done(
    state: &mut AppState,
    request: RequestTag,
    result: Result<serde_json::Value, String>,
    fx: &mut Vec<Effect>,
) {
    match result {
        Ok(value) => on_rpc_ok(state, request, value, fx),
        Err(message) => on_rpc_error(state, request, escape_line(&message)),
    }
}

fn on_rpc_error(state: &mut AppState, request: RequestTag, message: String) {
    match request {
        RequestTag::Send { agent_id } => {
            if let Some((sent_to, text)) = state.unsent.take()
                && sent_to == agent_id
                && state.draft_is_empty(&agent_id)
                && state
                    .tree
                    .agent(&agent_id)
                    .is_some_and(|a| a.state != AgentState::Archived)
            {
                state.draft_mut(&agent_id).set_text(&text);
            }
        }
        RequestTag::Respond { permission_id } => {
            if let Some(Dialog::Permission {
                permission,
                sending,
                ..
            }) = &mut state.dialog
                && permission.id == permission_id
            {
                *sending = false;
            }
        }
        RequestTag::CreateAgent => {
            if let Some(Dialog::NewAgent(form)) = &mut state.dialog {
                form.submitting = false;
                form.error = Some(message.clone());
            }
        }
        RequestTag::CreateWorkspace => {
            if let Some(Dialog::NewWorkspace(form)) = &mut state.dialog {
                form.submitting = false;
                form.error = Some(message.clone());
            }
        }
        RequestTag::Interrupt { .. }
        | RequestTag::Resume { .. }
        | RequestTag::Archive { .. }
        | RequestTag::LoadHistory { .. } => {}
    }
    state.set_error(message);
}

fn on_rpc_ok(
    state: &mut AppState,
    request: RequestTag,
    value: serde_json::Value,
    fx: &mut Vec<Effect>,
) {
    state.clear_error();
    match request {
        RequestTag::Send { .. } => state.unsent = None,
        RequestTag::Respond { permission_id } => remove_permission(state, &permission_id),
        RequestTag::CreateAgent => {
            if matches!(state.dialog, Some(Dialog::NewAgent(_))) {
                state.close_dialog();
            }
            match serde_json::from_value::<Agent>(value) {
                Ok(agent) => {
                    let node = NodeId::Agent(agent.id.clone());
                    state.tree.insert_agent(agent);
                    // Watch the new agent, unless the user is typing at another.
                    if state.focus != Focus::Input {
                        state.tree.reveal(&node);
                    }
                }
                Err(_) => request_snapshot(state, fx, false),
            }
        }
        RequestTag::CreateWorkspace => {
            if matches!(state.dialog, Some(Dialog::NewWorkspace(_))) {
                state.close_dialog();
            }
            match serde_json::from_value::<Workspace>(value) {
                Ok(workspace) => {
                    let node = NodeId::Workspace(workspace.id.clone());
                    state.tree.insert_workspace(workspace);
                    if state.focus != Focus::Input {
                        state.tree.reveal(&node);
                    }
                }
                Err(_) => request_snapshot(state, fx, false),
            }
        }
        RequestTag::Interrupt { .. }
        | RequestTag::Resume { .. }
        | RequestTag::Archive { .. }
        | RequestTag::LoadHistory { .. } => {}
    }
}

/// The call effect for `method` with the serialized `params`.
fn call(
    request: RequestTag,
    method: &'static str,
    params: serde_json::Result<serde_json::Value>,
) -> Result<Effect, String> {
    match params {
        Ok(params) => Ok(Effect::Call {
            request,
            method,
            params,
        }),
        Err(_) => Err("could not encode the request".to_owned()),
    }
}

/// Push `effect`, or show why there is none.
fn push_call(state: &mut AppState, fx: &mut Vec<Effect>, effect: Result<Effect, String>) {
    match effect {
        Ok(effect) => fx.push(effect),
        Err(message) => state.set_error(message),
    }
}

// ---- keys ----

fn on_key(state: &mut AppState, key: KeyInput, fx: &mut Vec<Effect>) {
    if key.ctrl && key.key == Key::Char('c') {
        fx.push(Effect::Quit);
        return;
    }
    match state.focus {
        Focus::Tree => tree_key(state, key, fx),
        Focus::Input => input_key(state, key, fx),
        Focus::Dialog => dialog_key(state, key, fx),
    }
}

/// Keys that work in both the tree and the input: send mode and scrolling.
fn common_key(state: &mut AppState, key: &KeyInput) -> bool {
    if key.ctrl || key.alt {
        return false;
    }
    match key.key {
        Key::Tab => state.send_mode = next_mode(state.send_mode, true),
        Key::BackTab => state.send_mode = next_mode(state.send_mode, false),
        Key::PageUp => state.stream.scroll_up(state.page_lines()),
        Key::PageDown => state.stream.scroll_down(state.page_lines()),
        _ => return false,
    }
    true
}

fn next_mode(mode: SendBehavior, forward: bool) -> SendBehavior {
    use SendBehavior::{Interrupt, Queue, Steer};
    match (mode, forward) {
        (Queue, true) | (Interrupt, false) => Steer,
        (Steer, true) | (Queue, false) => Interrupt,
        (Interrupt, true) | (Steer, false) => Queue,
    }
}

fn tree_key(state: &mut AppState, key: KeyInput, fx: &mut Vec<Effect>) {
    if common_key(state, &key) || key.ctrl || key.alt {
        return;
    }
    match key.key {
        Key::Up => move_cursor(state, -1),
        Key::Down => move_cursor(state, 1),
        Key::Left => collapse_or_go_to_parent(state),
        Key::Right => expand_or_enter(state),
        Key::Enter => toggle_or_enter(state),
        Key::Char(c) => tree_char(state, c, fx),
        _ => {}
    }
}

fn tree_char(state: &mut AppState, c: char, fx: &mut Vec<Effect>) {
    match c {
        'k' => move_cursor(state, -1),
        'j' => move_cursor(state, 1),
        'i' => focus_input(state),
        'q' => fx.push(Effect::Quit),
        'x' => agent_call(state, fx, AGENT_INTERRUPT, |agent_id| {
            RequestTag::Interrupt { agent_id }
        }),
        'r' => agent_call(state, fx, AGENT_RESUME, |agent_id| RequestTag::Resume {
            agent_id,
        }),
        'A' => match state.selected_agent() {
            Some(agent) => {
                let dialog = Dialog::ConfirmArchive {
                    agent_id: agent.id.clone(),
                    title: escape_line(&agent.title),
                };
                state.open_dialog(dialog);
            }
            None => state.set_error("select an agent first".to_owned()),
        },
        'n' => match cursor_workspace(state) {
            Some(workspace_id) => {
                state.open_dialog(Dialog::NewAgent(NewAgentForm::new(workspace_id)));
            }
            None => state.set_error("select a workspace first".to_owned()),
        },
        'w' => match cursor_project(state) {
            Some(project_id) => {
                state.open_dialog(Dialog::NewWorkspace(NewWorkspaceForm::new(project_id)));
            }
            None => state.set_error("select a project first".to_owned()),
        },
        'p' => go_to_next_waiting_agent(state),
        _ => {}
    }
}

/// `method` with the selected agent's id, for interrupt and resume.
fn agent_call(
    state: &mut AppState,
    fx: &mut Vec<Effect>,
    method: &'static str,
    tag: impl FnOnce(AgentId) -> RequestTag,
) {
    let Some(agent_id) = state.selected_agent_id() else {
        state.set_error("select an agent first".to_owned());
        return;
    };
    let params = AgentIdParams {
        id: agent_id.clone(),
    };
    push_call(
        state,
        fx,
        call(tag(agent_id), method, serde_json::to_value(&params)),
    );
}

fn cursor_workspace(state: &AppState) -> Option<WorkspaceId> {
    match state.tree.cursor()? {
        NodeId::Workspace(id) => Some(id.clone()),
        NodeId::Agent(id) => state.tree.agent(id).map(|a| a.workspace_id.clone()),
        NodeId::Project(_) => None,
    }
}

fn cursor_project(state: &AppState) -> Option<dgc_protocol::ids::ProjectId> {
    match state.tree.cursor()? {
        NodeId::Project(id) => Some(id.clone()),
        NodeId::Workspace(id) => state.tree.workspace(id).map(|w| w.project_id.clone()),
        NodeId::Agent(id) => {
            let workspace_id = &state.tree.agent(id)?.workspace_id;
            state
                .tree
                .workspace(workspace_id)
                .map(|w| w.project_id.clone())
        }
    }
}

fn move_cursor(state: &mut AppState, delta: isize) {
    let rows = state.rows();
    if rows.is_empty() {
        return;
    }
    let target = match state.cursor_index() {
        Some(index) => index
            .saturating_add_signed(delta)
            .min(rows.len().saturating_sub(1)),
        None => 0,
    };
    state.tree.set_cursor(rows[target].node.clone());
}

fn focus_input(state: &mut AppState) {
    if let Some(agent_id) = state.selected_agent_id() {
        state.bind_draft(&agent_id);
        state.focus = Focus::Input;
    } else {
        state.set_error("select an agent first".to_owned());
    }
}

fn toggle_or_enter(state: &mut AppState) {
    let rows = state.rows();
    let Some(row) = state.cursor_index().map(|i| &rows[i]) else {
        return;
    };
    match row.expanded {
        Some(true) => state.tree.collapse(row.node.clone()),
        Some(false) => state.tree.expand(&row.node),
        None => focus_input(state),
    }
}

fn expand_or_enter(state: &mut AppState) {
    let rows = state.rows();
    let Some(index) = state.cursor_index() else {
        return;
    };
    let row = &rows[index];
    match row.expanded {
        Some(false) => state.tree.expand(&row.node),
        Some(true) => {
            if let Some(child) = rows.get(index + 1).filter(|r| r.depth > row.depth) {
                state.tree.set_cursor(child.node.clone());
            }
        }
        None => focus_input(state),
    }
}

fn collapse_or_go_to_parent(state: &mut AppState) {
    let rows = state.rows();
    let Some(index) = state.cursor_index() else {
        return;
    };
    let row = &rows[index];
    if row.expanded == Some(true) {
        state.tree.collapse(row.node.clone());
    } else if let Some(parent) = rows[..index].iter().rev().find(|r| r.depth < row.depth) {
        state.tree.set_cursor(parent.node.clone());
    }
}

/// `p`: move to the next agent with a pending permission request (wrapping)
/// and show its dialog even if it was closed before.
fn go_to_next_waiting_agent(state: &mut AppState) {
    let agents = state.tree.agents_in_order();
    let current = state
        .selected_agent_id()
        .and_then(|id| agents.iter().position(|a| a.id == id));
    let waiting = |agent: &Agent| state.permissions.iter().any(|p| p.agent_id == agent.id);
    let start = current.map_or(0, |i| i + 1);
    let next = agents[start.min(agents.len())..]
        .iter()
        .chain(agents[..start.min(agents.len())].iter())
        .find(|a| waiting(a))
        .map(|a| a.id.clone());
    let Some(agent_id) = next else {
        state.set_error("no permission request is pending".to_owned());
        return;
    };
    let permission = state
        .permissions
        .iter()
        .find(|p| p.agent_id == agent_id)
        .cloned();
    state.tree.reveal(&NodeId::Agent(agent_id));
    if let Some(permission) = permission {
        state.dismissed.remove(&permission.id);
        state.open_dialog(Dialog::permission(permission));
    }
}

fn input_key(state: &mut AppState, key: KeyInput, fx: &mut Vec<Effect>) {
    if common_key(state, &key) {
        return;
    }
    match key.key {
        Key::Esc if !key.ctrl && !key.alt => state.focus = Focus::Tree,
        Key::Enter if !key.ctrl && !key.alt => send_message(state, fx),
        _ => {
            state.input.handle(&key);
        }
    }
}

fn send_message(state: &mut AppState, fx: &mut Vec<Effect>) {
    let text = state.input.text().to_owned();
    if text.trim().is_empty() {
        return;
    }
    let Some(agent_id) = state.input_target().map(|a| a.id.clone()) else {
        state.set_error("select an agent first".to_owned());
        return;
    };
    let params = AgentSendParams {
        agent_id: agent_id.clone(),
        text: text.clone(),
        behavior: state.send_mode,
    };
    let request = RequestTag::Send {
        agent_id: agent_id.clone(),
    };
    match call(request, AGENT_SEND, serde_json::to_value(&params)) {
        Ok(effect) => {
            fx.push(effect);
            state.input.take();
            state.unsent = Some((agent_id, text));
            // Follow the conversation again.
            state.stream.scroll_down(usize::MAX);
        }
        Err(message) => state.set_error(message),
    }
}

// ---- dialogs ----

fn dialog_key(state: &mut AppState, key: KeyInput, fx: &mut Vec<Effect>) {
    match &state.dialog {
        Some(Dialog::Permission { .. }) => permission_key(state, &key, fx),
        Some(Dialog::NewAgent(_)) => agent_form_key(state, key, fx),
        Some(Dialog::NewWorkspace(_)) => workspace_form_key(state, key, fx),
        Some(Dialog::ConfirmArchive { .. }) => archive_key(state, &key, fx),
        None => state.focus = state.focus_before_dialog,
    }
}

fn permission_key(state: &mut AppState, key: &KeyInput, fx: &mut Vec<Effect>) {
    let Some(Dialog::Permission {
        permission,
        sending,
        ticks_until_armed,
        seen_end,
        ..
    }) = &mut state.dialog
    else {
        return;
    };
    if key.key == Key::Esc {
        // Closing does not answer: the request stays pending.
        let id = permission.id.clone();
        state.dismissed.insert(id);
        state.close_dialog();
        return;
    }
    let motion = match key.key {
        Key::Up => Some(ScrollMotion::Lines(-1)),
        Key::Down => Some(ScrollMotion::Lines(1)),
        Key::PageUp => Some(ScrollMotion::PageUp),
        Key::PageDown => Some(ScrollMotion::PageDown),
        Key::Home => Some(ScrollMotion::Home),
        Key::End => Some(ScrollMotion::End),
        _ => None,
    };
    if let Some(motion) = motion {
        state.scroll_permission(motion);
        return;
    }
    let decision = match key.printable() {
        Some('a') => Decision::Allow,
        Some('s') => Decision::AllowForSession,
        Some('d') => Decision::Deny,
        _ => return,
    };
    // Not yet armed (just opened or replaced), or already answering.
    if *sending || *ticks_until_armed > 0 {
        return;
    }
    // Allowing needs the whole request to have been on screen; denying never
    // does.
    if decision != Decision::Deny && !*seen_end {
        return;
    }
    let params = PermissionRespondParams {
        id: permission.id.clone(),
        decision,
    };
    let request = RequestTag::Respond {
        permission_id: permission.id.clone(),
    };
    match call(request, PERMISSION_RESPOND, serde_json::to_value(&params)) {
        Ok(effect) => {
            *sending = true;
            fx.push(effect);
        }
        Err(message) => state.set_error(message),
    }
}

fn archive_key(state: &mut AppState, key: &KeyInput, fx: &mut Vec<Effect>) {
    let Some(Dialog::ConfirmArchive { agent_id, .. }) = &state.dialog else {
        return;
    };
    let agent_id = agent_id.clone();
    match (key.key, key.printable()) {
        (Key::Esc, _) | (_, Some('n')) => state.close_dialog(),
        (Key::Enter, _) | (_, Some('y')) => {
            state.close_dialog();
            let params = AgentIdParams {
                id: agent_id.clone(),
            };
            let request = RequestTag::Archive { agent_id };
            push_call(
                state,
                fx,
                call(request, AGENT_ARCHIVE, serde_json::to_value(&params)),
            );
        }
        _ => {}
    }
}

fn agent_form_key(state: &mut AppState, key: KeyInput, fx: &mut Vec<Effect>) {
    let Some(Dialog::NewAgent(form)) = &mut state.dialog else {
        return;
    };
    match key.key {
        Key::Esc => state.close_dialog(),
        Key::Enter => submit_agent(state, fx),
        Key::Tab => form.toggle_provider(),
        Key::BackTab | Key::Up => form.move_field(false),
        Key::Down => form.move_field(true),
        _ => {
            if let Some(editor) = form.editor_mut() {
                editor.handle(&key);
            }
        }
    }
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn submit_agent(state: &mut AppState, fx: &mut Vec<Effect>) {
    let Some(Dialog::NewAgent(form)) = &mut state.dialog else {
        return;
    };
    if form.submitting {
        return;
    }
    let prompt = form.prompt.text().to_owned();
    if prompt.trim().is_empty() {
        form.error = Some("prompt is required".to_owned());
        return;
    }
    let params = AgentCreateParams {
        workspace_id: form.workspace_id.clone(),
        title: non_empty(form.title.text()).unwrap_or_else(|| default_title(&prompt)),
        provider: form.provider,
        model: non_empty(form.model.text()),
        mode: non_empty(form.mode.text()),
        initial_prompt: prompt,
        labels: Default::default(),
        parent_id: None,
    };
    match call(
        RequestTag::CreateAgent,
        AGENT_CREATE,
        serde_json::to_value(&params),
    ) {
        Ok(effect) => {
            form.submitting = true;
            form.error = None;
            fx.push(effect);
        }
        Err(message) => form.error = Some(message),
    }
}

fn workspace_form_key(state: &mut AppState, key: KeyInput, fx: &mut Vec<Effect>) {
    let Some(Dialog::NewWorkspace(form)) = &mut state.dialog else {
        return;
    };
    match key.key {
        Key::Esc => state.close_dialog(),
        Key::Enter => submit_workspace(state, fx),
        Key::Tab => form.toggle_isolation(),
        Key::BackTab | Key::Up | Key::Down => {
            if form.isolation == Isolation::Worktree {
                form.move_field();
            }
        }
        _ => {
            if let Some(editor) = form.editor_mut() {
                editor.handle(&key);
            }
        }
    }
}

fn submit_workspace(state: &mut AppState, fx: &mut Vec<Effect>) {
    let Some(Dialog::NewWorkspace(form)) = &mut state.dialog else {
        return;
    };
    if form.submitting {
        return;
    }
    let (branch, base_ref) = match form.isolation {
        Isolation::Local => (None, None),
        Isolation::Worktree => {
            let Some(branch) = non_empty(form.branch.text()) else {
                form.error = Some("branch is required for a worktree".to_owned());
                return;
            };
            (Some(branch), non_empty(form.base.text()))
        }
    };
    let params = WorkspaceCreateParams {
        project_id: form.project_id.clone(),
        isolation: form.isolation,
        branch,
        base_ref,
        slug: None,
    };
    match call(
        RequestTag::CreateWorkspace,
        WORKSPACE_CREATE,
        serde_json::to_value(&params),
    ) {
        Ok(effect) => {
            form.submitting = true;
            form.error = None;
            fx.push(effect);
        }
        Err(message) => form.error = Some(message),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dgc_protocol::ids::{AgentId, PermissionId, ProjectId, WorkspaceId};
    use dgc_protocol::model::{
        AgentState, PendingPermission, PermissionKind, Project, ProviderKind, Resolution,
        WorkspaceState,
    };
    use serde_json::json;

    use super::*;
    use crate::tui::input::LineEditor;
    use crate::tui::state::{PERMISSION_ARM_TICKS, STREAM_CAP, Tree};

    fn key(state: &mut AppState, key: KeyInput) -> Vec<Effect> {
        update(state, AppEvent::Key(key))
    }

    fn press(state: &mut AppState, c: char) -> Vec<Effect> {
        key(state, KeyInput::char(c))
    }

    fn press_key(state: &mut AppState, k: Key) -> Vec<Effect> {
        key(state, KeyInput::new(k))
    }

    fn type_text(state: &mut AppState, text: &str) {
        for c in text.chars() {
            press(state, c);
        }
    }

    fn project(name: &str) -> Project {
        Project {
            id: ProjectId::generate(),
            path: PathBuf::from("/p"),
            name: name.to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    fn workspace(project_id: &ProjectId, created_at: &str) -> Workspace {
        Workspace {
            id: WorkspaceId::generate(),
            project_id: project_id.clone(),
            isolation: Isolation::Local,
            path: PathBuf::from("/p"),
            branch: None,
            base_ref: None,
            state: WorkspaceState::Active,
            owned: false,
            created_at: created_at.to_owned(),
        }
    }

    fn agent(
        workspace_id: &WorkspaceId,
        title: &str,
        provider: ProviderKind,
        created_at: &str,
    ) -> Agent {
        Agent {
            id: AgentId::generate(),
            workspace_id: workspace_id.clone(),
            title: title.to_owned(),
            provider,
            model: None,
            mode: None,
            auto_approve: false,
            provider_session_id: None,
            state: AgentState::Idle,
            parent_id: None,
            labels: Default::default(),
            created_at: created_at.to_owned(),
            last_error: None,
        }
    }

    fn permission(agent_id: &AgentId) -> PendingPermission {
        PendingPermission::new(
            PermissionId::generate(),
            agent_id.clone(),
            PermissionKind::Command,
            "rm -rf target".to_owned(),
            serde_json::Value::Null,
            String::new(),
        )
    }

    fn event(agent_id: &AgentId, seq: u64, kind: EventKind) -> AgentEvent {
        AgentEvent {
            agent_id: agent_id.clone(),
            seq,
            at: String::new(),
            kind,
        }
    }

    fn text(agent_id: &AgentId, seq: u64, text: &str) -> AgentEvent {
        event(
            agent_id,
            seq,
            EventKind::AssistantText {
                text: text.to_owned(),
                final_: true,
            },
        )
    }

    struct Fixture {
        state: AppState,
        project: Project,
        workspace: Workspace,
        a1: Agent,
        a2: Agent,
    }

    impl Fixture {
        /// One project, one workspace and two agents (claude, then codex);
        /// the cursor is on the project row.
        fn new() -> Self {
            let project = project("doge");
            let workspace = workspace(&project.id, "2026-01-01T00:00:01Z");
            let a1 = agent(
                &workspace.id,
                "first",
                ProviderKind::Claude,
                "2026-01-01T00:00:02Z",
            );
            let a2 = agent(
                &workspace.id,
                "second",
                ProviderKind::Codex,
                "2026-01-01T00:00:03Z",
            );
            let mut state = AppState::default();
            update(
                &mut state,
                AppEvent::Snapshot {
                    projects: vec![project.clone()],
                    workspaces: vec![workspace.clone()],
                    agents: vec![a1.clone(), a2.clone()],
                    permissions: Vec::new(),
                },
            );
            Self {
                state,
                project,
                workspace,
                a1,
                a2,
            }
        }

        /// Snapshot with the same tree plus `permissions`.
        fn snapshot_with(&mut self, permissions: Vec<PendingPermission>) -> Vec<Effect> {
            update(
                &mut self.state,
                AppEvent::Snapshot {
                    projects: vec![self.project.clone()],
                    workspaces: vec![self.workspace.clone()],
                    agents: vec![self.a1.clone(), self.a2.clone()],
                    permissions,
                },
            )
        }

        /// Move the cursor onto the first agent; returns the effects of the move.
        fn select_a1(&mut self) -> Vec<Effect> {
            let mut fx = press_key(&mut self.state, Key::Down);
            fx.extend(press_key(&mut self.state, Key::Down));
            fx
        }

        /// The empty last page of a history load.
        fn finish_history(&mut self, agent: &AgentId) {
            let generation = self.state.stream.generation();
            update(&mut self.state, last_page(agent, generation));
        }

        fn history(&mut self, agent: &AgentId, seqs: std::ops::RangeInclusive<u64>, done: bool) {
            let generation = self.state.stream.generation();
            update(&mut self.state, page(agent, seqs, done, generation));
        }
    }

    fn tick(state: &mut AppState, times: usize) {
        for _ in 0..times {
            update(state, AppEvent::Tick);
        }
    }

    /// Tick until a newly opened permission dialog accepts its keys.
    fn arm(state: &mut AppState) {
        tick(state, usize::from(PERMISSION_ARM_TICKS));
    }

    fn requested(agent: &AgentId, seq: u64, permission: &PendingPermission) -> AppEvent {
        AppEvent::AgentEvent(event(
            agent,
            seq,
            EventKind::PermissionRequested {
                permission: permission.clone(),
            },
        ))
    }

    fn resolved(agent: &AgentId, seq: u64, permission: &PendingPermission) -> AppEvent {
        AppEvent::AgentEvent(event(
            agent,
            seq,
            EventKind::PermissionResolved {
                id: permission.id.clone(),
                resolution: Resolution::Cancelled,
            },
        ))
    }

    fn open_permission_id(state: &AppState) -> Option<PermissionId> {
        match &state.dialog {
            Some(Dialog::Permission { permission, .. }) => Some(permission.id.clone()),
            _ => None,
        }
    }

    fn lines(state: &AppState) -> Vec<String> {
        state.stream.lines().iter().cloned().collect()
    }

    /// The effect for the load that is current in `state`.
    fn load_history(state: &AppState, agent: &Agent, since_seq: u64) -> Effect {
        Effect::LoadHistory {
            agent_id: agent.id.clone(),
            since_seq,
            generation: state.stream.generation(),
        }
    }

    /// The generation of the single `LoadHistory` in `fx`.
    fn generation_of(fx: &[Effect]) -> u64 {
        let generations: Vec<u64> = fx
            .iter()
            .filter_map(|e| match e {
                Effect::LoadHistory { generation, .. } => Some(*generation),
                _ => None,
            })
            .collect();
        assert_eq!(generations.len(), 1, "{fx:?}");
        generations[0]
    }

    /// The empty page that ends a history load.
    fn last_page(agent: &AgentId, generation: u64) -> AppEvent {
        AppEvent::History {
            agent_id: agent.clone(),
            events: Vec::new(),
            done: true,
            generation,
        }
    }

    fn page(
        agent: &AgentId,
        seqs: std::ops::RangeInclusive<u64>,
        done: bool,
        generation: u64,
    ) -> AppEvent {
        AppEvent::History {
            agent_id: agent.clone(),
            events: seqs
                .map(|seq| text(agent, seq, &format!("t{seq}")))
                .collect(),
            done,
            generation,
        }
    }

    #[test]
    fn selecting_agent_loads_history_from_zero() {
        let mut f = Fixture::new();
        assert_eq!(f.state.stream.agent_id(), None);
        assert_eq!(press_key(&mut f.state, Key::Down), vec![], "workspace row");
        assert_eq!(
            press(&mut f.state, 'j'),
            vec![load_history(&f.state, &f.a1, 0)]
        );
        assert_eq!(f.state.stream.agent_id(), Some(&f.a1.id));
        assert!(f.state.stream.is_loading());
        // Moving to the next agent starts over for it.
        f.history(&f.a1.id.clone(), 1..=2, false);
        assert_eq!(
            press(&mut f.state, 'j'),
            vec![load_history(&f.state, &f.a2, 0)]
        );
        assert!(f.state.stream.lines().is_empty());
        assert_eq!(f.state.stream.last_seq(), 0);
    }

    #[test]
    fn history_pages_until_done_without_duplicates() {
        let mut f = Fixture::new();
        f.select_a1();
        let id = f.a1.id.clone();
        let generation = f.state.stream.generation();
        assert_eq!(
            update(&mut f.state, page(&id, 1..=3, false, generation)),
            vec![load_history(&f.state, &f.a1, 3)]
        );
        // The next page repeats seq 3.
        assert_eq!(
            update(&mut f.state, page(&id, 3..=5, false, generation)),
            vec![load_history(&f.state, &f.a1, 5)]
        );
        let last = AppEvent::History {
            agent_id: id.clone(),
            events: Vec::new(),
            done: true,
            generation,
        };
        assert_eq!(update(&mut f.state, last), vec![]);
        assert!(!f.state.stream.is_loading());
        assert_eq!(
            lines(&f.state),
            (1..=5)
                .map(|s| format!("[{s}] assistant: t{s}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn history_that_shows_nothing_new_ends_paging() {
        let mut f = Fixture::new();
        f.select_a1();
        let id = f.a1.id.clone();
        f.history(&id, 1..=2, false);
        // A misbehaving backend repeating the page must not loop forever.
        let generation = f.state.stream.generation();
        let fx = update(&mut f.state, page(&id, 2..=2, false, generation));
        assert_eq!(fx, vec![]);
        assert!(!f.state.stream.is_loading());
    }

    #[test]
    fn history_for_an_unselected_agent_is_ignored() {
        let mut f = Fixture::new();
        f.select_a1();
        let other = f.a2.id.clone();
        let generation = f.state.stream.generation();
        let fx = update(&mut f.state, page(&other, 1..=1, false, generation));
        assert_eq!(fx, vec![]);
        assert!(f.state.stream.lines().is_empty());
        assert!(f.state.stream.is_loading());
    }

    #[test]
    fn live_events_after_history_are_deduplicated_by_seq() {
        let mut f = Fixture::new();
        f.select_a1();
        let id = f.a1.id.clone();
        f.history(&id, 1..=2, false);
        // seq 3 arrives live while history is still loading: held back.
        update(&mut f.state, AppEvent::AgentEvent(text(&id, 3, "t3")));
        assert_eq!(f.state.stream.lines().len(), 2);
        f.finish_history(&id);
        assert_eq!(f.state.stream.lines().len(), 3);
        assert_eq!(f.state.stream.last_seq(), 3);
        // Replays and duplicates change nothing; newer events append.
        update(&mut f.state, AppEvent::AgentEvent(text(&id, 2, "t2")));
        update(&mut f.state, AppEvent::AgentEvent(text(&id, 3, "t3")));
        assert_eq!(f.state.stream.lines().len(), 3);
        update(&mut f.state, AppEvent::AgentEvent(text(&id, 4, "t4")));
        assert_eq!(
            lines(&f.state).last().map(String::as_str),
            Some("[4] assistant: t4")
        );
        // Another agent's events never reach the stream.
        let other = f.a2.id.clone();
        update(&mut f.state, AppEvent::AgentEvent(text(&other, 1, "no")));
        assert_eq!(f.state.stream.lines().len(), 4);
    }

    #[test]
    fn stream_buffer_keeps_last_2000_lines() {
        let mut f = Fixture::new();
        f.select_a1();
        let id = f.a1.id.clone();
        f.history(&id, 1..=2500, true);
        assert_eq!(f.state.stream.lines().len(), STREAM_CAP);
        assert_eq!(
            f.state.stream.lines().front().unwrap(),
            "[501] assistant: t501"
        );
        assert_eq!(
            f.state.stream.lines().back().unwrap(),
            "[2500] assistant: t2500"
        );
        assert_eq!(f.state.stream.last_seq(), 2500);
    }

    #[test]
    fn multi_line_and_control_text_becomes_escaped_lines() {
        let mut f = Fixture::new();
        f.select_a1();
        let id = f.a1.id.clone();
        f.finish_history(&id);
        update(
            &mut f.state,
            AppEvent::AgentEvent(text(&id, 1, "a\x1b[2J\nb\u{202e}")),
        );
        assert_eq!(
            lines(&f.state),
            vec!["[1] assistant: a\\x1b[2J", "    b\\u{202e}"]
        );
    }

    #[test]
    fn page_keys_scroll_the_stream_and_new_lines_keep_the_view() {
        let mut f = Fixture::new();
        f.select_a1();
        let id = f.a1.id.clone();
        f.history(&id, 1..=100, true);
        update(
            &mut f.state,
            AppEvent::Resize {
                width: 80,
                height: 25,
            },
        );
        assert_eq!(f.state.page_lines(), 20);
        press_key(&mut f.state, Key::PageUp);
        assert_eq!(f.state.stream.scroll(), 20);
        update(&mut f.state, AppEvent::AgentEvent(text(&id, 101, "new")));
        assert_eq!(
            f.state.stream.scroll(),
            21,
            "the view stays on the same lines"
        );
        press_key(&mut f.state, Key::PageDown);
        press_key(&mut f.state, Key::PageDown);
        assert_eq!(f.state.stream.scroll(), 0, "0 follows the tail");
    }

    #[test]
    fn enter_in_input_sends_with_current_mode() {
        let mut f = Fixture::new();
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        assert_eq!(f.state.focus, Focus::Input);
        type_text(&mut f.state, "hi there");
        press_key(&mut f.state, Key::Tab);
        assert_eq!(f.state.send_mode, SendBehavior::Steer);
        let fx = press_key(&mut f.state, Key::Enter);
        assert_eq!(
            fx,
            vec![Effect::Call {
                request: RequestTag::Send {
                    agent_id: f.a1.id.clone()
                },
                method: "agent.send",
                params: json!({"agent_id": f.a1.id, "text": "hi there", "behavior": "steer"}),
            }]
        );
        assert!(f.state.input.is_empty());
        assert_eq!(f.state.focus, Focus::Input, "stay in the conversation");
        // An empty or blank message sends nothing.
        type_text(&mut f.state, "  ");
        assert_eq!(press_key(&mut f.state, Key::Enter), vec![]);
    }

    #[test]
    fn failed_send_restores_the_message() {
        let mut f = Fixture::new();
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        type_text(&mut f.state, "hello");
        press_key(&mut f.state, Key::Enter);
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::Send {
                    agent_id: f.a1.id.clone(),
                },
                result: Err("daemon said no".to_owned()),
            },
        );
        assert_eq!(f.state.input.text(), "hello");
        assert_eq!(f.state.status.error.as_deref(), Some("daemon said no"));
    }

    #[test]
    fn tab_cycles_send_mode() {
        let mut state = AppState::default();
        assert_eq!(state.send_mode, SendBehavior::Queue);
        press_key(&mut state, Key::Tab);
        assert_eq!(state.send_mode, SendBehavior::Steer);
        press_key(&mut state, Key::Tab);
        assert_eq!(state.send_mode, SendBehavior::Interrupt);
        press_key(&mut state, Key::Tab);
        assert_eq!(state.send_mode, SendBehavior::Queue);
        press_key(&mut state, Key::BackTab);
        assert_eq!(state.send_mode, SendBehavior::Interrupt);
    }

    #[test]
    fn permission_dialog_opens_for_selected_waiting_agent_and_responds() {
        let mut f = Fixture::new();
        let perm = permission(&f.a1.id);
        f.snapshot_with(vec![perm.clone()]);
        assert!(f.state.dialog.is_none(), "nothing selected yet");
        assert!(
            f.state.rows().iter().any(|r| r.waiting_permission),
            "the tree marks the waiting agent"
        );
        f.select_a1();
        assert_eq!(f.state.focus, Focus::Dialog);
        assert!(matches!(f.state.dialog, Some(Dialog::Permission { .. })));
        arm(&mut f.state);

        let fx = press(&mut f.state, 'a');
        assert_eq!(
            fx,
            vec![Effect::Call {
                request: RequestTag::Respond {
                    permission_id: perm.id.clone()
                },
                method: "permission.respond",
                params: json!({"id": perm.id, "decision": "allow"}),
            }]
        );
        // Still open until the daemon confirms; keys do not answer twice.
        assert!(f.state.dialog.is_some());
        assert_eq!(press(&mut f.state, 'd'), vec![]);

        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::Respond {
                    permission_id: perm.id.clone(),
                },
                result: Ok(json!({})),
            },
        );
        assert!(f.state.dialog.is_none());
        assert_eq!(f.state.focus, Focus::Tree);
        assert_eq!(f.state.permission_count(), 0);
    }

    #[test]
    fn permission_keys_map_to_decisions() {
        for (c, decision) in [('s', "allow_for_session"), ('d', "deny")] {
            let mut f = Fixture::new();
            let perm = permission(&f.a1.id);
            f.snapshot_with(vec![perm.clone()]);
            f.select_a1();
            arm(&mut f.state);
            let fx = press(&mut f.state, c);
            assert_eq!(
                fx,
                vec![Effect::Call {
                    request: RequestTag::Respond {
                        permission_id: perm.id.clone()
                    },
                    method: "permission.respond",
                    params: json!({"id": perm.id, "decision": decision}),
                }]
            );
        }
    }

    #[test]
    fn failed_respond_keeps_the_dialog_usable() {
        let mut f = Fixture::new();
        let perm = permission(&f.a1.id);
        f.snapshot_with(vec![perm.clone()]);
        f.select_a1();
        arm(&mut f.state);
        press(&mut f.state, 'a');
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::Respond {
                    permission_id: perm.id.clone(),
                },
                result: Err("already resolved\x1b".to_owned()),
            },
        );
        assert_eq!(
            f.state.status.error.as_deref(),
            Some("already resolved\\x1b")
        );
        assert_eq!(press(&mut f.state, 'd').len(), 1, "can answer again");
    }

    #[test]
    fn closing_the_permission_dialog_leaves_it_pending() {
        let mut f = Fixture::new();
        let perm = permission(&f.a1.id);
        f.snapshot_with(vec![perm.clone()]);
        f.select_a1();
        assert_eq!(press_key(&mut f.state, Key::Esc), vec![]);
        assert!(f.state.dialog.is_none());
        assert_eq!(f.state.focus, Focus::Tree);
        assert_eq!(f.state.permission_count(), 1);
        // Neither ticks nor snapshots reopen it...
        update(&mut f.state, AppEvent::Tick);
        f.snapshot_with(vec![perm.clone()]);
        assert!(f.state.dialog.is_none());
        assert!(f.state.rows().iter().any(|r| r.waiting_permission));
        // ...but `p` brings it back.
        press(&mut f.state, 'p');
        assert!(matches!(f.state.dialog, Some(Dialog::Permission { .. })));
    }

    #[test]
    fn p_jumps_to_the_next_waiting_agent() {
        let mut f = Fixture::new();
        let perm = permission(&f.a2.id);
        f.snapshot_with(vec![perm.clone()]);
        assert_eq!(
            press(&mut f.state, 'p'),
            vec![load_history(&f.state, &f.a2, 0)]
        );
        assert_eq!(f.state.selected_agent_id(), Some(f.a2.id.clone()));
        assert!(matches!(
            &f.state.dialog,
            Some(Dialog::Permission { permission, .. }) if permission.id == perm.id
        ));
        // Nothing pending: say so.
        let mut f = Fixture::new();
        press(&mut f.state, 'p');
        assert!(f.state.status.error.is_some());
    }

    #[test]
    fn permission_events_open_and_close_the_dialog() {
        let mut f = Fixture::new();
        f.select_a1();
        f.finish_history(&f.a1.id.clone());
        let perm = permission(&f.a1.id);
        update(
            &mut f.state,
            AppEvent::AgentEvent(event(
                &f.a1.id,
                1,
                EventKind::PermissionRequested {
                    permission: perm.clone(),
                },
            )),
        );
        assert!(matches!(f.state.dialog, Some(Dialog::Permission { .. })));
        assert_eq!(f.state.permission_count(), 1);
        update(
            &mut f.state,
            AppEvent::AgentEvent(event(
                &f.a1.id,
                2,
                EventKind::PermissionResolved {
                    id: perm.id.clone(),
                    resolution: Resolution::Cancelled,
                },
            )),
        );
        assert!(f.state.dialog.is_none());
        assert_eq!(f.state.focus, Focus::Tree);
        assert_eq!(f.state.permission_count(), 0);
    }

    #[test]
    fn ctrl_u_then_a_does_not_answer_a_permission() {
        let mut f = Fixture::new();
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        type_text(&mut f.state, "add");
        let perm = permission(&f.a1.id);
        update(&mut f.state, requested(&f.a1.id, 1, &perm));
        assert!(f.state.dialog.is_none(), "no dialog while typing");
        assert_eq!(f.state.input.text(), "add");
        // Emptying the input does not open it either...
        key(&mut f.state, KeyInput::ctrl('u'));
        assert!(f.state.dialog.is_none());
        assert_eq!(f.state.focus, Focus::Input);
        // ...so the next typed `a` is text, not an answer.
        assert_eq!(press(&mut f.state, 'a'), vec![]);
        assert_eq!(f.state.input.text(), "a");
        assert_eq!(f.state.permission_count(), 1);
    }

    #[test]
    fn backspacing_the_input_empty_does_not_open_a_permission() {
        let mut f = Fixture::new();
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        type_text(&mut f.state, "a");
        let perm = permission(&f.a1.id);
        update(&mut f.state, requested(&f.a1.id, 1, &perm));
        press_key(&mut f.state, Key::Backspace);
        assert!(f.state.dialog.is_none());
        assert_eq!(press(&mut f.state, 'd'), vec![]);
        assert_eq!(f.state.input.text(), "d");
    }

    #[test]
    fn typing_after_sending_does_not_answer_a_permission() {
        let mut f = Fixture::new();
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        type_text(&mut f.state, "run it");
        assert_eq!(press_key(&mut f.state, Key::Enter).len(), 1);
        assert!(f.state.input.is_empty());
        // The agent asks for permission right after the message is sent.
        let perm = permission(&f.a1.id);
        update(&mut f.state, requested(&f.a1.id, 1, &perm));
        assert!(f.state.dialog.is_none());
        assert_eq!(press(&mut f.state, 'a'), vec![]);
        assert_eq!(f.state.input.text(), "a");
        assert_eq!(f.state.permission_count(), 1);
    }

    #[test]
    fn esc_to_tree_opens_pending_permission_but_keys_wait_three_ticks() {
        let mut f = Fixture::new();
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        let perm = permission(&f.a1.id);
        update(&mut f.state, requested(&f.a1.id, 1, &perm));
        assert!(f.state.dialog.is_none(), "never opens in the input");
        press_key(&mut f.state, Key::Esc);
        assert_eq!(open_permission_id(&f.state), Some(perm.id.clone()));
        // A key typed right after Esc must not answer.
        assert_eq!(press(&mut f.state, 'a'), vec![]);
        tick(&mut f.state, 1);
        assert_eq!(press(&mut f.state, 'a'), vec![]);
        tick(&mut f.state, 1);
        assert_eq!(press(&mut f.state, 'a'), vec![]);
        tick(&mut f.state, 1);
        assert_eq!(press(&mut f.state, 'a').len(), 1);
    }

    #[test]
    fn decision_keys_are_ignored_for_three_ticks_after_a_dialog_opens() {
        let mut f = Fixture::new();
        f.snapshot_with(vec![permission(&f.a1.id)]);
        f.select_a1();
        assert!(f.state.dialog.is_some());
        for c in ['a', 's', 'd'] {
            assert_eq!(press(&mut f.state, c), vec![], "{c} right after opening");
        }
        tick(&mut f.state, 1);
        assert_eq!(press(&mut f.state, 's'), vec![], "one tick is not enough");
        tick(&mut f.state, 1);
        assert_eq!(press(&mut f.state, 's'), vec![], "two ticks may be 250 ms");
        tick(&mut f.state, 1);
        assert_eq!(press(&mut f.state, 's').len(), 1);

        // Esc is not delayed.
        let mut f = Fixture::new();
        f.snapshot_with(vec![permission(&f.a1.id)]);
        f.select_a1();
        press_key(&mut f.state, Key::Esc);
        assert!(f.state.dialog.is_none());
    }

    #[test]
    fn resolve_then_next_permission_opens_and_an_immediate_key_is_ignored() {
        let mut f = Fixture::new();
        let p1 = permission(&f.a1.id);
        let p2 = permission(&f.a1.id);
        f.snapshot_with(vec![p1.clone(), p2.clone()]);
        f.select_a1();
        arm(&mut f.state);
        assert_eq!(open_permission_id(&f.state), Some(p1.id.clone()));
        // P1 is resolved elsewhere; P2 takes its place in the same update.
        update(&mut f.state, resolved(&f.a1.id, 1, &p1));
        assert_eq!(open_permission_id(&f.state), Some(p2.id.clone()));
        assert_eq!(
            press(&mut f.state, 'a'),
            vec![],
            "meant for P1, must not answer P2"
        );
        arm(&mut f.state);
        let fx = press(&mut f.state, 'a');
        assert!(matches!(
            fx.as_slice(),
            [Effect::Call { request: RequestTag::Respond { permission_id }, .. }]
                if *permission_id == p2.id
        ));
    }

    #[test]
    fn our_own_answer_then_the_next_permission_is_guarded_too() {
        let mut f = Fixture::new();
        let p1 = permission(&f.a1.id);
        let p2 = permission(&f.a1.id);
        f.snapshot_with(vec![p1.clone(), p2.clone()]);
        f.select_a1();
        arm(&mut f.state);
        press(&mut f.state, 'a');
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::Respond {
                    permission_id: p1.id.clone(),
                },
                result: Ok(json!({})),
            },
        );
        assert_eq!(open_permission_id(&f.state), Some(p2.id.clone()));
        assert_eq!(press(&mut f.state, 'a'), vec![]);
    }

    #[test]
    fn a_draft_in_the_tree_blocks_auto_open_but_p_opens_explicitly() {
        let mut f = Fixture::new();
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        type_text(&mut f.state, "draft");
        press_key(&mut f.state, Key::Esc);
        assert_eq!(f.state.focus, Focus::Tree);
        let perm = permission(&f.a1.id);
        update(&mut f.state, requested(&f.a1.id, 1, &perm));
        assert!(f.state.dialog.is_none(), "the draft is not empty");
        press(&mut f.state, 'p');
        assert_eq!(open_permission_id(&f.state), Some(perm.id.clone()));
        assert_eq!(f.state.input.text(), "draft");
    }

    /// Which agent a `Send` effect addresses and with what text.
    fn sent(fx: &[Effect]) -> Vec<(String, String)> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Call { params, .. } => Some((
                    params.get("agent_id")?.as_str()?.to_owned(),
                    params.get("text")?.as_str()?.to_owned(),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_draft_stays_with_its_agent() {
        let mut f = Fixture::new();
        f.select_a1();
        press(&mut f.state, 'i');
        type_text(&mut f.state, "for a1");
        press_key(&mut f.state, Key::Esc);
        // Select the other agent, open the input and send.
        press(&mut f.state, 'j');
        assert_eq!(f.state.selected_agent_id(), Some(f.a2.id.clone()));
        press(&mut f.state, 'i');
        assert_eq!(f.state.focus, Focus::Input);
        assert!(f.state.input.is_empty(), "a1's text is not a2's draft");
        let fx = press_key(&mut f.state, Key::Enter);
        assert_eq!(sent(&fx), vec![], "nothing to send to a2");
        type_text(&mut f.state, "for a2");
        press_key(&mut f.state, Key::Esc);
        // Back at a1: its own draft is restored, and it goes to a1 only.
        press(&mut f.state, 'k');
        assert_eq!(f.state.selected_agent_id(), Some(f.a1.id.clone()));
        press(&mut f.state, 'i');
        assert_eq!(f.state.input.text(), "for a1");
        let fx = press_key(&mut f.state, Key::Enter);
        assert_eq!(sent(&fx), vec![(f.a1.id.to_string(), "for a1".to_owned())]);
        // a2 kept its draft.
        press_key(&mut f.state, Key::Esc);
        press(&mut f.state, 'j');
        press(&mut f.state, 'i');
        assert_eq!(f.state.input.text(), "for a2");
    }

    #[test]
    fn another_agents_draft_does_not_block_the_permission_dialog() {
        let mut f = Fixture::new();
        f.select_a1();
        press(&mut f.state, 'i');
        type_text(&mut f.state, "for a1");
        press_key(&mut f.state, Key::Esc);
        press(&mut f.state, 'j');
        let perm = permission(&f.a2.id);
        update(&mut f.state, requested(&f.a2.id, 1, &perm));
        assert_eq!(
            open_permission_id(&f.state),
            Some(perm.id.clone()),
            "a2 has no draft, so its request opens"
        );
    }

    #[test]
    fn a_permission_of_an_unselected_agent_opens_no_dialog() {
        let mut f = Fixture::new();
        f.select_a1();
        let other = permission(&f.a2.id);
        update(&mut f.state, requested(&f.a2.id, 1, &other));
        assert!(f.state.dialog.is_none());
        assert_eq!(f.state.focus, Focus::Tree);
        assert_eq!(f.state.permission_count(), 1);
        // Neither does a snapshot that carries it.
        f.snapshot_with(vec![other]);
        assert!(f.state.dialog.is_none());
    }

    #[test]
    fn a_new_permission_does_not_replace_an_open_dialog() {
        let mut f = Fixture::new();
        let p1 = permission(&f.a1.id);
        f.snapshot_with(vec![p1.clone()]);
        f.select_a1();
        assert_eq!(f.state.focus, Focus::Dialog);
        let p2 = permission(&f.a1.id);
        let p3 = permission(&f.a2.id);
        update(&mut f.state, requested(&f.a1.id, 1, &p2));
        update(&mut f.state, requested(&f.a2.id, 1, &p3));
        assert_eq!(open_permission_id(&f.state), Some(p1.id.clone()));
        assert_eq!(f.state.permission_count(), 3);
        // A snapshot does not replace it either.
        f.snapshot_with(vec![p2, p1.clone(), p3]);
        assert_eq!(open_permission_id(&f.state), Some(p1.id));
    }

    #[test]
    fn unknown_agent_event_triggers_snapshot() {
        let mut f = Fixture::new();
        let stranger = AgentId::generate();
        let fx = update(
            &mut f.state,
            AppEvent::AgentEvent(text(&stranger, 1, "hello")),
        );
        assert_eq!(fx, vec![Effect::LoadSnapshot]);
        // A burst does not request a snapshot per event.
        let fx = update(
            &mut f.state,
            AppEvent::AgentEvent(text(&stranger, 2, "again")),
        );
        assert_eq!(fx, vec![]);
        // Once the snapshot is answered (or has timed out), a new one may follow.
        f.snapshot_with(vec![]);
        let fx = update(
            &mut f.state,
            AppEvent::AgentEvent(text(&stranger, 3, "still unknown")),
        );
        assert_eq!(fx, vec![Effect::LoadSnapshot]);
        for _ in 0..SNAPSHOT_RETRY_TICKS {
            update(&mut f.state, AppEvent::Tick);
        }
        let fx = update(
            &mut f.state,
            AppEvent::AgentEvent(text(&stranger, 4, "unanswered")),
        );
        assert_eq!(fx, vec![Effect::LoadSnapshot]);
    }

    #[test]
    fn state_changes_update_the_tree_and_archived_agents_disappear() {
        let mut f = Fixture::new();
        f.select_a1();
        let state_changed = |agent: &Agent, seq, state| {
            AppEvent::AgentEvent(event(&agent.id, seq, EventKind::StateChanged { state }))
        };
        update(&mut f.state, state_changed(&f.a2, 1, AgentState::Running));
        let rows = f.state.rows();
        assert_eq!(rows[3].agent_state, Some(AgentState::Running));

        // The agent being typed at is archived: the cursor falls back to the
        // sibling but the input keeps focus and has no target.
        press_key(&mut f.state, Key::Enter);
        assert_eq!(f.state.focus, Focus::Input);
        type_text(&mut f.state, "q");
        let fx = update(&mut f.state, state_changed(&f.a1, 1, AgentState::Archived));
        assert_eq!(f.state.rows().len(), 3);
        assert_eq!(fx, vec![]);
        assert_eq!(
            f.state.focus,
            Focus::Input,
            "typing must not become commands"
        );
        assert_eq!(f.state.stream.agent_id(), None);
        assert_eq!(f.state.input.text(), "q");
        assert_eq!(press(&mut f.state, 'x'), vec![], "`x` is text here");
        assert_eq!(f.state.input.text(), "qx");
        // Sending says there is nowhere to send to, and keeps the text.
        assert_eq!(press_key(&mut f.state, Key::Enter), vec![]);
        assert_eq!(
            f.state.status.error.as_deref(),
            Some("select an agent first")
        );
        assert_eq!(f.state.input.text(), "qx");
        assert_eq!(f.state.agent_count(), 1);
        // Back in the tree the sibling is shown.
        let fx = press_key(&mut f.state, Key::Esc);
        assert_eq!(f.state.selected_agent_id(), Some(f.a2.id.clone()));
        assert_eq!(fx, vec![load_history(&f.state, &f.a2, 0)]);
    }

    #[test]
    fn a_sibling_permission_cannot_be_answered_after_the_target_is_lost() {
        let mut f = Fixture::new();
        f.snapshot_with(vec![permission(&f.a2.id)]);
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        // a1 disappears from a snapshot; the cursor falls back to a2.
        update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents: vec![f.a2.clone()],
                permissions: vec![permission(&f.a2.id)],
            },
        );
        assert_eq!(f.state.focus, Focus::Input);
        assert!(f.state.dialog.is_none());
        for c in ['a', 's', 'd'] {
            assert_eq!(press(&mut f.state, c), vec![]);
        }
        assert_eq!(f.state.input.text(), "asd");
        assert_eq!(f.state.permission_count(), 1);
    }

    #[test]
    fn tree_orders_siblings_and_hides_archived() {
        let zeta = project("zeta");
        let alpha = project("alpha");
        let ws_late = workspace(&alpha.id, "2026-02-01T00:00:00Z");
        let ws_early = workspace(&alpha.id, "2026-01-01T00:00:00Z");
        let mut ws_archived = workspace(&alpha.id, "2026-01-15T00:00:00Z");
        ws_archived.state = WorkspaceState::Archived;
        let late = agent(
            &ws_early.id,
            "late",
            ProviderKind::Claude,
            "2026-03-01T00:00:00Z",
        );
        let early = agent(
            &ws_early.id,
            "early",
            ProviderKind::Codex,
            "2026-02-01T00:00:00Z",
        );
        let mut gone = agent(
            &ws_early.id,
            "gone",
            ProviderKind::Claude,
            "2026-02-15T00:00:00Z",
        );
        gone.state = AgentState::Archived;
        let mut tree = Tree::default();
        tree.replace(
            vec![zeta, alpha],
            vec![ws_late, ws_archived, ws_early],
            vec![late, gone, early],
        );
        let labels: Vec<_> = tree
            .rows(&[])
            .into_iter()
            .map(|r| (r.depth, r.label))
            .collect();
        assert_eq!(
            labels,
            vec![
                (0, "alpha".to_owned()),
                (1, "ws local".to_owned()),
                (2, "early".to_owned()),
                (2, "late".to_owned()),
                (1, "ws local".to_owned()),
                (0, "zeta".to_owned()),
            ]
        );
    }

    #[test]
    fn selection_survives_snapshots_by_id_and_falls_back_to_a_sibling() {
        let mut f = Fixture::new();
        f.select_a1();
        // A snapshot with the agents in the other order keeps the selection.
        let mut newer = f.a1.clone();
        newer.created_at = "2026-12-01T00:00:00Z".to_owned();
        let fx = update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents: vec![newer, f.a2.clone()],
                permissions: vec![],
            },
        );
        assert_eq!(fx, vec![]);
        assert_eq!(f.state.selected_agent_id(), Some(f.a1.id.clone()));
        assert_eq!(f.state.cursor_index(), Some(3));
        // The selected agent disappears: the neighbour is selected.
        let fx = update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents: vec![f.a2.clone()],
                permissions: vec![],
            },
        );
        assert_eq!(f.state.selected_agent_id(), Some(f.a2.id.clone()));
        assert_eq!(fx, vec![load_history(&f.state, &f.a2, 0)]);
        // The whole project goes away: nothing is selected.
        let fx = update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![],
                workspaces: vec![],
                agents: vec![],
                permissions: vec![],
            },
        );
        assert_eq!(fx, vec![]);
        assert!(f.state.tree.cursor().is_none());
        assert!(f.state.stream.agent_id().is_none());
    }

    #[test]
    fn tree_navigation_folds_and_unfolds() {
        let mut f = Fixture::new();
        assert_eq!(f.state.rows().len(), 4);
        press_key(&mut f.state, Key::Down); // workspace
        press_key(&mut f.state, Key::Left); // fold it
        assert_eq!(f.state.rows().len(), 2);
        press_key(&mut f.state, Key::Right); // unfold
        assert_eq!(f.state.rows().len(), 4);
        press_key(&mut f.state, Key::Right); // into the first agent
        assert_eq!(f.state.selected_agent_id(), Some(f.a1.id.clone()));
        press_key(&mut f.state, Key::Left); // back to the workspace
        assert!(f.state.selected_agent().is_none());
        press_key(&mut f.state, Key::Enter); // toggles
        assert_eq!(f.state.rows().len(), 2);
        press_key(&mut f.state, Key::Enter);
        assert_eq!(f.state.rows().len(), 4);
        // The cursor stops at the ends.
        for _ in 0..10 {
            press(&mut f.state, 'j');
        }
        assert_eq!(f.state.cursor_index(), Some(3));
        for _ in 0..10 {
            press(&mut f.state, 'k');
        }
        assert_eq!(f.state.cursor_index(), Some(0));
    }

    #[test]
    fn i_and_enter_focus_the_input_only_on_an_agent() {
        let mut f = Fixture::new();
        press(&mut f.state, 'i');
        assert_eq!(f.state.focus, Focus::Tree);
        assert!(f.state.status.error.is_some());
        f.select_a1();
        press(&mut f.state, 'i');
        assert_eq!(f.state.focus, Focus::Input);
        press_key(&mut f.state, Key::Esc);
        assert_eq!(f.state.focus, Focus::Tree);
        // In the input, letters are text and not commands.
        press(&mut f.state, 'i');
        assert_eq!(press(&mut f.state, 'q'), vec![]);
        assert_eq!(f.state.input.text(), "q");
    }

    #[test]
    fn interrupt_and_resume_act_on_the_selected_agent() {
        let mut f = Fixture::new();
        assert_eq!(press(&mut f.state, 'x'), vec![], "no agent selected");
        assert!(f.state.status.error.is_some());
        f.select_a1();
        let params = json!({"id": f.a1.id});
        assert_eq!(
            press(&mut f.state, 'x'),
            vec![Effect::Call {
                request: RequestTag::Interrupt {
                    agent_id: f.a1.id.clone()
                },
                method: "agent.interrupt",
                params: params.clone(),
            }]
        );
        assert_eq!(
            press(&mut f.state, 'r'),
            vec![Effect::Call {
                request: RequestTag::Resume {
                    agent_id: f.a1.id.clone()
                },
                method: "agent.resume",
                params,
            }]
        );
    }

    #[test]
    fn lagged_resubscribes_and_reloads() {
        let mut f = Fixture::new();
        f.select_a1();
        let id = f.a1.id.clone();
        f.history(&id, 1..=3, true);
        let fx = update(&mut f.state, AppEvent::Lagged);
        assert_eq!(
            fx,
            vec![
                Effect::Resubscribe,
                Effect::LoadSnapshot,
                load_history(&f.state, &f.a1, 3)
            ]
        );
        assert_eq!(f.state.stream.lines().len(), 3, "the lines stay");
        // Events that arrive while catching up are applied after it.
        update(&mut f.state, AppEvent::AgentEvent(text(&id, 6, "t6")));
        f.history(&id, 4..=5, false);
        f.finish_history(&id);
        assert_eq!(
            lines(&f.state)
                .iter()
                .map(|l| l.split(' ').next().unwrap().to_owned())
                .collect::<Vec<_>>(),
            ["[1]", "[2]", "[3]", "[4]", "[5]", "[6]"]
        );
    }

    #[test]
    fn connection_changes_update_the_status_and_reload_on_connect() {
        let mut f = Fixture::new();
        f.select_a1();
        let id = f.a1.id.clone();
        f.history(&id, 1..=4, true);
        let fx = update(
            &mut f.state,
            AppEvent::Connection(ConnectionState::Disconnected {
                reason: "eof\x1b".to_owned(),
            }),
        );
        assert_eq!(fx, vec![]);
        assert_eq!(
            f.state.status.connection,
            ConnectionState::Disconnected {
                reason: "eof\\x1b".to_owned()
            }
        );
        let fx = update(
            &mut f.state,
            AppEvent::Connection(ConnectionState::Reconnecting),
        );
        assert_eq!(fx, vec![]);
        let fx = update(
            &mut f.state,
            AppEvent::Connection(ConnectionState::Connected),
        );
        assert_eq!(
            fx,
            vec![Effect::LoadSnapshot, load_history(&f.state, &f.a1, 4)]
        );
        assert_eq!(f.state.status.connection, ConnectionState::Connected);
    }

    #[test]
    fn rpc_errors_show_until_success_or_ten_ticks() {
        let mut state = AppState::default();
        let fail = |state: &mut AppState| {
            update(
                state,
                AppEvent::RpcDone {
                    request: RequestTag::CreateAgent,
                    result: Err("boom\n\x1b[31m".to_owned()),
                },
            )
        };
        fail(&mut state);
        assert_eq!(state.status.error.as_deref(), Some("boom\\n\\x1b[31m"));
        for _ in 0..9 {
            update(&mut state, AppEvent::Tick);
        }
        assert!(state.status.error.is_some());
        update(&mut state, AppEvent::Tick);
        assert!(state.status.error.is_none());

        fail(&mut state);
        update(
            &mut state,
            AppEvent::RpcDone {
                request: RequestTag::Interrupt {
                    agent_id: AgentId::generate(),
                },
                result: Ok(json!({})),
            },
        );
        assert!(state.status.error.is_none());
    }

    #[test]
    fn new_agent_dialog_requires_prompt_and_warns_on_auto_approve_mode() {
        let mut f = Fixture::new();
        press_key(&mut f.state, Key::Down); // workspace row
        press(&mut f.state, 'n');
        assert_eq!(f.state.focus, Focus::Dialog);

        // The prompt is required.
        assert_eq!(press_key(&mut f.state, Key::Enter), vec![]);
        let Some(Dialog::NewAgent(form)) = &f.state.dialog else {
            panic!("dialog closed");
        };
        assert_eq!(form.error.as_deref(), Some("prompt is required"));
        assert!(!form.auto_approve_mode());

        // Mode field: Prompt -> Title -> Mode.
        press_key(&mut f.state, Key::Up);
        press_key(&mut f.state, Key::Up);
        type_text(&mut f.state, "bypassPermissions");
        let Some(Dialog::NewAgent(form)) = &f.state.dialog else {
            panic!("dialog closed");
        };
        assert!(form.auto_approve_mode(), "claude bypassPermissions warns");
        // The same text means nothing to codex; its own auto mode does.
        press_key(&mut f.state, Key::Tab);
        let Some(Dialog::NewAgent(form)) = &mut f.state.dialog else {
            panic!("dialog closed");
        };
        assert_eq!(form.provider, ProviderKind::Codex);
        assert!(!form.auto_approve_mode());
        form.mode = LineEditor::default();
        type_text(&mut f.state, "full-access");
        let Some(Dialog::NewAgent(form)) = &f.state.dialog else {
            panic!("dialog closed");
        };
        assert!(form.auto_approve_mode(), "codex full-access warns");

        // Fill the prompt and the model, then create.
        press_key(&mut f.state, Key::Down);
        press_key(&mut f.state, Key::Down);
        type_text(&mut f.state, "fix the test\nplease");
        press_key(&mut f.state, Key::Up);
        press_key(&mut f.state, Key::Up);
        press_key(&mut f.state, Key::Up);
        type_text(&mut f.state, " gpt-5 ");
        let fx = press_key(&mut f.state, Key::Enter);
        assert_eq!(
            fx,
            vec![Effect::Call {
                request: RequestTag::CreateAgent,
                method: "agent.create",
                params: json!({
                    "workspace_id": f.workspace.id,
                    "title": "fix the testplease",
                    "provider": "codex",
                    "model": "gpt-5",
                    "mode": "full-access",
                    "initial_prompt": "fix the testplease",
                    "labels": {},
                    "parent_id": null,
                }),
            }]
        );
        // Enter again while the call is in flight does nothing.
        assert_eq!(press_key(&mut f.state, Key::Enter), vec![]);
    }

    #[test]
    fn create_agent_failure_shows_in_dialog_and_success_selects_the_agent() {
        let mut f = Fixture::new();
        press_key(&mut f.state, Key::Down);
        press(&mut f.state, 'n');
        type_text(&mut f.state, "go");
        press_key(&mut f.state, Key::Enter);
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::CreateAgent,
                result: Err("workspace is archived".to_owned()),
            },
        );
        let Some(Dialog::NewAgent(form)) = &f.state.dialog else {
            panic!("dialog closed");
        };
        assert_eq!(form.error.as_deref(), Some("workspace is archived"));
        assert!(!form.submitting);

        assert_eq!(press_key(&mut f.state, Key::Enter).len(), 1, "retry");
        let created = agent(
            &f.workspace.id,
            "go",
            ProviderKind::Claude,
            "2026-06-01T00:00:00Z",
        );
        let fx = update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::CreateAgent,
                result: Ok(serde_json::to_value(&created).expect("agent json")),
            },
        );
        assert!(f.state.dialog.is_none());
        assert_eq!(f.state.focus, Focus::Tree);
        assert_eq!(f.state.selected_agent_id(), Some(created.id.clone()));
        assert_eq!(fx, vec![load_history(&f.state, &created, 0)]);
        assert!(f.state.status.error.is_none());
    }

    #[test]
    fn esc_cancels_dialogs() {
        let mut f = Fixture::new();
        press_key(&mut f.state, Key::Down);
        press(&mut f.state, 'n');
        press_key(&mut f.state, Key::Esc);
        assert!(f.state.dialog.is_none());
        assert_eq!(f.state.focus, Focus::Tree);
        press(&mut f.state, 'w');
        press_key(&mut f.state, Key::Esc);
        assert!(f.state.dialog.is_none());
        // `n` needs a workspace under the cursor.
        key(&mut f.state, KeyInput::new(Key::Up));
        press(&mut f.state, 'n');
        assert!(f.state.dialog.is_none());
        assert!(f.state.status.error.is_some());
    }

    #[test]
    fn new_worktree_workspace_requires_branch() {
        let mut f = Fixture::new();
        press(&mut f.state, 'w');
        let Some(Dialog::NewWorkspace(form)) = &f.state.dialog else {
            panic!("no dialog");
        };
        assert_eq!(form.isolation, Isolation::Local);

        press_key(&mut f.state, Key::Tab);
        assert_eq!(press_key(&mut f.state, Key::Enter), vec![]);
        let Some(Dialog::NewWorkspace(form)) = &f.state.dialog else {
            panic!("no dialog");
        };
        assert_eq!(form.isolation, Isolation::Worktree);
        assert_eq!(
            form.error.as_deref(),
            Some("branch is required for a worktree")
        );

        type_text(&mut f.state, "feat/x");
        press_key(&mut f.state, Key::Down);
        type_text(&mut f.state, "main");
        let fx = press_key(&mut f.state, Key::Enter);
        assert_eq!(
            fx,
            vec![Effect::Call {
                request: RequestTag::CreateWorkspace,
                method: "workspace.create",
                params: json!({
                    "project_id": f.project.id,
                    "isolation": "worktree",
                    "branch": "feat/x",
                    "base_ref": "main",
                    "slug": null,
                }),
            }]
        );

        let mut created = workspace(&f.project.id, "2026-06-01T00:00:00Z");
        created.isolation = Isolation::Worktree;
        created.branch = Some("feat/x".to_owned());
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::CreateWorkspace,
                result: Ok(serde_json::to_value(&created).expect("workspace json")),
            },
        );
        assert!(f.state.dialog.is_none());
        assert_eq!(
            f.state.tree.cursor(),
            Some(&NodeId::Workspace(created.id.clone()))
        );
        assert!(
            f.state.rows().iter().any(|r| r.label == "ws feat/x (wt)"),
            "{:?}",
            f.state.rows()
        );
    }

    #[test]
    fn new_local_workspace_needs_no_branch() {
        let mut f = Fixture::new();
        press(&mut f.state, 'w');
        // Typing does nothing for a local workspace.
        type_text(&mut f.state, "junk");
        let fx = press_key(&mut f.state, Key::Enter);
        assert_eq!(
            fx,
            vec![Effect::Call {
                request: RequestTag::CreateWorkspace,
                method: "workspace.create",
                params: json!({
                    "project_id": f.project.id,
                    "isolation": "local",
                    "branch": null,
                    "base_ref": null,
                    "slug": null,
                }),
            }]
        );
    }

    #[test]
    fn archive_requires_confirmation() {
        let mut f = Fixture::new();
        f.select_a1();
        assert_eq!(press(&mut f.state, 'A'), vec![], "only opens a dialog");
        assert!(matches!(
            f.state.dialog,
            Some(Dialog::ConfirmArchive { .. })
        ));
        assert_eq!(f.state.focus, Focus::Dialog);
        // Anything but confirming cancels or is ignored.
        assert_eq!(press(&mut f.state, 'x'), vec![]);
        assert!(f.state.dialog.is_some());
        assert_eq!(press(&mut f.state, 'n'), vec![]);
        assert!(f.state.dialog.is_none());

        press(&mut f.state, 'A');
        let fx = press(&mut f.state, 'y');
        assert_eq!(
            fx,
            vec![Effect::Call {
                request: RequestTag::Archive {
                    agent_id: f.a1.id.clone()
                },
                method: "agent.archive",
                params: json!({"id": f.a1.id}),
            }]
        );
        assert!(f.state.dialog.is_none());
        assert_eq!(f.state.focus, Focus::Tree);
    }

    #[test]
    fn archive_dialog_needs_an_agent() {
        let mut f = Fixture::new();
        assert_eq!(press(&mut f.state, 'A'), vec![]);
        assert!(f.state.dialog.is_none());
        assert!(f.state.status.error.is_some());
    }

    #[test]
    fn q_quits() {
        let mut state = AppState::default();
        assert_eq!(press(&mut state, 'q'), vec![Effect::Quit]);
        // Ctrl-C quits from anywhere, including the input.
        let mut f = Fixture::new();
        f.select_a1();
        press(&mut f.state, 'i');
        assert_eq!(key(&mut f.state, KeyInput::ctrl('c')), vec![Effect::Quit]);
        // Other control chords are not commands.
        let mut state = AppState::default();
        assert_eq!(key(&mut state, KeyInput::ctrl('q')), vec![]);
    }

    #[test]
    fn resize_updates_the_size() {
        let mut state = AppState::default();
        update(
            &mut state,
            AppEvent::Resize {
                width: 60,
                height: 15,
            },
        );
        assert_eq!(state.size, (60, 15));
    }

    #[test]
    fn snapshot_keeps_a_permission_requested_after_it_was_asked_for() {
        let mut f = Fixture::new();
        let stranger = AgentId::generate();
        let fx = update(
            &mut f.state,
            AppEvent::AgentEvent(text(&stranger, 1, "hello")),
        );
        assert_eq!(fx, vec![Effect::LoadSnapshot]);
        // The permission arrives live; the snapshot (taken earlier) lacks it.
        let perm = permission(&stranger);
        update(&mut f.state, requested(&stranger, 2, &perm));
        assert_eq!(f.state.permission_count(), 1);
        f.snapshot_with(vec![]);
        assert_eq!(f.state.permission_count(), 1);
        // The next snapshot, requested after it, is authoritative again.
        f.snapshot_with(vec![]);
        assert_eq!(f.state.permission_count(), 0);
    }

    #[test]
    fn snapshot_does_not_resurrect_a_permission_resolved_after_it_was_asked_for() {
        let mut f = Fixture::new();
        let perm = permission(&f.a1.id);
        f.snapshot_with(vec![perm.clone()]);
        let stranger = AgentId::generate();
        update(
            &mut f.state,
            AppEvent::AgentEvent(text(&stranger, 1, "hello")),
        );
        update(&mut f.state, resolved(&f.a1.id, 1, &perm));
        assert_eq!(f.state.permission_count(), 0);
        // The stale snapshot still lists it.
        f.snapshot_with(vec![perm]);
        assert_eq!(f.state.permission_count(), 0);
        assert!(!f.state.rows().iter().any(|r| r.waiting_permission));
    }

    #[test]
    fn our_own_answer_during_a_snapshot_request_is_not_resurrected() {
        let mut f = Fixture::new();
        let perm = permission(&f.a1.id);
        f.snapshot_with(vec![perm.clone()]);
        f.select_a1();
        arm(&mut f.state);
        press(&mut f.state, 'a');
        let fx = update(
            &mut f.state,
            AppEvent::Connection(ConnectionState::Connected),
        );
        assert!(fx.contains(&Effect::LoadSnapshot));
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::Respond {
                    permission_id: perm.id.clone(),
                },
                result: Ok(json!({})),
            },
        );
        f.snapshot_with(vec![perm]);
        assert_eq!(f.state.permission_count(), 0);
    }

    #[test]
    fn snapshot_without_a_request_in_flight_is_authoritative() {
        let mut f = Fixture::new();
        let perm = permission(&f.a1.id);
        update(&mut f.state, requested(&f.a1.id, 1, &perm));
        assert_eq!(f.state.permission_count(), 1);
        f.snapshot_with(vec![]);
        assert_eq!(f.state.permission_count(), 0);
    }

    #[test]
    fn auto_approve_modes_warn_per_provider() {
        let warns = |provider, mode: &str| {
            let mut form = NewAgentForm::new(WorkspaceId::generate());
            form.provider = provider;
            form.mode.set_text(mode);
            form.auto_approve_mode()
        };
        for mode in ["bypassPermissions", "auto", "acceptEdits", " auto "] {
            assert!(warns(ProviderKind::Claude, mode), "claude {mode}");
        }
        for mode in ["", "default", "plan", "full-access", "Auto"] {
            assert!(!warns(ProviderKind::Claude, mode), "claude {mode:?}");
        }
        assert!(warns(ProviderKind::Codex, "full-access"));
        for mode in [
            "",
            "read-only",
            "workspace-write",
            "untrusted",
            "auto",
            "acceptEdits",
        ] {
            assert!(!warns(ProviderKind::Codex, mode), "codex {mode:?}");
        }
    }

    #[test]
    fn daemon_text_in_labels_and_titles_is_escaped() {
        let mut p = project("p\x1b[2J\u{202e}x");
        p.name = "p\x1b[2J\u{202e}x".to_owned();
        let mut ws = workspace(&p.id, "2026-01-01T00:00:01Z");
        ws.isolation = Isolation::Worktree;
        ws.branch = Some("b\nfake\r".to_owned());
        let mut a = agent(
            &ws.id,
            "t\x07\u{9b}\u{202e}\nrow",
            ProviderKind::Claude,
            "2026-01-01T00:00:02Z",
        );
        a.title = "t\x07\u{9b}\u{202e}\nrow".to_owned();
        let mut state = AppState::default();
        update(
            &mut state,
            AppEvent::Snapshot {
                projects: vec![p],
                workspaces: vec![ws],
                agents: vec![a.clone()],
                permissions: vec![],
            },
        );
        let labels: Vec<String> = state.rows().into_iter().map(|r| r.label).collect();
        assert_eq!(
            labels,
            vec![
                "p\\x1b[2J\\u{202e}x".to_owned(),
                "ws b\\nfake\\r (wt)".to_owned(),
                "t\\x07\\u{9b}\\u{202e}\\nrow".to_owned(),
            ]
        );
        for label in &labels {
            assert!(!label.chars().any(char::is_control), "{label:?}");
            assert!(!label.contains('\u{202e}'), "{label:?}");
        }
        press(&mut state, 'j');
        press(&mut state, 'j');
        press(&mut state, 'j');
        press(&mut state, 'A');
        match &state.dialog {
            Some(Dialog::ConfirmArchive { title, .. }) => {
                assert_eq!(title, "t\\x07\\u{9b}\\u{202e}\\nrow");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn each_new_history_load_has_its_own_generation() {
        let mut f = Fixture::new();
        press_key(&mut f.state, Key::Down);
        let g1 = generation_of(&press_key(&mut f.state, Key::Down));
        let g2 = generation_of(&press_key(&mut f.state, Key::Down));
        let g3 = generation_of(&press_key(&mut f.state, Key::Up));
        let g4 = generation_of(&update(&mut f.state, AppEvent::Lagged));
        let g5 = generation_of(&update(
            &mut f.state,
            AppEvent::Connection(ConnectionState::Connected),
        ));
        let all = [g1, g2, g3, g4, g5];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "{all:?}");
            }
        }
        assert_eq!(f.state.stream.generation(), g5);
    }

    #[test]
    fn late_pages_from_an_old_load_chain_cause_no_gap_and_no_duplicate() {
        let mut f = Fixture::new();
        press_key(&mut f.state, Key::Down);
        let a1 = f.a1.id.clone();
        let g1 = generation_of(&press_key(&mut f.state, Key::Down));
        // The first chain gets its first page and asks for the next.
        let fx = update(&mut f.state, page(&a1, 1..=3, false, g1));
        assert_eq!(generation_of(&fx), g1);
        // The user moves A -> B -> A quickly: a new chain starts from 0.
        press_key(&mut f.state, Key::Down);
        let g3 = generation_of(&press_key(&mut f.state, Key::Up));
        assert_ne!(g1, g3);
        assert_eq!(f.state.stream.last_seq(), 0);

        // The old chain's next page arrives late: it must not move last_seq.
        let fx = update(&mut f.state, page(&a1, 10..=12, false, g1));
        assert_eq!(fx, vec![]);
        assert_eq!(f.state.stream.last_seq(), 0);
        // Nor may its end marker stop the new chain's loading.
        update(&mut f.state, last_page(&a1, g1));
        assert!(f.state.stream.is_loading());

        // The new chain delivers everything.
        let fx = update(&mut f.state, page(&a1, 1..=3, false, g3));
        assert_eq!(fx, vec![load_history(&f.state, &f.a1, 3)]);
        // Another stale page between the new pages is ignored too.
        update(&mut f.state, page(&a1, 10..=12, false, g1));
        let fx = update(&mut f.state, page(&a1, 4..=9, false, g3));
        assert_eq!(fx, vec![load_history(&f.state, &f.a1, 9)]);
        update(&mut f.state, last_page(&a1, g3));
        assert!(!f.state.stream.is_loading());
        // And once it is finished, so is any straggler.
        assert_eq!(update(&mut f.state, page(&a1, 1..=5, false, g1)), vec![]);
        assert_eq!(
            lines(&f.state)
                .iter()
                .map(|l| l.split(' ').next().unwrap().to_owned())
                .collect::<Vec<_>>(),
            (1..=9).map(|s| format!("[{s}]")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn lagged_during_a_load_starts_a_new_chain() {
        let mut f = Fixture::new();
        f.select_a1();
        let a1 = f.a1.id.clone();
        let g1 = f.state.stream.generation();
        update(&mut f.state, page(&a1, 1..=3, false, g1));
        let fx = update(&mut f.state, AppEvent::Lagged);
        let g2 = f.state.stream.generation();
        assert_ne!(g1, g2);
        assert_eq!(
            fx,
            vec![
                Effect::Resubscribe,
                Effect::LoadSnapshot,
                load_history(&f.state, &f.a1, 3)
            ]
        );
        // The old chain's continuation arrives late and is dropped.
        assert_eq!(update(&mut f.state, page(&a1, 4..=5, false, g1)), vec![]);
        assert_eq!(f.state.stream.last_seq(), 3);
        // A live event meanwhile is held until the new chain is complete.
        update(&mut f.state, AppEvent::AgentEvent(text(&a1, 8, "t8")));
        update(&mut f.state, page(&a1, 4..=7, false, g2));
        update(&mut f.state, last_page(&a1, g2));
        assert_eq!(
            lines(&f.state)
                .iter()
                .map(|l| l.split(' ').next().unwrap().to_owned())
                .collect::<Vec<_>>(),
            (1..=8).map(|s| format!("[{s}]")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn failed_history_load_ends_loading_and_shows_the_error() {
        let mut f = Fixture::new();
        f.select_a1();
        let a1 = f.a1.id.clone();
        let generation = f.state.stream.generation();
        // The backend reports a failed load as an empty final page plus the error.
        update(&mut f.state, last_page(&a1, generation));
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::LoadHistory {
                    agent_id: a1.clone(),
                },
                result: Err("agent.events: boom\x1b".to_owned()),
            },
        );
        assert!(!f.state.stream.is_loading());
        assert_eq!(
            f.state.status.error.as_deref(),
            Some("agent.events: boom\\x1b")
        );
        // Live events show again instead of being held forever.
        update(&mut f.state, AppEvent::AgentEvent(text(&a1, 1, "t1")));
        assert_eq!(f.state.stream.lines().len(), 1);
    }

    #[test]
    fn permission_hint_and_missing_target_are_exposed_to_the_renderer() {
        let mut f = Fixture::new();
        f.select_a1();
        assert_eq!(f.state.permission_hint(), None);
        press_key(&mut f.state, Key::Enter);
        assert!(!f.state.input_target_missing());
        assert_eq!(f.state.permission_hint(), None);
        let perm = permission(&f.a1.id);
        update(&mut f.state, requested(&f.a1.id, 1, &perm));
        let hint = f.state.permission_hint().expect("hint while typing");
        assert!(hint.contains("Esc") && hint.contains('p'), "{hint}");
        press_key(&mut f.state, Key::Esc);
        assert_eq!(f.state.permission_hint(), None, "the dialog shows it now");

        // Losing the target is visible too.
        let mut f = Fixture::new();
        f.select_a1();
        press_key(&mut f.state, Key::Enter);
        update(
            &mut f.state,
            AppEvent::AgentEvent(event(
                &f.a1.id,
                1,
                EventKind::StateChanged {
                    state: AgentState::Archived,
                },
            )),
        );
        assert!(f.state.input_target_missing());
    }

    #[test]
    fn permission_dialog_reports_when_its_keys_are_enabled() {
        let mut f = Fixture::new();
        f.snapshot_with(vec![permission(&f.a1.id)]);
        f.select_a1();
        let armed = |state: &AppState| state.dialog.as_ref().is_some_and(Dialog::permission_armed);
        assert!(!armed(&f.state));
        tick(&mut f.state, 1);
        assert!(!armed(&f.state));
        tick(&mut f.state, 1);
        assert!(!armed(&f.state), "two ticks can be as short as 250 ms");
        tick(&mut f.state, 1);
        assert!(armed(&f.state));
        tick(&mut f.state, 5);
        assert!(armed(&f.state));
    }
}
