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

/// Build a render plan. This function was moved from `state.rs` to avoid a very large
/// `state.rs` file. It references the UI state types defined in `state.rs`.
#[allow(clippy::too_many_arguments)]
pub fn build_render_plan(
    title: &str,
    status: crate::tui::state::Status,
    log: &[String],
    _textarea: &tui_textarea::TextArea,
    _input_mode: crate::tui::state::InputMode,
    w: u16,
    _h: u16, // Total height (not used directly, main_content_height is used instead)
    main_content_height: u16, // Add actual main content area height
    model: Option<&str>,
    spinner_state: usize,                          // Add spinner_state parameter
    prompt_tokens: u32,                            // prompt tokens
    _total_tokens: Option<u32>,                    // total tokens (if available)
    scroll_state: &crate::tui::state::ScrollState, // Add scroll_state parameter
    todo_list: &[crate::tui::state::TodoItem],     // Add todo_list parameter
    _repomap_status: crate::tui::state::RepomapStatus, // Add repomap_status parameter
) -> crate::tui::state::RenderPlan {
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
    let tokens_suffix = if prompt_tokens > 0 {
        format!(" - tokens:{}", prompt_tokens)
    } else {
        String::new()
    };

    let title_full = format!(
        "{}{}{} - {} - {}",
        title, model_suffix, tokens_suffix, status_str, cwd
    );
    let title_trim = truncate_display(&title_full, w_usize);
    let sep = "-".repeat(w_usize);
    let footer_lines = vec![title_trim, sep];

    // Build wrapped physical lines from logs with scroll support
    let max_log_rows = main_content_height as usize;
    let mut all_phys_lines: Vec<String> = Vec::new();

    // Build all physical lines first
    for line in log.iter() {
        let line = line.trim_end_matches('\n');
        let parts = wrap_display(line, w_usize);
        all_phys_lines.extend(parts);
    }

    // Add todo list items to the log as regular messages
    if !todo_list.is_empty() {
        // Add a separator before the todo list
        all_phys_lines.push("--- Todo List ---".to_string());

        // Add each todo item with its status symbol
        for todo in todo_list {
            let status_symbol = match todo.status.as_str() {
                "pending" => "◌",
                "in_progress" => "◔",
                "completed" => "✓",
                _ => "○",
            };
            all_phys_lines.push(format!("{} {}", status_symbol, todo.content));
        }

        // Add a separator after the todo list
        all_phys_lines.push("-----------------".to_string());
    }

    let total_lines = all_phys_lines.len();

    // Apply scroll offset
    let log_lines = if scroll_state.auto_scroll || scroll_state.offset == 0 {
        // Show the most recent lines (bottom of log)
        let start_idx = total_lines.saturating_sub(max_log_rows);
        let mut lines = all_phys_lines[start_idx..].to_vec();
        // Ensure we don't exceed the display area
        if lines.len() > max_log_rows {
            lines.truncate(max_log_rows);
        }
        lines
    } else {
        // Show lines based on scroll offset (offset 0 = most recent, higher = older)
        let end_idx = total_lines.saturating_sub(scroll_state.offset);
        let start_idx = end_idx.saturating_sub(max_log_rows);
        let mut lines = all_phys_lines[start_idx..end_idx].to_vec();
        // Ensure we don't exceed the display area
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
