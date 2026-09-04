//! Formatting helpers for session listings and details.
//!
//! Shared between the `dgc session` CLI subcommands and the TUI
//! `/session` command so both surfaces present consistent information.

use crate::session::data::{SessionData, SessionSummary};

/// Shorten a session ID for display (first 8 characters).
pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Format an RFC3339 timestamp as a compact local time (`MM-DD HH:MM`).
/// Falls back to the raw string when parsing fails.
pub fn format_timestamp(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| rfc3339.to_string())
}

/// Truncate a title to `max` characters, appending an ellipsis when cut.
pub fn truncate_title(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        title.to_string()
    } else {
        let cut: String = title.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut)
    }
}

fn pad_right(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

fn pad_left(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(width - len), s)
    }
}

const TITLE_WIDTH: usize = 28;
const UPDATED_WIDTH: usize = 11;
const NUM_WIDTH: usize = 8;

/// Render a list of session summaries as a compact table.
///
/// `current_id` (full or partial) is marked with a leading `*`.
pub fn format_summary_list(summaries: &[SessionSummary], current_id: Option<&str>) -> String {
    if summaries.is_empty() {
        return "No sessions found.".to_string();
    }

    let header = format!(
        "{} {} {} {} {}",
        pad_right("ID", 9),
        pad_right("TITLE", TITLE_WIDTH + 1),
        pad_right("UPDATED", UPDATED_WIDTH + 1),
        pad_left("TOKENS", NUM_WIDTH),
        pad_left("REQS", 5)
    );
    let mut out = header;
    out.push('\n');

    for summary in summaries {
        let marker = if current_id
            .is_some_and(|cur| summary.meta.id == cur || summary.meta.id.starts_with(cur))
        {
            "*"
        } else {
            " "
        };
        let id = format!("{}{}", marker, short_id(&summary.meta.id));
        let title = truncate_title(&summary.meta.title, TITLE_WIDTH);
        let updated = format_timestamp(&summary.updated_at);
        out.push_str(&format!(
            "{} {} {} {} {}\n",
            pad_right(&id, 9),
            pad_right(&title, TITLE_WIDTH + 1),
            pad_right(&updated, UPDATED_WIDTH + 1),
            pad_left(&summary.token_count.to_string(), NUM_WIDTH),
            pad_left(&summary.requests.to_string(), 5)
        ));
    }

    // Trim the trailing newline for log-friendly output.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Render detailed information about a single session.
pub fn format_detail(data: &SessionData) -> String {
    format!(
        "Session {}{}\n  Title: {}\n  Created: {}\n  Updated: {}\n  Messages: {}\n  Tokens: {}\n  Requests: {}\n  Tool calls: {}\n  Lines edited: {}\n  Changed files: {}",
        short_id(&data.meta.id),
        if data.meta.id.len() > 8 {
            format!(" ({})", data.meta.id)
        } else {
            String::new()
        },
        data.meta.title,
        format_timestamp(&data.meta.created_at),
        format_timestamp(&data.timestamp),
        data.conversation.len(),
        data.token_count,
        data.requests,
        data.tool_calls,
        data.lines_edited,
        data.changed_files.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_summary() -> SessionSummary {
        SessionSummary {
            meta: crate::session::data::SessionMeta {
                id: "0198abcd-1234-5678-9abc-def012345678".to_string(),
                created_at: "2026-09-01T10:00:00+00:00".to_string(),
                title: "Fix login bug".to_string(),
                title_is_default: false,
            },
            updated_at: "2026-09-04T10:30:00+00:00".to_string(),
            messages: 12,
            token_count: 3456,
            requests: 4,
            tool_calls: 9,
            changed_files: 2,
        }
    }

    #[test]
    fn test_short_id() {
        assert_eq!(short_id("0198abcd-1234"), "0198abcd");
        assert_eq!(short_id("short"), "short");
    }

    #[test]
    fn test_truncate_title() {
        assert_eq!(truncate_title("hello", 10), "hello");
        let long = "a".repeat(40);
        let truncated = truncate_title(&long, 10);
        assert_eq!(truncated.chars().count(), 10);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn test_format_summary_list_empty() {
        let out = format_summary_list(&[], None);
        assert_eq!(out, "No sessions found.");
    }

    #[test]
    fn test_format_summary_list_marks_current() {
        let summary = sample_summary();
        let out = format_summary_list(std::slice::from_ref(&summary), Some("0198abcd"));
        assert!(out.contains('*'), "current session should be marked");
        assert!(out.contains("0198abcd"));
        assert!(out.contains("Fix login bug"));
        assert!(out.contains("3456"));
    }

    #[test]
    fn test_format_summary_list_no_current_marker() {
        let summary = sample_summary();
        let out = format_summary_list(std::slice::from_ref(&summary), None);
        assert!(!out.contains('*'));
    }

    #[test]
    fn test_format_detail() {
        let mut data = SessionData::new();
        data.meta.id = "0198abcd-1234-5678-9abc-def012345678".to_string();
        data.meta.title = "Test session".to_string();
        data.add_conversation_entry(HashMap::new());
        let out = format_detail(&data);
        assert!(out.contains("Session 0198abcd"));
        assert!(out.contains("Test session"));
        assert!(out.contains("Messages: 1"));
        assert!(out.contains(&data.meta.id));
    }
}
