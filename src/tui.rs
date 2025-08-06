use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute, queue,
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

pub struct TuiApp {
    pub title: String,
    pub input: String,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderPlan {
    header_lines: Vec<String>,
    log_lines: Vec<String>,
    input_line: String,
}

fn build_render_plan(title: &str, log: &[String], input: &str, w: u16, h: u16) -> RenderPlan {
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
    let title_trim = truncate_display(title, w_usize);
    let sep = "-".repeat(w_usize);
    let header_lines = vec![format!("\r{}\n", title_trim), format!("\r{}\n", sep)];

    let max_log_rows = h.saturating_sub(3) as usize;
    let start = log.len().saturating_sub(max_log_rows);
    let mut log_lines = Vec::new();
    for line in &log[start..] {
        log_lines.push(format!("\r{}\n", truncate_display(line, w_usize)));
    }

    let input_trim = truncate_display(&format!("> {input}"), w_usize);
    let input_line = format!("\r{input_trim}");

    RenderPlan {
        header_lines,
        log_lines,
        input_line,
    }
}

impl TuiApp {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            input: String::new(),
            log: Vec::new(),
        }
    }

    pub fn push_log<S: Into<String>>(&mut self, s: S) {
        self.log.push(s.into());
    }

    pub fn run(&mut self) -> Result<()> {
        struct TuiGuard;
        impl Drop for TuiGuard {
            fn drop(&mut self) {
                let mut stdout = io::stdout();
                // Best-effort cleanup; ignore errors to avoid panic in Drop
                let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
                let _ = terminal::disable_raw_mode();
            }
        }

        let mut stdout = io::stdout();
        terminal::enable_raw_mode()?;
        execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
        // Ensure cleanup even on early returns or panics
        let _guard = TuiGuard;

        self.event_loop()
    }

    fn event_loop(&mut self) -> Result<()> {
        loop {
            self.draw()?;
            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(k) => match k.code {
                        KeyCode::Esc => return Ok(()),
                        KeyCode::Enter => {
                            let line = std::mem::take(&mut self.input);
                            if line.trim() == "/quit" {
                                return Ok(());
                            }
                            self.handle_command(line);
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

    fn handle_command(&mut self, line: String) {
        if let Some(rest) = line.strip_prefix("/ask ") {
            self.push_log(format!("> {rest}"));
            // Start a placeholder streaming session line and append tokens
            self.push_log(String::new()); // new line for streaming output
        // Here we'd trigger the LLM stream via a callback; for now, just simulate in UI layer
        } else {
            self.push_log(format!("> {line}"));
        }
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
        let plan = build_render_plan(&self.title, &self.log, &self.input, w, h);
        // Queue all drawing operations to reduce syscalls and flicker
        queue!(
            stdout,
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0)
        )?;

        // Header and logs
        for line in &plan.header_lines {
            write!(stdout, "{line}")?;
        }
        for line in &plan.log_lines {
            write!(stdout, "{line}")?;
        }

        // Input line: move to bottom, clear, then write with CR
        queue!(
            stdout,
            cursor::MoveTo(0, h.saturating_sub(1)),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        write!(stdout, "{}", plan.input_line)?;

        // Finally flush once
        stdout.flush()?;
        Ok(())
    }
}
