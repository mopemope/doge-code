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

// 新規: LLMレスポンスの構造化セグメント定義 (後で別ファイルに移動する予定)
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

// Soft-wrap a single logical line into multiple physical lines within max width, preserving Unicode display width.
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

pub fn build_render_plan(
    title: &str,
    status: Status,
    log: &[String],
    input: &str,
    w: u16,
    h: u16,
    model: Option<&str>,
) -> RenderPlan {
    let w_usize = w as usize;
    let status_str = match status {
        Status::Idle => "Idle",
        Status::Streaming => "Streaming",
        Status::Cancelled => "Cancelled",
        Status::Done => "Done",
        Status::Error => "Error",
    };
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(cwd?)".into());
    let model_suffix = model.map(|m| format!(" — model:{m}")).unwrap_or_default();
    let title_full = format!("{title}{model_suffix} — [{status_str}]  {cwd}");
    let title_trim = truncate_display(&title_full, w_usize);
    let sep = "-".repeat(w_usize);
    // Header lines are plain text without embedded CR/LF; positioning is handled by the view layer.
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

    let input_prompt = if input.is_empty() {
        "> ".to_string()
    } else {
        format!("> {input}")
    };
    let input_trim = truncate_display(&input_prompt, w_usize);
    let input_line = input_trim;

    RenderPlan {
        header_lines,
        log_lines,
        input_line,
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
    pub theme: Theme, // 新規追加
    // 新規: 現在進行中のLLMレスポンスの構造化されたセグメント
    pub(crate) current_llm_response: Option<Vec<LlmResponseSegment>>,
    // 新規: LLMレスポンス解析用バッファ
    pub(crate) llm_parsing_buffer: String,
}

impl TuiApp {
    pub fn new(title: impl Into<String>, model: Option<String>, theme_name: &str) -> Self {
        // 引数を追加
        let (tx, rx) = std::sync::mpsc::channel();
        let (input_history, history_index) = load_input_history();
        let at_index = AtFileIndex::new(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        // テーマの選択ロジックを追加
        let theme = match theme_name.to_lowercase().as_str() {
            "light" => Theme::light(),
            _ => Theme::dark(), // デフォルトはdark
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
            theme,                             // 新規追加
            current_llm_response: None,        // 新規: 初期化
            llm_parsing_buffer: String::new(), // 新規: 初期化
        };
        // initial scan (blocking once at startup)
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
        // capture last segment starting with '@' up to whitespace or end
        let s = self.input.as_str();
        let mut start = None;
        for (i, ch) in s.char_indices() {
            if ch == '@' {
                start = Some(i);
            }
            if ch.is_whitespace() {
                if let Some(st) = start {
                    if i > st {
                        return Some(s[st..i].to_string());
                    } else {
                        start = None;
                    }
                }
            }
        }
        start.map(|st| s[st..].to_string())
    }

    pub fn update_completion(&mut self) {
        // If we just applied/closed completion, suppress reopening once.
        if self.compl.suppress_once {
            self.compl.suppress_once = false;
            self.compl.visible = false;
            return;
        }
        // Only trigger completion when the character immediately before the cursor is '@'.
        let s = self.input.as_str();
        let mut rev = s.chars().rev();
        let prev = rev.next();
        let prev2 = rev.next();
        let prev_char_is_at = prev == Some('@');
        if !prev_char_is_at {
            self.compl.reset();
            return;
        }
        // If this '@' starts a new token (at BOL or preceded by whitespace), start from full set by forcing query="@".
        if prev2.is_none() || prev2.map(|c| c.is_whitespace()).unwrap_or(false) {
            let tok = "@".to_string();
            self.compl.visible = true;
            self.compl.query = tok.clone();
            self.compl.items = self.at_index.complete(&tok);
            self.compl.selected = 0;
            return;
        }
        // Otherwise, build from current token content after '@'.
        if let Some(tok) = self.current_at_token() {
            if tok.starts_with('@') {
                self.compl.visible = true;
                self.compl.query = tok.clone();
                self.compl.items = self.at_index.complete(&tok);
                self.compl.selected = 0;
                return;
            }
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
            // replace current token in input with @rel
            if let Some(tok) = self.current_at_token() {
                if let Some(pos) = self.input.rfind(&tok) {
                    let mut ins = format!("@{}", item.rel);
                    if ins.contains(' ') {
                        ins = format!("@\"{}\"", item.rel);
                    }
                    self.input.replace_range(pos..pos + tok.len(), &ins);
                }
            }
            if let Ok(mut r) = self.at_index.recent.write() {
                r.touch(&item.rel);
            }
        }
        self.compl.reset();
        self.compl.suppress_once = true;
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
    // Reuse session store base dir but keep a flat file for input history
    let base = crate::session::SessionStore::new_default()
        .map(|s| s.root)
        .unwrap_or_else(|_| std::path::PathBuf::from(r"./.doge/sessions"));
    std::fs::create_dir_all(&base).ok();
    base.join(r"input_history.json")
}

fn load_input_history() -> (Vec<String>, usize) {
    let path = history_store_path();
    let s = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".into());
    let mut v: Vec<String> = serde_json::from_str(&s).unwrap_or_default();
    // cap size
    if v.len() > 1000 {
        let start = v.len() - 1000;
        v = v[start..].to_vec();
    }
    let idx = v.len();
    (v, idx)
}

pub(crate) fn save_input_history(hist: &[String]) {
    let path = history_store_path();
    // keep last 1000 entries
    let slice: Vec<&String> = hist.iter().rev().take(1000).collect();
    let out: Vec<String> = slice.into_iter().rev().cloned().collect();
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&out).unwrap_or("[]".into()),
    );
}
