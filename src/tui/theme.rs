use crossterm::style::Color;
// use serde::{Deserialize, Serialize}; // 削除

#[derive(Debug, Clone)]
#[allow(dead_code)] // 将来的に使用するフィールドの警告を抑制
pub struct Theme {
    pub name: String,
    pub header_fg: Color,
    pub header_separator: Color,
    pub user_input_fg: Color,
    pub llm_response_bg: Color,
    pub llm_response_fg: Color,
    pub llm_code_block_bg: Color, // 新規: コードブロック用背景色
    pub info_fg: Color,
    pub warning_fg: Color,
    pub error_fg: Color,
    pub status_idle_fg: Color,
    pub status_streaming_fg: Color,
    pub status_cancelled_fg: Color,
    pub status_done_fg: Color,
    pub status_error_fg: Color,
    pub completion_selected_bg: Color,
    pub completion_selected_fg: Color,
    pub completion_item_fg: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "dark".to_string(),
            header_fg: Color::Cyan,
            header_separator: Color::DarkGrey,
            user_input_fg: Color::Blue,
            llm_response_bg: Color::Black,
            llm_response_fg: Color::White,
            llm_code_block_bg: Color::DarkGrey, // 新規: Darkテーマ用コードブロック背景色
            info_fg: Color::DarkGrey,
            warning_fg: Color::Yellow,
            error_fg: Color::Red,
            status_idle_fg: Color::Cyan,
            status_streaming_fg: Color::Green,
            status_cancelled_fg: Color::Yellow,
            status_done_fg: Color::Green,
            status_error_fg: Color::Red,
            completion_selected_bg: Color::DarkGrey,
            completion_selected_fg: Color::White,
            completion_item_fg: Color::Grey,
        }
    }

    pub fn light() -> Self {
        Self {
            name: "light".to_string(),
            header_fg: Color::DarkBlue,
            header_separator: Color::Grey,
            user_input_fg: Color::DarkBlue,
            llm_response_bg: Color::White,
            llm_response_fg: Color::Black,
            llm_code_block_bg: Color::Grey, // 新規: Lightテーマ用コードブロック背景色
            info_fg: Color::Grey,
            warning_fg: Color::DarkYellow,
            error_fg: Color::DarkRed,
            status_idle_fg: Color::DarkBlue,
            status_streaming_fg: Color::DarkGreen,
            status_cancelled_fg: Color::DarkYellow,
            status_done_fg: Color::DarkGreen,
            status_error_fg: Color::DarkRed,
            completion_selected_bg: Color::Grey,
            completion_selected_fg: Color::Black,
            completion_item_fg: Color::DarkGrey,
        }
    }
}
