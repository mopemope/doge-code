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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate_string_with_graphemes("hello world", 5), "he...");
        assert_eq!(truncate_string_with_graphemes("hello", 5), "hello");
        assert_eq!(truncate_string_with_graphemes("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_unicode() {
        // "こんにちは" (width 10)
        assert_eq!(truncate_string_with_graphemes("こんにちは", 4), "..."); // Only fit nothing + ...
        assert_eq!(truncate_string_with_graphemes("こんにちは", 5), "こ..."); // "こ" is width 2. 2 < 5-3=2. next "ん" is width 2. 2+2=4 > 2? wait. target=2. current=0. +2=2 <= 2. push. next +2=4 > 2. break. -> "こ..."

        // precise check:
        // target = 5 - 3 = 2.
        // "こ" (2) -> current 2. OK.
        // "ん" (2) -> current 4 > 2. Break.
        // Result: "こ..."
    }

    #[test]
    fn test_truncate_emoji() {
        let s = "🦀🚀🔥"; // width 2+2+2 = 6
        assert_eq!(truncate_string_with_graphemes(s, 4), "..."); // target 1. 🦀(2)>1. break. -> ...
        assert_eq!(truncate_string_with_graphemes(s, 5), "🦀..."); // target 2. 🦀(2)<=2. push. 🚀(2)+2=4>2. break. -> 🦀...
    }
}
