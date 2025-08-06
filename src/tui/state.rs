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
    let model = model.unwrap_or("");
    let title_full = if model.is_empty() {
        format!("{title} — [{status_str}]  {cwd}")
    } else {
        format!("{title} — [{status_str}]  {cwd}  model:{model}")
    };
    let title_trim = truncate_display(&title_full, w_usize);
    let sep = "-".repeat(w_usize);
    let header_lines = vec![format!("\r{}\n", title_trim), format!("\r{}\n", sep)];

    let max_log_rows = h.saturating_sub(3) as usize;
    let start = log.len().saturating_sub(max_log_rows);
    let mut log_lines = Vec::new();
    for line in &log[start..] {
        let line = line.trim_end_matches('\n');
        log_lines.push(format!("\r{}\n", truncate_display(line, w_usize)));
    }

    let input_prompt = if input.is_empty() {
        "> ".to_string()
    } else {
        format!("> {input}")
    };
    let input_trim = truncate_display(&input_prompt, w_usize);
    let input_line = format!("\r{input_trim}");

    RenderPlan {
        header_lines,
        log_lines,
        input_line,
    }
}
