use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, ClearType},
};
use std::io::{self, Write};

pub struct TuiApp {
    pub title: String,
    pub input: String,
    pub log: Vec<String>,
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
        let mut stdout = io::stdout();
        terminal::enable_raw_mode()?;
        execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
        let res = self.event_loop();
        execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show)?;
        terminal::disable_raw_mode()?;
        res
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

    pub fn append_stream_token(&mut self, s: &str) {
        if let Some(last) = self.log.last_mut() {
            last.push_str(s);
        }
    }

    fn draw(&self) -> Result<()> {
        let mut stdout = io::stdout();
        let (w, h) = terminal::size()?;
        execute!(
            stdout,
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0)
        )?;

        // Header
        writeln!(stdout, "{}", self.title)?;
        writeln!(stdout, "{}", "─".repeat(w as usize))?;

        // Log area
        let max_log_rows = h.saturating_sub(3) as usize; // header(2) + input(1)
        let start = self.log.len().saturating_sub(max_log_rows);
        for line in &self.log[start..] {
            let mut s = line.clone();
            if s.len() > (w as usize) {
                s.truncate(w as usize);
            }
            writeln!(stdout, "{s}")?;
        }

        // Input line
        execute!(stdout, cursor::MoveTo(0, h.saturating_sub(1)))?;
        write!(stdout, "> {}", self.input)?;
        stdout.flush()?;
        Ok(())
    }
}
