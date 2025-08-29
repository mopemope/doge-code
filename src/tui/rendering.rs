use crate::tui::state::{RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use tracing::debug;

impl TuiApp {
    pub fn view(&mut self, f: &mut Frame, model: Option<&str>) {
        let size = f.area();

        debug!("Screen size: {}x{}", size.width, size.height);

        // Adjust layout for multi-line input area
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header
                Constraint::Min(1),    // Main content
                Constraint::Length(5), // Footer (increased height for textarea)
            ])
            .split(size);

        let main_content_height = chunks[1].height;
        debug!(
            "Layout chunks: header={}x{}, main={}x{}, footer={}x{}",
            chunks[0].width,
            chunks[0].height,
            chunks[1].width,
            chunks[1].height,
            chunks[2].width,
            chunks[2].height
        );
        debug!("Main content area height: {}", main_content_height);
        debug!("Total log lines: {}", self.log.len());

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
            self.tokens_used,
            &self.scroll_state,
        );

        debug!(
            "Render plan: log_lines={}, scroll_info={:?}",
            plan.log_lines.len(),
            plan.scroll_info
        );

        self.render_header(f, chunks[0], &plan, &self.theme);
        self.render_main_content(f, chunks[1], &plan, &self.theme);

        self.render_footer(f, chunks[2]);

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
        debug!(
            "render_main_content: area={}x{} (x={}, y={}), plan.log_lines={}",
            area.width,
            area.height,
            area.x,
            area.y,
            plan.log_lines.len()
        );

        // Check if we have more lines than the area can display
        if plan.log_lines.len() > area.height as usize {
            debug!(
                "WARNING: More lines ({}) than area height ({})",
                plan.log_lines.len(),
                area.height
            );
        }

        let mut lines: Vec<Line> = Vec::new();
        let mut is_in_code_block = false;

        // Ensure we don't try to render more lines than the area can display
        let max_displayable = area.height as usize;
        let lines_to_render = plan.log_lines.len().min(max_displayable);

        debug!(
            "Rendering {} lines (max displayable: {})",
            lines_to_render, max_displayable
        );

        for (i, log_line) in plan.log_lines.iter().take(lines_to_render).enumerate() {
            if i < 5 || i >= lines_to_render.saturating_sub(5) {
                debug!(
                    "Line {}: '{}'",
                    i,
                    log_line.chars().take(50).collect::<String>()
                );
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

        debug!("Created {} lines for Paragraph widget", lines.len());

        let paragraph = Paragraph::new(lines)
            .style(theme.log_style)
            .block(Block::default()); // Add block to ensure proper boundaries
        f.render_widget(paragraph, area);

        debug!(
            "Paragraph widget rendered {} lines in area {}x{}",
            lines_to_render, area.width, area.height
        );
    }

    fn render_footer(&mut self, f: &mut Frame, area: Rect) {
        let input_style = if self.input_mode == crate::tui::state::InputMode::Shell {
            self.theme.shell_input_style
        } else {
            self.theme.input_style
        };

        self.textarea.set_style(input_style);

        if self.status == crate::tui::state::Status::ShellCommandRunning {
            self.textarea.set_style(input_style.fg(Color::DarkGray));
        }

        f.render_widget(&self.textarea, area);

        if self.completion_active && !self.completion_candidates.is_empty() {
            let max_width = self
                .completion_candidates
                .iter()
                .map(|s| s.len())
                .max()
                .unwrap_or(10) as u16;
            let max_display_items = 10; // Limit the number of displayed completion items
            let list_height = (self.completion_candidates.len() as u16).min(max_display_items);

            // Position the popup above the input area
            let popup_y = area.y.saturating_sub(list_height);

            let completion_area = Rect {
                x: area.x,
                y: popup_y,
                width: max_width + 2, // +2 for padding
                height: list_height,
            };

            let items: Vec<ListItem> = self
                .completion_candidates
                .iter()
                .enumerate()
                .map(|(i, candidate)| {
                    let style = if i == self.completion_index {
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
}
