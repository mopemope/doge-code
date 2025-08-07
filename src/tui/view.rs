use anyhow::Result;
use crossterm::{
    cursor, execute, queue,
    style::{Color, ResetColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{self, Write};

use crate::tui::state::{Status, build_render_plan};

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
    // When true, the last inserted model hint line should be removed on first stream token.
    model_hint_pending: bool,
}

impl TuiApp {
    pub fn new(title: impl Into<String>, model: Option<String>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            title: title.into(),
            input: String::new(),
            log: Vec::new(),
            handler: None,
            inbox_rx: Some(rx),
            inbox_tx: Some(tx),
            max_log_lines: 500,
            status: Status::Idle,
            model,
            model_hint_pending: false,
        }
    }

    pub fn with_handler(mut self, h: Box<dyn crate::tui::commands::CommandHandler + Send>) -> Self {
        self.handler = Some(h);
        self
    }

    pub fn sender(&self) -> Option<std::sync::mpsc::Sender<String>> {
        self.inbox_tx.clone()
    }

    pub fn push_log<S: Into<String>>(&mut self, s: S) {
        self.log.push(s.into());
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
        let mut last_ctrl_c_at: Option<std::time::Instant> = None;
        loop {
            // We cannot downcast trait object safely here; show no model hint in view and instead inject a header line from executor.
            let _model_hint: Option<&str> = None;
            if let Some(rx) = self.inbox_rx.as_ref() {
                let mut drained = Vec::new();
                while let Ok(msg) = rx.try_recv() {
                    drained.push(msg);
                }
                for msg in drained {
                    match msg.as_str() {
                        "::status:done" => {
                            self.status = Status::Done;
                            // Safety net: ensure any pending model hint is cleared.
                            if self.model_hint_pending {
                                Self::purge_model_hint(&mut self.log);
                                self.model_hint_pending = false;
                            }
                        }
                        "::status:cancelled" => {
                            self.status = Status::Cancelled;
                        }
                        "::status:streaming" => {
                            self.status = Status::Streaming;
                        }
                        "::status:error" => {
                            self.status = Status::Error;
                        }
                        _ if msg.starts_with("::model:hint:") => {
                            let payload = &msg["::model:hint:".len()..];
                            self.push_log(payload.to_string());
                            self.model_hint_pending = true;
                        }
                        _ if msg.starts_with("::append:") => {
                            if self.model_hint_pending {
                                Self::purge_model_hint(&mut self.log);
                                self.model_hint_pending = false;
                            }
                            let payload = &msg["::append:".len()..];
                            self.append_stream_token(payload);
                        }
                        _ => self.push_log(msg),
                    }
                }
            }
            let model_hint = self.model.as_deref();
            self.draw_with_model(model_hint)?;
            if crossterm::event::poll(std::time::Duration::from_millis(50))? {
                match crossterm::event::read()? {
                    crossterm::event::Event::Key(k) => match k.code {
                        // Handle Ctrl+C before generic Char(c) to avoid being shadowed
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
                        }
                        crossterm::event::KeyCode::Esc => {
                            self.dispatch("/cancel");
                        }
                        crossterm::event::KeyCode::Enter => {
                            let line = std::mem::take(&mut self.input);
                            if line.trim() == "/quit" {
                                return Ok(());
                            }
                            self.dispatch(&line);
                        }
                        crossterm::event::KeyCode::Backspace => {
                            self.input.pop();
                        }
                        crossterm::event::KeyCode::Char(c) => {
                            self.input.push(c);
                        }
                        _ => {}
                    },
                    crossterm::event::Event::Resize(_, _) => {}
                    _ => {}
                }
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

    fn draw_with_model(&self, model: Option<&str>) -> Result<()> {
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
        queue!(
            stdout,
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0)
        )?;
        if let Some(first) = plan.header_lines.first() {
            queue!(stdout, SetForegroundColor(Color::Cyan))?;
            write!(stdout, "{first}")?;
            queue!(stdout, ResetColor)?;
        }
        if let Some(second) = plan.header_lines.get(1) {
            queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
            write!(stdout, "{second}")?;
            queue!(stdout, ResetColor)?;
        }
        for line in plan.header_lines.iter().skip(2) {
            write!(stdout, "{line}")?;
        }
        for line in &plan.log_lines {
            let cmp = line.trim_start_matches('\r').trim_end_matches('\n');
            if cmp.starts_with("> ") {
                queue!(stdout, SetForegroundColor(Color::Blue))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.contains("[Done]") || cmp.contains("[done]") {
                queue!(stdout, SetForegroundColor(Color::Green))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.contains("[Cancelled]")
                || cmp.contains("[cancelled]")
                || cmp.contains("[canceled]")
            {
                queue!(stdout, SetForegroundColor(Color::Red))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.starts_with('[') {
                queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else {
                write!(stdout, "{line}")?;
            }
        }
        queue!(
            stdout,
            cursor::MoveTo(0, h.saturating_sub(1)),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        write!(stdout, "{}", plan.input_line)?;
        stdout.flush()?;
        Ok(())
    }
}

impl TuiApp {
    // Remove a transient model hint from the end of the log if present.
    fn purge_model_hint(log: &mut Vec<String>) {
        let mut removed = false;
        for _ in 0..2 {
            if let Some(last) = log.last() {
                if last.starts_with("\r[model:") || last.trim().is_empty() {
                    if last.trim().is_empty() {
                        log.pop();
                        continue;
                    }
                    log.pop();
                    removed = true;
                    break;
                } else {
                    break;
                }
            }
        }
        if removed && !log.last().map(|s| s.trim().is_empty()).unwrap_or(false) {
            log.push(String::new());
        }
    }
}
