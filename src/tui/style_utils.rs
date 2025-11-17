use ratatui::style::{Color, Modifier, Style};
use std::mem;
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, PartialEq)]
pub struct StyledSpan {
    pub content: String,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

impl Default for StyledLine {
    fn default() -> Self {
        Self::new()
    }
}

impl StyledLine {
    pub fn new() -> Self {
        Self { spans: Vec::new() }
    }

    pub fn from_span(content: impl Into<String>, style: Style) -> Self {
        Self {
            spans: vec![StyledSpan {
                content: content.into(),
                style,
            }],
        }
    }

    pub fn text(&self) -> String {
        self.spans
            .iter()
            .map(|s| s.content.as_str())
            .collect::<String>()
    }

    pub fn prepend_margin(&mut self, margin: &str, style: Style) {
        if margin.is_empty() {
            return;
        }
        self.spans.insert(
            0,
            StyledSpan {
                content: margin.to_string(),
                style,
            },
        );
    }
}

fn flush_segment(segments: &mut Vec<StyledSpan>, buffer: &mut String, style: Style) {
    if buffer.is_empty() {
        return;
    }
    segments.push(StyledSpan {
        content: mem::take(buffer),
        style,
    });
}

pub fn wrap_segments(segments: &[StyledSpan], width: usize) -> Vec<StyledLine> {
    if width == 0 {
        return vec![StyledLine::new()];
    }

    let mut lines: Vec<StyledLine> = Vec::new();
    let mut current_segments: Vec<StyledSpan> = Vec::new();
    let mut buffer = String::new();
    let mut current_style = Style::default();
    let mut style_initialized = false;
    let mut current_width = 0usize;

    for seg in segments {
        for ch in seg.content.chars() {
            if !style_initialized {
                current_style = seg.style;
                style_initialized = true;
            }

            if seg.style != current_style {
                flush_segment(&mut current_segments, &mut buffer, current_style);
                current_style = seg.style;
            }

            if ch == '\n' {
                flush_segment(&mut current_segments, &mut buffer, current_style);
                lines.push(StyledLine {
                    spans: std::mem::take(&mut current_segments),
                });
                current_width = 0;
                style_initialized = false;
                continue;
            }

            let ch_width = ch.width().unwrap_or(0);
            if ch_width > 0 && current_width + ch_width > width && current_width > 0 {
                flush_segment(&mut current_segments, &mut buffer, current_style);
                lines.push(StyledLine {
                    spans: std::mem::take(&mut current_segments), // This takes ownership, so we need to recreate it
                });
                current_width = 0;
                style_initialized = false;
                current_style = seg.style;
            }

            buffer.push(ch);
            current_width += ch_width;
        }
    }

    flush_segment(&mut current_segments, &mut buffer, current_style);
    if !current_segments.is_empty() {
        lines.push(StyledLine {
            spans: std::mem::take(&mut current_segments),
        });
    }

    if lines.is_empty() {
        lines.push(StyledLine::new());
    }

    lines
}

pub fn apply_inline_styles(text: &str, base_style: Style, code_style: Style) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    let mut buffer = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut code = false;

    while i < chars.len() {
        let ch = chars[i];
        if !code && i + 1 < chars.len() {
            if ch == '*' && chars[i + 1] == '*' {
                flush_segment(
                    &mut spans,
                    &mut buffer,
                    compose_inline_style(base_style, bold, italic, strike, code, code_style),
                );
                bold = !bold;
                i += 2;
                continue;
            }
            if ch == '_' && chars[i + 1] == '_' {
                flush_segment(
                    &mut spans,
                    &mut buffer,
                    compose_inline_style(base_style, bold, italic, strike, code, code_style),
                );
                bold = !bold;
                i += 2;
                continue;
            }
            if ch == '~' && chars[i + 1] == '~' {
                flush_segment(
                    &mut spans,
                    &mut buffer,
                    compose_inline_style(base_style, bold, italic, strike, code, code_style),
                );
                strike = !strike;
                i += 2;
                continue;
            }
        }

        if !code && (ch == '*' || ch == '_') {
            flush_segment(
                &mut spans,
                &mut buffer,
                compose_inline_style(base_style, bold, italic, strike, code, code_style),
            );
            italic = !italic;
            i += 1;
            continue;
        }

        if ch == '`' {
            flush_segment(
                &mut spans,
                &mut buffer,
                compose_inline_style(base_style, bold, italic, strike, code, code_style),
            );
            code = !code;
            i += 1;
            continue;
        }

        buffer.push(ch);
        i += 1;
    }

    flush_segment(
        &mut spans,
        &mut buffer,
        compose_inline_style(base_style, bold, italic, strike, code, code_style),
    );

    spans
}

fn compose_inline_style(
    base: Style,
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    code_style: Style,
) -> Style {
    if code {
        return code_style;
    }

    let mut style = base;
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if strike {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

pub fn is_tool_line(line: &str) -> bool {
    // Check if line contains tool execution status markers (without exposing args/results)
    line.contains("[") && (line.contains(" => ❌") || line.contains(" => ✅"))
}

pub fn style_for_plain_line(line: &str, theme: &crate::tui::theme::Theme) -> Style {
    if line.starts_with("```") || line.trim_start().starts_with("```") {
        theme.code_block_style
    } else if line.starts_with("[shell]$") {
        Style::default().fg(Color::Yellow)
    } else if line.starts_with("[stdout]") {
        Style::default().fg(Color::White)
    } else if line.starts_with("[stderr]") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("> ") {
        Style::default().fg(Color::Cyan)
    } else if is_tool_line(line) {
        // Enhanced styling for tool executions with timestamp
        if line.contains(" => ❌") {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if line.contains(" => ✅") {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan) // For in-progress or neutral tool display
        }
    } else if line.starts_with("[tool]") {
        // Old format for backward compatibility
        if line.contains("=> ERR") {
            Style::default().fg(Color::Red)
        } else if line.contains("=> OK") {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Yellow)
        }
    } else if line.starts_with("  ") {
        theme.llm_response_style
    } else {
        theme.log_style
    }
}

pub fn render_plain_entry(
    text: &str,
    width: usize,
    theme: &crate::tui::theme::Theme,
) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    for part in text.split('\n') {
        let style = style_for_plain_line(part, theme);
        let wrapped = wrap_segments(
            &[StyledSpan {
                content: part.to_string(),
                style,
            }],
            width.max(1),
        );
        lines.extend(wrapped);
    }
    lines
}

fn parse_ordered_marker(line: &str) -> Option<(&str, &str)> {
    let chars = line.char_indices();
    let mut has_digit = false;
    for (idx, ch) in chars {
        if ch.is_ascii_digit() {
            has_digit = true;
            continue;
        }
        if ch == '.' && has_digit {
            let marker = &line[..=idx];
            let rest = line[idx + 1..].trim_start();
            return Some((marker, rest));
        }
        break;
    }
    None
}

pub fn render_markdown_entry(
    text: &str,
    width: usize,
    theme: &crate::tui::theme::Theme,
) -> Vec<StyledLine> {
    let margin = "  ";
    let margin_width = margin
        .chars()
        .map(|c| c.width().unwrap_or(0))
        .sum::<usize>();
    let available_width = width.saturating_sub(margin_width).max(1);
    let mut lines = Vec::new();
    let mut in_code_block = false;

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        let indent_count = line.chars().take_while(|c| *c == ' ').count();
        let indent_str = " ".repeat(indent_count);
        let trimmed = line[indent_count..].trim_end();

        if trimmed.is_empty() {
            let mut blank = StyledLine::new();
            blank.prepend_margin(margin, theme.llm_response_style);
            lines.push(blank);
            continue;
        }

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            let fence_style = theme.code_block_style;
            let mut block_lines = wrap_segments(
                &[StyledSpan {
                    content: trimmed.to_string(),
                    style: fence_style,
                }],
                available_width,
            );
            for line in &mut block_lines {
                line.prepend_margin(margin, fence_style);
            }
            lines.extend(block_lines);
            continue;
        }

        if in_code_block {
            let code_style = theme.code_block_style;
            let code_margin = format!("{}  {}", margin, indent_str);
            let mut block_lines = wrap_segments(
                &[StyledSpan {
                    content: trimmed.to_string(),
                    style: code_style,
                }],
                available_width.saturating_sub(indent_count + 2).max(1),
            );
            for line in &mut block_lines {
                line.prepend_margin(&code_margin, code_style);
            }
            lines.extend(block_lines);
            continue;
        }

        if let Some(stripped) = trimmed.strip_prefix('>') {
            let content = stripped.trim_start();
            let mut spans = apply_inline_styles(
                content,
                theme.llm_response_style.add_modifier(Modifier::DIM),
                theme.code_block_style,
            );
            if spans.is_empty() {
                spans.push(StyledSpan {
                    content: String::new(),
                    style: theme.llm_response_style,
                });
            }
            let mut block_lines = wrap_segments(&spans, available_width.saturating_sub(2).max(1));
            for line in &mut block_lines {
                line.prepend_margin(&format!("{}│ ", margin), theme.llm_response_style);
            }
            lines.extend(block_lines);
            continue;
        }

        let mut handled = false;

        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let bullet_style = theme.llm_response_style;
            let mut spans =
                apply_inline_styles(rest.trim_start(), bullet_style, theme.code_block_style);
            if spans.is_empty() {
                spans.push(StyledSpan {
                    content: String::new(),
                    style: bullet_style,
                });
            }
            let leading_width = indent_count + 2; // bullet + space
            let mut block_lines =
                wrap_segments(&spans, available_width.saturating_sub(leading_width).max(1));
            for (idx, line) in block_lines.iter_mut().enumerate() {
                if idx == 0 {
                    line.prepend_margin(&format!("{}{}• ", margin, indent_str), bullet_style);
                } else {
                    line.prepend_margin(&format!("{}{}  ", margin, indent_str), bullet_style);
                }
            }
            lines.extend(block_lines);
            handled = true;
        }

        if handled {
            continue;
        }

        if let Some((marker, rest)) = parse_ordered_marker(trimmed) {
            let list_style = theme.llm_response_style;
            let mut spans = apply_inline_styles(rest, list_style, theme.code_block_style);
            if spans.is_empty() {
                spans.push(StyledSpan {
                    content: String::new(),
                    style: list_style,
                });
            }
            let marker_width = marker
                .chars()
                .map(|c| c.width().unwrap_or(0))
                .sum::<usize>()
                + indent_count
                + 1;
            let mut block_lines =
                wrap_segments(&spans, available_width.saturating_sub(marker_width).max(1));
            for (idx, line) in block_lines.iter_mut().enumerate() {
                if idx == 0 {
                    line.prepend_margin(
                        &format!("{}{}{} ", margin, indent_str, marker.trim_end()),
                        list_style,
                    );
                } else {
                    line.prepend_margin(
                        &format!(
                            "{}{}{}",
                            margin,
                            indent_str,
                            " ".repeat(marker_width - indent_count)
                        ),
                        list_style,
                    );
                }
            }
            lines.extend(block_lines);
            continue;
        }

        let mut heading_level = 0usize;
        let mut heading_text = trimmed;
        for ch in trimmed.chars() {
            if ch == '#' {
                heading_level += 1;
            } else {
                break;
            }
        }
        if heading_level > 0 && trimmed.chars().nth(heading_level) == Some(' ') {
            heading_text = trimmed[heading_level..].trim_start();
        } else {
            heading_level = 0;
        }

        if heading_level > 0 {
            let mut style = theme.llm_response_style.add_modifier(Modifier::BOLD);
            style = match heading_level {
                1 => style.fg(Color::LightCyan),
                2 => style.fg(Color::LightMagenta),
                3 => style.fg(Color::LightYellow),
                _ => style,
            };
            let spans = apply_inline_styles(heading_text, style, theme.code_block_style);
            let mut block_lines = wrap_segments(&spans, available_width.max(1));
            for line in &mut block_lines {
                line.prepend_margin(margin, style);
            }
            lines.extend(block_lines);
            continue;
        }

        let paragraph_style = theme.llm_response_style;
        let spans = apply_inline_styles(trimmed, paragraph_style, theme.code_block_style);
        let mut block_lines = wrap_segments(&spans, available_width.max(1));
        for line in &mut block_lines {
            line.prepend_margin(margin, paragraph_style);
        }
        lines.extend(block_lines);
    }

    lines
}
