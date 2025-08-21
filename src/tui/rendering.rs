use crate::tui::state::{LlmResponseSegment, RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, ListItem, Paragraph, Wrap},
};
use std::io;
use std::rc::Rc;

impl TuiApp {
    pub fn draw_with_model(&mut self, model: Option<&str>) -> io::Result<()> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
        terminal.draw(|f| self.view(f, model))?;
        Ok(())
    }

    pub fn view(&mut self, f: &mut Frame, model: Option<&str>) {
        let theme = &self.theme;
        let size = f.area();
        let plan = build_render_plan(
            &self.title,
            self.status,
            &self.log,
            &self.input,
            self.input_mode,
            self.cursor,
            size.width,
            size.height,
            model,
            self.spinner_state,
        );

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header
                Constraint::Min(1),    // Main content
                Constraint::Length(1), // Footer
            ])
            .split(size);

        self.render_header(f, chunks[0], &plan, theme);
        self.render_main_content(f, chunks[1], &plan, theme);
        self.render_footer(f, chunks[2], &plan, theme);

        self.render_completion(f, chunks[1], theme); // Render completion over main content

        f.set_cursor(plan.input_cursor_col, chunks[2].y);
    }

    fn render_header(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        let header_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let title_text = if !plan.footer_lines.is_empty() {
            plan.footer_lines[0].clone()
        } else {
            String::new()
        };
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
        let mut lines: Vec<Line> = Vec::new();
        let mut is_in_code_block = false;

        for log_line in &plan.log_lines {
            if log_line.starts_with("```") {
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
            } else {
                lines.push(Line::from(log_line.as_str()));
            }
        }

        if let Some(response_segments) = &self.current_llm_response {
            for segment in response_segments {
                match segment {
                    LlmResponseSegment::Text { content } => {
                        lines.push(Line::from(Span::styled(
                            content.clone(),
                            theme.llm_response_style,
                        )));
                    }
                    LlmResponseSegment::CodeBlock { language, content } => {
                        let lang_line = format!("```{language}");
                        lines.push(Line::from(Span::styled(lang_line, theme.code_block_style)));
                        for line in content.lines() {
                            lines.push(Line::from(Span::styled(
                                line.to_string(),
                                theme.code_block_style,
                            )));
                        }
                        lines.push(Line::from(Span::styled(
                            "```".to_string(),
                            theme.code_block_style,
                        )));
                    }
                }
            }
        }

        let paragraph = Paragraph::new(lines)
            .style(theme.log_style)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
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

        let max_items = 10;
        let items_to_show = self.compl.items.iter().take(max_items).collect::<Vec<_>>();
        let list_height = items_to_show.len() as u16;
        let list_width = items_to_show
            .iter()
            .map(|item| item.rel.len())
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
                ListItem::new(item.rel.as_str()).style(style)
            })
            .collect();

        let list = ratatui::widgets::List::new(list_items)
            .block(Block::default().borders(Borders::ALL).title("Completion"))
            .style(theme.completion_style);

        f.render_widget(list, completion_area);
    }
}

pub fn layout_with_footer(area: Rect, footer_height: u16) -> Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(footer_height)])
        .split(area)
}
