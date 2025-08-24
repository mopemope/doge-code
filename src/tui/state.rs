use anyhow::Result;
use crossterm::{
    cursor, execute,
    terminal::{self},
};
use std::io;
use std::sync::mpsc::{Receiver, Sender};
use tracing::debug;
use unicode_width::UnicodeWidthChar;

use crate::tui::completion::{AtFileIndex, CompletionState};
use crate::tui::theme::Theme;

#[derive(Debug, Clone, PartialEq)]
pub enum CurrentToken {
    FilePath(String),
    SlashCommand(String),
}

#[derive(PartialEq, Default, Clone, Copy, Debug)]
pub enum InputMode {
    #[default]
    Normal,
    Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Preparing,           // リクエスト準備中
    Sending,             // リクエスト送信中
    Waiting,             // レスポンス待機中
    Streaming,           // ストリーミング受信中
    Processing,          // ツール実行中
    ShellCommandRunning, // シェルコマンド実行中
    Cancelled,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderPlan {
    pub footer_lines: Vec<String>,
    pub log_lines: Vec<String>,
    pub input_line: String,
    // visual column within input_line where terminal cursor should be placed
    pub input_cursor_col: u16,
    // scroll indicator info
    pub scroll_info: Option<ScrollInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollInfo {
    pub current_line: usize,
    pub total_lines: usize,
    pub is_scrolling: bool,
    pub new_messages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollState {
    pub offset: usize,       // 表示開始行のオフセット（0が最新）
    pub auto_scroll: bool,   // 自動スクロール有効/無効
    pub new_messages: usize, // スクロール中に追加された新しいメッセージ数
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            offset: 0,
            auto_scroll: true,
            new_messages: 0,
        }
    }
}

pub use crate::tui::state_render::{truncate_display, wrap_display};

// Build a render plan. cursor_char_idx is the character index within `input` (not counting the "> " prompt)
#[allow(clippy::too_many_arguments)]
pub fn build_render_plan(
    title: &str,
    status: Status,
    log: &[String],
    input: &str,
    input_mode: InputMode,
    cursor_char_idx: usize,
    w: u16,
    _h: u16, // Total height (not used directly, main_content_height is used instead)
    main_content_height: u16, // Add actual main content area height
    model: Option<&str>,
    spinner_state: usize,       // Add spinner_state parameter
    tokens_used: u32,           // Add tokens_used parameter
    scroll_state: &ScrollState, // Add scroll_state parameter
) -> RenderPlan {
    let w_usize = w as usize;
    let status_str = match status {
        Status::Idle => "Ready".to_string(),
        Status::Preparing => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Preparing request... {}", spinner_char)
        }
        Status::Sending => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Sending request... {}", spinner_char)
        }
        Status::Waiting => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!(
                "Waiting for response... {} (Press Esc to cancel)",
                spinner_char
            )
        }
        Status::Streaming => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Receiving response... {}", spinner_char)
        }
        Status::Processing => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Processing tools... {}", spinner_char)
        }
        Status::ShellCommandRunning => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Executing command... {}", spinner_char)
        }
        Status::Cancelled => "Cancelled".to_string(),
        Status::Done => "Done".to_string(),
        Status::Error => "Error".to_string(),
    };
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(cwd?)".into());
    let model_suffix = model.map(|m| format!(" - model:{m}")).unwrap_or_default();
    let tokens_suffix = if tokens_used > 0 {
        format!(" - tokens:{}", tokens_used)
    } else {
        String::new()
    };
    let title_full = format!("{title}{model_suffix}{tokens_suffix} - [{status_str}]  {cwd}");
    let title_trim = truncate_display(&title_full, w_usize);
    let sep = "-".repeat(w_usize);
    let footer_lines = vec![title_trim, sep];

    // Build wrapped physical lines from logs with scroll support
    let max_log_rows = main_content_height as usize;
    let mut all_phys_lines: Vec<String> = Vec::new();

    debug!(target: "tui_render", "build_render_plan: max_log_rows={}, input_log_lines={}", 
        max_log_rows, log.len());

    // Build all physical lines first
    for (i, line) in log.iter().enumerate() {
        let line = line.trim_end_matches('\n');
        let parts = wrap_display(line, w_usize);
        debug!(target: "tui_render", "Log line {}: '{}' -> {} wrapped parts", i, line, parts.len());
        all_phys_lines.extend(parts);
    }

    let total_lines = all_phys_lines.len();
    debug!(target: "tui_render", "Total physical lines after wrapping: {}", total_lines);

    // Apply scroll offset
    let log_lines = if scroll_state.auto_scroll || scroll_state.offset == 0 {
        // Show the most recent lines (bottom of log)
        let start_idx = total_lines.saturating_sub(max_log_rows);
        debug!(target: "tui_render", "Auto-scroll: showing lines {}..{} (total={})", 
            start_idx, total_lines, total_lines);
        let mut lines = all_phys_lines[start_idx..].to_vec();
        // Ensure we don't exceed the display area
        if lines.len() > max_log_rows {
            lines.truncate(max_log_rows);
            debug!(target: "tui_render", "Truncated to {} lines to fit display area", max_log_rows);
        }
        lines
    } else {
        // Show lines based on scroll offset (offset 0 = most recent, higher = older)
        let end_idx = total_lines.saturating_sub(scroll_state.offset);
        let start_idx = end_idx.saturating_sub(max_log_rows);
        debug!(target: "tui_render", "Manual scroll: offset={}, showing lines {}..{} (total={})", 
            scroll_state.offset, start_idx, end_idx, total_lines);
        let mut lines = all_phys_lines[start_idx..end_idx].to_vec();
        // Ensure we don't exceed the display area
        if lines.len() > max_log_rows {
            lines.truncate(max_log_rows);
            debug!(target: "tui_render", "Truncated to {} lines to fit display area", max_log_rows);
        }
        lines
    };

    debug!(target: "tui_render", "Final log_lines count: {} (max_log_rows={}, area_allows={})", 
        log_lines.len(), max_log_rows, max_log_rows);

    // Create scroll info
    let scroll_info = if total_lines > max_log_rows {
        let current_line = if scroll_state.auto_scroll || scroll_state.offset == 0 {
            total_lines
        } else {
            total_lines.saturating_sub(scroll_state.offset)
        };
        Some(ScrollInfo {
            current_line,
            total_lines,
            is_scrolling: !scroll_state.auto_scroll && scroll_state.offset > 0,
            new_messages: scroll_state.new_messages,
        })
    } else {
        None
    };

    // Prepare prompt+input as a sequence of chars with widths
    let prompt = {
        let mut s = match input_mode {
            InputMode::Normal => String::from("> "),
            InputMode::Shell => String::from("$ "),
        };
        s.push_str(input);
        s
    };
    let chars: Vec<char> = prompt.chars().collect();
    let widths: Vec<usize> = chars.iter().map(|ch| ch.width().unwrap_or(0)).collect();
    // Ensure cursor position refers into prompt (offset by 2 for "> ")
    let mut cursor_in_prompt = 2usize.saturating_add(cursor_char_idx);
    if cursor_in_prompt > chars.len() {
        cursor_in_prompt = chars.len();
    }

    // If total width fits, show whole prompt
    let total_width: usize = widths.iter().sum();
    if total_width <= w_usize {
        // entire prompt visible
        let input_line = prompt.clone();
        // cursor col is width of chars[0..cursor_in_prompt]
        let col = widths[..cursor_in_prompt].iter().sum::<usize>() as u16;
        return RenderPlan {
            footer_lines,
            log_lines,
            input_line,
            input_cursor_col: col,
            scroll_info,
        };
    }

    // Otherwise, build a window around the cursor: expand leftwards then rightwards greedily
    // Start from cursor (or last char if cursor at end)
    if cursor_in_prompt >= chars.len() && !chars.is_empty() {
        cursor_in_prompt = chars.len().saturating_sub(1);
    }
    // Start window at cursor
    let mut start = cursor_in_prompt;
    let mut sum = widths.get(cursor_in_prompt).cloned().unwrap_or(0);
    // expand left while possible
    while start > 0 && sum + widths[start - 1] <= w_usize {
        start -= 1;
        sum += widths[start];
    }
    // expand right while possible
    let mut end = start + 1;
    while end < chars.len() && sum + widths[end] <= w_usize {
        sum += widths[end];
        end += 1;
    }

    // Build visible string
    let input_line: String = chars[start..end].iter().collect();
    // compute cursor col as width of chars[start..cursor_in_prompt]
    let col = if cursor_in_prompt >= start {
        widths[start..cursor_in_prompt].iter().sum::<usize>() as u16
    } else {
        0u16
    };

    RenderPlan {
        footer_lines,
        log_lines,
        input_line,
        input_cursor_col: col,
        scroll_info,
    }
}

pub struct TuiApp {
    pub title: String,
    pub input: String,
    pub log: Vec<String>,
    pub(crate) handler: Option<Box<dyn crate::tui::commands::CommandHandler + Send>>,
    pub(crate) inbox_rx: Option<Receiver<String>>,
    pub(crate) inbox_tx: Option<Sender<String>>,
    pub max_log_lines: usize,
    pub status: Status,
    pub model: Option<String>,
    // input history and navigation index; index==history.len() means current (editing) buffer
    pub input_history: Vec<String>,
    pub history_index: usize,
    pub draft: String,
    // completion
    pub at_index: AtFileIndex,
    pub compl: CompletionState,
    // theme
    pub theme: Theme,
    // llm parsing buffer for streaming
    pub(crate) llm_parsing_buffer: String,
    // cursor position within input in number of chars (not bytes)
    pub cursor: usize,
    // spinner state for "Thinking..." display
    pub spinner_state: usize,
    // Flag to prevent duplicate LLM response display
    pub is_llm_response_active: bool,
    /// Stores the content of the last LLM response added to the log to prevent duplicate printing.
    pub last_llm_response_content: Option<String>,
    // Input mode
    pub input_mode: InputMode,
    // session management
    // pub current_session: Option<SessionData>,
    // pub session_store: SessionStore,
    // Token usage tracking
    pub tokens_used: u32,
    // redraw flag
    pub dirty: bool,
    // scroll state
    pub scroll_state: ScrollState,
}

impl TuiApp {
    pub fn new(title: impl Into<String>, model: Option<String>, theme_name: &str) -> Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (input_history, history_index) = load_input_history();
        let at_index = AtFileIndex::new(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        let theme = match theme_name.to_lowercase().as_str() {
            "light" => Theme::light(),
            _ => Theme::dark(),
        };

        let app = Self {
            title: title.into(),
            input: String::new(),
            log: Vec::new(),
            handler: None,
            inbox_rx: Some(rx),
            inbox_tx: Some(tx),
            max_log_lines: 500,
            status: Status::Idle,
            model,
            input_history,
            history_index,
            draft: String::new(),
            at_index,
            compl: Default::default(),
            theme,
            llm_parsing_buffer: String::new(),
            cursor: 0,
            spinner_state: 0,              // Initialize spinner state
            is_llm_response_active: false, // Initialize the flag
            last_llm_response_content: None,
            input_mode: InputMode::default(),
            tokens_used: 0,
            dirty: true, // initial full render
            scroll_state: ScrollState::default(),
        };
        app.at_index.scan();
        Ok(app)
    }

    /*
    /// Switch to a different session
    pub fn switch_session(&mut self, session_id: &str) -> Result<()> {
        // Save current session if it exists
        if let Some(ref current_session) = self.current_session {
            self.session_store.save(current_session)?;
        }

        // Load the new session
        let new_session = self.session_store.load(session_id)?;
        self.current_session = Some(new_session);
        self.log
            .push(format!("Switched to session: {}", session_id));
        Ok(())
    }

    /// Save the current session
    pub fn save_current_session(&mut self) -> Result<()> {
        if let Some(ref current_session) = self.current_session {
            self.session_store.save(current_session)?;
            self.log.push("Session saved".to_string());
        }
        Ok(())
    }

    /// List all sessions
    pub fn list_sessions(&self) -> Result<Vec<crate::session::SessionMeta>> {
        Ok(self.session_store.list()?)
    }

    /// Create a new session
    pub fn create_session(&mut self, title: impl Into<String>) -> Result<String> {
        // Save current session if it exists
        if let Some(ref current_session) = self.current_session {
            self.session_store.save(current_session)?;
        }

        let new_session = self.session_store.create(title)?;
        let session_id = new_session.meta.id.clone();
        self.current_session = Some(new_session);
        self.log.push(format!(
            "Created and switched to new session: {}",
            session_id
        ));
        Ok(session_id)
    }

    /// Delete a session
    pub fn delete_session(&mut self, session_id: &str) -> Result<()> {
        self.session_store.delete(session_id)?;
        self.log.push(format!("Deleted session: {}", session_id));
        Ok(())
    }

    /// Get current session ID
    pub fn current_session_id(&self) -> Option<String> {
        self.current_session.as_ref().map(|s| s.meta.id.clone())
    }
    */

    pub fn with_handler(mut self, h: Box<dyn crate::tui::commands::CommandHandler + Send>) -> Self {
        self.handler = Some(h);
        self
    }

    pub fn sender(&self) -> Option<Sender<String>> {
        self.inbox_tx.clone()
    }

    pub fn current_at_token(&self) -> Option<CurrentToken> {
        let s = &self.input;
        let cursor_char = self.cursor;

        // Find the start of the word under the cursor
        let mut start_char = 0;
        for i in (0..cursor_char).rev() {
            if s.chars().nth(i).unwrap().is_whitespace() {
                start_char = i + 1;
                break;
            }
        }

        // Find the end of the word
        let mut end_char = s.chars().count();
        for i in start_char..s.chars().count() {
            if s.chars().nth(i).unwrap().is_whitespace() {
                end_char = i;
                break;
            }
        }

        // If cursor is outside the word, no token
        if cursor_char < start_char || cursor_char > end_char {
            return None;
        }

        // If cursor is at the end of the word, it's only valid if it's also the end of the string.
        // If there is a char at `end_char` it must be a whitespace, so cursor at `end_char` is outside.
        if cursor_char == end_char && end_char < s.chars().count() {
            return None;
        }

        let token: String = s
            .chars()
            .skip(start_char)
            .take(end_char - start_char)
            .collect();

        if token.starts_with('@') {
            Some(CurrentToken::FilePath(token))
        } else if token.starts_with('/') {
            Some(CurrentToken::SlashCommand(token))
        } else {
            None
        }
    }

    pub fn update_completion(&mut self) {
        if self.compl.suppress_once {
            self.compl.suppress_once = false;
            self.compl.visible = false;
            return;
        }

        if let Some(tok) = self.current_at_token() {
            self.compl.visible = true;
            match tok {
                CurrentToken::FilePath(ref token) => {
                    self.compl.query = token.clone();
                    self.compl.items = self.at_index.complete(token);
                    self.compl.slash_command_items.clear();
                    self.compl.selected = 0;
                }
                CurrentToken::SlashCommand(ref token) => {
                    self.compl.query = token.clone();
                    // Get the slash commands from the handler if it's a TuiExecutor
                    if let Some(handler) = &self.handler
                        && let Some(executor) = handler
                            .as_any()
                            .downcast_ref::<crate::tui::commands::TuiExecutor>()
                    {
                        self.compl.slash_command_items = self
                            .at_index
                            .complete_slash_command(token, &executor.slash_commands);
                    }
                    self.compl.items.clear();
                    self.compl.selected = 0;
                }
            }
        } else {
            self.compl.reset();
        }
    }

    pub fn push_log<S: Into<String>>(&mut self, s: S) {
        let lines_before = self.log.len();
        let content = s.into();
        for line in content.split('\n') {
            self.log.push(line.to_string());
        }
        if self.log.len() > self.max_log_lines {
            let overflow = self.log.len() - self.max_log_lines;
            self.log.drain(0..overflow);
        }

        let lines_added = self.log.len().saturating_sub(lines_before);
        debug!(target: "tui_log", "push_log: added {} lines, total now {}, content: '{}'", 
            lines_added, self.log.len(), content.chars().take(50).collect::<String>());

        // Count new messages when not auto-scrolling
        if !self.scroll_state.auto_scroll {
            let new_lines = self.log.len().saturating_sub(lines_before);
            self.scroll_state.new_messages =
                self.scroll_state.new_messages.saturating_add(new_lines);
            debug!(target: "tui_log", "New messages count: {}", self.scroll_state.new_messages);
        }

        // Auto-scroll to bottom when new content is added
        if self.scroll_state.auto_scroll {
            self.scroll_state.offset = 0;
        }
    }

    /// Scroll up by the specified number of lines
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_state.auto_scroll = false;
        self.scroll_state.offset = self.scroll_state.offset.saturating_add(lines);
        self.dirty = true;
    }

    /// Scroll down by the specified number of lines
    pub fn scroll_down(&mut self, lines: usize) {
        if self.scroll_state.offset <= lines {
            self.scroll_state.offset = 0;
            self.scroll_state.auto_scroll = true;
            self.scroll_state.new_messages = 0; // Clear new message count when reaching bottom
        } else {
            self.scroll_state.offset = self.scroll_state.offset.saturating_sub(lines);
        }
        self.dirty = true;
    }

    /// Scroll to the top of the log
    pub fn scroll_to_top(&mut self) {
        self.scroll_state.auto_scroll = false;
        // Set offset to maximum to show the oldest content
        self.scroll_state.offset = self.log.len();
        self.dirty = true;
    }

    /// Scroll to the bottom of the log (enable auto-scroll)
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_state.auto_scroll = true;
        self.scroll_state.offset = 0;
        self.scroll_state.new_messages = 0; // Clear new message count
        self.dirty = true;
    }

    /// Page up (scroll up by visible area size)
    pub fn page_up(&mut self, visible_lines: usize) {
        self.scroll_up(visible_lines.saturating_sub(1).max(1));
    }

    /// Page down (scroll down by visible area size)
    pub fn page_down(&mut self, visible_lines: usize) {
        self.scroll_down(visible_lines.saturating_sub(1).max(1));
    }

    /// Clears the log and resets the last LLM response content.
    pub fn clear_log(&mut self) {
        self.log.clear();
        self.last_llm_response_content = None;
        self.scroll_state = ScrollState::default();
    }

    pub fn dispatch(&mut self, line: &str) {
        // Clear the last LLM response content as a new user command is being processed
        self.last_llm_response_content = None;
        if self.handler.is_some() {
            let mut handler = self.handler.take().unwrap();
            handler.handle(line, self);
            self.handler = Some(handler);
            return;
        }
        self.push_log(format!("> {line}"));
    }

    pub fn apply_completion(&mut self) {
        if !self.compl.visible {
            return;
        }
        // Check if we're completing slash commands or file paths
        if !self.compl.slash_command_items.is_empty() {
            // Handle slash command completion
            if let Some(item) = self
                .compl
                .slash_command_items
                .get(self.compl.selected)
                .cloned()
                && let Some(CurrentToken::SlashCommand(ref tok)) = self.current_at_token()
                && let Some(pos) = self.input.rfind(tok)
            {
                // compute char index of pos
                let prefix = &self.input[..pos];
                let start_char_idx = prefix.chars().count();
                self.input.replace_range(pos..pos + tok.len(), &item);
                // update cursor to after inserted text
                self.cursor = start_char_idx + item.chars().count();
            }
        } else {
            // Handle file path completion
            if let Some(item) = self.compl.items.get(self.compl.selected).cloned() {
                if let Some(CurrentToken::FilePath(ref tok)) = self.current_at_token()
                    && let Some(pos) = self.input.rfind(tok)
                {
                    // compute char index of pos
                    let prefix = &self.input[..pos];
                    let start_char_idx = prefix.chars().count();
                    let mut ins = format!("@{}", item.rel);
                    if ins.contains(' ') {
                        ins = format!("@\"{}\"", item.rel);
                    }
                    self.input.replace_range(pos..pos + tok.len(), &ins);
                    // update cursor to after inserted text
                    self.cursor = start_char_idx + ins.chars().count();
                }
                if let Ok(mut r) = self.at_index.recent.write() {
                    r.touch(&item.rel);
                }
            }
        }
        self.compl.reset();
        self.compl.suppress_once = true;
    }

    // Insert a string at current cursor position
    pub(crate) fn insert_at_cursor(&mut self, s: &str) {
        let byte_pos = self.char_to_byte_idx(self.cursor);
        self.input.insert_str(byte_pos, s);
        self.cursor += s.chars().count();
    }

    // Remove the character before the cursor (backspace). Returns whether anything changed.
    pub(crate) fn backspace_at_cursor(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let end = self.char_to_byte_idx(self.cursor);
        let start = self.char_to_byte_idx(self.cursor - 1);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
        true
    }

    // Delete the character at the cursor (like Delete key). Returns whether anything changed.
    pub(crate) fn delete_at_cursor(&mut self) -> bool {
        let char_count = self.input.chars().count();
        if self.cursor >= char_count {
            return false;
        }
        let start = self.char_to_byte_idx(self.cursor);
        let end = self.char_to_byte_idx(self.cursor + 1);
        self.input.replace_range(start..end, "");
        true
    }

    pub(crate) fn char_to_byte_idx(&self, char_idx: usize) -> usize {
        if char_idx == 0 {
            return 0;
        }
        match self.input.char_indices().nth(char_idx) {
            Some((byte_idx, _)) => byte_idx,
            None => self.input.len(),
        }
    }

    pub fn run(&mut self) -> Result<()> {
        struct TuiGuard;
        impl Drop for TuiGuard {
            fn drop(&mut self) {
                let mut stdout = io::stdout();
                let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
                let _ = terminal::disable_raw_mode();
            }
        }
        let mut stdout = io::stdout();
        terminal::enable_raw_mode()?;
        execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
        let _guard = TuiGuard;
        let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
        let mut terminal = ratatui::Terminal::new(backend)?;
        self.event_loop(&mut terminal)
    }
}

fn history_store_path() -> std::path::PathBuf {
    let project_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let base = project_dir.join(".doge/sessions");
    std::fs::create_dir_all(&base).ok();
    base.join("input_history.json")
}

fn load_input_history() -> (Vec<String>, usize) {
    let path = history_store_path();
    let s = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".into());
    let mut v: Vec<String> = serde_json::from_str(&s).unwrap_or_default();
    if v.len() > 1000 {
        let start = v.len() - 1000;
        v = v[start..].to_vec();
    }
    let idx = v.len();
    (v, idx)
}

pub(crate) fn save_input_history(hist: &[String]) {
    let path = history_store_path();
    let slice: Vec<&String> = hist.iter().rev().take(1000).collect();
    let out: Vec<String> = slice.into_iter().rev().cloned().collect();
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&out).unwrap_or("[]".into()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_app() -> TuiApp {
        TuiApp::new("test", None, "dark").unwrap()
    }

    #[test]
    fn test_current_at_token() {
        let mut app = create_test_app();

        // No token
        app.input = "hello world".to_string();
        app.cursor = 5;
        assert_eq!(app.current_at_token(), None);

        // Cursor at the beginning of a token
        app.input = "hello @world".to_string();
        app.cursor = 6;
        assert_eq!(
            app.current_at_token(),
            Some(CurrentToken::FilePath("@world".to_string()))
        );

        // Cursor in the middle of a token
        app.input = "hello @world".to_string();
        app.cursor = 9;
        assert_eq!(
            app.current_at_token(),
            Some(CurrentToken::FilePath("@world".to_string()))
        );

        // Cursor at the end of a token
        app.input = "hello @world".to_string();
        app.cursor = 12;
        assert_eq!(
            app.current_at_token(),
            Some(CurrentToken::FilePath("@world".to_string()))
        );

        // Not a token
        app.input = "hello world@".to_string();
        app.cursor = 12;
        assert_eq!(app.current_at_token(), None);

        // Multiple tokens
        app.input = "hello @world1 @world2".to_string();
        app.cursor = 9;
        assert_eq!(
            app.current_at_token(),
            Some(CurrentToken::FilePath("@world1".to_string()))
        );
        app.cursor = 18;
        assert_eq!(
            app.current_at_token(),
            Some(CurrentToken::FilePath("@world2".to_string()))
        );

        // Cursor on whitespace
        app.input = "hello @world1 @world2".to_string();
        app.cursor = 13;
        assert_eq!(app.current_at_token(), None);

        // Slash command
        app.input = "/help".to_string();
        app.cursor = 3;
        assert_eq!(
            app.current_at_token(),
            Some(CurrentToken::SlashCommand("/help".to_string()))
        );
    }
}
