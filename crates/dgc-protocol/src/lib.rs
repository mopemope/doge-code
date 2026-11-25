//! Socket protocol shared by the dgc orchestrator daemon and its clients.
//!
//! This crate holds types only and performs no I/O. See
//! `docs/superpowers/specs/2026-11-25-orchestrator-design.md` §7.

pub mod event;
pub mod ids;
pub mod methods;
pub mod model;
pub mod rpc;

/// Major version of the daemon socket protocol. Clients and daemon must agree.
pub const PROTOCOL_VERSION: u32 = 1;

/// Upper bound for one newline-delimited protocol message, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 1_048_576;

/// Upper bound for human-readable summaries carried in events, in bytes.
pub const MAX_SUMMARY_BYTES: usize = 4096;

/// Upper bound for the serialized `raw` payload of a permission request, in bytes.
pub const MAX_PERMISSION_RAW_BYTES: usize = 65_536;

/// Cut `text` to at most [`MAX_SUMMARY_BYTES`] bytes on a UTF-8 char boundary.
pub fn truncate_summary(text: &str) -> String {
    if text.len() <= MAX_SUMMARY_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_SUMMARY_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_summary_keeps_short_text() {
        assert_eq!(truncate_summary("hello"), "hello");
    }

    #[test]
    fn truncate_summary_cuts_on_char_boundary() {
        // 3-byte chars: 4096 is not a multiple of 3, so the cut must back off.
        let text = "あ".repeat(2000);
        let cut = truncate_summary(&text);
        assert!(cut.len() <= MAX_SUMMARY_BYTES);
        assert_eq!(cut.len(), 4095);
        assert!(text.starts_with(&cut));
    }
}
