use super::shell_output::{Mailbox, SharedMailbox, ShellChunk};
use anyhow::Result;
use portable_pty::{CommandBuilder, NativePtySystem, PtyPair, PtySize, PtySystem};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

pub struct ShellSession {
    // Retain PTY ownership until the shell session is dropped.
    _pty_pair: PtyPair,
    pub writer: Box<dyn Write + Send>,
    output: SharedMailbox,
}

impl ShellSession {
    pub fn new() -> Result<Self> {
        Self::spawn(CommandBuilder::new("bash"))
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Result<Self> {
        let mut command = CommandBuilder::new("bash");
        command.arg("--noprofile");
        command.arg("--norc");
        Self::spawn(command)
    }

    fn spawn(cmd: CommandBuilder) -> Result<Self> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let _child = pair.slave.spawn_command(cmd)?;

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let output = Arc::new(Mutex::new(Mailbox::default()));
        let producer = Arc::downgrade(&output);
        // Existing PTY reader: aggregate into fixed memory rather than queuing
        // an unbounded number of UI messages. It does not own the consumer.
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                let read = reader.read(&mut buffer);
                let Some(output) = producer.upgrade() else {
                    break;
                };
                let mut output = output
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match read {
                    Ok(n) if n > 0 => output.push(&buffer[..n]),
                    _ => {
                        output.close();
                        break;
                    }
                }
            }
        });

        Ok(Self {
            _pty_pair: pair,
            writer,
            output,
        })
    }

    pub(crate) fn take_output(&self) -> ShellChunk {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
    }

    pub fn write(&mut self, data: &str) -> Result<()> {
        write!(self.writer, "{}", data)?;
        Ok(())
    }
}
