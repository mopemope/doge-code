use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Streaming,
    Cancelled,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderPlan {
    pub header_lines: Vec<String>,
    pub log_lines: Vec<String>,
    pub input_line: String,
}

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

// Soft-wrap a single logical line into multiple physical lines within max width, preserving Unicode display width.
pub fn wrap_display(s: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut width = 0usize;
    for ch in s.chars() {
        let ch_w = ch.width().unwrap_or(0);
        if ch_w == 0 {
            cur.push(ch);
            continue;
        }
        if width + ch_w > max {
            lines.push(cur);
            cur = String::new();
            width = 0;
        }
        cur.push(ch);
        width += ch_w;
    }
    lines.push(cur);
    lines
}

pub fn build_render_plan(
    title: &str,
    status: Status,
    log: &[String],
    input: &str,
    w: u16,
    h: u16,
    model: Option<&str>,
) -> RenderPlan {
    let w_usize = w as usize;
    let status_str = match status {
        Status::Idle => "Idle",
        Status::Streaming => "Streaming",
        Status::Cancelled => "Cancelled",
        Status::Done => "Done",
        Status::Error => "Error",
    };
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(cwd?)".into());
    let model_suffix = model.map(|m| format!(" — model:{m}")).unwrap_or_default();
    let title_full = format!("{title}{model_suffix} — [{status_str}]  {cwd}");
    let title_trim = truncate_display(&title_full, w_usize);
    let sep = "-".repeat(w_usize);
    // Header lines are plain text without embedded CR/LF; positioning is handled by the view layer.
    let header_lines = vec![title_trim, sep];

    // Build wrapped physical lines from logs (from end to start to keep last rows)
    let max_log_rows = h.saturating_sub(3) as usize;
    let mut phys_rev: Vec<String> = Vec::new();
    for line in log.iter().rev() {
        let line = line.trim_end_matches('\n');
        let parts = wrap_display(line, w_usize);
        for p in parts.into_iter().rev() {
            phys_rev.push(p);
            if phys_rev.len() >= max_log_rows {
                break;
            }
        }
        if phys_rev.len() >= max_log_rows {
            break;
        }
    }
    phys_rev.reverse();

    let log_lines = phys_rev;

    let input_prompt = if input.is_empty() {
        "> ".to_string()
    } else {
        format!("> {input}")
    };
    let input_trim = truncate_display(&input_prompt, w_usize);
    let input_line = input_trim;

    RenderPlan {
        header_lines,
        log_lines,
        input_line,
    }
}
