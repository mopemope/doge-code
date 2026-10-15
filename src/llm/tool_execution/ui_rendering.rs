use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Truncates a string to a maximum width, respecting grapheme clusters.
/// Appends "..." if truncated.
pub fn truncate_string_with_graphemes(s: &str, max_width: usize) -> String {
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return s.to_string();
    }

    let mut current_width = 0;
    let mut result = String::new();
    // Reserve space for "..."
    let target_width = max_width.saturating_sub(3);

    for g in s.graphemes(true) {
        let g_width = UnicodeWidthStr::width(g);
        if current_width + g_width > target_width {
            break;
        }
        result.push_str(g);
        current_width += g_width;
    }

    result.push_str("...");
    result
}

/// UI-only failure details; model/tool JSON remains unchanged. Keep both ends
/// of long diagnostics so an exit reason cannot be buried by routine output.
pub(super) fn tool_failure_message(
    name: &str,
    summary: &str,
    value: Option<&serde_json::Value>,
) -> String {
    let mut message = format!(
        "{name}: {}",
        crate::tools::budget::head_tail_truncate(summary, 1000).text
    );
    if let Some(value) = value {
        for key in [
            "status",
            "exit_code",
            "error",
            "message",
            "stderr",
            "stdout",
        ] {
            if let Some(detail) = value.get(key).filter(|v| !v.is_null()) {
                let detail = detail
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| detail.to_string());
                if !detail.trim().is_empty() && !summary.contains(&detail) {
                    message.push_str(&format!(
                        "\n{key}: {}",
                        crate::tools::budget::head_tail_truncate(&detail, 2000).text
                    ));
                }
            }
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_graphemes_and_display_width() {
        for (input, width, expected) in [
            ("hello world", 5, "he..."),
            ("hello", 5, "hello"),
            ("hello", 10, "hello"),
            ("こんにちは", 4, "..."),
            ("こんにちは", 5, "こ..."),
            ("🦀🚀🔥", 4, "..."),
            ("🦀🚀🔥", 5, "🦀..."),
        ] {
            assert_eq!(truncate_string_with_graphemes(input, width), expected);
        }
    }

    #[test]
    fn failed_tool_keeps_cause_and_tail_of_long_diagnostics() {
        let value = serde_json::json!({"status":"policy_denied", "error":{"message":"command not allowed", "hint":"check allowed_commands"}, "stderr":format!("first error\n{}\nlast failure", "x".repeat(10_000))});
        let message = tool_failure_message("execute_process", "Process failed", Some(&value));
        for expected in [
            "execute_process",
            "policy_denied",
            "command not allowed",
            "check allowed_commands",
            "first error",
            "last failure",
        ] {
            assert!(message.contains(expected), "{message}");
        }
        assert!(message.len() < 4000);
        let error = tool_failure_message(
            "fs_read",
            &format!("first cause {} final actionable cause", "x".repeat(10_000)),
            None,
        );
        assert!(error.contains("first cause") && error.contains("final actionable cause"));
    }
}
