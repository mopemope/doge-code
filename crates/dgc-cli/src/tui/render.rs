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
use super::state::{
    AgentField, AppState, Dialog, Focus, NewAgentForm, NewWorkspaceForm, Row, WorkspaceField,
};
use crate::output::{escape_line, escape_text, kind_name, provider_name, state_name};

const ACCENT: Color = Color::Cyan;
const WARN: Color = Color::Yellow;
const DANGER: Color = Color::Red;

/// A terminal narrower than this gets a 16-column tree.
const NARROW: u16 = 50;
const NARROW_TREE: u16 = 16;

/// Draw the whole screen.
pub fn render(state: &AppState, frame: &mut Frame) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }
    let [body, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [tree, right] = Layout::horizontal([
        Constraint::Length(tree_width(area.width)),
        Constraint::Min(0),
    ])
    .areas(body);
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

/// 30% of the terminal, 18 to 40 columns; 16 when the terminal is narrow.
fn tree_width(total: u16) -> u16 {
    if total < NARROW {
        NARROW_TREE.min(total)
    } else {
        u16::try_from(u32::from(total) * 30 / 100).map_or(40, |w| w.clamp(18, 40))
    }
}

// ---- text helpers ----

fn char_width(c: char) -> usize {
    let mut buf = [0u8; 4];
    Span::raw(&*c.encode_utf8(&mut buf)).width()
}

fn text_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// `text` cut to `max` columns, ending in `…` when something was dropped.
fn truncate_cols(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > max - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('\u{2026}');
    out
}

/// The last `max` columns of `text`.
fn tail_cols(text: &str, max: usize) -> String {
    let mut used = 0;
    let mut start = text.len();
    for (index, c) in text.char_indices().rev() {
        let w = char_width(c);
        if used + w > max {
            break;
        }
        used += w;
        start = index;
    }
    text[start..].to_owned()
}

/// Break `line` (no newlines) into rows of at most `width` columns, at a
/// space when the row has one. Nothing is dropped (a space stays at the end
/// of its row); an empty line gives one empty row.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    // Byte offset and column just after the last space of `current`.
    let mut space: Option<(usize, usize)> = None;
    for c in line.chars() {
        let w = char_width(c);
        if used + w > width && !current.is_empty() {
            match space.take() {
                Some((at, columns)) if !current[..at].trim().is_empty() => {
                    let rest = current.split_off(at);
                    rows.push(std::mem::replace(&mut current, rest));
                    used -= columns;
                }
                _ => {
                    rows.push(std::mem::take(&mut current));
                    used = 0;
                }
            }
        }
        current.push(c);
        used += w;
        if c == ' ' {
            space = Some((current.len(), used));
        }
    }
    rows.push(current);
    rows
}

/// `text` (newlines allowed) wrapped to `width` columns.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap_line(line, width))
        .collect()
}

/// `agt_…` and the last six characters of the id.
fn short_id(id: &AgentId) -> String {
    let s = id.as_str();
    let prefix = s.split_once('_').map_or("", |(p, _)| p);
    let tail: String = s
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}_\u{2026}{tail}")
}

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

fn tree_line(row: &Row, width: usize, narrow: bool, selected: bool) -> Line<'static> {
    let indent = " ".repeat(row.depth * if narrow { 1 } else { 2 });
    let mut spans = Vec::new();
    match row.agent_state {
        Some(_) => {
            let (symbol, color) = state_symbol(row);
            let suffix = if row.auto_approve { " [auto]" } else { "" };
            let fixed = text_width(&indent) + 2 + text_width(suffix);
            // The provider name goes first when at least a few columns of the
            // title still fit; in a very narrow tree only the title is kept.
            let provider = row
                .provider
                .map(|p| format!("{} ", provider_name(p)))
                .filter(|p| width.saturating_sub(fixed + text_width(p)) >= 4)
                .unwrap_or_default();
            let room = width.saturating_sub(fixed + text_width(&provider)).max(1);
            spans.push(Span::raw(indent));
            spans.push(Span::styled(symbol.to_string(), Style::default().fg(color)));
            spans.push(Span::raw(format!(" {provider}")));
            spans.push(Span::raw(truncate_cols(&row.label, room)));
            if row.auto_approve {
                spans.push(Span::styled(suffix, Style::default().fg(WARN)));
            }
        }
        None => {
            let marker = if row.expanded == Some(true) {
                '\u{25be}'
            } else {
                '\u{25b8}'
            };
            let prefix = format!("{indent}{marker} ");
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
    let cursor = state.cursor_index();
    let offset = cursor.map_or(0, |c| (c + 1).saturating_sub(height));
    let narrow = frame.area().width < NARROW;
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, row)| tree_line(row, usize::from(inner.width), narrow, Some(i) == cursor))
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
    let lines: Vec<Line> = match &agent_id {
        None => vec![Line::styled(
            "select an agent (\u{2191}\u{2193}, Enter) to see its stream",
            dim(),
        )],
        Some(id) if state.stream.agent_id() != Some(id) => {
            vec![Line::styled("loading\u{2026}", dim())]
        }
        Some(_) => stream_tail(state, width, height),
    };
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
    if state.input.is_empty() && !focused {
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
    let hints = match state.focus {
        Focus::Tree => {
            "\u{2191}\u{2193} move  Enter open  i type  n agent  w workspace  p permission  \
             x stop  r resume  A archive  Tab mode  q quit"
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
    let width = pane.width.min(72);
    let inner_width = usize::from(width - 2);
    let max_inner_height = usize::from(pane.height - 2);
    let body = match dialog {
        Dialog::Permission {
            permission,
            sending,
            ..
        } => permission_body(
            state,
            permission,
            *sending,
            dialog.permission_armed(),
            inner_width,
            max_inner_height,
        ),
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

fn permission_body(
    state: &AppState,
    permission: &PendingPermission,
    sending: bool,
    armed: bool,
    width: usize,
    max_height: usize,
) -> DialogBody {
    let mut header = Vec::new();
    let who = match state.tree.agent(&permission.agent_id) {
        Some(agent) => format!(
            "agent: {}  {}  {}",
            short_id(&agent.id),
            provider_name(agent.provider),
            escape_line(&agent.title)
        ),
        None => format!("agent: {}  (unknown)", short_id(&permission.agent_id)),
    };
    header.extend(wrapped(&who, width, bold()));
    if state.stream_agent_id().as_ref() != Some(&permission.agent_id) {
        header.extend(wrapped(
            "(not the selected agent)",
            width,
            Style::default().fg(WARN),
        ));
    }
    header.extend(wrapped(
        &format!("kind: {}", kind_name(permission.kind)),
        width,
        Style::default(),
    ));
    header.push(Line::raw(""));

    let mut footer = vec![Line::raw("")];
    if sending {
        footer.push(Line::styled(
            "sending answer\u{2026}",
            Style::default().fg(WARN),
        ));
    } else {
        if !armed {
            footer.push(Line::styled(
                "keys enabled shortly",
                Style::default().fg(WARN),
            ));
        }
        let legend =
            Style::default().add_modifier(if armed { Modifier::BOLD } else { Modifier::DIM });
        footer.extend(wrapped(
            "[a]llow  [s]ession  [d]eny  [Esc] close",
            width,
            legend,
        ));
    }

    let summary = if permission.summary.is_empty() {
        "(no summary)".to_owned()
    } else {
        escape_text(&permission.summary)
    };
    let rows = wrap_text(&summary, width);
    let room = max_height
        .saturating_sub(header.len() + footer.len())
        .max(1);
    let mut lines = header;
    if rows.len() <= room {
        lines.extend(rows.into_iter().map(Line::from));
    } else {
        // Keep the start and the end (where a truncation marker or the
        // dangerous part of a command tends to be) and say what is left out.
        let head = (room - 1) / 2 + (room - 1) % 2;
        let tail = (room - 1) - head;
        let hidden = rows.len() - head - tail;
        let marker = truncate_cols(
            &format!("\u{2026}({hidden} lines hidden; full text: dgc-next permit ls)"),
            width,
        );
        lines.extend(rows[..head].iter().cloned().map(Line::from));
        lines.push(Line::styled(marker, Style::default().fg(WARN)));
        lines.extend(rows[rows.len() - tail..].iter().cloned().map(Line::from));
    }
    lines.extend(footer);
    DialogBody {
        title: " Permission ",
        border: WARN,
        lines,
        cursor: None,
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
    let mut lines = vec![Line::raw("")];
    let mut cursor = None;
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
        ("prompt", &form.prompt, AgentField::Prompt, "(optional)"),
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
    if form.auto_approve_mode() {
        lines.extend(wrapped(
            "WARNING: this mode lets the agent run commands and edit files without asking you (auto-approve).",
            width,
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
    }
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
    use crate::tui::AppEvent;
    use crate::tui::input::KeyInput;
    use crate::tui::state::{
        Dialog, Focus, NewAgentForm, NewWorkspaceForm, NodeId, PERMISSION_ARM_TICKS,
        PERMISSION_HINT, STREAM_CAP,
    };
    use crate::tui::update::update;

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
            provider_session_id: None,
            state,
            parent_id: None,
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

    #[test]
    fn permission_dialog_shows_agent_and_full_summary_escaped() {
        let mut f = Fx::new();
        // The stream shows a1; the permission belongs to a2.
        let a1 = f.a1.clone();
        f.select(&a1, &["unrelated"]);
        let a2 = f.a2.clone();
        let summary = format!(
            "rm -rf /tmp/x\r\u{1b}[2J\u{202e}evil\nsecond line {}",
            "A".repeat(5000)
        );
        let permission = permission(&a2.id, &summary);
        assert!(
            permission.summary.contains("(truncated,"),
            "protocol cut it"
        );
        f.state.open_dialog(Dialog::permission(permission));
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
        assert!(
            right.contains("…(truncated,"),
            "protocol marker visible: {right}"
        );
        assert!(right.contains("bytestotal)"), "{right}");
        assert!(
            right.contains("hidden"),
            "the dialog says rows were left out: {right}"
        );
        // A summary that fits is shown entirely, without a hidden marker.
        let short_one = permission_with_lines(&a2.id, 12);
        f.state.open_dialog(Dialog::permission(short_one));
        let buffer = draw(&f.state, 120, 40);
        let right = squash_from(&buffer, 36);
        assert!(right.contains("row0"), "{right}");
        assert!(right.contains("row11"), "{right}");
        assert!(!right.contains("hidden"), "{right}");
    }

    fn permission_with_lines(agent_id: &AgentId, n: usize) -> PendingPermission {
        let summary: Vec<String> = (0..n).map(|i| format!("row{i}")).collect();
        permission(agent_id, &summary.join("\n"))
    }

    #[test]
    fn permission_dialog_hides_middle_rows_but_keeps_head_and_tail() {
        let mut f = Fx::new();
        let a2 = f.a2.clone();
        f.select(&a2, &["x"]);
        f.state
            .open_dialog(Dialog::permission(permission_with_lines(&a2.id, 100)));
        let buffer = draw(&f.state, 100, 24);
        let right = squash_from(&buffer, 30);
        assert!(right.contains("row0"), "{right}");
        assert!(right.contains("row99"), "{right}");
        assert!(right.contains("hidden"), "{right}");
        assert!(!right.contains("row50"), "{right}");
        // Even a tiny pane never cuts silently.
        let buffer = draw(&f.state, 40, 8);
        assert!(squash_from(&buffer, 0).contains("hidden"));
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
    fn tree_width_follows_the_layout_rules() {
        assert_eq!(tree_width(120), 36);
        assert_eq!(tree_width(50), 18);
        assert_eq!(tree_width(200), 40);
        assert_eq!(tree_width(49), 16);
        assert_eq!(tree_width(20), 16);
        assert_eq!(tree_width(10), 10);
        assert_eq!(tree_width(u16::MAX), 40);
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
}
