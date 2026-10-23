//! PTY data boundaries independent of the general UI message channel.
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
pub(crate) const MAILBOX_BYTES: usize = 64 * 1024;
pub(crate) const RETAINED_BYTES: usize = 64 * 1024;
pub(crate) const CONTEXT_BYTES: usize = 16 * 1024;

#[derive(Default)]
pub(crate) struct Mailbox {
    bytes: VecDeque<u8>,
    dropped: bool,
    eof: bool,
}
#[derive(Default)]
pub(crate) struct ShellChunk {
    pub bytes: Vec<u8>,
    pub dropped: bool,
    pub eof: bool,
}
pub(crate) type SharedMailbox = Arc<Mutex<Mailbox>>;
impl Mailbox {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        let excess = self
            .bytes
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(MAILBOX_BYTES);
        self.dropped |= excess > 0;
        if bytes.len() >= MAILBOX_BYTES {
            self.bytes.clear();
            self.bytes.extend(&bytes[bytes.len() - MAILBOX_BYTES..]);
        } else {
            self.bytes.drain(..excess);
            self.bytes.extend(bytes);
        }
    }
    pub(crate) fn close(&mut self) {
        self.eof = true;
    }
    pub(crate) fn drain(&mut self) -> ShellChunk {
        ShellChunk {
            bytes: self.bytes.drain(..).collect(),
            dropped: std::mem::take(&mut self.dropped),
            eof: std::mem::take(&mut self.eof),
        }
    }
}
#[derive(Default)]
pub(crate) struct ShellOutput {
    text: String,
    truncated: bool,
    decoder: Utf8Decoder,
}
impl ShellOutput {
    pub(crate) fn feed(&mut self, chunk: ShellChunk) {
        self.truncated |= chunk.dropped;
        if chunk.dropped {
            self.decoder.pending.clear();
        }
        self.text
            .push_str(&self.decoder.decode(&chunk.bytes, chunk.eof));
        if self.text.len() > RETAINED_BYTES {
            let start = tail_start(&self.text, RETAINED_BYTES);
            self.text.drain(..start);
            self.truncated = true;
        }
    }
    pub(crate) fn context(&self) -> String {
        const NOTICE: &str = "[shell output truncated]\n";
        let lines = self
            .text
            .lines()
            .rev()
            .take(50)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        let clipped =
            self.truncated || lines.len() > CONTEXT_BYTES || self.text.lines().count() > 50;
        let budget = CONTEXT_BYTES - if clipped { NOTICE.len() } else { 0 };
        let start = tail_start(&lines, budget);
        format!("{}{}", if clipped { NOTICE } else { "" }, &lines[start..])
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}
impl Utf8Decoder {
    fn decode(&mut self, bytes: &[u8], eof: bool) -> String {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let mut rest = input.as_slice();
        let mut text = String::new();
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(&String::from_utf8_lossy(&rest[..valid]));
                    rest = &rest[valid..];
                    if let Some(invalid) = error.error_len() {
                        text.push('�');
                        rest = &rest[invalid..];
                    } else {
                        // A valid UTF-8 prefix can leave at most three bytes.
                        self.pending.extend_from_slice(rest);
                        break;
                    }
                }
            }
        }
        if eof && !self.pending.is_empty() {
            text.push('�');
            self.pending.clear();
        }
        text
    }
}

fn tail_start(text: &str, limit: usize) -> usize {
    let mut start = text.len().saturating_sub(limit);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

impl crate::tui::state::TuiApp {
    pub(crate) fn poll_shell_output(&mut self) {
        if let Some(session) = &self.shell_session {
            let chunk = session.take_output();
            if !chunk.bytes.is_empty() || chunk.dropped || chunk.eof {
                self.shell_output_buffer.feed(chunk);
                self.dirty = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_flood_is_bounded_at_transport_retention_and_context() {
        let mut mailbox = Mailbox::default();
        for _ in 0..2560 {
            mailbox.push(&[b'x'; 4096]);
        }
        let chunk = mailbox.drain();
        assert!(chunk.bytes.len() <= MAILBOX_BYTES);
        assert!(chunk.dropped);
        let mut output = ShellOutput::default();
        output.feed(chunk);
        assert!(output.text.len() <= RETAINED_BYTES);
        let context = output.context();
        assert!(context.len() <= CONTEXT_BYTES);
        assert!(context.contains("truncated"));
        assert!(context.ends_with("xxxx"));
        assert!(mailbox.drain().bytes.is_empty());
    }
    #[test]
    fn repeated_drains_keep_bounded_unicode_tail_and_fifty_lines() {
        let mut output = ShellOutput::default();
        for _ in 0..100 {
            output.feed(ShellChunk {
                bytes: "日本語\n".repeat(1000).into_bytes(),
                ..Default::default()
            });
        }
        assert!(output.text.len() <= RETAINED_BYTES);
        assert!(output.context().len() <= CONTEXT_BYTES);
        assert!(output.context().lines().count() <= 51); // notice + fifty retained lines
        assert!(output.context().ends_with("日本語"));
    }
}

#[cfg(test)]
mod utf8_regressions {
    use super::*;
    #[test]
    fn every_utf8_split_and_eof_preserve_valid_text() {
        let text = "日本語🙂\u{1b}[31m赤\u{1b}[0m";
        for split in 0..=text.len() {
            let mut output = ShellOutput::default();
            output.feed(ShellChunk {
                bytes: text.as_bytes()[..split].to_vec(),
                ..Default::default()
            });
            output.feed(ShellChunk {
                bytes: text.as_bytes()[split..].to_vec(),
                eof: true,
                ..Default::default()
            });
            assert_eq!(output.text, text, "split={split}");
        }
        let mut output = ShellOutput::default();
        for byte in text.bytes() {
            output.feed(ShellChunk {
                bytes: vec![byte],
                ..Default::default()
            });
        }
        output.feed(ShellChunk {
            eof: true,
            ..Default::default()
        });
        assert_eq!(output.text, text);
    }
    #[test]
    fn invalid_bytes_incomplete_eof_and_dropped_prefix_are_explicit() {
        let mut output = ShellOutput::default();
        output.feed(ShellChunk {
            bytes: vec![b'a', 0xff, 0xe6],
            ..Default::default()
        });
        assert_eq!(output.text, "a�");
        output.feed(ShellChunk {
            eof: true,
            ..Default::default()
        });
        assert_eq!(output.text, "a��");
        let mut output = ShellOutput::default();
        output.feed(ShellChunk {
            bytes: vec![0xe6],
            ..Default::default()
        });
        output.feed(ShellChunk {
            bytes: b"suffix".to_vec(),
            dropped: true,
            eof: true,
        });
        assert_eq!(output.text, "suffix");
        assert!(output.context().starts_with("[shell output truncated]"));
    }
}
