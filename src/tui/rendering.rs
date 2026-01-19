use crate::tui::state::{RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};

impl TuiApp {
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

        if self.window_width != size.width as usize {
            self.window_width = size.width as usize;
            self.recalculate_all_heights();
        }

        let params = crate::tui::state::BuildRenderPlanParams {
            title: &self.title,
            status: self.status,
            log: &self.log,
            width: size.width,
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
    }

    fn render_status_line(&self, f: &mut Frame, area: Rect, model: Option<&str>, theme: &Theme) {
        let model_name = model.unwrap_or("unknown");

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

        let spans = vec![
            Span::styled(
                " DOGE-CODE ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("| model: "),
            Span::styled(model_name, Style::default().fg(Color::Cyan)),
            Span::raw(" | "),
            Span::styled(
                status_str,
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | tokens: "),
            Span::raw(self.tokens_prompt_used.to_string()),
            Span::raw(" | "),
            Span::styled(spinner, Style::default().fg(status_color)),
        ];

        let line = Line::from(spans);
        let para = Paragraph::new(line).style(theme.footer_style);

        f.render_widget(para, area);
    }

    fn render_main_content(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        f.render_widget(Clear, area);

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
            _ => {
                self.render_log_panel(f, area, plan, theme);
            }
        }
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
