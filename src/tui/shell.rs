use anyhow::Result;
use portable_pty::{CommandBuilder, NativePtySystem, PtyPair, PtySize, PtySystem};
use std::io::{Read, Write};
use std::sync::mpsc::Sender;

pub struct ShellSession {
    pub pty_pair: PtyPair,
    pub writer: Box<dyn Write + Send>,
}

impl ShellSession {
    pub fn new(tx: Sender<String>) -> Result<Self> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let cmd = CommandBuilder::new("bash");
        let _child = pair.slave.spawn_command(cmd)?;

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(n) if n > 0 => {
                        // We use base64 encoding to send bytes safely through the String channel
                        // This avoids UTF-8 corruption of partial ANSI sequences
                        use base64::{Engine as _, engine::general_purpose};
                        let encoded = general_purpose::STANDARD.encode(&buffer[..n]);
                        if tx.send(format!("::shell_output_bin:{}", encoded)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => break,  // EOF
                    Err(_) => break, // Error
                }
            }
        });

        Ok(Self {
            pty_pair: pair,
            writer,
        })
    }

    pub fn write(&mut self, data: &str) -> Result<()> {
        write!(self.writer, "{}", data)?;
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.pty_pair.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }
}
