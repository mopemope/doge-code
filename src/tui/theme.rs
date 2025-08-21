use ratatui::style::{Color, Style};

#[derive(Debug, Clone)]
#[allow(dead_code)] // suppress warnings for fields intended for future use
pub struct Theme {
    pub name: String,
    pub footer_style: Style,
    pub log_style: Style,
    pub input_style: Style,
    pub shell_input_style: Style,
    pub llm_response_style: Style,
    pub code_block_style: Style,
    pub completion_style: Style,
    pub completion_selected_style: Style,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "dark".to_string(),
            footer_style: Style::default().fg(Color::Cyan),
            log_style: Style::default().fg(Color::White),
            input_style: Style::default().fg(Color::White),
            shell_input_style: Style::default().fg(Color::Yellow),
            llm_response_style: Style::default().fg(Color::Green),
            code_block_style: Style::default().fg(Color::LightCyan),
            completion_style: Style::default().fg(Color::Gray),
            completion_selected_style: Style::default().bg(Color::DarkGray).fg(Color::White),
        }
    }

    pub fn light() -> Self {
        Self {
            name: "light".to_string(),
            footer_style: Style::default().fg(Color::Blue),
            log_style: Style::default().fg(Color::Black),
            input_style: Style::default().fg(Color::Black),
            shell_input_style: Style::default().fg(Color::Blue),
            llm_response_style: Style::default().fg(Color::Green),
            code_block_style: Style::default().fg(Color::Magenta),
            completion_style: Style::default().fg(Color::DarkGray),
            completion_selected_style: Style::default().bg(Color::Gray).fg(Color::Black),
        }
    }
}
