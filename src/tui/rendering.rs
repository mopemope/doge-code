use crate::tui::state::{RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, ListItem, Paragraph},
};
use tracing::debug;

impl TuiApp {
    pub fn view(&mut self, f: &mut Frame, model: Option<&str>) {
        let theme = &self.theme;
        let size = f.area();

        debug!(target: "tui_render", "Screen size: {}x{}", size.width, size.height);

        // Calculate layout first to get actual main content area height
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header
                Constraint::Min(1),    // Main content
                Constraint::Length(1), // Footer
            ])
            .split(size);

        let main_content_height = chunks[1].height;
        debug!(target: "tui_render", "Layout chunks: header={}x{}, main={}x{}, footer={}x{}", 
            chunks[0].width, chunks[0].height,
            chunks[1].width, chunks[1].height,
            chunks[2].width, chunks[2].height);
        debug!(target: "tui_render", "Main content area height: {}", main_content_height);
        debug!(target: "tui_render", "Total log lines: {}", self.log.len());

        let plan = build_render_plan(
            &self.title,
            self.status,
            &self.log,
            &self.input,
            self.input_mode,
            self.cursor,
            size.width,
            size.height,
            main_content_height,
            model,
            self.spinner_state,
            self.tokens_used,
            &self.scroll_state,
        );

        debug!(target: "tui_render", "Render plan: log_lines={}, scroll_info={:?}", 
            plan.log_lines.len(), plan.scroll_info);

        self.render_header(f, chunks[0], &plan, theme);
        self.render_main_content(f, chunks[1], &plan, theme);
        self.render_footer(f, chunks[2], &plan, theme);

        self.render_completion(f, chunks[1], theme); // Render completion over main content

        f.set_cursor_position((plan.input_cursor_col, chunks[2].y));
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
        debug!(target: "tui_render", "render_main_content: area={}x{} (x={}, y={}), plan.log_lines={}", 
            area.width, area.height, area.x, area.y, plan.log_lines.len());

        // Check if we have more lines than the area can display
        if plan.log_lines.len() > area.height as usize {
            debug!(target: "tui_render", "WARNING: More lines ({}) than area height ({})", 
                plan.log_lines.len(), area.height);
        }

        let mut lines: Vec<Line> = Vec::new();
        let mut is_in_code_block = false;

        // Ensure we don't try to render more lines than the area can display
        let max_displayable = area.height as usize;
        let lines_to_render = plan.log_lines.len().min(max_displayable);

        debug!(target: "tui_render", "Rendering {} lines (max displayable: {})", 
            lines_to_render, max_displayable);

        for (i, log_line) in plan.log_lines.iter().take(lines_to_render).enumerate() {
            if i < 5 || i >= lines_to_render.saturating_sub(5) {
                debug!(target: "tui_render", "Line {}: '{}'", i, 
                    log_line.chars().take(50).collect::<String>());
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

        debug!(target: "tui_render", "Created {} lines for Paragraph widget", lines.len());

        let paragraph = Paragraph::new(lines)
            .style(theme.log_style)
            .block(Block::default()); // Add block to ensure proper boundaries
        f.render_widget(paragraph, area);

        debug!(target: "tui_render", "Paragraph widget rendered {} lines in area {}x{}", 
            lines_to_render, area.width, area.height);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        let mut input_style = if self.input_mode == crate::tui::state::InputMode::Shell {
            theme.shell_input_style
        } else {
            theme.input_style
        };

        if self.status == crate::tui::state::Status::ShellCommandRunning {
            input_style = input_style.fg(Color::DarkGray);
        }

        let input = Paragraph::new(plan.input_line.as_str())
            .style(input_style)
            .block(Block::default());
        f.render_widget(input, area);
    }

    fn render_completion(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        if !self.compl.visible {
            return;
        }

        // Determine if we're showing file paths or slash commands
        let is_slash_command = !self.compl.slash_command_items.is_empty();

        let max_items = 10;
        let items_to_show: Vec<String> = if is_slash_command {
            self.compl
                .slash_command_items
                .iter()
                .take(max_items)
                .cloned()
                .collect()
        } else {
            self.compl
                .items
                .iter()
                .take(max_items)
                .map(|item| item.rel.clone())
                .collect()
        };

        let list_height = items_to_show.len() as u16;
        let list_width = items_to_show
            .iter()
            .map(|item| item.chars().count())
            .max()
            .unwrap_or(20) as u16
            + 4;

        if area.height < list_height + 2 || area.width < list_width + 2 {
            return;
        }

        let completion_area = Rect {
            x: area.x + 1,
            y: area.y + area.height.saturating_sub(list_height + 2),
            width: list_width,
            height: list_height + 2,
        };

        let list_items: Vec<ListItem> = items_to_show
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let style = if i == self.compl.selected {
                    theme.completion_selected_style
                } else {
                    theme.completion_style
                };
                ListItem::new(item.as_str()).style(style)
            })
            .collect();

        let list = ratatui::widgets::List::new(list_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if is_slash_command {
                        "Slash Commands"
                    } else {
                        "Completion"
                    }),
            )
            .style(theme.completion_style);

        f.render_widget(list, completion_area);
    }
}
