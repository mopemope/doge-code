use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
#[allow(dead_code)] // suppress warnings for fields intended for future use
pub struct Theme {
    pub name: String,
    pub background_style: Style, // NEW: Full screen background
    pub title_style: Style,      // NEW: Title/header text style
    pub footer_style: Style,
    pub log_style: Style,
    pub input_style: Style,
    pub shell_input_style: Style,
    pub llm_response_style: Style,
    pub code_block_style: Style,
    pub completion_style: Style,
    pub completion_selected_style: Style,
    pub border_style: Style,
    pub border_type: ratatui::widgets::BorderType,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "dark".to_string(),
            background_style: Style::default().bg(Color::Black),
            title_style: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            footer_style: Style::default().fg(Color::Cyan),
            log_style: Style::default().fg(Color::White),
            input_style: Style::default().fg(Color::White),
            shell_input_style: Style::default().fg(Color::Yellow),
            llm_response_style: Style::default().fg(Color::White),
            code_block_style: Style::default().fg(Color::LightCyan),
            completion_style: Style::default().fg(Color::Gray),
            completion_selected_style: Style::default().bg(Color::DarkGray).fg(Color::White),
            border_style: Style::default().fg(Color::DarkGray),
            border_type: ratatui::widgets::BorderType::Plain,
        }
    }

    pub fn light() -> Self {
        Self {
            name: "light".to_string(),
            background_style: Style::default().bg(Color::White),
            title_style: Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            footer_style: Style::default().fg(Color::Blue),
            log_style: Style::default().fg(Color::Black),
            input_style: Style::default().fg(Color::Black),
            shell_input_style: Style::default().fg(Color::Blue),
            llm_response_style: Style::default().fg(Color::Black),
            code_block_style: Style::default().fg(Color::Magenta),
            completion_style: Style::default().fg(Color::DarkGray),
            completion_selected_style: Style::default().bg(Color::Gray).fg(Color::Black),
            border_style: Style::default().fg(Color::Gray),
            border_type: ratatui::widgets::BorderType::Plain,
        }
    }
}
