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
                            let line = std::mem::take(&mut self.input);
                            if line.trim() == "/quit" {
                                return Ok(());
                            }
                            self.dispatch(&line);
                            dirty = true;
                        }
                        crossterm::event::KeyCode::Backspace => {
                            self.input.pop();
                            dirty = true;
                        }
                        crossterm::event::KeyCode::Char(c) => {
                            self.input.push(c);
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

        // Draw header (2 lines)
        queue!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        if let Some(first) = plan.header_lines.first() {
            queue!(stdout, SetForegroundColor(Color::Cyan))?;
            write!(stdout, "{first}")?;
            queue!(stdout, ResetColor)?;
        }
        queue!(
            stdout,
            cursor::MoveTo(0, 1),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        if let Some(second) = plan.header_lines.get(1) {
            queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
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

        stdout.flush()?;
        Ok(())
    }
}

impl TuiApp {}
