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
    pub border_style: Style,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "dark".to_string(),
            footer_style: Style::default().fg(Color::Cyan),
            log_style: Style::default().fg(Color::White),
            input_style: Style::default().fg(Color::White),
            shell_input_style: Style::default().fg(Color::Yellow),
            llm_response_style: Style::default().fg(Color::White), // Changed from Green to White
            code_block_style: Style::default().fg(Color::LightCyan),
            completion_style: Style::default().fg(Color::Gray),
            completion_selected_style: Style::default().bg(Color::DarkGray).fg(Color::White),
            border_style: Style::default().fg(Color::DarkGray),
        }
    }

    pub fn light() -> Self {
        Self {
            name: "light".to_string(),
            footer_style: Style::default().fg(Color::Blue),
            log_style: Style::default().fg(Color::Black),
            input_style: Style::default().fg(Color::Black),
            shell_input_style: Style::default().fg(Color::Blue),
            llm_response_style: Style::default().fg(Color::White), // Changed from Green to White
            code_block_style: Style::default().fg(Color::Magenta),
            completion_style: Style::default().fg(Color::DarkGray),
            completion_selected_style: Style::default().bg(Color::Gray).fg(Color::Black),
            border_style: Style::default().fg(Color::Gray),
        }
    }

    pub fn cyberpunk() -> Self {
        Self {
            name: "cyberpunk".to_string(),
            footer_style: Style::default()
                .fg(Color::Rgb(0, 255, 255))
                .bg(Color::Rgb(16, 16, 32)), // Neon Cyan on dark blue
            log_style: Style::default().fg(Color::Rgb(220, 220, 220)), // Off-white
            input_style: Style::default().fg(Color::Rgb(0, 255, 0)),   // Matrix Green
            shell_input_style: Style::default().fg(Color::Rgb(255, 0, 255)), // Neon Pink
            llm_response_style: Style::default().fg(Color::Rgb(0, 255, 255)), // Cyan
            code_block_style: Style::default().fg(Color::Rgb(255, 255, 0)), // Yellow
            completion_style: Style::default().fg(Color::Rgb(128, 0, 128)), // Purple
            completion_selected_style: Style::default()
                .bg(Color::Rgb(255, 0, 255))
                .fg(Color::Black), // Pink bg, black text
            border_style: Style::default().fg(Color::Rgb(255, 0, 255)), // Neon Pink borders
        }
    }
}
