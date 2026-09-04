//! Shared character-budget helpers for tool outputs.
//!
//! Tools must keep their own serialized output below the global truncation
//! caps (`src/llm/message_utils.rs`), because the global truncator is only a
//! safety net. These helpers implement the common head/tail truncation
//! patterns used across tools.

/// Default per-tool output budget in characters. Chosen so that a fully
/// serialized JSON payload (with overhead) stays safely under the 8,000-char
/// global cap.
pub const DEFAULT_TOOL_BUDGET_CHARS: usize = 6_000;

/// Characters reserved inside a budget for the truncation marker line.
const MARKER_RESERVE: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetedText {
    pub text: String,
    pub truncated: bool,
}

impl BudgetedText {
    fn unchanged(text: &str) -> Self {
        Self {
            text: text.to_string(),
            truncated: false,
        }
    }
}

/// Slice the first `n` chars of `s` on a char boundary.
pub fn safe_take_chars(s: &str, n: usize) -> &str {
    let total = s.chars().count();
    if n >= total {
        return s;
    }
    let byte_idx = s.char_indices().nth(n).map_or(s.len(), |(idx, _)| idx);
    &s[..byte_idx]
}

/// Slice the last `n` chars of `s` on a char boundary.
pub fn safe_tail_chars(s: &str, n: usize) -> &str {
    let total = s.chars().count();
    if n >= total {
        return s;
    }
    let start = total - n;
    let byte_idx = s.char_indices().nth(start).map_or(s.len(), |(idx, _)| idx);
    &s[byte_idx..]
}

/// Truncate to `budget` chars keeping the head and the tail, so both the
/// beginning of the output and final summary/error lines survive.
///
/// ```text
/// <head>\n...[output truncated: N of T chars omitted]...\n<tail>
/// ```
pub fn head_tail_truncate(s: &str, budget: usize) -> BudgetedText {
    let total = s.chars().count();
    if budget == 0 || total <= budget {
        return BudgetedText::unchanged(s);
    }

    let usable = budget.saturating_sub(MARKER_RESERVE);
    if usable == 0 {
        let head = safe_take_chars(s, budget);
        return BudgetedText {
            text: head.to_string(),
            truncated: true,
        };
    }

    let head_chars = usable * 7 / 10;
    let tail_chars = usable - head_chars;

    let head = safe_take_chars(s, head_chars);
    let tail = safe_tail_chars(s, tail_chars);
    let omitted = total - head.chars().count() - tail.chars().count();

    let text = format!(
        "{}\n...[output truncated: {} of {} chars omitted]...\n{}",
        head, omitted, total, tail
    );
    BudgetedText {
        text,
        truncated: true,
    }
}

/// Truncate to `budget` chars keeping only the head (for diffs and other
/// outputs where the beginning matters and the tail adds little).
pub fn head_truncate(s: &str, budget: usize) -> BudgetedText {
    let total = s.chars().count();
    if budget == 0 || total <= budget {
        return BudgetedText::unchanged(s);
    }
    let head = safe_take_chars(s, budget.saturating_sub(MARKER_RESERVE).max(1));
    let omitted = total - head.chars().count();
    BudgetedText {
        text: format!(
            "{}\n...[output truncated: {} of {} chars omitted]",
            head, omitted, total
        ),
        truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_take_chars_respects_char_boundary() {
        let s = "あいうえお";
        assert_eq!(safe_take_chars(s, 3), "あいう");
        assert_eq!(safe_take_chars(s, 100), s);
        assert_eq!(safe_take_chars(s, 0), "");
    }

    #[test]
    fn test_safe_tail_chars_respects_char_boundary() {
        let s = "あいうえお";
        assert_eq!(safe_tail_chars(s, 2), "えお");
        assert_eq!(safe_tail_chars(s, 100), s);
        assert_eq!(safe_tail_chars(s, 0), "");
    }

    #[test]
    fn test_head_tail_truncate_keeps_head_and_tail() {
        let mut s = String::new();
        for i in 0..200 {
            s.push_str(&format!("line-{i:04}\n"));
        }
        let result = head_tail_truncate(&s, 500);
        assert!(result.truncated);
        assert!(result.text.starts_with("line-0000"));
        assert!(result.text.trim_end().ends_with("line-0199"));
        assert!(result.text.contains("output truncated"));
        assert!(result.text.chars().count() <= 500);
    }

    #[test]
    fn test_head_tail_truncate_short_input_unchanged() {
        let s = "short";
        let result = head_tail_truncate(s, 100);
        assert!(!result.truncated);
        assert_eq!(result.text, s);
    }

    #[test]
    fn test_head_tail_truncate_multibyte() {
        let s = "あ".repeat(1000);
        let result = head_tail_truncate(&s, 300);
        assert!(result.truncated);
        assert!(result.text.chars().count() <= 300);
        assert!(result.text.starts_with("あああ"));
        assert!(result.text.ends_with("あああ"));
    }

    #[test]
    fn test_head_truncate_keeps_head_only() {
        let s = "a".repeat(1000);
        let result = head_truncate(&s, 200);
        assert!(result.truncated);
        assert!(result.text.starts_with("aaaa"));
        assert!(result.text.contains("output truncated"));
        assert!(result.text.chars().count() <= 200);
    }

    #[test]
    fn test_head_truncate_zero_budget() {
        let result = head_truncate("abc", 0);
        assert!(!result.truncated);
        assert_eq!(result.text, "abc");
    }
}
