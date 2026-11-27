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
pub const PROTOCOL_VERSION: u32 = 3;

/// Upper bound for one newline-delimited protocol message, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 1_048_576;

/// Upper bound for human-readable summaries carried in events, in bytes.
pub const MAX_SUMMARY_BYTES: usize = 4096;

/// Upper bound for the serialized `raw` payload of a permission request, in bytes.
pub const MAX_PERMISSION_RAW_BYTES: usize = 65_536;

/// Upper bound for the text of one `assistant_text` / `user_message` event,
/// measured as JSON-escaped bytes, so that every event stays far below
/// [`MAX_MESSAGE_BYTES`]. Longer text is split into several events.
pub const MAX_EVENT_TEXT_BYTES: usize = 256 * 1024;

/// Split `text` into consecutive pieces whose JSON-escaped size is at most
/// [`MAX_EVENT_TEXT_BYTES`], on char boundaries. Empty text gives one empty
/// piece; concatenating the pieces gives `text` back.
pub fn chunk_text(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut size = 0;
    for (index, c) in text.char_indices() {
        let cost = json_escaped_len(c);
        if size + cost > MAX_EVENT_TEXT_BYTES && index > start {
            chunks.push(text[start..index].to_owned());
            start = index;
            size = 0;
        }
        size += cost;
    }
    if start < text.len() || chunks.is_empty() {
        chunks.push(text[start..].to_owned());
    }
    chunks
}

/// Bytes `c` takes inside a JSON string as serde_json writes it.
fn json_escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// Cut `text` to at most [`MAX_SUMMARY_BYTES`] bytes on a UTF-8 char
/// boundary. A cut text ends with `…(truncated, N bytes total)` (N is the
/// original length) and the result, marker included, stays within the limit.
pub fn truncate_summary(text: &str) -> String {
    if text.len() <= MAX_SUMMARY_BYTES {
        return text.to_owned();
    }
    let marker = format!("…(truncated, {} bytes total)", text.len());
    let mut end = MAX_SUMMARY_BYTES.saturating_sub(marker.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes `text` takes inside a JSON string.
    fn escaped_len(text: &str) -> usize {
        serde_json::to_string(text).expect("serialize").len() - 2
    }

    #[test]
    fn chunk_text_keeps_short_text_whole() {
        assert_eq!(chunk_text("hello"), vec!["hello".to_owned()]);
        assert_eq!(chunk_text(""), vec![String::new()]);
    }

    #[test]
    fn chunk_text_splits_long_text_on_char_boundaries() {
        let text = "aあ😀".repeat(100_000); // 800 KB
        let chunks = chunk_text(&text);
        assert!(chunks.len() >= 4, "{}", chunks.len());
        assert!(
            chunks
                .iter()
                .all(|c| escaped_len(c) <= MAX_EVENT_TEXT_BYTES)
        );
        assert!(chunks.iter().all(|c| !c.is_empty()));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunk_text_counts_json_escapes() {
        // Each control char escapes to six bytes (`\u0001`).
        let text = "\u{1}".repeat(MAX_EVENT_TEXT_BYTES);
        let chunks = chunk_text(&text);
        assert!(
            chunks
                .iter()
                .all(|c| escaped_len(c) <= MAX_EVENT_TEXT_BYTES)
        );
        assert_eq!(chunks.concat(), text);
        assert!(chunks.len() >= 6);
    }

    #[test]
    fn truncate_summary_keeps_short_text() {
        assert_eq!(truncate_summary("hello"), "hello");
    }

    #[test]
    fn truncate_summary_cuts_on_char_boundary_and_says_so() {
        // 3-byte chars: the cut must back off to a char boundary.
        let text = "あ".repeat(2000);
        let cut = truncate_summary(&text);
        assert!(cut.len() <= MAX_SUMMARY_BYTES, "{}", cut.len());
        let marker = "…(truncated, 6000 bytes total)";
        let kept = cut.strip_suffix(marker).expect("marker at the end");
        assert!(text.starts_with(kept));
        assert!(kept.len() > MAX_SUMMARY_BYTES - 64);
    }

    #[test]
    fn truncate_summary_marks_a_long_command() {
        let text = "x".repeat(10 * 1024);
        let cut = truncate_summary(&text);
        assert!(cut.len() <= MAX_SUMMARY_BYTES);
        assert!(cut.ends_with("…(truncated, 10240 bytes total)"), "{cut}");
        // Exactly at the limit nothing is cut.
        let exact = "y".repeat(MAX_SUMMARY_BYTES);
        assert_eq!(truncate_summary(&exact), exact);
    }
}
