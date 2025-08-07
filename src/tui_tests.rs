#[cfg(test)]
mod tests {
    use super::super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn plan_basic_ascii() {
        let title = "TITLE123456"; // longer than width
        let logs = vec!["aaa".to_string(), "bbbbbbbbbbbb".to_string(), "c".to_string()];
        let input = "hello world";
        let plan = build_render_plan(title, crate::tui::state::Status::Idle, &logs, input, 10, 8, None);
        assert_eq!(plan.header_lines[0], "TITLE12345");
        assert_eq!(plan.header_lines[1], "----------");
        assert_eq!(plan.log_lines, vec!["aaa", "bbbbbbbbbb", "c"]);
        assert_eq!(plan.input_line, "> hello wo");
    }

    #[test]
    fn plan_japanese_width() {
        let title = "日本語タイトルABCDEFG"; // includes wide chars
        let logs = vec!["あいうえおかきくけこ".to_string(), "ABCあい".to_string()];
        let input = "漢字かなABC";
        let plan = build_render_plan(title, crate::tui::state::Status::Idle, &logs, input, 10, 6, None);
        for line in plan.header_lines.iter().chain(plan.log_lines.iter()) {
            let s = line.as_str();
            assert!(UnicodeWidthStr::width(s) <= 10, "line too wide: {}", s);
        }
        assert!(UnicodeWidthStr::width(plan.input_line.as_str()) <= 10);
    }

    #[test]
    fn plan_small_terminal() {
        let title = "X";
        let logs = vec!["abc".to_string(); 10];
        let input = "y";
        let plan = build_render_plan(title, crate::tui::state::Status::Idle, &logs, input, 1, 3, None);
        for line in plan.header_lines.iter().chain(plan.log_lines.iter()) {
            let s = line.as_str();
            assert!(UnicodeWidthStr::width(s) <= 1);
        }
        assert!(UnicodeWidthStr::width(plan.input_line.as_str()) <= 1);
    }

    #[test]
    fn soft_wrap_long_line() {
        let title = "wrap";
        // 30 chars should wrap into 3 lines with width 10
        let logs = vec!["abcdefghijklmnopqrstuvwxyz1234".to_string()];
        let plan = build_render_plan(title, crate::tui::state::Status::Idle, &logs, "", 10, 8, None);
        // Expect 3 wrapped lines in log (since height allows)
        assert_eq!(plan.log_lines.len(), 3);
        for l in &plan.log_lines {
            let s = l.trim();
            assert!(UnicodeWidthStr::width(s) <= 10, "line too wide: {}", s);
        }
    }
}