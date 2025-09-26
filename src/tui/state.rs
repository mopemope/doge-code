use crate::{config::IGNORE_FILE, tui::diff_review::DiffReviewState, tui::theme::Theme};
use anyhow::Result;
use crossterm::{
    cursor, execute,
    terminal::{self},
};
use ratatui::widgets::Block;
use std::collections::VecDeque;
use std::io;
use std::sync::mpsc::{Receiver, Sender};
use tracing::debug;
use tui_textarea::TextArea;

// Add TodoItem definition
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String, // pending, in_progress, completed
}

// Session list state for TUI
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListState {
    pub sessions: Vec<crate::session::SessionMeta>,
    pub selected_index: usize,
}

#[derive(PartialEq, Default, Clone, Copy, Debug)]
pub enum CompletionType {
    #[default]
    None,
    Command,
    FilePath,
}

#[derive(PartialEq, Default, Clone, Copy, Debug)]
pub enum InputMode {
    #[default]
    Normal,
    Shell,
    SessionList, // Session list selection mode
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Preparing,           // Request preparation
    Sending,             // Request sending
    Waiting,             // Response waiting
    Streaming,           // Streaming reception
    Processing,          // Tool execution
    ShellCommandRunning, // Shell command execution
    Cancelled,
    Done,
    Error,
}

/// Enum to represent the status of the repomap
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepomapStatus {
    #[default]
    NotStarted,
    Building,
    Ready,
    Error,
}

/// Parameters for building a render plan.
#[derive(Debug)]
pub struct BuildRenderPlanParams<'a> {
    pub title: &'a str,
    pub status: Status,
    pub log: &'a [String],
    pub width: u16,
    pub main_content_height: u16,
    pub model: Option<&'a str>,
    pub spinner_state: usize,
    pub scroll_state: &'a ScrollState,
    pub todo_list: &'a [TodoItem],
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
    /// Todo list items to be rendered separately from the log area
    pub todo_list: Vec<TodoItem>,
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
    pub offset: usize,       // Display start line offset (0 is the latest)
    pub auto_scroll: bool,   // Auto-scroll enabled/disabled
    pub new_messages: usize, // Number of new messages added during scrolling
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

pub use crate::tui::state_render::build_render_plan;

/* build_render_plan moved to src/tui/state_render.rs; use the re-exported function via `pub use crate::tui::state_render::build_render_plan` above */

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;

pub struct TuiApp {
    pub title: String,
    pub textarea: TextArea<'static>,
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

    // theme
    pub theme: Theme,
    // llm parsing buffer for streaming
    pub(crate) llm_parsing_buffer: String,
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
    // Prompt tokens (for header display)
    pub tokens_prompt_used: u32,
    // Total tokens (if available)
    pub tokens_total_used: Option<u32>,
    // redraw flag
    pub dirty: bool,
    // scroll state
    pub scroll_state: ScrollState,
    // completion state
    pub completion_candidates: Vec<String>,
    pub completion_index: usize,
    pub completion_active: bool,
    pub completion_type: CompletionType,
    pub completion_scroll: usize,
    // auto-compact threshold (can be updated by main from AppConfig)
    pub auto_compact_prompt_token_threshold: u32,
    // auto-compact flag to avoid duplicate triggers
    pub auto_compact_pending: bool,
    pub pending_instructions: VecDeque<String>,
    pub diff_review: Option<DiffReviewState>,
    // todo list
    pub todo_list: Vec<TodoItem>,
    /// If true, the todo list received from `todo_write` that contained only
    /// completed items should be hidden when the next user instruction is
    /// dispatched. This preserves the current display but clears the list on
    /// the following command as requested.
    pub hide_todo_on_next_instruction: bool,
    // last user input for retrying after compact
    pub last_user_input: Option<String>,
    // session list state
    pub session_list_state: Option<SessionListState>,
    /// Status of the repomap
    pub repomap_status: RepomapStatus,
    /// Start time for processing elapsed time tracking
    pub processing_start_time: Option<std::time::Instant>,
    /// Final elapsed time string for display after processing completes (remains until next instruction)
    pub last_elapsed_time: Option<String>,
}

impl TuiApp {
    pub fn get_all_commands(&self) -> Vec<String> {
        let mut commands = vec![
            "/help".to_string(),
            "/map".to_string(),
            "/tools".to_string(),
            "/clear".to_string(),
            "/open".to_string(),
            "/quit".to_string(),
            "/theme".to_string(),
            "/session".to_string(),
            "/rebuild-repomap".to_string(),
            "/tokens".to_string(),
            "/cancel".to_string(),
            "/compact".to_string(),
            "/git-worktree".to_string(),
        ];

        // Get custom commands
        if let Some(handler) = &self.handler {
            commands.extend(handler.get_custom_commands());
        }

        commands
    }

    pub fn update_completion_candidates(&mut self, input: &str) {
        if !input.starts_with('/') {
            self.completion_active = false;
            self.completion_candidates.clear();
            return;
        }

        let command_part = &input[1..]; // Remove the leading '/'
        let candidates: Vec<String> = self
            .get_all_commands()
            .into_iter()
            .filter(|cmd| {
                cmd[1..]
                    .to_lowercase()
                    .contains(&command_part.to_lowercase())
            })
            .collect();

        if candidates.is_empty() {
            self.completion_active = false;
            self.completion_candidates.clear();
            self.completion_type = CompletionType::None;
        } else {
            self.completion_active = true;
            self.completion_candidates = candidates;
            self.completion_index = 0;
            self.completion_scroll = 0; // Reset scroll
            self.completion_type = CompletionType::Command;
        }
        self.dirty = true;
    }

    pub fn update_file_path_completion_candidates(&mut self, input: &str) {
        debug!("Updating file path completion for input: {}", input);

        if let Some(at_pos) = input.rfind('@') {
            let path_part = &input[at_pos + 1..];
            // debug!("Path part: {}", path_part);
            let project_root = match std::env::current_dir() {
                Ok(path) => path,
                Err(_e) => {
                    // debug!("Error getting current dir: {}", e);
                    self.completion_active = false;
                    self.completion_candidates.clear();
                    return;
                }
            };
            // debug!("Project root: {:?}", project_root);

            let mut candidates = Vec::new();
            let walker = ignore::WalkBuilder::new(&project_root)
                .ignore(false)
                .hidden(false)
                .add_custom_ignore_filename(IGNORE_FILE)
                .build();

            for result in walker {
                match result {
                    Ok(entry) => {
                        if let Ok(relative_path) = entry.path().strip_prefix(&project_root) {
                            let path_str = relative_path.to_string_lossy();
                            if path_str.to_lowercase().contains(&path_part.to_lowercase()) {
                                debug!("Found candidate: {}", path_str);
                                candidates.push(path_str.to_string());
                            }
                        }
                    }
                    Err(e) => {
                        debug!("Error walking directory: {}", e);
                    }
                }
            }
            candidates.sort();
            // debug!("Found {} candidates", candidates.len());

            if candidates.is_empty() {
                self.completion_active = false;
                self.completion_candidates.clear();
                self.completion_type = CompletionType::None;
            } else {
                self.completion_active = true;
                self.completion_candidates = candidates;
                self.completion_index = 0;
                self.completion_scroll = 0; // Reset scroll
                self.completion_type = CompletionType::FilePath;
            }
        } else {
            self.completion_active = false;
            self.completion_candidates.clear();
            self.completion_type = CompletionType::None;
        }
        self.dirty = true;
    }

    pub fn new(title: impl Into<String>, model: Option<String>, theme_name: &str) -> Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (input_history, history_index) = load_input_history();

        let theme = match theme_name.to_lowercase().as_str() {
            "light" => Theme::light(),
            _ => Theme::dark(),
        };

        let mut textarea = TextArea::default();
        textarea.set_block(Block::default().title("Input"));
        textarea.set_placeholder_text("Enter your message...");

        let app = Self {
            title: title.into(),
            textarea,
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

            theme,
            llm_parsing_buffer: String::new(),
            spinner_state: 0,              // Initialize spinner state
            is_llm_response_active: false, // Initialize the flag
            last_llm_response_content: None,
            input_mode: InputMode::default(),
            tokens_used: 0,
            tokens_prompt_used: 0,
            tokens_total_used: None,
            dirty: true, // initial full render
            scroll_state: ScrollState::default(),
            completion_candidates: Vec::new(),
            completion_index: 0,
            completion_active: false,
            completion_type: CompletionType::None,
            completion_scroll: 0,
            // auto-compact threshold default (can be overridden by main)
            auto_compact_prompt_token_threshold:
                crate::config::DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
            // auto-compact starts not pending
            auto_compact_pending: false,
            pending_instructions: VecDeque::new(),
            diff_review: None,
            // todo list
            todo_list: Vec::new(),
            hide_todo_on_next_instruction: false,
            // last user input for retrying after compact
            last_user_input: None,
            // session list state
            session_list_state: None,
            // repomap status
            repomap_status: RepomapStatus::default(), // Initialize with NotStarted
            processing_start_time: None,
            last_elapsed_time: None,
        };

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
    pub fn create_session(&mut self) -> Result<String> {
        // Save current session if it exists
        if let Some(ref current_session) = self.current_session {
            self.session_store.save(current_session)?;
        }

        let new_session = self.session_store.create()?;
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

        let _lines_added = self.log.len().saturating_sub(lines_before);
        // debug!(
        //     "push_log: added {} lines, total now {}, content: \"{}\"",
        //     lines_added,
        //     self.log.len(),
        //     content.chars().take(50).collect::<String>()
        // );

        // Count new messages when not auto-scrolling
        if !self.scroll_state.auto_scroll {
            let new_lines = self.log.len().saturating_sub(lines_before);
            self.scroll_state.new_messages =
                self.scroll_state.new_messages.saturating_add(new_lines);
            debug!("New messages count: {}", self.scroll_state.new_messages);
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

    /// Enter session list mode with the provided sessions
    pub fn enter_session_list_mode(&mut self, sessions: Vec<crate::session::SessionMeta>) {
        self.session_list_state = Some(SessionListState {
            sessions,
            selected_index: 0,
        });
        self.input_mode = InputMode::SessionList;
        self.dirty = true;
    }

    pub fn dispatch(&mut self, line: &str) {
        // Clear the last LLM response content as a new user command is being processed
        self.last_llm_response_content = None;

        // If the todo list was flagged to be hidden on the next instruction,
        // clear it now and reset the flag. This ensures the todo list created
        // by `todo_write` that contains only completed items will be hidden
        // starting from the user's next command.
        if self.hide_todo_on_next_instruction {
            self.todo_list.clear();
            self.hide_todo_on_next_instruction = false;
        }

        if self.handler.is_some() {
            let mut handler = self.handler.take().unwrap();
            handler.handle(line, self);
            self.handler = Some(handler);
            return;
        }
        self.push_log(format!("> {}", line));
    }

    pub fn run(&mut self) -> Result<()> {
        // Temporarily disable terminal check for development environments
        // Check if stdin and stdout are terminals
        // if !atty::is(atty::Stream::Stdin) || !atty::is(atty::Stream::Stdout) {
        //     return Err(anyhow::anyhow!("Standard input/output is not a terminal. Please run in a terminal environment."));
        // }

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
        serde_json::to_string_pretty(&out).unwrap_or_else(|_| "[]".to_string()),
    );
}
