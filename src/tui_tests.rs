#[cfg(test)]
mod tests {
    use super::super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn plan_basic_ascii() {
        let title = "TITLE123456"; // longer than width
        let logs = vec!["aaa".to_string(), "bbbbbbbbbbbb".to_string(), "c".to_string()];
        let input = "hello world";
        let plan = build_render_plan(title, &logs, input, 10, 8);
        assert_eq!(plan.header_lines[0], "\rTITLE12345\n");
        assert_eq!(plan.header_lines[1], "\r----------\n");
        assert_eq!(plan.log_lines, vec!["\raaa\n", "\rbbbbbbbbbb\n", "\rc\n"]);
        assert_eq!(plan.input_line, "\r> hello wo");
    }

    #[test]
    fn plan_japanese_width() {
        let title = "日本語タイトルABCDEFG"; // includes wide chars
        let logs = vec!["あいうえおかきくけこ".to_string(), "ABCあい".to_string()];
        let input = "漢字かなABC";
        let plan = build_render_plan(title, &logs, input, 10, 6);
        for line in plan.header_lines.iter().chain(plan.log_lines.iter()) {
            let s = line.trim(); // remove CR/LF
            assert!(UnicodeWidthStr::width(s) <= 10, "line too wide: {}", s);
        }
        let input_s = plan.input_line.trim_start_matches('\r');
        assert!(UnicodeWidthStr::width(input_s) <= 10);
    }

    #[test]
    fn plan_small_terminal() {
        let title = "X";
        let logs = vec!["abc".to_string(); 10];
        let input = "y";
        let plan = build_render_plan(title, &logs, input, 1, 3);
        for line in plan.header_lines.iter().chain(plan.log_lines.iter()) {
            let s = line.trim();
            assert!(UnicodeWidthStr::width(s) <= 1);
        }
        let input_s = plan.input_line.trim_start_matches('\r');
        assert!(UnicodeWidthStr::width(input_s) <= 1);
    }
}
