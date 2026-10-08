use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::state::{DiffReviewFocus, InputMode, TuiApp};

impl TuiApp {
    pub(crate) fn input_key_hint(&self, width: u16) -> &'static str {
        if width < 55 {
            return "F1 keys";
        }
        match self.input_mode {
            InputMode::HistorySearch => "F1 keys · Enter use · Esc back",
            InputMode::FileSearch => "F1 keys · Enter open · Esc back",
            InputMode::Normal
                if self.diff_review_focus == DiffReviewFocus::Review
                    && self.diff_review.is_some() =>
            {
                "F1 keys"
            }
            InputMode::Normal
                if self.completion_active && !self.completion_candidates.is_empty() =>
            {
                "F1 keys · Tab complete · ↑/↓ select"
            }
            InputMode::Normal if self.foreground_busy() => {
                "F1 keys · Enter queue · Alt+Enter newline"
            }
            InputMode::Normal => "F1 keys · Enter send · Alt+Enter newline",
        }
    }

    fn key_help_context(&self) -> &'static str {
        match self.input_mode {
            InputMode::HistorySearch => "History search",
            InputMode::FileSearch => "File search",
            InputMode::Normal
                if self.diff_review.is_some()
                    && self.diff_review_focus == DiffReviewFocus::Review =>
            {
                "Review"
            }
            InputMode::Normal
                if self.completion_active && !self.completion_candidates.is_empty() =>
            {
                "Completion"
            }
            InputMode::Normal => "Input",
        }
    }

    fn key_help_entries(&self) -> Vec<&'static str> {
        let mut entries = vec![
            "F1 / Esc: close help and return to the same draft or search.",
            "↑/↓, PgUp/PgDn, mouse wheel: scroll help. Home/End: first/last page.",
            "",
        ];
        match self.input_mode {
            InputMode::HistorySearch => entries.extend([
                "Type: filter input history.",
                "↑/↓ or Ctrl+R: select a result.",
                "Enter: replace the draft with the selected history entry (does not send it).",
                "Esc / Ctrl+G: return to the unchanged draft.",
            ]),
            InputMode::FileSearch => entries.extend([
                "Type: filter project files. ↑/↓: select a result.",
                "Enter: open the selected file in the configured editor.",
                "Esc / Ctrl+G: return to the unchanged draft.",
            ]),
            InputMode::Normal
                if self.diff_review.is_some()
                    && self.diff_review_focus == DiffReviewFocus::Review =>
            {
                entries.push("F6: return focus to input; draft text is preserved.");
                entries.push("[ / ]: select hunk. c: add/edit hunk comment. l: select lines (Shift+arrows extends, Enter edits, Esc cancels). d: delete selected comment. s: confirm comments. n: archive active batch/new current-review batch. h: read-only history. v: source/latest review.");
                entries.push("Comment editor: Enter saves locally; Alt+Enter newline; Esc cancels. Confirmation: Enter starts one repair run; Esc keeps comments; d discards batch; v shows source/latest review.");
                if let Some(review) = &self.diff_review {
                    if review.rejecting {
                        entries.push("Rollback running: close help, then Esc to request cancellation. Wait for the result before accepting or dismissing.");
                    } else {
                        entries.extend(["←/→: select a file. ↑/↓: scroll diff. PgUp/PgDn: page. Home/End: first/last diff row.", "a: accept applied changes. q / Esc: dismiss the panel (changes stay applied).", "e: inspect captured session evidence."]);
                        if review.rejectable {
                            entries.push("r: restore this review's captured pre-edit contents.");
                        } else {
                            entries.push("View only: rollback is unavailable for this review.");
                        }
                    }
                }
            }
            InputMode::Normal => {
                if self.completion_active && !self.completion_candidates.is_empty() {
                    entries.extend([
                        "↑/↓: select a completion. Tab: complete the token at the cursor.",
                        "Enter: apply completion; an exact slash command submits the draft.",
                        "Esc: close completion without cancelling a job.",
                    ]);
                } else {
                    entries
                        .push("Enter: send the draft (queued while another foreground job runs).");
                }
                entries.extend([
                    "Alt+Enter: insert a newline. Left/Right: move the cursor.",
                    "↑/↓: history for a single-line draft; move between rows in a multiline draft.",
                    "Ctrl+R: search input history. Ctrl+P: search project files.",
                    "@path: file completion. /: command completion. /help: list commands.",
                    "Ctrl+V: paste from the clipboard.",
                    "PgUp/PgDn, mouse wheel: scroll logs. Ctrl+Up/Down: one log row.",
                    "Ctrl+Home: oldest logs. Ctrl+End / Ctrl+L: return to live logs.",
                    "Ctrl+D: switch log/dashboard view.",
                ]);
                if self.diff_review.is_some() {
                    entries.push(
                        "F6: focus review controls; ordinary typing remains in input until then.",
                    );
                }
                entries.push("Esc: request cancellation (unless a completion list is open).");
            }
        }
        entries.extend(["", "Ctrl+C: request cancellation; press again within 3 seconds to exit.", "/jobs: inspect running/recent jobs. /cancel [job-id]: request cancellation.", "Cancellation waits for cleanup and saves; queued prompts wait for foreground ownership to be released."]);
        entries
    }

    pub(crate) fn handle_key_help_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::F(1) && key.modifiers.is_empty() {
            if key.kind == KeyEventKind::Press {
                self.key_help_open = !self.key_help_open;
                self.key_help_scroll = 0;
                self.dirty = true;
            }
            return true;
        }
        if !self.key_help_open {
            return false;
        }
        if key.kind != KeyEventKind::Release {
            match key.code {
                KeyCode::Esc => {
                    self.key_help_open = false;
                    self.dirty = true;
                }
                KeyCode::Up => self.scroll_key_help(-1),
                KeyCode::Down => self.scroll_key_help(1),
                KeyCode::PageUp => self.scroll_key_help(-self.help_page_rows()),
                KeyCode::PageDown => self.scroll_key_help(self.help_page_rows()),
                KeyCode::Home => {
                    self.key_help_scroll = 0;
                    self.dirty = true;
                }
                KeyCode::End => {
                    self.key_help_scroll = usize::MAX;
                    self.dirty = true;
                }
                _ => {}
            }
        }
        true // Never type, submit, change focus, or trigger review actions through help.
    }

    fn help_page_rows(&self) -> isize {
        self.main_content_height.saturating_sub(2).max(1) as isize
    }

    pub(crate) fn scroll_key_help(&mut self, rows: isize) {
        self.key_help_scroll = self.key_help_scroll.saturating_add_signed(rows);
        self.dirty = true;
    }

    pub(crate) fn render_key_help(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border_style)
            .style(self.theme.background_style)
            .title(format!("Keyboard help: {}", self.key_help_context()));
        let inner = block.inner(area);
        let lines: Vec<Line> = self
            .key_help_entries()
            .into_iter()
            .flat_map(|entry| {
                super::style_utils::wrap_segments(
                    &[super::style_utils::StyledSpan {
                        content: entry.to_string(),
                        style: self.theme.log_style,
                    }],
                    inner.width as usize,
                )
                .into_iter()
                .map(|row| {
                    Line::from(
                        row.spans
                            .into_iter()
                            .map(|span| Span::styled(span.content, span.style))
                            .collect::<Vec<_>>(),
                    )
                })
            })
            .collect();
        let max_scroll = lines.len().saturating_sub(inner.height as usize);
        self.key_help_scroll = self.key_help_scroll.min(max_scroll);
        let footer = format!(
            "F1/Esc close | {}/{}",
            self.key_help_scroll + 1,
            max_scroll + 1
        );
        frame.render_widget(Clear, area);
        frame.render_widget(block.title_bottom(footer), area);
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(self.key_help_scroll)
                    .take(inner.height as usize)
                    .collect::<Vec<_>>(),
            )
            .style(self.theme.log_style),
            inner,
        );
    }
}
