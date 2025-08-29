use unicode_width::UnicodeWidthChar;

pub fn truncate_display(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut width = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let ch_w = ch.width().unwrap_or(0);
        if ch_w == 0 {
            out.push(ch);
            continue;
        }
        if width + ch_w > max {
            break;
        }
        out.push(ch);
        width += ch_w;
    }
    out
}

pub fn wrap_display(s: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut width = 0usize;

    for ch in s.chars() {
        let ch_w = ch.width().unwrap_or(0);

        // Handle newline characters explicitly
        if ch == '\n' {
            lines.push(cur);
            cur = String::new();
            width = 0;
            continue;
        }

        // Skip zero-width characters
        if ch_w == 0 {
            cur.push(ch);
            continue;
        }

        // If adding this character would exceed the max width, wrap the line
        if width + ch_w > max {
            lines.push(cur);
            cur = String::new();
            width = 0;
        }

        cur.push(ch);
        width += ch_w;
    }

    // Add the final line if it's not empty
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }

    lines
}
