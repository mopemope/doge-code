use crate::tui::diff_review::DiffLineKind;
use crate::tui::state::{RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
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

        // Apply theme background to entire screen
        let background_block = Block::default().style(self.theme.background_style);
        f.render_widget(background_block, size);

        // Simple 3-panel Layout: Status Line, Main Content, Input Area
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Status Line
                Constraint::Min(1),    // Main content
                Constraint::Length(3), // Input area
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
            && !self.completion_candidates.is_empty()
            && self.diff_review_focus != crate::tui::state::DiffReviewFocus::Review
        {
            self.render_completion_popup(f, chunks[2]);
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

        let spans = vec![
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
        ];

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

        // file list
        let items: Vec<ListItem> = review
            .files
            .iter()
            .enumerate()
            .map(|(idx, file)| {
                let mut label = file.path.clone();
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
        f.render_widget(files_list, layout[0]);

        // diff content (with compact evidence summary when pane hidden)
        let diff_title = if show_evidence {
            "Diff Preview (↑/↓ scroll)".to_string()
        } else if let Some(summary) = evidence_summary {
            format!("Diff Preview | {summary}")
        } else {
            "Diff Preview (↑/↓ scroll)".to_string()
        };
        let diff_block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .title(diff_title);

        if let Some(file) = review.files.get(review.selected) {
            let diff_lines: Vec<Line> = file
                .lines
                .iter()
                .map(|diff_line| {
                    let style = match diff_line.kind {
                        DiffLineKind::Header => Style::default().fg(Color::Cyan),
                        DiffLineKind::FileMeta => Style::default().fg(Color::Magenta),
                        DiffLineKind::HunkHeader => Style::default().fg(Color::Yellow),
                        DiffLineKind::Addition => Style::default().fg(Color::Green),
                        DiffLineKind::Removal => Style::default().fg(Color::Red),
                        DiffLineKind::Context => Style::default().fg(Color::DarkGray),
                        DiffLineKind::Other => Style::default(),
                    };
                    Line::from(Span::styled(diff_line.content.clone(), style))
                })
                .collect();

            let scroll = file.scroll.min(u16::MAX as usize) as u16;
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
            "Review changes: ↑/↓ scroll, PgUp/PgDn fast, ←/→ file, e evidence, a accept, r reject, q dismiss"
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

        let block_title = match self.input_mode {
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
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .title("History Search (Ctrl+R)");

        let area = centered_rect(60, 40, area);
        f.render_widget(Clear, area);
        f.render_widget(block.clone(), area);

        let inner = block.inner(area);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(inner);

        let search_text = format!("Search: {}_", state.query);
        let search_para = Paragraph::new(search_text).style(theme.input_style);
        f.render_widget(search_para, chunks[0]);

        let items: Vec<ListItem> = state
            .results
            .iter()
            .enumerate()
            .map(|(i, res)| {
                let style = if i == state.selected_index {
                    theme.completion_selected_style
                } else {
                    theme.completion_style
                };
                ListItem::new(res.clone()).style(style)
            })
            .collect();

        let list = List::new(items).highlight_style(theme.completion_selected_style);
        f.render_widget(list, chunks[1]);
    }

    fn render_file_search(
        &self,
        f: &mut Frame,
        area: Rect,
        state: &crate::tui::state::FileSearchState,
        theme: &Theme,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .title("File Search (Ctrl+P)");

        let area = centered_rect(60, 40, area);
        f.render_widget(Clear, area);
        f.render_widget(block.clone(), area);

        let inner = block.inner(area);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(inner);

        let search_text = format!("Search: {}_", state.query);
        let search_para = Paragraph::new(search_text).style(theme.input_style);
        f.render_widget(search_para, chunks[0]);

        let items: Vec<ListItem> = state
            .results
            .iter()
            .enumerate()
            .map(|(i, res)| {
                let style = if i == state.selected_index {
                    theme.completion_selected_style
                } else {
                    theme.completion_style
                };
                ListItem::new(res.clone()).style(style)
            })
            .collect();

        let list = List::new(items).highlight_style(theme.completion_selected_style);
        f.render_widget(list, chunks[1]);
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

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
