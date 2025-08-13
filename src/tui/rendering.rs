use anyhow::Result;
use crossterm::{
    cursor, queue,
    style::{ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use tracing::{debug, info}; // tracingをインポート

use crate::tui::state::{TuiApp, build_render_plan}; // import TuiApp

impl TuiApp {
    pub fn draw_with_model(&self, model: Option<&str>) -> Result<()> {
        let mut stdout = io::stdout();
        let (w, h) = terminal::size()?;
        // compute cursor char index for build_render_plan
        let cursor_idx = self.cursor;
        let plan = build_render_plan(
            &self.title,
            self.status,
            &self.log,
            &self.input,
            cursor_idx,
            w,
            h,
            model,
            self.spinner_state, // Pass spinner_state
        );

        debug!("Starting draw_with_model");
        // Log plan.log_lines for debugging
        debug!("plan.log_lines for debugging:");
        for (i, line) in plan.log_lines.iter().enumerate() {
            debug!("  [{}] '{}'", i, line);
        }

        // Draw header (2 lines)
        queue!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        if let Some(first) = plan.header_lines.first() {
            debug!(header_first_line = first, "Rendering header first line"); // デバッグログ追加
            // Check if the status is "Thinking..." and apply spinner color
            if first.contains("Thinking") {
                // "Thinking..." から "Thinking" に変更
                // Find the position of "Thinking..." and the spinner character
                if let Some(thinking_pos) = first.find("Thinking...") {
                    let before_thinking = &first[..thinking_pos];
                    let thinking_and_spinner = &first[thinking_pos..];

                    // Write the part before "Thinking..."
                    queue!(stdout, SetForegroundColor(self.theme.header_fg))?;
                    write!(stdout, "{}", before_thinking)?;

                    // Write "Thinking..." in status_idle_fg color
                    queue!(stdout, SetForegroundColor(self.theme.status_idle_fg))?;
                    write!(stdout, "Thinking...")?;

                    // Write the spinner character in spinner_fg color
                    // Get the first character after "Thinking... "
                    let spinner_part = &thinking_and_spinner["Thinking...".len()..];
                    if let Some(spinner_char) = spinner_part.chars().next() {
                        queue!(stdout, SetForegroundColor(self.theme.spinner_fg))?;
                        write!(stdout, "{}", spinner_char)?;
                    }
                    queue!(stdout, ResetColor)?;
                } else {
                    // Fallback if "Thinking..." is not found as expected
                    queue!(stdout, SetForegroundColor(self.theme.header_fg))?;
                    write!(stdout, "{}", first)?;
                    queue!(stdout, ResetColor)?;
                }
            } else {
                queue!(stdout, SetForegroundColor(self.theme.header_fg))?;
                write!(stdout, "{}", first)?;
                queue!(stdout, ResetColor)?;
            }
        }
        queue!(
            stdout,
            cursor::MoveTo(0, 1),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        if let Some(second) = plan.header_lines.get(1) {
            queue!(stdout, SetForegroundColor(self.theme.header_separator))?;
            write!(stdout, "{second}")?;
            queue!(stdout, ResetColor)?;
        }

        // Draw log area starting at row 2 up to h-2
        let start_row = 2u16;
        let max_rows = h.saturating_sub(2).saturating_sub(1); // leave one line for input
        let mut in_code_block = false; // new: flag whether inside a code block
        let mut in_llm_response_block = false; // flag to track if we are in an LLM response block

        let mut i = 0;
        while i < plan.log_lines.len() && i < max_rows as usize {
            let row = start_row + i as u16;
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            let cmp = plan.log_lines[i].as_str();

            // Check for LLM response block start marker (trimmed for flexibility)
            if cmp.trim() == "[LlmResponseStart]" {
                debug!("Found [LlmResponseStart] marker at line {}, row {}", i, row);
                info!("Detected [LlmResponseStart] marker");
                // Draw top border
                if i < max_rows as usize - 1 {
                    queue!(
                        stdout,
                        cursor::MoveTo(0, row),
                        terminal::Clear(ClearType::CurrentLine)
                    )?;
                    queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?;
                    queue!(stdout, SetBackgroundColor(self.theme.llm_response_bg))?;
                    write!(stdout, "┌")?;
                    for _ in 0..(w - 2) {
                        write!(stdout, "─")?;
                    }
                    write!(stdout, "┐")?;
                    queue!(stdout, ResetColor)?;
                    i += 1;
                    in_llm_response_block = true;
                    info!("Drew top border of LLM response box");
                } else {
                    break; // Not enough space for a box
                }
                debug!("Finished drawing top border for [LlmResponseStart]");
                continue;
            }

            // Check for LLM response block end marker (trimmed for flexibility)
            if cmp.trim() == "[LlmResponseEnd]" {
                debug!("Found [LlmResponseEnd] marker at line {}, row {}", i, row);
                info!("Detected [LlmResponseEnd] marker");
                // Draw bottom border
                if i < max_rows as usize {
                    // If we are in an LLM response block, we need to draw the bottom border on the current row
                    // and not skip to the next line
                    if in_llm_response_block {
                        queue!(
                            stdout,
                            cursor::MoveTo(0, row),
                            terminal::Clear(ClearType::CurrentLine)
                        )?;
                        queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?;
                        queue!(stdout, SetBackgroundColor(self.theme.llm_response_bg))?;
                        write!(stdout, "└")?;
                        for _ in 0..(w - 2) {
                            write!(stdout, "─")?;
                        }
                        write!(stdout, "┘")?;
                        queue!(stdout, ResetColor)?;
                        in_llm_response_block = false;
                        info!("Drew bottom border of LLM response box");
                    }
                }
                debug!("Finished drawing bottom border for [LlmResponseEnd]");
                i += 1;
                continue;
            }

            // If we are in an LLM response block, handle it
            if in_llm_response_block {
                debug!(
                    "Drawing line inside LLM response block at line {}, row {}: '{}'",
                    i, row, cmp
                );
                info!("Drawing line inside LLM response block: {}", cmp);
                // Check for special markers
                if cmp.starts_with(" [CodeBlockStart(") && cmp.ends_with(")]") {
                    in_code_block = true;
                    // Skip drawing this line
                    i += 1;
                    continue;
                } else if cmp == " [CodeBlockEnd]" {
                    in_code_block = false;
                    // Skip drawing this line
                    i += 1;
                    continue;
                }

                // If we are in a code block, draw the line with code block styling
                if in_code_block {
                    queue!(stdout, cursor::MoveTo(0, row))?;
                    queue!(stdout, terminal::Clear(ClearType::CurrentLine))?;
                    queue!(stdout, SetBackgroundColor(self.theme.llm_code_block_bg))?;
                    queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?;
                    write!(stdout, "{}", plan.log_lines[i])?;
                    queue!(stdout, ResetColor)?;
                    i += 1;
                    continue;
                }

                // Draw the line inside the box
                queue!(stdout, cursor::MoveTo(0, row))?;
                queue!(stdout, terminal::Clear(ClearType::CurrentLine))?;
                queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?;
                queue!(stdout, SetBackgroundColor(self.theme.llm_response_bg))?;
                write!(stdout, "│")?;
                write!(
                    stdout,
                    "{:<width$}",
                    plan.log_lines[i],
                    width = (w - 2) as usize
                )?;
                write!(stdout, "│")?;
                queue!(stdout, ResetColor)?;
                i += 1;
            } else {
                debug!(
                    "Drawing line outside LLM response block at line {}, row {}: '{}'",
                    i, row, cmp
                );
                // Handle non-LLM response lines (user input, info, errors, etc.)

                // Check for special markers
                if cmp.starts_with(" [CodeBlockStart(") && cmp.ends_with(")]") {
                    in_code_block = true;
                    i += 1;
                    continue;
                } else if cmp == " [CodeBlockEnd]" {
                    in_code_block = false;
                    i += 1;
                    continue;
                }

                // If in code block, draw with code block styling
                if in_code_block {
                    queue!(stdout, SetBackgroundColor(self.theme.llm_code_block_bg))?;
                    queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?;
                    write!(stdout, "{cmp}")?;
                    queue!(stdout, ResetColor)?;
                    i += 1;
                    continue;
                }

                // Draw other lines with their respective styles
                if cmp.starts_with("> ") {
                    queue!(stdout, SetForegroundColor(self.theme.user_input_fg))?;
                    write!(stdout, "{cmp}")?;
                    queue!(stdout, ResetColor)?;
                } else if cmp.contains("[Cancelled]")
                    || cmp.contains("[cancelled]")
                    || cmp.contains("[canceled]")
                {
                    queue!(stdout, SetForegroundColor(self.theme.error_fg))?;
                    write!(stdout, "{cmp}")?;
                    queue!(stdout, ResetColor)?;
                } else if cmp.starts_with('[') {
                    queue!(stdout, SetForegroundColor(self.theme.info_fg))?;
                    write!(stdout, "{cmp}")?;
                    queue!(stdout, ResetColor)?;
                } else if cmp.starts_with("LLM error:")
                    || cmp.contains("error")
                    || cmp.contains("Error")
                {
                    queue!(stdout, SetForegroundColor(self.theme.error_fg))?;
                    write!(stdout, "{cmp}")?;
                    queue!(stdout, ResetColor)?;
                } else if cmp.contains("warning") || cmp.contains("Warning") {
                    queue!(stdout, SetForegroundColor(self.theme.warning_fg))?;
                    write!(stdout, "{cmp}")?;
                    queue!(stdout, ResetColor)?;
                } else {
                    // Regular text line, draw normally
                    write!(stdout, "{cmp}")?;
                }
                i += 1;
            }
        }

        // If we're still in an LLM response block at the end, close it
        if in_llm_response_block {
            info!("Closing LLM response block at the end");
            let row = start_row + i as u16;
            if i < max_rows as usize {
                queue!(stdout, cursor::MoveTo(0, row))?;
                queue!(stdout, terminal::Clear(ClearType::CurrentLine))?;
                queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?;
                queue!(stdout, SetBackgroundColor(self.theme.llm_response_bg))?;
                write!(stdout, "└")?;
                for _ in 0..(w - 2) {
                    write!(stdout, "─")?;
                }
                write!(stdout, "┘")?;
                queue!(stdout, ResetColor)?;
                i += 1;
            }
        }

        // Clear any remaining rows in the log area if current content is shorter
        let used_rows = i as u16;
        for row in start_row + used_rows..start_row + max_rows {
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
        }
        // Clear any remaining rows in the log area if current content is shorter
        let used_rows = plan.log_lines.len() as u16;
        for row in start_row + used_rows..start_row + max_rows {
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
        }

        // Draw input line at bottom
        let input_row = h.saturating_sub(1);
        queue!(
            stdout,
            cursor::MoveTo(0, input_row),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        write!(stdout, "{}", plan.input_line)?;

        // draw completion popup above input if visible
        if self.compl.visible && !self.compl.items.is_empty() {
            let popup_h = std::cmp::min(self.compl.items.len(), 10) as u16;
            for i in 0..popup_h as usize {
                let row = input_row.saturating_sub(1 + i as u16);
                let item = &self.compl.items[i];
                let mark = if i == self.compl.selected { ">" } else { " " };
                let line = format!(
                    "{mark} {}  [{}]",
                    item.rel,
                    item.ext.clone().unwrap_or_default()
                );
                queue!(
                    stdout,
                    cursor::MoveTo(0, row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                if i == self.compl.selected {
                    queue!(
                        stdout,
                        SetBackgroundColor(self.theme.completion_selected_bg),
                        SetForegroundColor(self.theme.completion_selected_fg)
                    )?;
                } else {
                    queue!(stdout, SetForegroundColor(self.theme.completion_item_fg))?;
                }
                write!(stdout, "{line}")?;
                if i == self.compl.selected {
                    queue!(stdout, ResetColor)?;
                }
            }
        }

        // Position terminal cursor at visual column provided by plan
        let col = plan.input_cursor_col;
        queue!(stdout, cursor::MoveTo(col, input_row), cursor::Show)?;

        stdout.flush()?;
        Ok(())
    }
}
