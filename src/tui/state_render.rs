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

/// Build a render plan. This function was moved from `state.rs` to avoid a very large
/// `state.rs` file. It references the UI state types defined in `state.rs`.
pub fn build_render_plan(
    params: crate::tui::state::BuildRenderPlanParams,
) -> crate::tui::state::RenderPlan {
    let title = params.title;
    let status = params.status;
    let log = params.log;
    let w = params.width;
    let main_content_height = params.main_content_height;
    let model = params.model;
    let spinner_state = params.spinner_state;
    let scroll_state = params.scroll_state;
    let todo_list = params.todo_list;
    let w_usize = w as usize;
    let status_str = match status {
        crate::tui::state::Status::Idle => "Ready".to_string(),
        crate::tui::state::Status::Preparing => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Preparing request... {}", spinner_char)
        }
        crate::tui::state::Status::Sending => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Sending request... {}", spinner_char)
        }
        crate::tui::state::Status::Waiting => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!(
                "Waiting for response... {} (Press Esc to cancel)",
                spinner_char
            )
        }
        crate::tui::state::Status::Streaming => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Receiving response... {}", spinner_char)
        }
        crate::tui::state::Status::Processing => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Processing tools... {}", spinner_char)
        }
        crate::tui::state::Status::ShellCommandRunning => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Executing command... {}", spinner_char)
        }
        crate::tui::state::Status::Cancelled => "Cancelled".to_string(),
        crate::tui::state::Status::Done => "Done".to_string(),
        crate::tui::state::Status::Error => "Error".to_string(),
    };

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(cwd?)".into());
    let model_suffix = model.map(|m| format!(" - model:{}", m)).unwrap_or_default();

    let title_full = format!("{}{} - {} - {}", title, model_suffix, status_str, cwd);
    let title_trim = truncate_display(&title_full, w_usize);
    let sep = "-".repeat(w_usize);
    let footer_lines = vec![title_trim, sep];

    // Build wrapped physical lines from logs with scroll support
    let max_log_rows = main_content_height as usize;
    let mut all_phys_lines: Vec<crate::tui::state::StyledLine> = Vec::new();

    for entry in log.iter() {
        all_phys_lines.extend(entry.render(w_usize, params.theme));
    }

    if !todo_list.is_empty() {
        all_phys_lines.extend(
            crate::tui::state::LogEntry::Plain("--- Todo List ---".to_string())
                .render(w_usize, params.theme),
        );

        for todo in todo_list {
            let status_symbol = match todo.status.as_str() {
                "pending" => "◌",
                "in_progress" => "◔",
                "completed" => "✓",
                _ => "○",
            };
            let line = format!("{} {}", status_symbol, todo.content);
            all_phys_lines
                .extend(crate::tui::state::LogEntry::Plain(line).render(w_usize, params.theme));
        }

        all_phys_lines.extend(
            crate::tui::state::LogEntry::Plain("-----------------".to_string())
                .render(w_usize, params.theme),
        );
    }

    let total_lines = all_phys_lines.len();

    // Apply scroll offset
    let log_lines = if scroll_state.auto_scroll || scroll_state.offset == 0 {
        let start_idx = total_lines.saturating_sub(max_log_rows);
        let mut lines = all_phys_lines[start_idx..].to_vec();
        if lines.len() > max_log_rows {
            lines.truncate(max_log_rows);
        }
        lines
    } else {
        let end_idx = total_lines.saturating_sub(scroll_state.offset);
        let start_idx = end_idx.saturating_sub(max_log_rows);
        let mut lines = all_phys_lines[start_idx..end_idx].to_vec();
        if lines.len() > max_log_rows {
            lines.truncate(max_log_rows);
        }
        lines
    };

    // Create scroll info
    let scroll_info = if total_lines > max_log_rows {
        let current_line = if scroll_state.auto_scroll || scroll_state.offset == 0 {
            total_lines
        } else {
            total_lines.saturating_sub(scroll_state.offset)
        };
        Some(crate::tui::state::ScrollInfo {
            current_line,
            total_lines,
            is_scrolling: !scroll_state.auto_scroll && scroll_state.offset > 0,
            new_messages: scroll_state.new_messages,
        })
    } else {
        None
    };

    // The new `ratatui-textarea` handles its own rendering, so we don't need complex logic here.
    // We just pass an empty string for now, as the rendering part will handle the widget.
    let input_line = String::new();
    let input_cursor_col = 0;

    crate::tui::state::RenderPlan {
        footer_lines,
        log_lines,
        input_line,
        input_cursor_col,
        scroll_info,
        // Pass an empty todo list since we've already added the items to the log
        todo_list: vec![],
    }
}
