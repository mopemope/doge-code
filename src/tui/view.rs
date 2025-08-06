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
}

impl TuiApp {
    pub fn new(title: impl Into<String>) -> Self {
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
        loop {
            if let Some(rx) = self.inbox_rx.as_ref() {
                let mut drained = Vec::new();
                while let Ok(msg) = rx.try_recv() {
                    drained.push(msg);
                }
                for msg in drained {
                    match msg.as_str() {
                        "::status:done" => {
                            self.status = Status::Done;
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
                        _ => self.push_log(msg),
                    }
                }
            }
            self.draw()?;
            if crossterm::event::poll(std::time::Duration::from_millis(50))? {
                match crossterm::event::read()? {
                    crossterm::event::Event::Key(k) => match k.code {
                        crossterm::event::KeyCode::Esc => {
                            self.dispatch("/cancel");
                        }
                        crossterm::event::KeyCode::Enter => {
                            let line = std::mem::take(&mut self.input);
                            if line.trim() == "/quit" {
                                return Ok(());
                            }
                            if line.starts_with("/ask ") {
                                let ts = chrono::Local::now().format("%H:%M:%S");
                                self.push_log(format!("[{ts}] --------------------------------"));
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
        if let Some(last) = self.log.last_mut() {
            last.push_str(s);
        }
    }

    fn draw(&self) -> Result<()> {
        let mut stdout = io::stdout();
        let (w, h) = terminal::size()?;
        let plan = build_render_plan(&self.title, self.status, &self.log, &self.input, w, h);
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
            if line.starts_with("\r> ") {
                queue!(stdout, SetForegroundColor(Color::Blue))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if line.contains("[Done]") || line.contains("[done]") {
                queue!(stdout, SetForegroundColor(Color::Green))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if line.contains("[Cancelled]")
                || line.contains("[cancelled]")
                || line.contains("[canceled]")
            {
                queue!(stdout, SetForegroundColor(Color::Red))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if line.starts_with("\r[") {
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
