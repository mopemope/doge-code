use crate::tui::state::{RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ansi_to_tui::IntoText;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation},
};
use std::fmt::Write;
// use tracing::debug;

impl TuiApp {
    pub fn view(&mut self, f: &mut Frame, model: Option<&str>) {
        let size = f.area();

        //debug!("Screen size: {}x{}", size.width, size.height);

        // Adjust layout for multi-line input area
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header
                Constraint::Min(1),    // Main content
                Constraint::Length(2), // Status Footer
                Constraint::Length(5), // Input area (increased to 5 for 3 visible lines)
            ])
            .split(size);

        let main_content_height = chunks[1].height;
        // debug!(
        //     "Layout chunks: header={}x{}, main={}x{}, footer={}x{}",
        //     chunks[0].width,
        //     chunks[0].height,
        //     chunks[1].width,
        //     chunks[1].height,
        //     chunks[2].width,
        //     chunks[2].height
        // );
        // debug!("Main content area height: {}", main_content_height);
        // debug!("Total log lines: {}", self.log.len());

        let plan = build_render_plan(
            &self.title,
            self.status,
            &self.log,
            &self.textarea, // Pass textarea
            self.input_mode,
            size.width,
            size.height,
            main_content_height,
            model,
            self.spinner_state,
            self.tokens_prompt_used,
            self.tokens_total_used,
            &self.scroll_state,
            &self.todo_list,     // Pass todo_list
            self.repomap_status, // Pass repomap_status
        );

        // debug!(
        //     "Render plan: log_lines={}, scroll_info={:?}",
        //     plan.log_lines.len(),
        //     plan.scroll_info
        // );

        self.render_header(f, chunks[0], &plan, &self.theme);
        self.render_main_content(f, chunks[1], &plan, &self.theme);

        self.render_status_footer(f, chunks[2], &self.theme);
        self.render_input_area(f, chunks[3]);

        // If there are todo items in the plan, add them to the log as regular messages
        if !plan.todo_list.is_empty() {
            // We'll add todo items to the log in the build_render_plan function
            // This section is intentionally left empty to remove the separate panel rendering
        }

        if let Some(diff_output) = &self.diff_output.clone() {
            self.render_diff_popup(f, size, diff_output);
        }

        // The cursor is now handled by the TextArea widget, so no need to set it manually.
    }

    fn render_header(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        let header_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let mut title_text = if !plan.footer_lines.is_empty() {
            plan.footer_lines[0].clone()
        } else {
            String::new()
        };

        // Add scroll indicator to title if scrolling
        if let Some(scroll_info) = &plan.scroll_info {
            if scroll_info.is_scrolling {
                let mut scroll_indicator = format!(
                    " [SCROLL: {}/{}]",
                    scroll_info.current_line, scroll_info.total_lines
                );
                if scroll_info.new_messages > 0 {
                    scroll_indicator.push_str(&format!(" (+{})", scroll_info.new_messages));
                }
                title_text.push_str(&scroll_indicator);
            } else if scroll_info.total_lines > 0 {
                // Show total lines even when not scrolling if there's content
                let lines_indicator = format!(" [{}L]", scroll_info.total_lines);
                title_text.push_str(&lines_indicator);
            }
        }

        let title = Paragraph::new(title_text)
            .style(theme.footer_style)
            .alignment(Alignment::Left);
        f.render_widget(title, header_chunks[0]);

        let separator_text = if plan.footer_lines.len() > 1 {
            plan.footer_lines[1].clone()
        } else {
            "-".repeat(area.width as usize)
        };
        let separator = Paragraph::new(separator_text).style(theme.footer_style);
        f.render_widget(separator, header_chunks[1]);
    }

    fn render_main_content(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        // debug!(
        //     "render_main_content: area={}x{} (x={}, y={}), plan.log_lines={}",
        //     area.width,
        //     area.height,
        //     area.x,
        //     area.y,
        //     plan.log_lines.len()
        // );

        // Check if we have more lines than the area can display
        if plan.log_lines.len() > area.height as usize {
            // debug!(
            //     "WARNING: More lines ({}) than area height ({})",
            //     plan.log_lines.len(), area.height
            // );
        }

        // If in session list mode, render the session list instead of log
        if self.input_mode == crate::tui::state::InputMode::SessionList
            && let Some(session_list_state) = &self.session_list_state
        {
            self.render_session_list(f, area, session_list_state, theme);
            return;
        }

        let mut lines: Vec<Line> = Vec::new();
        let mut is_in_code_block = false;

        // Ensure we don't try to render more lines than the area can display
        let max_displayable = area.height as usize;
        let lines_to_render = plan.log_lines.len().min(max_displayable);

        // debug!(
        //     "Rendering {} lines (max displayable: {})",
        //     lines_to_render, max_displayable
        // );

        for (i, log_line) in plan.log_lines.iter().take(lines_to_render).enumerate() {
            if i < 5 || i >= lines_to_render.saturating_sub(5) {
                // debug!(
                //     "Line {}: '{}'",
                //     i,
                //     log_line.chars().take(50).collect::<String>()
                // );
            }

            if log_line.starts_with("```") || log_line.trim_start().starts_with("```") {
                is_in_code_block = !is_in_code_block;
                lines.push(Line::from(Span::styled(
                    log_line.clone(),
                    theme.code_block_style,
                )));
            } else if is_in_code_block {
                lines.push(Line::from(Span::styled(
                    log_line.clone(),
                    theme.code_block_style,
                )));
            } else if log_line.starts_with("[shell]$") {
                lines.push(Line::from(Span::styled(
                    log_line.clone(),
                    Style::default().fg(Color::Yellow),
                )));
            } else if log_line.starts_with("[stdout]") {
                lines.push(Line::from(Span::styled(
                    log_line.clone(),
                    Style::default().fg(Color::White),
                )));
            } else if log_line.starts_with("[stderr]") {
                lines.push(Line::from(Span::styled(
                    log_line.clone(),
                    Style::default().fg(Color::Red),
                )));
            } else if log_line.starts_with("> ") {
                lines.push(Line::from(Span::styled(
                    log_line.clone(),
                    Style::default().fg(Color::Cyan),
                )));
            } else if log_line.starts_with("[tool]") {
                // Expect format: [tool] name({...}) => OK|ERR
                // Color green for OK, red for ERR, yellow otherwise
                let mut style = Style::default().fg(Color::Yellow);
                if log_line.contains("=> ERR") {
                    style = Style::default().fg(Color::Red);
                } else if log_line.contains("=> OK") {
                    style = Style::default().fg(Color::Green);
                }
                lines.push(Line::from(Span::styled(log_line.clone(), style)));
            } else if log_line.starts_with("  ") {
                // LLM response with margin - use special styling
                lines.push(Line::from(Span::styled(
                    log_line.clone(),
                    theme.llm_response_style,
                )));
            } else {
                lines.push(Line::from(log_line.as_str()));
            }
        }

        // debug!("Created {} lines for Paragraph widget", lines.len());

        let paragraph = Paragraph::new(lines)
            .style(theme.log_style)
            .block(Block::default()); // Add block to ensure proper boundaries
        f.render_widget(paragraph, area);

        // debug!(
        //     "Paragraph widget rendered {} lines in area {}x{}",
        //     lines_to_render, area.width, area.height
        // );
    }

    fn render_session_list(
        &self,
        f: &mut Frame,
        area: Rect,
        session_list_state: &crate::tui::state::SessionListState,
        theme: &Theme,
    ) {
        let items: Vec<ListItem> = session_list_state
            .sessions
            .iter()
            .enumerate()
            .map(|(i, session)| {
                let content = format!(
                    "{} ({}) - Created: {}",
                    session.title, session.id, session.created_at
                );
                let style = if i == session_list_state.selected_index {
                    theme.completion_selected_style
                } else {
                    theme.completion_style
                };
                ListItem::new(content).style(style)
            })
            .collect();

        let list =
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(
                    "Sessions (↑↓ to navigate, Enter to switch, d to delete, q/ESC to close)",
                ))
                .highlight_style(theme.completion_selected_style)
                .highlight_symbol(">> ");

        f.render_widget(list, area);
    }

    fn render_input_area(&mut self, f: &mut Frame, area: Rect) {
        let input_style = if self.input_mode == crate::tui::state::InputMode::Shell {
            self.theme.shell_input_style
        } else {
            self.theme.input_style
        };

        self.textarea.set_style(input_style);

        if self.status == crate::tui::state::Status::ShellCommandRunning {
            self.textarea.set_style(input_style.fg(Color::DarkGray));
        }

        // Set the block title based on the input mode
        let block_title = if self.input_mode == crate::tui::state::InputMode::Shell {
            "Input (Shell Mode - Press ESC to exit)"
        } else {
            "Input"
        };

        self.textarea
            .set_block(Block::default().borders(Borders::ALL).title(block_title));

        f.render_widget(&self.textarea, area);

        if self.completion_active && !self.completion_candidates.is_empty() {
            let max_width = self
                .completion_candidates
                .iter()
                .map(|s| s.len())
                .max()
                .unwrap_or(10) as u16;
            let max_display_items = 20; // Limit the number of displayed completion items
            let list_height = (self.completion_candidates.len() as u16).min(max_display_items);

            // Calculate the height of the list block including borders and title
            // Borders: top (1) + bottom (1) = 2
            // Title: 1 line
            let block_height = 2 + 1; // 3 lines
            let total_height = list_height + block_height;

            // Position the popup above the input area, considering the full height of the block
            let popup_y = area.y.saturating_sub(total_height);

            let completion_area = Rect {
                x: area.x,
                y: popup_y,
                width: max_width + 2, // +2 for padding
                height: total_height,
            };

            let items: Vec<ListItem> = self
                .completion_candidates
                .iter()
                .skip(self.completion_scroll)
                .take(max_display_items as usize)
                .enumerate()
                .map(|(i, candidate)| {
                    let style = if (i + self.completion_scroll) == self.completion_index {
                        self.theme.completion_selected_style
                    } else {
                        self.theme.completion_style
                    };
                    ListItem::new(candidate.as_str()).style(style)
                })
                .collect();

            let title = match self.completion_type {
                crate::tui::state::CompletionType::Command => "Commands",
                crate::tui::state::CompletionType::FilePath => "Files",
                _ => "Completion", // Fallback, should not happen if completion_active is true
            };

            let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));

            f.render_widget(Clear, completion_area); // Clear the area behind the popup
            f.render_widget(list, completion_area);
        }
    }

    fn render_diff_popup(&mut self, f: &mut Frame, area: Rect, diff_content: &str) {
        let text = match diff_content.as_bytes().into_text() {
            Ok(text) => text,
            Err(_) => diff_content.into(), // Fallback to plain text
        };

        let block = Block::default()
            .title("Git Diff (Press ESC or q to close)")
            .borders(Borders::ALL);

        let line_count = text.height();

        let paragraph = Paragraph::new(text)
            .block(block)
            .scroll((self.diff_scroll, 0));

        // Create a centered popup area
        let popup_area = centered_rect(80, 90, area);
        f.render_widget(Clear, popup_area); // Clear the background
        f.render_widget(paragraph, popup_area);

        if line_count > popup_area.height as usize {
            let mut scrollbar_state =
                ratatui::widgets::ScrollbarState::new(line_count - popup_area.height as usize)
                    .position(self.diff_scroll as usize);

            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                popup_area.inner(Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut scrollbar_state,
            );
        }
    }

    fn render_status_footer(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let mut status_text = String::with_capacity(100);
        status_text.push_str("v0.1.0 | ");

        // Token usage
        write!(status_text, "Prompt: {} ", self.tokens_prompt_used).unwrap();
        if let Some(total) = self.tokens_total_used {
            write!(status_text, "Total: {} | ", total).unwrap();
        } else {
            status_text.push_str("Total: N/A | ");
        }

        // Repomap status
        let repomap_str = match self.repomap_status {
            crate::tui::state::RepomapStatus::NotStarted => "NotStarted",
            crate::tui::state::RepomapStatus::Building => "Building",
            crate::tui::state::RepomapStatus::Ready => "Ready",
            crate::tui::state::RepomapStatus::Error => "Error",
        };
        write!(status_text, "Repomap: {} | ", repomap_str).unwrap();

        // Input mode
        let mode_str = match self.input_mode {
            crate::tui::state::InputMode::Normal => "Normal",
            crate::tui::state::InputMode::Shell => "Shell",
            crate::tui::state::InputMode::SessionList => "SessionList",
        };
        write!(status_text, "Mode: {}", mode_str).unwrap();

        let footer_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let status_paragraph = Paragraph::new(status_text)
            .style(theme.footer_style)
            .alignment(Alignment::Left);
        f.render_widget(status_paragraph, footer_chunks[0]);

        // Separator line
        let separator = Paragraph::new("─".repeat(area.width as usize)).style(theme.footer_style);
        f.render_widget(separator, footer_chunks[1]);
    }
}

/// helper function to create a centered rect using up certain percentage of the available rect `r`
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
