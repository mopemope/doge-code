use anyhow::Result;
use crossterm::{
    cursor, execute,
    terminal::{self},
};
use std::io;
use std::sync::mpsc::{Receiver, Sender};
use unicode_width::UnicodeWidthChar;

use crate::tui::completion::{AtFileIndex, CompletionState};
use crate::tui::theme::Theme;

#[derive(Debug, Clone)]
pub enum LlmResponseSegment {
    Text { content: String },
    CodeBlock { language: String, content: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Streaming,
    Cancelled,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderPlan {
    pub header_lines: Vec<String>,
    pub log_lines: Vec<String>,
    pub input_line: String,
    // visual column within input_line where terminal cursor should be placed
    pub input_cursor_col: u16,
}

pub fn truncate_display(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut width = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let ch_w = ch.width().unwrap_or(0);
        if ch_w == 0 {
            out.push(ch);
            continue;
        }
        if width + ch_w > max {
            break;
        }
        out.push(ch);
        width += ch_w;
    }
    out
}

pub fn wrap_display(s: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut width = 0usize;
    for ch in s.chars() {
        let ch_w = ch.width().unwrap_or(0);
        if ch_w == 0 {
            cur.push(ch);
            continue;
        }
        if width + ch_w > max {
            lines.push(cur);
            cur = String::new();
            width = 0;
        }
        cur.push(ch);
        width += ch_w;
    }
    lines.push(cur);
    lines
}

// Build a render plan. cursor_char_idx is the character index within `input` (not counting the "> " prompt)
pub fn build_render_plan(
    title: &str,
    status: Status,
    log: &[String],
    input: &str,
    cursor_char_idx: usize,
    w: u16,
    h: u16,
    model: Option<&str>,
    spinner_state: usize, // Add spinner_state parameter
) -> RenderPlan {
    let w_usize = w as usize;
    let status_str = match status {
        Status::Idle => {
            // Define spinner characters
            let spinner_chars = ['/', '-', '\\', '|'];
            // Get the current spinner character based on spinner_state
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Thinking... {}", spinner_char)
        }
        Status::Streaming => "Streaming".to_string(),
        Status::Cancelled => "Cancelled".to_string(),
        Status::Done => "Done".to_string(),
        Status::Error => "Error".to_string(),
    };
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(cwd?)".into());
    let model_suffix = model.map(|m| format!(" - model:{m}")).unwrap_or_default();
    let title_full = format!("{title}{model_suffix} - [{status_str}]  {cwd}");
    let title_trim = truncate_display(&title_full, w_usize);
    let sep = "-".repeat(w_usize);
    let header_lines = vec![title_trim, sep];

    // Build wrapped physical lines from logs (from end to start to keep last rows)
    let max_log_rows = h.saturating_sub(3) as usize;
    let mut phys_rev: Vec<String> = Vec::new();
    for line in log.iter().rev() {
        let line = line.trim_end_matches('\n');
        let parts = wrap_display(line, w_usize);
        for p in parts.into_iter().rev() {
            phys_rev.push(p);
            if phys_rev.len() >= max_log_rows {
                break;
            }
        }
        if phys_rev.len() >= max_log_rows {
            break;
        }
    }
    phys_rev.reverse();
    let log_lines = phys_rev;

    // Prepare prompt+input as a sequence of chars with widths
    let prompt = {
        let mut s = String::from("> ");
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
            header_lines,
            log_lines,
            input_line,
            input_cursor_col: col,
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
        header_lines,
        log_lines,
        input_line,
        input_cursor_col: col,
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
    // llm response
    pub(crate) current_llm_response: Option<Vec<LlmResponseSegment>>,
    pub(crate) llm_parsing_buffer: String,
    // cursor position within input in number of chars (not bytes)
    pub cursor: usize,
    // spinner state for "Thinking..." display
    pub spinner_state: usize,
}

impl TuiApp {
    pub fn new(title: impl Into<String>, model: Option<String>, theme_name: &str) -> Self {
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
            current_llm_response: None,
            llm_parsing_buffer: String::new(),
            cursor: 0,
            spinner_state: 0, // Initialize spinner state
        };
        app.at_index.scan();
        app
    }

    pub fn with_handler(mut self, h: Box<dyn crate::tui::commands::CommandHandler + Send>) -> Self {
        self.handler = Some(h);
        self
    }

    pub fn sender(&self) -> Option<Sender<String>> {
        self.inbox_tx.clone()
    }

    pub fn current_at_token(&self) -> Option<String> {
        let s = self.input.as_str();
        let mut start = None;
        for (i, ch) in s.char_indices() {
            if ch == '@' {
                start = Some(i);
            }
            if ch.is_whitespace()
                && let Some(st) = start
            {
                if i > st {
                    return Some(s[st..i].to_string());
                } else {
                    start = None;
                }
            }
        }
        start.map(|st| s[st..].to_string())
    }

    pub fn update_completion(&mut self) {
        if self.compl.suppress_once {
            self.compl.suppress_once = false;
            self.compl.visible = false;
            return;
        }
        // Only trigger completion when the character immediately before the cursor is '@'.
        let prev_char_is_at = match self.cursor {
            0 => false,
            _ => self.input.chars().nth(self.cursor - 1) == Some('@'),
        };
        if !prev_char_is_at {
            self.compl.reset();
            return;
        }
        // If this '@' starts a new token (BOL or preceded by whitespace)
        let prev2_is_space = if self.cursor < 2 {
            true
        } else {
            match self.input.chars().nth(self.cursor.saturating_sub(2)) {
                Some(c) => c.is_whitespace(),
                None => true,
            }
        };
        if prev2_is_space {
            let tok = "@".to_string();
            self.compl.visible = true;
            self.compl.query = tok.clone();
            self.compl.items = self.at_index.complete(&tok);
            self.compl.selected = 0;
            return;
        }
        if let Some(tok) = self.current_at_token()
            && tok.starts_with('@')
        {
            self.compl.visible = true;
            self.compl.query = tok.clone();
            self.compl.items = self.at_index.complete(&tok);
            self.compl.selected = 0;
            return;
        }
        self.compl.reset();
    }

    pub fn push_log<S: Into<String>>(&mut self, s: S) {
        for line in s.into().split('\n') {
            self.log.push(line.to_string());
        }
        if self.log.len() > self.max_log_lines {
            let overflow = self.log.len() - self.max_log_lines;
            self.log.drain(0..overflow);
        }
    }

    pub fn dispatch(&mut self, line: &str) {
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
        if let Some(item) = self.compl.items.get(self.compl.selected).cloned() {
            if let Some(tok) = self.current_at_token()
                && let Some(pos) = self.input.rfind(&tok)
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
        self.event_loop()
    }
}

fn history_store_path() -> std::path::PathBuf {
    let base = crate::session::SessionStore::new_default()
        .map(|s| s.root)
        .unwrap_or_else(|_| std::path::PathBuf::from("./.doge/sessions"));
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
