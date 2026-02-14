use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn truncate_display(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut width = 0usize;
    let mut out = String::new();
    for g in s.graphemes(true) {
        let g_w = UnicodeWidthStr::width(g);
        if g_w == 0 {
            out.push_str(g);
            continue;
        }
        if width + g_w > max {
            break;
        }
        out.push_str(g);
        width += g_w;
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
    let plan_list = params.plan_list;
    let w_usize = w as usize;
    let status_str = match status {
        crate::tui::state::Status::Ready => "Ready".to_string(),
        crate::tui::state::Status::Thinking => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Thinking... {}", spinner_char)
        }
        crate::tui::state::Status::Running => {
            let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spinner_char = spinner_chars[spinner_state % spinner_chars.len()];
            format!("Running... {}", spinner_char)
        }
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

    // Calculate total height from cached heights
    let total_plan_rows = if !plan_list.is_empty() {
        // "--- Plan List ---" (1) + items (N) + "-----------------" (1)
        plan_list.len() + 2
    } else {
        0
    };

    let total_log_rows: usize = params.log_heights.iter().sum();
    let total_rows = total_log_rows + total_plan_rows;

    // Calculate visible range
    let view_end_row = if scroll_state.auto_scroll || scroll_state.offset == 0 {
        total_rows
    } else {
        total_rows.saturating_sub(scroll_state.offset)
    };
    let view_start_row = view_end_row.saturating_sub(max_log_rows);

    let mut log_lines: Vec<crate::tui::style_utils::StyledLine> = Vec::new();
    let mut current_row = 0;

    // 1. Render visible Log entries
    for (i, entry) in log.iter().enumerate() {
        let height = *params.log_heights.get(i).unwrap_or(&1);
        let entry_start = current_row;
        let entry_end = current_row + height;

        // Check intersection with visible window [view_start_row, view_end_row)
        if entry_end > view_start_row && entry_start < view_end_row {
            let rendered_lines = entry.render(w_usize, params.theme);

            // Calculate slice of this entry to include
            let slice_start = view_start_row.saturating_sub(entry_start);
            let slice_end = (view_end_row.saturating_sub(entry_start)).min(height);

            if slice_start < rendered_lines.len() {
                let end = slice_end.min(rendered_lines.len());
                if slice_start < end {
                    log_lines.extend_from_slice(&rendered_lines[slice_start..end]);
                }
            }
        }
        current_row += height;
        if current_row >= view_end_row {
            break;
        }
    }

    // 2. Render visible Plan List items (if active and in view)
    if !plan_list.is_empty() && current_row < view_end_row {
        // Helper to render a plain line similar to LogEntry logic
        let render_plain = |text: String, row_idx: usize| {
            if row_idx >= view_start_row && row_idx < view_end_row {
                crate::tui::state::LogEntry::Plain(text)
                    .render(w_usize, params.theme)
                    .into_iter()
                    .next() // Assuming single line for these headers/items
            } else {
                None
            }
        };

        // Header
        let header_text = "--- Plan List ---".to_string();
        if let Some(line) = render_plain(header_text, current_row) {
            log_lines.push(line);
        }
        current_row += 1;

        // Items
        // Build parent-child map
        let mut children: std::collections::HashMap<
            Option<&String>,
            Vec<&crate::tui::state::PlanItem>,
        > = std::collections::HashMap::new();
        for item in plan_list {
            children
                .entry(item.parent_id.as_ref())
                .or_default()
                .push(item);
        }

        // Use a stack for depth-first traversal (item, depth)
        let mut stack: Vec<(&crate::tui::state::PlanItem, usize)> = Vec::new();

        // Initialize stack with root items (reverse order to process first-to-last)
        if let Some(roots) = children.get(&None) {
            for root in roots.iter().rev() {
                stack.push((root, 0));
            }
        }

        // Track visited to handle cycles or orphans safely (though orphans won't be reached here)
        let mut visited_ids = std::collections::HashSet::new();

        while let Some((item, depth)) = stack.pop() {
            if current_row >= view_end_row {
                break;
            }

            if !visited_ids.insert(&item.id) {
                continue; // Prevent infinite loops in case of cycles
            }

            let status_symbol = match item.status.as_str() {
                "pending" => "◌",
                "in_progress" => "◔",
                "completed" => "✓",
                _ => "○",
            };

            let indent = "  ".repeat(depth);

            // Wait, double indent? "  " * depth is enough?
            // The original indentation in my thought was just `indent`.
            // "{} {}" -> indent symbol content.
            // Let's use 2 spaces per level.
            let line_text = format!("{}{} {}", indent, status_symbol, item.content);

            if let Some(line) = render_plain(line_text, current_row) {
                log_lines.push(line);
            }
            current_row += 1;

            // Push children (reverse order)
            if let Some(kids) = children.get(&Some(&item.id)) {
                for kid in kids.iter().rev() {
                    stack.push((kid, depth + 1));
                }
            }
        }

        // Footer
        if current_row < view_end_row
            && let Some(line) = render_plain("-----------------".to_string(), current_row)
        {
            log_lines.push(line);
        }
        // current_row += 1; // Unused assignment
    }

    // Create scroll info
    let scroll_info = if total_rows > max_log_rows {
        let current_line = if scroll_state.auto_scroll || scroll_state.offset == 0 {
            total_rows
        } else {
            total_rows.saturating_sub(scroll_state.offset)
        };
        Some(crate::tui::state::ScrollInfo {
            current_line,
            total_lines: total_rows,
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
        // Pass an empty plan list since we've already added the items to the log
        plan_list: vec![],
    }
}
