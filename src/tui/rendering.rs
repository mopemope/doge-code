use crate::tui::diff_review::DiffLineKind;
use crate::tui::state::{RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

impl TuiApp {
    fn diff_review_columns(area: Rect) -> [Rect; 2] {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        [columns[0], columns[1]]
    }

    pub fn view(&mut self, f: &mut Frame, model: Option<&str>) {
        let size = f.area();
        // Search needs room for results even when the preserved draft is long.
        let draft_rows = if self.input_mode == crate::tui::state::InputMode::Normal {
            self.textarea.lines().len()
        } else {
            1
        };
        let input_height = draft_rows
            .saturating_add(1)
            .clamp(3, 8)
            .min(size.height.saturating_sub(2) as usize) as u16;

        // Apply theme background to entire screen
        let background_block = Block::default().style(self.theme.background_style);
        f.render_widget(background_block, size);

        // Simple 3-panel Layout: Status Line, Main Content, Input Area
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),            // Status Line
                Constraint::Min(1),               // Main content
                Constraint::Length(input_height), // Bounded multiline input area
            ])
            .split(size);

        let main_content_height = chunks[1].height;
        self.main_content_height = main_content_height as usize;

        let log_width = if self.diff_review.is_some()
            && self.input_mode == crate::tui::state::InputMode::Normal
        {
            Self::diff_review_columns(chunks[1])[0].width
        } else {
            chunks[1].width
        };
        if self.log_width != log_width as usize || self.log_heights.len() != self.log.len() {
            self.log_width = log_width as usize;
            self.recalculate_all_heights();
        }
        self.clamp_log_scroll();

        let params = crate::tui::state::BuildRenderPlanParams {
            title: &self.title,
            status: self.status,
            log: &self.log,
            width: log_width,
            main_content_height,
            model,
            spinner_state: self.spinner_state,
            scroll_state: &self.scroll_state,
            plan_list: &self.plan_list,
            theme: &self.theme,
            log_heights: &self.log_heights,
        };
        let plan = build_render_plan(params);

        self.render_status_line(f, chunks[0], model, &self.theme);
        self.render_main_content(f, chunks[1], &plan, &self.theme);
        self.render_input_area(f, chunks[2]);

        if self.completion_active
            && !self.key_help_open
            && !self.completion_candidates.is_empty()
            && self.diff_review_focus != crate::tui::state::DiffReviewFocus::Review
        {
            self.render_completion_popup(f, chunks[2]);
        }
        if self.key_help_open {
            self.render_key_help(f, chunks[1]);
        }
        if self.comment_editor.is_some() || self.feedback_confirmation {
            self.render_feedback_modal(f, size);
        }
    }

    fn render_status_line(&self, f: &mut Frame, area: Rect, model: Option<&str>, theme: &Theme) {
        let model_name = self
            .inference_label
            .as_deref()
            .unwrap_or_else(|| model.unwrap_or("unknown"));

        let status_str = match self.status {
            crate::tui::state::Status::Ready => "READY",
            crate::tui::state::Status::Thinking => "THINKING",
            crate::tui::state::Status::Running => "RUNNING",
            crate::tui::state::Status::Error => "ERROR",
        };

        let status_color = match self.status {
            crate::tui::state::Status::Ready => Color::Green,
            crate::tui::state::Status::Thinking => Color::Yellow,
            crate::tui::state::Status::Running => Color::Blue,
            crate::tui::state::Status::Error => Color::Red,
        };

        // DOGE-CODE | model: [model] | [status] | tokens: [used] | [spinner]
        let spinner = if self.status == crate::tui::state::Status::Thinking
            || self.status == crate::tui::state::Status::Running
        {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            frames[self.spinner_state % frames.len()]
        } else {
            " "
        };

        let content = format!(
            " DOGE-CODE | model: {} | {} | tokens: {} | {}",
            model_name, status_str, self.tokens_prompt_used, spinner
        );

        let _para = Paragraph::new(content)
            .style(theme.footer_style)
            .bg(theme.background_style.bg.unwrap_or(Color::Reset)); // simple background

        // Overlay status color on the status text part?
        // For simplicity, just color the whole line or parts of it.
        // Let's make the status word colored.

        let display_status_str = if let Some(detailed) = &self.detailed_status {
            detailed.as_str()
        } else {
            status_str
        };

        let mut spans = Vec::new();
        if !self.pending_instructions.is_empty() {
            spans.push(Span::styled(
                format!("[{} queued] ", self.pending_instructions.len()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend([
            Span::styled(
                format!(" [{}] ", display_status_str),
                Style::default()
                    .bg(status_color)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(model_name, Style::default().fg(Color::Cyan)),
            Span::raw(" | "),
            Span::raw(format!("{} tokens", self.tokens_prompt_used)),
            Span::raw(" "),
            Span::styled(spinner, Style::default().fg(status_color)),
        ]);

        let line = Line::from(spans);
        let para = Paragraph::new(line).style(theme.footer_style);

        f.render_widget(para, area);
    }

    fn render_main_content(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        f.render_widget(Clear, area);

        if self.diff_review.is_some() && self.input_mode == crate::tui::state::InputMode::Normal {
            let columns = Self::diff_review_columns(area);

            self.render_log_panel(f, columns[0], plan, theme);
            self.render_diff_review(f, columns[1], theme);
            return;
        }

        match self.input_mode {
            crate::tui::state::InputMode::HistorySearch => {
                if let Some(history_search_state) = &self.history_search_state {
                    self.render_log_panel(f, area, plan, theme);
                    self.render_history_search(f, area, history_search_state, theme);
                }
            }
            crate::tui::state::InputMode::FileSearch => {
                if let Some(file_search_state) = &self.file_search_state {
                    self.render_log_panel(f, area, plan, theme);
                    self.render_file_search(f, area, file_search_state, theme);
                }
            }
            _ => match self.view_mode {
                crate::tui::state::ViewMode::Dashboard => {
                    self.render_dashboard(f, area, theme);
                }
                crate::tui::state::ViewMode::Log => {
                    self.render_log_panel(f, area, plan, theme);
                }
            },
        }
    }

    fn render_dashboard(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        // Dashboard Layout
        // Split into Left (Tasks) and Right (Stats/Context)
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(60), // Tasks
                Constraint::Percentage(40), // Stats
            ])
            .split(area);

        self.render_tasks_panel(f, chunks[0], theme);
        self.render_stats_panel(f, chunks[1], theme);
    }

    fn render_tasks_panel(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .title("Tasks");

        let items: Vec<ListItem> = self
            .plan_list
            .iter()
            .map(|item| {
                let (symbol, style) = match item.status.as_str() {
                    "completed" => ("✓", Style::default().fg(Color::Green)),
                    "in_progress" => ("➜", Style::default().fg(Color::Yellow)),
                    "failed" => ("✗", Style::default().fg(Color::Red)),
                    _ => ("•", Style::default().fg(Color::Gray)),
                };

                let content = format!("{} {}", symbol, item.content);
                ListItem::new(content).style(style)
            })
            .collect();

        let list = List::new(items).block(block);
        f.render_widget(list, area);
    }

    fn render_stats_panel(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .title("Context & Stats");

        let inner_area = block.inner(area);
        f.render_widget(block, area);

        let stats_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(inner_area);

        // tokens
        let token_text = format!("Tokens Used: {}", self.tokens_prompt_used);
        f.render_widget(
            Paragraph::new(token_text).style(theme.log_style),
            stats_chunks[0],
        );

        // Model
        let model_text = format!("Model: {}", self.model.as_deref().unwrap_or("Unknown"));
        f.render_widget(
            Paragraph::new(model_text).style(theme.log_style),
            stats_chunks[1],
        );

        // Repo Status
        let status_text = format!("Repo Status: {:?}", self.repomap_status);
        f.render_widget(
            Paragraph::new(status_text).style(theme.log_style),
            stats_chunks[2],
        );
    }

    fn render_log_panel(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        // Clear log panel area to prevent artifacts
        f.render_widget(Clear, area);

        // Create paragraph with the content lines
        let lines: Vec<Line> = plan
            .log_lines
            .iter()
            .map(|styled_line| {
                let mut spans: Vec<Span> = Vec::new();
                for segment in &styled_line.spans {
                    spans.push(Span::styled(segment.content.clone(), segment.style));
                }
                if spans.is_empty() {
                    Line::raw("")
                } else {
                    Line::from(spans)
                }
            })
            .collect();

        // Adjust scroll if needed? The plan struct usually handles visible lines,
        // but `Paragraph` also takes scroll. `plan.log_lines` are likely already the visible ones
        // or the whole buffer depending on `build_render_plan`.
        // Looking at original code: `build_render_plan` seems to calculate viewport.
        // But original code didn't use `.scroll()` on paragraph for `render_log_panel`?
        // Wait, original `render_log_panel` created a `Paragraph` with `lines`.
        // If `lines` are already sliced, we don't need scroll.
        // Assuming `build_render_plan` does the slicing.

        let paragraph = Paragraph::new(lines)
            .style(theme.log_style)
            .block(Block::default()); // No border
        f.render_widget(paragraph, area);
    }

    fn render_diff_review(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let Some(review) = &self.diff_review else {
            return;
        };

        // Small-terminal fallback: omit evidence pane when height is tight so
        // the diff stays visible. Compact summary goes into the diff title.
        // Never panic on tiny areas.
        let show_evidence = area.height >= 22 && area.width >= 40;
        let evidence_summary = review.evidence_summary();

        let layout = if show_evidence {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    // Show as many changed files as fit (capped), not just one
                    Constraint::Length((review.files.len() as u16 + 2).min(8)),
                    Constraint::Min(4),
                    Constraint::Length(8),
                    Constraint::Length(3),
                ])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length((review.files.len() as u16 + 2).min(10)),
                    Constraint::Min(5),
                    Constraint::Length(3),
                ])
                .split(area)
        };

        let diff_idx = 1usize;
        let footer_idx = if show_evidence { 3 } else { 2 };

        // Record the inner height of the diff viewport so scroll clamping in
        // the event loop can account for the visible window.
        self.diff_viewport_height
            .set(layout[diff_idx].height.saturating_sub(2) as usize);
        self.diff_viewport_width
            .set(layout[diff_idx].width.saturating_sub(2) as usize);

        // file list
        let items: Vec<ListItem> = review
            .files
            .iter()
            .enumerate()
            .map(|(idx, file)| {
                let mut label = file.path.clone();
                if self.review_feedback.as_ref().is_some_and(|d| {
                    d.repair_review_id == review.review_id
                        && d.repair_review_id.is_some()
                        && !d.batch.comments.iter().any(|c| c.anchor.path == file.path)
                }) {
                    label.push_str(" [outside comments]");
                }
                let additions = file.additions();
                let removals = file.removals();
                if additions > 0 || removals > 0 {
                    label.push_str(&format!(" (+{}/-{})", additions, removals));
                }

                let style = if idx == review.selected {
                    theme.completion_selected_style
                } else {
                    theme.completion_style
                };

                ListItem::new(label).style(style)
            })
            .collect();

        let files_block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .title("Changed Files (←/→ to focus)");
        let files_list = List::new(items).block(files_block);
        let mut file_state = ListState::default().with_selected(Some(review.selected));
        f.render_stateful_widget(files_list, layout[0], &mut file_state);

        // diff content (with compact evidence summary when pane hidden)
        let diff_title = if show_evidence {
            "Diff Preview (↑/↓ scroll)".to_string()
        } else if let Some(summary) = evidence_summary {
            format!("Diff Preview | {summary}")
        } else {
            "Diff Preview (↑/↓ scroll)".to_string()
        };
        let diff_title = if let Some(draft) = self
            .review_feedback
            .as_ref()
            .filter(|d| d.repair_review_id == review.review_id && d.repair_review_id.is_some())
        {
            let outside = review
                .files
                .iter()
                .filter(|f| !draft.batch.comments.iter().any(|c| c.anchor.path == f.path))
                .count();
            format!(
                "Repair | outside commented paths: {outside} | batch {}",
                draft.batch.id
            )
        } else {
            diff_title
        };
        let diff_block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .title(diff_title);

        if let Some(file) = review.files.get(review.selected) {
            let projection = self.diff_projection();
            let diff_lines: Vec<Line> = projection
                .iter()
                .map(|row| {
                    let mut style = match row.kind {
                        DiffLineKind::Header => Style::default().fg(Color::Cyan),
                        DiffLineKind::FileMeta => Style::default().fg(Color::Magenta),
                        DiffLineKind::HunkHeader => Style::default().fg(Color::Yellow),
                        DiffLineKind::Addition => Style::default().fg(Color::Green),
                        DiffLineKind::Removal => Style::default().fg(Color::Red),
                        DiffLineKind::Context => Style::default().fg(Color::DarkGray),
                        DiffLineKind::Other => Style::default().fg(Color::Cyan),
                    };
                    if row.selected {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    Line::from(Span::styled(row.content.clone(), style))
                })
                .collect();

            let scroll = file
                .scroll
                .min(
                    projection
                        .len()
                        .saturating_sub(layout[diff_idx].height.saturating_sub(2) as usize),
                )
                .min(u16::MAX as usize) as u16;
            let paragraph = Paragraph::new(diff_lines)
                .block(diff_block)
                .scroll((scroll, 0));
            f.render_widget(paragraph, layout[diff_idx]);
        } else {
            let paragraph = Paragraph::new("No diff available")
                .block(diff_block)
                .style(theme.log_style);
            f.render_widget(paragraph, layout[diff_idx]);
        }

        // evidence pane
        if show_evidence {
            let evidence_block = Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border_style)
                .title("Evidence");
            let inner = evidence_block.inner(layout[2]);
            f.render_widget(evidence_block, layout[2]);
            let lines = Self::evidence_lines(review);
            // Clamp to visible height; never panic.
            let max_lines = inner.height as usize;
            let truncated = lines.len() > max_lines;
            let visible: Vec<Line> = lines.into_iter().take(max_lines.max(1)).collect();
            let mut with_warning = visible;
            if truncated {
                with_warning.push(Line::from(Span::styled(
                    "… evidence truncated",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            let para = Paragraph::new(with_warning).style(theme.log_style);
            f.render_widget(para, inner);
        }

        // instructions footer for diff
        let help = if self.diff_review_focus == crate::tui::state::DiffReviewFocus::Input {
            "Input focused: type normally. F6 focuses review controls.".to_string()
        } else if review.rejecting {
            "Rollback running: Esc cancels; wait for the result before accepting or dismissing."
                .to_string()
        } else if review.rejectable {
            "←/→ file · ↑/↓ scroll · [/] hunk · c comment · d delete · s confirm · v source · a accept · r rollback · e evidence · q dismiss"
                .to_string()
        } else {
            format!(
                "View only: {} | e evidence, a accept, q dismiss",
                review
                    .reject_reason
                    .as_deref()
                    .unwrap_or("No turn-owned rollback capture.")
            )
        };
        let instructions = Paragraph::new(help).style(theme.footer_style).block(
            Block::default().borders(Borders::ALL).title(
                if self.diff_review_focus == crate::tui::state::DiffReviewFocus::Review {
                    "Review focused (F6: input)"
                } else {
                    "Review (F6: focus)"
                },
            ),
        );
        f.render_widget(instructions, layout[footer_idx]);
    }

    fn render_feedback_modal(&self, f: &mut Frame, area: Rect) {
        f.render_widget(Clear, area);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(1),
                Constraint::Length(4),
            ])
            .split(area);
        if let Some(editor) = &self.comment_editor {
            let a = &editor.anchor;
            let heading = format!(
                "Comment: {} | old {},{} → new {},{}",
                a.path, a.hunk.old.start, a.hunk.old.count, a.hunk.new.start, a.hunk.new.count
            );
            f.render_widget(
                Paragraph::new(heading)
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Hunk comment (saved locally)"),
                    ),
                parts[0],
            );
            let body = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(parts[1]);
            f.render_widget(
                Paragraph::new(editor.anchor.hunk.excerpt.clone())
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title("Source hunk")),
                body[0],
            );
            f.render_widget(&editor.textarea, body[1]);
            f.render_widget(Paragraph::new("Enter saves (empty deletes) · Alt+Enter newline · Esc cancels editing. Saving never invokes AI; ordinary input is preserved.").wrap(ratatui::widgets::Wrap{trim:false}).block(Block::default().borders(Borders::ALL)),parts[2]);
        } else if let Some(draft) = &self.review_feedback {
            let title = format!(
                "Confirm {} comments | batch {} revision {} | source {}",
                draft.batch.comments.len(),
                draft.batch.id,
                draft.batch.revision,
                draft.batch.source.review_id.as_deref().unwrap_or("unknown")
            );
            f.render_widget(
                Paragraph::new(title)
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("One batch repair"),
                    ),
                parts[0],
            );
            let mut text = String::new();
            for c in &draft.batch.comments {
                text.push_str(&format!(
                    "{} | old {},{} → new {},{}\n{}\n\n",
                    c.anchor.path,
                    c.anchor.hunk.old.start,
                    c.anchor.hunk.old.count,
                    c.anchor.hunk.new.start,
                    c.anchor.hunk.new.count,
                    c.text
                ));
            }
            if let Some(outcome) = &draft.outcome {
                text.push_str(outcome);
                text.push('\n');
            }
            let rows = crate::tui::review_feedback::wrap_cells(
                &text,
                parts[1].width.saturating_sub(2) as usize,
            );
            let scroll = self.feedback_confirmation_scroll.min(
                rows.len()
                    .saturating_sub(parts[1].height.saturating_sub(2) as usize),
            );
            f.render_widget(
                Paragraph::new(
                    rows.into_iter()
                        .skip(scroll)
                        .map(Line::from)
                        .collect::<Vec<_>>(),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("All saved comments · ↑/↓ or PgUp/PgDn"),
                ),
                parts[1],
            );
            let help = if let Some(error) = &self.feedback_error {
                format!(
                    "Blocked: {error} | Esc keeps comments · d discards batch · v source/latest"
                )
            } else {
                "Enter: submit one repair run (may use several model/tool iterations). Esc: keep editing. d: discard batch. v: source/latest. All changes stay applied; repair may change other paths.".into()
            };
            f.render_widget(
                Paragraph::new(help)
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL)),
                parts[2],
            );
        }
    }

    fn evidence_lines(review: &crate::tui::diff_review::DiffReviewState) -> Vec<Line<'static>> {
        let Some(file) = review.files.get(review.selected) else {
            return vec![Line::from("No file selected")];
        };
        let Some(ev) = &file.evidence else {
            if review.evidence_warnings.is_empty() {
                return vec![Line::from("No linked provenance evidence.")];
            }
            return review
                .evidence_warnings
                .iter()
                .take(4)
                .map(|w| Line::from(Span::styled(w.clone(), Style::default().fg(Color::Yellow))))
                .collect();
        };
        let mut lines = Vec::new();
        if ev.requirement_ids.is_empty() {
            lines.push(Line::from("Requirements: -"));
        } else {
            lines.push(Line::from(format!(
                "Requirements: {}",
                ev.requirement_ids.join(", ")
            )));
        }
        if ev.plan_item_ids.is_empty() {
            lines.push(Line::from("Plan: -"));
        } else {
            lines.push(Line::from(format!("Plan: {}", ev.plan_item_ids.join(", "))));
        }
        if ev.obligations.is_empty() {
            lines.push(Line::from("Obligations: none"));
        } else {
            for ob in ev.obligations.iter().take(8) {
                let (icon, label) = match ob.state.as_str() {
                    "observed_passing" => ("✓", "observed passing"),
                    "observed_failing" => ("✗", "observed failing"),
                    "pending" => ("?", "pending"),
                    "stale" => ("!", "stale"),
                    "diverged" => ("!", "diverged"),
                    "reverted" => ("↩", "reverted"),
                    "no_linked_change" => ("-", "no linked change"),
                    "mixed" => ("~", "mixed"),
                    other => ("?", other),
                };
                let cmd = ob
                    .command_summary
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect::<String>();
                let text = if cmd.is_empty() {
                    format!("{icon} {}  {label}", ob.id)
                } else {
                    format!("{icon} {}  {label}  ({cmd})", ob.id)
                };
                let style = match ob.state.as_str() {
                    "observed_passing" => Style::default().fg(Color::Green),
                    "observed_failing" => Style::default().fg(Color::Red),
                    "pending" => Style::default().fg(Color::Yellow),
                    "stale" | "diverged" => Style::default().fg(Color::Yellow),
                    "reverted" => Style::default().fg(Color::Magenta),
                    _ => Style::default(),
                };
                lines.push(Line::from(Span::styled(text, style)));
            }
            if ev.obligations.len() > 8 {
                lines.push(Line::from(format!(
                    "… {} more obligations",
                    ev.obligations.len() - 8
                )));
            }
        }
        for w in review.evidence_warnings.iter().take(2) {
            lines.push(Line::from(Span::styled(
                format!("! {w}"),
                Style::default().fg(Color::Yellow),
            )));
        }
        lines
    }

    fn render_input_area(&mut self, f: &mut Frame, area: Rect) {
        f.render_widget(Clear, area);

        let input_style = self.theme.input_style;
        self.textarea.set_style(input_style);

        // Simple border or just prompt?
        // Codex style: simple prompt >
        // We will stick to a minimal Block for bounds, maybe just top border or no border.
        // User asked for "OpenAI Codex CLI" like. minimalistic.

        let block_title = if self.key_help_open {
            "Help focused (F1/Esc: close)".to_string()
        } else {
            format!(
                "{} · {}",
                match self.input_mode {
                    crate::tui::state::InputMode::HistorySearch => "History Search",
                    crate::tui::state::InputMode::FileSearch => "File Search",
                    _ if self.diff_review.is_some() => {
                        if self.diff_review_focus == crate::tui::state::DiffReviewFocus::Input {
                            "Input focused (F6: review)"
                        } else {
                            "Input (F6: focus)"
                        }
                    }
                    _ => "Input",
                },
                self.input_key_hint(area.width)
            )
        };

        // Use standard border type if we want a visible separator
        self.textarea.set_block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(self.theme.border_style)
                .border_type(ratatui::widgets::BorderType::Plain)
                .title(block_title),
        );

        if self.status == crate::tui::state::Status::Running {
            self.textarea.set_style(input_style.fg(Color::DarkGray));
        }

        f.render_widget(&self.textarea, area);
    }

    fn render_history_search(
        &self,
        f: &mut Frame,
        area: Rect,
        state: &crate::tui::state::HistorySearchState,
        theme: &Theme,
    ) {
        render_search(
            f,
            area,
            "History Search (Ctrl+R)",
            &state.query,
            state.results.iter().map(String::as_str),
            state.selected_index,
            false,
            theme,
        );
    }

    fn render_file_search(
        &self,
        f: &mut Frame,
        area: Rect,
        state: &crate::tui::state::FileSearchState,
        theme: &Theme,
    ) {
        let root = self
            .cfg
            .as_ref()
            .map(|cfg| cfg.project_root.clone())
            .or_else(|| std::env::current_dir().ok());
        render_search(
            f,
            area,
            "File Search (Ctrl+P)",
            &state.query,
            state.results.iter().map(|path| {
                root.as_ref()
                    .and_then(|root| std::path::Path::new(path).strip_prefix(root).ok())
                    .and_then(|relative| relative.to_str())
                    .unwrap_or(path)
            }),
            state.selected_index,
            state.loading,
            theme,
        );
    }

    fn render_completion_popup(&self, f: &mut Frame, input_area: Rect) {
        let max_display_items = crate::tui::state::MAX_COMPLETION_DISPLAY_ITEMS;
        let candidates_len = self.completion_candidates.len();
        let display_count = candidates_len.min(max_display_items);

        let width = self
            .completion_candidates
            .iter()
            .map(|s| s.len())
            .max()
            .unwrap_or(20)
            .clamp(20, 60) as u16
            + 4;

        let height = display_count as u16 + 2;
        let y = input_area.y.saturating_sub(height);
        let x = input_area.x;
        // Ensure the popup doesn't go off-screen vertically if the terminal is too small
        let actual_y = if y == 0 { 0 } else { y };
        let area = Rect::new(x, actual_y, width, height);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border_style)
            .title("Suggestions");

        let start_idx = self.completion_scroll;
        let end_idx = (start_idx + display_count).min(candidates_len);

        let items: Vec<ListItem> = self.completion_candidates[start_idx..end_idx]
            .iter()
            .enumerate()
            .map(|(i, candidate)| {
                let actual_idx = start_idx + i;
                let style = if actual_idx == self.completion_index {
                    self.theme.completion_selected_style
                } else {
                    self.theme.completion_style
                };
                ListItem::new(candidate.clone()).style(style)
            })
            .collect();

        let list = List::new(items).block(block);

        f.render_widget(Clear, area);
        f.render_widget(list, area);
    }
}

/// A one-row preview; original result strings remain available to Enter.
fn search_preview(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("(blank input)");
    // Bound allocation even for very long rows and strip terminal control characters.
    let mut chars = first.trim().chars();
    let preview: String = chars
        .by_ref()
        .take(width.saturating_mul(4).saturating_add(1))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let suffix = if text.contains('\n') { " ↵" } else { "" };
    let available = width.saturating_sub(UnicodeWidthStr::width(suffix));
    let clipped = chars.next().is_some() || UnicodeWidthStr::width(preview.as_str()) > available;
    let limit = available.saturating_sub(usize::from(clipped));
    let mut row = crate::tui::state_render::truncate_display(&preview, limit);
    if clipped && available > 0 {
        row.push('…');
    }
    row.push_str(suffix);
    row
}

#[allow(clippy::too_many_arguments)]
fn render_search<'a>(
    f: &mut Frame,
    area: Rect,
    title: &str,
    query: &str,
    results: impl Iterator<Item = &'a str>,
    selected_index: usize,
    loading: bool,
    theme: &Theme,
) {
    let width = area.width.saturating_sub(2).min(100);
    let height = area.height.min(18);
    let area = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style)
        .title(title);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(u16::from(inner.height >= 4)),
    ])
    .split(inner);
    f.render_widget(
        Paragraph::new(search_query_line(query, chunks[0].width as usize)).style(theme.input_style),
        chunks[0],
    );
    let items: Vec<ListItem> = results
        .map(|res| {
            ListItem::new(search_preview(res, inner.width.saturating_sub(2) as usize))
                .style(theme.completion_style)
        })
        .collect();
    let count = items.len();
    if count == 0 {
        let message = if loading {
            "Scanning files…"
        } else {
            "No matches"
        };
        f.render_widget(
            Paragraph::new(message).style(theme.completion_style),
            chunks[1],
        );
    } else {
        // Stateful rendering scrolls to the selected row, including after a resize.
        let mut state = ListState::default().with_selected(Some(selected_index.min(count - 1)));
        let list = List::new(items)
            .highlight_style(theme.completion_selected_style)
            .highlight_symbol("> ");
        f.render_stateful_widget(list, chunks[1], &mut state);
    }
    let position = if count == 0 {
        0
    } else {
        selected_index.min(count - 1) + 1
    };
    f.render_widget(
        Paragraph::new(format!("{position}/{count} · ↑↓ · Enter · Esc"))
            .style(theme.completion_style),
        chunks[2],
    );
}

/// Search editing appends/removes at the end; keep that caret in view without
/// changing the full query used to filter results. Recompute on every resize.
fn search_query_line(query: &str, width: usize) -> Line<'_> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    if width == 0 {
        return Line::default();
    }
    let prefix = match width {
        32.. => "Search: ",
        24.. => "Find: ",
        _ => "",
    };
    let available = width - prefix.len() - 1; // Always reserve the end caret.
    let (ellipsis, visible) = if UnicodeWidthStr::width(query) <= available {
        ("", query)
    } else {
        let ellipsis = if available > 0 { "…" } else { "" };
        let budget = available.saturating_sub(1);
        let mut start = query.len();
        let mut used = 0;
        for (index, grapheme) in query.grapheme_indices(true).rev() {
            let cells = UnicodeWidthStr::width(grapheme);
            if used + cells > budget {
                break;
            }
            used += cells;
            start = index;
        }
        (ellipsis, &query[start..])
    };
    Line::from(vec![
        Span::raw(prefix),
        Span::raw(ellipsis),
        Span::raw(visible),
        Span::raw("_"),
    ])
}

#[cfg(test)]
mod search_preview_tests {
    use super::{search_preview, search_query_line};

    #[test]
    fn query_viewport_preserves_graphemes_and_caret_within_cell_budget() {
        use unicode_width::UnicodeWidthStr;
        let family = "👩‍👩‍👧‍👦";
        let query = format!("{}日本語e\u{301}{family}🇯🇵終端", "prefix-".repeat(40));
        for width in 0..=130 {
            let row = search_query_line(&query, width).to_string();
            assert!(
                UnicodeWidthStr::width(row.as_str()) <= width,
                "{width}: {row}"
            );
            if width > 0 {
                assert!(row.ends_with('_'));
            } else {
                assert!(row.is_empty());
            }
        }
        for tail in ["e\u{301}", family, "🇯🇵"] {
            let query = format!("{}{tail}", "prefix-".repeat(40));
            assert!(
                search_query_line(&query, 8)
                    .to_string()
                    .ends_with(&format!("{tail}_"))
            );
        }
        assert_eq!(search_query_line("界", 1).to_string(), "_");
        assert_eq!(search_query_line("界", 2).to_string(), "…_");
        assert_eq!(search_query_line("界", 3).to_string(), "界_");
        assert_eq!(search_query_line("", 40).to_string(), "Search: _");
        assert_eq!(search_query_line("short", 40).to_string(), "Search: short_");
    }

    #[test]
    fn previews_bound_large_rows_and_mark_clipped_combining_text() {
        let combined = format!("e{}tail", "\u{301}".repeat(1000));
        let preview = search_preview(&combined, 20);
        assert!(preview.ends_with('…'));
        assert!(preview.len() < 200);
        let preview = search_preview(&format!("\u{1b}\t{}\nsecond", "界".repeat(1000)), 20);
        assert!(!preview.chars().any(char::is_control));
        assert!(preview.ends_with("… ↵"));
        assert!(unicode_width::UnicodeWidthStr::width(preview.as_str()) <= 20);
    }
}
