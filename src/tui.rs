use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute, queue,
    style::{Color, ResetColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

pub trait CommandHandler {
    fn handle(&mut self, line: &str, ui: &mut TuiApp);
    fn sender(&self) -> Option<std::sync::mpsc::Sender<String>> {
        None
    }
}

pub struct TuiApp {
    pub title: String,
    pub input: String,
    pub log: Vec<String>,
    handler: Option<Box<dyn CommandHandler + Send>>, // simple callback hook
    inbox_rx: Option<std::sync::mpsc::Receiver<String>>, // background -> UI messages
    inbox_tx: Option<std::sync::mpsc::Sender<String>>, // for executors to clone
    pub max_log_lines: usize,
    pub status: Status,
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
struct RenderPlan {
    header_lines: Vec<String>,
    log_lines: Vec<String>,
    input_line: String,
}

fn build_render_plan(
    title: &str,
    status: Status,
    log: &[String],
    input: &str,
    w: u16,
    h: u16,
) -> RenderPlan {
    let w_usize = w as usize;
    fn truncate_display(s: &str, max: usize) -> String {
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
    let status_str = match status {
        Status::Idle => "Idle",
        Status::Streaming => "Streaming",
        Status::Cancelled => "Cancelled",
        Status::Done => "Done",
        Status::Error => "Error",
    };
    let title_full = format!("{title} — [{status_str}]");
    let title_trim = truncate_display(&title_full, w_usize);
    let sep = "-".repeat(w_usize);
    let header_lines = vec![format!("\r{}\n", title_trim), format!("\r{}\n", sep)];

    let max_log_rows = h.saturating_sub(3) as usize;
    let start = log.len().saturating_sub(max_log_rows);
    let mut log_lines = Vec::new();
    for line in &log[start..] {
        let line = line.trim_end_matches('\n');
        log_lines.push(format!("\r{}\n", truncate_display(line, w_usize)));
    }

    let input_prompt = if input.is_empty() {
        "> ".to_string()
    } else {
        format!("> {input}")
    };
    let input_trim = truncate_display(&input_prompt, w_usize);
    let input_line = format!("\r{input_trim}");

    RenderPlan {
        header_lines,
        log_lines,
        input_line,
    }
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

    pub fn with_handler(mut self, h: Box<dyn CommandHandler + Send>) -> Self {
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
                // Drain safely: collect first to avoid borrow conflict
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
            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(k) => match k.code {
                        KeyCode::Esc => {
                            // Map Esc to /cancel instead of quitting
                            self.dispatch("/cancel");
                        }
                        KeyCode::Enter => {
                            let line = std::mem::take(&mut self.input);
                            if line.trim() == "/quit" {
                                return Ok(());
                            }
                            if line.starts_with("/ask ") {
                                // Insert a faint separator line with timestamp
                                let ts = chrono::Local::now().format("%H:%M:%S");
                                self.push_log(format!("[{ts}] --------------------------------"));
                            }
                            self.dispatch(&line);
                        }
                        KeyCode::Backspace => {
                            self.input.pop();
                        }
                        KeyCode::Char(c) => {
                            self.input.push(c);
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
    }

    fn dispatch(&mut self, line: &str) {
        if self.handler.is_some() {
            // Avoid aliasing &mut self across trait call; take args needed and call via temporary
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
        // Header with colors
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
        // Log lines: simple heuristic coloring
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
