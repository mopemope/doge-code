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

    pub fn cyberpunk() -> Self {
        // Dark blue/purple base with neon accents
        let bg_color = Color::Rgb(10, 10, 30); // Deep space blue
        let neon_cyan = Color::Rgb(0, 255, 255); // Electric cyan
        let neon_pink = Color::Rgb(255, 0, 128); // Hot pink
        let neon_green = Color::Rgb(57, 255, 20); // Neon green
        let neon_yellow = Color::Rgb(255, 255, 0); // Electric yellow
        let neon_purple = Color::Rgb(191, 0, 255); // Vivid purple

        Self {
            name: "cyberpunk".to_string(),
            background_style: Style::default().bg(bg_color),
            title_style: Style::default()
                .fg(neon_cyan)
                .bg(bg_color)
                .add_modifier(Modifier::BOLD),
            footer_style: Style::default().fg(neon_cyan).bg(bg_color),
            log_style: Style::default().fg(Color::Rgb(200, 200, 220)).bg(bg_color),
            input_style: Style::default().fg(neon_green).bg(bg_color),
            shell_input_style: Style::default().fg(neon_pink).bg(bg_color),
            llm_response_style: Style::default().fg(neon_cyan).bg(bg_color),
            code_block_style: Style::default().fg(neon_yellow).bg(Color::Rgb(20, 20, 40)),
            completion_style: Style::default().fg(neon_purple).bg(bg_color),
            completion_selected_style: Style::default()
                .bg(neon_pink)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            border_style: Style::default().fg(neon_pink),
            border_type: ratatui::widgets::BorderType::Thick,
        }
    }
}
