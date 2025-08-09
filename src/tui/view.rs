use anyhow::Result;
use crossterm::{
    cursor, execute, queue,
    style::{ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

use crate::tui::Theme;
use crate::tui::completion::{AtFileIndex, CompletionState};
use crate::tui::state::{Status, build_render_plan}; // 新規追加

pub struct TuiApp {
    pub title: String,
    pub input: String,
    pub log: Vec<String>,
    pub(crate) handler: Option<Box<dyn crate::tui::commands::CommandHandler + Send>>,
    pub(crate) inbox_rx: Option<std::sync::mpsc::Receiver<String>>,
    pub(crate) inbox_tx: Option<std::sync::mpsc::Sender<String>>,
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
            theme, // 新規追加
        };
        // initial scan (blocking once at startup)
        app.at_index.scan();
        app
    }

    pub fn with_handler(mut self, h: Box<dyn crate::tui::commands::CommandHandler + Send>) -> Self {
        self.handler = Some(h);
        self
    }

    pub fn sender(&self) -> Option<std::sync::mpsc::Sender<String>> {
        self.inbox_tx.clone()
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

    fn event_loop(&mut self) -> Result<()> {
        // Sync history index to end if out of range (e.g., after loading)
        if self.history_index > self.input_history.len() {
            self.history_index = self.input_history.len();
        }
        let mut last_ctrl_c_at: Option<std::time::Instant> = None;
        let mut dirty = true; // initial full render
        loop {
            // Drain inbox; mark dirty on any state change
            if let Some(rx) = self.inbox_rx.as_ref() {
                let mut drained = Vec::new();
                while let Ok(msg) = rx.try_recv() {
                    drained.push(msg);
                }
                for msg in drained {
                    match msg.as_str() {
                        "::status:done" => {
                            self.status = Status::Done;
                            dirty = true;
                        }
                        "::status:cancelled" => {
                            self.status = Status::Cancelled;
                            dirty = true;
                        }
                        "::status:streaming" => {
                            self.status = Status::Streaming;
                            dirty = true;
                        }
                        "::status:error" => {
                            self.status = Status::Error;
                            dirty = true;
                        }

                        _ if msg.starts_with("::append:") => {
                            let payload = &msg["::append:".len()..];
                            self.append_stream_token(payload);
                            dirty = true;
                        }
                        _ => {
                            self.push_log(msg);
                            dirty = true;
                        }
                    }
                }
            }

            if crossterm::event::poll(std::time::Duration::from_millis(50))? {
                match crossterm::event::read()? {
                    crossterm::event::Event::Key(k) => match k.code {
                        crossterm::event::KeyCode::Char('c')
                            if k.modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            let now = std::time::Instant::now();
                            if let Some(prev) = last_ctrl_c_at {
                                if now.duration_since(prev) <= std::time::Duration::from_secs(3) {
                                    return Ok(());
                                }
                            }
                            last_ctrl_c_at = Some(now);
                            self.dispatch("/cancel");
                            self.push_log("[Press Ctrl+C again within 3s to exit]");
                            dirty = true;
                        }
                        crossterm::event::KeyCode::Esc => {
                            self.dispatch("/cancel");
                            dirty = true;
                        }
                        crossterm::event::KeyCode::Enter => {
                            if self.compl.visible {
                                self.apply_completion();
                                dirty = true;
                                continue;
                            }
                            let line = std::mem::take(&mut self.input);
                            // record history if non-empty and not duplicate of last
                            if !line.trim().is_empty() {
                                if self.input_history.last().map(|s| s.as_str())
                                    != Some(line.as_str())
                                {
                                    self.input_history.push(line.clone());
                                    save_input_history(&self.input_history);
                                }
                                self.history_index = self.input_history.len();
                                self.draft.clear();
                            }
                            if line.trim() == "/quit" {
                                return Ok(());
                            }
                            self.dispatch(&line);
                            dirty = true;
                        }
                        crossterm::event::KeyCode::Backspace => {
                            self.input.pop();
                            if self.history_index == self.input_history.len() {
                                self.draft = self.input.clone();
                            }
                            self.update_completion();
                            dirty = true;
                        }
                        crossterm::event::KeyCode::Up => {
                            if self.compl.visible {
                                if !self.compl.items.is_empty() {
                                    self.compl.selected =
                                        (self.compl.selected + 1) % self.compl.items.len();
                                    dirty = true;
                                }
                            } else if self.history_index > 0 {
                                if self.history_index == self.input_history.len() {
                                    self.draft = self.input.clone();
                                }
                                self.history_index -= 1;
                                self.input = self.input_history[self.history_index].clone();
                                dirty = true;
                            }
                        }
                        crossterm::event::KeyCode::Down => {
                            if self.compl.visible {
                                if !self.compl.items.is_empty() {
                                    if self.compl.selected == 0 {
                                        self.compl.selected = self.compl.items.len() - 1;
                                    } else {
                                        self.compl.selected -= 1;
                                    }
                                    dirty = true;
                                }
                            } else if self.history_index < self.input_history.len() {
                                self.history_index += 1;
                                if self.history_index == self.input_history.len() {
                                    self.input = self.draft.clone();
                                } else {
                                    self.input = self.input_history[self.history_index].clone();
                                }
                                dirty = true;
                            }
                        }
                        crossterm::event::KeyCode::Char(c) => {
                            // If completion popup is visible and space is pressed, close the popup and suppress reopening once.
                            if c == ' ' && self.compl.visible {
                                self.compl.reset();
                                self.compl.suppress_once = true;
                                self.input.push(c);
                                if self.history_index == self.input_history.len() {
                                    self.draft = self.input.clone();
                                }
                                dirty = true;
                                continue;
                            }
                            self.input.push(c);
                            // If user typed a new '@', enable completion again regardless of previous suppression.
                            if c == '@' {
                                self.compl.suppress_once = false;
                            }
                            if self.history_index == self.input_history.len() {
                                self.draft = self.input.clone();
                            }
                            self.update_completion();
                            dirty = true;
                        }
                        _ => {}
                    },
                    crossterm::event::Event::Resize(_, _) => {
                        dirty = true;
                    }
                    _ => {}
                }
            }

            if dirty {
                self.draw_with_model(self.model.as_deref())?;
                dirty = false;
            }
        }
    }

    fn dispatch(&mut self, line: &str) {
        if self.handler.is_some() {
            let mut handler = self.handler.take().unwrap();
            handler.handle(line, self);
            self.handler = Some(handler);
            return;
        }
        self.push_log(format!("> {line}"));
    }

    #[allow(dead_code)]
    pub fn append_stream_token(&mut self, s: &str) {
        // Normalize incoming token: split by '\n' and append as multiple logical lines if needed.
        let parts: Vec<&str> = s.split('\n').collect();
        if parts.is_empty() {
            return;
        }
        if let Some(last) = self.log.last_mut() {
            last.push_str(parts[0]);
        }
        for seg in parts.iter().skip(1) {
            self.log.push((*seg).to_string());
        }
    }

    pub fn draw_with_model(&self, model: Option<&str>) -> Result<()> {
        let mut stdout = io::stdout();
        let (w, h) = terminal::size()?;
        let plan = build_render_plan(
            &self.title,
            self.status,
            &self.log,
            &self.input,
            w,
            h,
            model,
        );

        // Draw header (2 lines)
        queue!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        if let Some(first) = plan.header_lines.first() {
            queue!(stdout, SetForegroundColor(self.theme.header_fg))?;
            write!(stdout, "{first}")?;
            queue!(stdout, ResetColor)?;
        }
        queue!(
            stdout,
            cursor::MoveTo(0, 1),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        if let Some(second) = plan.header_lines.get(1) {
            queue!(stdout, SetForegroundColor(self.theme.header_separator))?;
            write!(stdout, "{second}")?;
            queue!(stdout, ResetColor)?;
        }

        // Draw log area starting at row 2 up to h-2
        let start_row = 2u16;
        let max_rows = h.saturating_sub(2).saturating_sub(1); // leave one line for input
        for (i, line) in plan.log_lines.iter().take(max_rows as usize).enumerate() {
            let row = start_row + i as u16;
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            let cmp = line.as_str();
            if cmp.starts_with("> ") {
                queue!(stdout, SetForegroundColor(self.theme.user_input_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.contains("[Cancelled]")
                || cmp.contains("[cancelled]")
                || cmp.contains("[canceled]")
            {
                queue!(stdout, SetForegroundColor(self.theme.error_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.starts_with('[') {
                queue!(stdout, SetForegroundColor(self.theme.info_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.starts_with("LLM error:")
                || cmp.contains("error")
                || cmp.contains("Error")
            {
                queue!(stdout, SetForegroundColor(self.theme.error_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.contains("warning") || cmp.contains("Warning") {
                queue!(stdout, SetForegroundColor(self.theme.warning_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else {
                // LLM response lines: darker grey/black background with white foreground for contrast
                queue!(stdout, SetBackgroundColor(self.theme.llm_response_bg))?;
                queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            }
        }
        // Clear any remaining rows in the log area if current content is shorter
        let used_rows = plan.log_lines.len() as u16;
        for row in start_row + used_rows..start_row + max_rows {
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
        }

        // Draw input line at bottom
        let input_row = h.saturating_sub(1);
        queue!(
            stdout,
            cursor::MoveTo(0, input_row),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        write!(stdout, "{}", plan.input_line)?;

        // draw completion popup above input if visible
        if self.compl.visible && !self.compl.items.is_empty() {
            let popup_h = std::cmp::min(self.compl.items.len(), 10) as u16;
            for i in 0..popup_h as usize {
                let row = input_row.saturating_sub(1 + i as u16);
                let item = &self.compl.items[i];
                let mark = if i == self.compl.selected { ">" } else { " " };
                let line = format!(
                    "{mark} {}  [{}]",
                    item.rel,
                    item.ext.clone().unwrap_or_default()
                );
                queue!(
                    stdout,
                    cursor::MoveTo(0, row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                if i == self.compl.selected {
                    queue!(
                        stdout,
                        SetBackgroundColor(self.theme.completion_selected_bg),
                        SetForegroundColor(self.theme.completion_selected_fg)
                    )?;
                } else {
                    queue!(stdout, SetForegroundColor(self.theme.completion_item_fg))?;
                }
                write!(stdout, "{line}")?;
                if i == self.compl.selected {
                    queue!(stdout, ResetColor)?;
                }
            }
        }

        // Position terminal cursor at visual end of input line using unicode width
        let col = UnicodeWidthStr::width(plan.input_line.as_str()) as u16;
        queue!(stdout, cursor::MoveTo(col, input_row), cursor::Show)?;

        stdout.flush()?;
        Ok(())
    }
}

impl TuiApp {
    fn current_at_token(&self) -> Option<String> {
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
    fn update_completion(&mut self) {
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
    fn apply_completion(&mut self) {
        if !self.compl.visible {
            return;
        }
        if let Some(item) = self.compl.items.get(self.compl.selected).cloned() {
            // replace current token in input with @rel
            if let Some(tok) = self.current_at_token() {
                if let Some(pos) = self.input.rfind(&tok) {
                    let mut ins = format!("@{}", item.rel);
                    if ins.contains(' ') {
                        ins = format!("@'{}'", item.rel);
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
}

fn history_store_path() -> std::path::PathBuf {
    // Reuse session store base dir but keep a flat file for input history
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
    // cap size
    if v.len() > 1000 {
        let start = v.len() - 1000;
        v = v[start..].to_vec();
    }
    let idx = v.len();
    (v, idx)
}

fn save_input_history(hist: &[String]) {
    let path = history_store_path();
    // keep last 1000 entries
    let slice: Vec<&String> = hist.iter().rev().take(1000).collect();
    let out: Vec<String> = slice.into_iter().rev().cloned().collect();
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&out).unwrap_or("[]".into()),
    );
}
