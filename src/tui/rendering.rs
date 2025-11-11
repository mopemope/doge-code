use crate::tui::diff_review::DiffLineKind;
use crate::tui::state::{RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use std::fmt::Write;
// use tracing::debug;

impl TuiApp {
    pub fn view(&mut self, f: &mut Frame, model: Option<&str>) {
        let size = f.area();

        // Clear full frame to avoid ghost artifacts when layout/content changes.
        f.render_widget(Clear, size);

        //debug!("Screen size: {}x{}", size.width, size.height);

        // Adjust layout for multi-line input area
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header
                Constraint::Min(1),    // Main content
                Constraint::Length(1), // Status Footer (single line)
                Constraint::Length(5), // Input area (increased to 5 for 3 visible lines)
            ])
            .split(size);

        let main_content_height = chunks[1].height;
        // debug!(
        //     "Layout chunks: header={}x{}, main={}x{}, footer={}x{}",
        //     chunks[0].width,
        //     chunks[0].height,
        //     chunks[1].width,
        //     chunks[1].height,
        //     chunks[2].width,
        //     chunks[2].height
        // );
        // debug!("Main content area height: {}", main_content_height);
        // debug!("Total log lines: {}", self.log.len());

        let params = crate::tui::state::BuildRenderPlanParams {
            title: &self.title,
            status: self.status,
            log: &self.log,
            width: size.width,
            main_content_height,
            model,
            spinner_state: self.spinner_state,
            scroll_state: &self.scroll_state,
            todo_list: &self.todo_list,
            theme: &self.theme,
        };
        let plan = build_render_plan(params);

        // debug!(
        //     "Render plan: log_lines={}, scroll_info={:?}",
        //     plan.log_lines.len(),
        //     plan.scroll_info
        // );

        self.render_header(f, chunks[0], &plan, &self.theme);
        self.render_main_content(f, chunks[1], &plan, &self.theme);

        self.render_status_footer(f, chunks[2], &self.theme);
        self.render_input_area(f, chunks[3]);

        // The cursor is now handled by the TextArea widget, so no need to set it manually.
    }

    fn render_header(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        // Clear header area fully to avoid artifacts when content shrinks.
        f.render_widget(Clear, area);

        let header_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let mut title_text = if !plan.footer_lines.is_empty() {
            plan.footer_lines[0].clone()
        } else {
            String::new()
        };

        // Add scroll indicator to title if scrolling
        if let Some(scroll_info) = &plan.scroll_info {
            if scroll_info.is_scrolling {
                let mut scroll_indicator = format!(
                    " [SCROLL: {}/{}]",
                    scroll_info.current_line, scroll_info.total_lines
                );
                if scroll_info.new_messages > 0 {
                    scroll_indicator.push_str(&format!(" (+{})", scroll_info.new_messages));
                }
                title_text.push_str(&scroll_indicator);
            } else if scroll_info.total_lines > 0 {
                // Show total lines even when not scrolling if there's content
                let lines_indicator = format!(" [{}L]", scroll_info.total_lines);
                title_text.push_str(&lines_indicator);
            }
        }

        let title = Paragraph::new(title_text)
            .style(theme.footer_style)
            .alignment(Alignment::Left);
        f.render_widget(title, header_chunks[0]);

        let separator_text = if plan.footer_lines.len() > 1 {
            plan.footer_lines[1].clone()
        } else {
            "-".repeat(area.width as usize)
        };
        let separator = Paragraph::new(separator_text).style(theme.footer_style);
        f.render_widget(separator, header_chunks[1]);

        // Ensure the entire header area is filled to prevent artifacts
        if area.height > 2 {
            // If header area is larger than our content (shouldn't happen with current layout but be safe)
            let blank_lines_needed = area.height - 2;
            if blank_lines_needed > 0 {
                let blank_lines: Vec<Line> =
                    (0..blank_lines_needed).map(|_| Line::raw(" ")).collect();

                let blank_paragraph = Paragraph::new(blank_lines).style(theme.footer_style);

                let blank_area = Rect {
                    x: area.x,
                    y: area.y + 2,
                    width: area.width,
                    height: blank_lines_needed,
                };
                f.render_widget(blank_paragraph, blank_area);
            }
        }
    }

    fn render_main_content(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        // Always clear the main content area before drawing any mode-specific content.
        // This avoids artifacts when switching between log, diff review, and session list views.
        f.render_widget(Clear, area);

        if self.input_mode == crate::tui::state::InputMode::SessionList
            && let Some(session_list_state) = &self.session_list_state
        {
            self.render_session_list(f, area, session_list_state, theme);
        } else if self.diff_review.is_some() {
            // For diff review mode, we use a horizontal split
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(area);

            // Render log panel on the left
            self.render_log_panel(f, columns[0], plan, theme);
            // Render diff review on the right
            self.render_diff_review(f, columns[1], theme);
        } else {
            // For normal mode, use the full area for the log panel
            // Ensure that if we were previously in diff review mode, the right-side area is cleared
            // by explicitly rendering the log panel on the entire area
            self.render_log_panel(f, area, plan, theme);
        }
    }

    fn render_log_panel(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        // Clear log panel area to prevent artifacts when content height or layout changes
        f.render_widget(Clear, area);

        // Create paragraph with the content lines
        let lines: Vec<Line> = plan
            .log_lines
            .iter()
            .map(|styled_line| {
                let spans: Vec<Span> = styled_line
                    .spans
                    .iter()
                    .map(|segment| Span::styled(segment.content.clone(), segment.style))
                    .collect();
                if spans.is_empty() {
                    Line::raw("")
                } else {
                    Line::from(spans)
                }
            })
            .collect();

        let paragraph = Paragraph::new(lines.clone())
            .style(theme.log_style)
            .block(Block::default());
        f.render_widget(paragraph, area);

        // After rendering content, fill any remaining area with blank lines to ensure
        // complete coverage and prevent artifacts from previous renders
        // We'll render this as a separate widget to ensure it clears properly
        if lines.len() < area.height as usize {
            let blank_lines_needed = area.height as usize - lines.len();
            let blank_lines: Vec<Line> = (0..blank_lines_needed).map(|_| Line::raw(" ")).collect();

            let blank_paragraph = Paragraph::new(blank_lines)
                .style(Style::default().bg(theme.log_style.bg.unwrap_or(Color::Reset)))
                .block(Block::default());

            // Position the blank area starting just after our content
            let blank_area = Rect {
                x: area.x,
                y: area.y + lines.len() as u16,
                width: area.width,
                height: blank_lines_needed as u16,
            };
            f.render_widget(blank_paragraph, blank_area);
        }
    }

    fn render_diff_review(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let Some(review) = &self.diff_review else {
            return;
        };

        // Ensure diff review area is clean when toggling on/off.
        f.render_widget(Clear, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area);

        // file list
        let items: Vec<ListItem> = review
            .files
            .iter()
            .enumerate()
            .map(|(idx, file)| {
                let mut label = file.path.clone();
                let additions = file
                    .lines
                    .iter()
                    .filter(|line| matches!(line.kind, DiffLineKind::Addition))
                    .count();
                let removals = file
                    .lines
                    .iter()
                    .filter(|line| matches!(line.kind, DiffLineKind::Removal))
                    .count();
                if additions > 0 || removals > 0 {
                    label.push_str(&format!(" (+{}/-{})", additions, removals));
                }

                let style = if idx == review.selected {
                    theme.completion_selected_style
                } else {
                    theme.completion_style
                };

                ListItem::new(label).style(style)
            })
            .collect();

        let files_block = Block::default()
            .borders(Borders::ALL)
            .title("Changed Files (←/→ to focus)");
        let files_list = List::new(items).block(files_block);
        f.render_widget(files_list, layout[0]);

        // diff content
        let diff_block = Block::default()
            .borders(Borders::ALL)
            .title("Diff Preview (↑/↓ scroll)");

        if let Some(file) = review.files.get(review.selected) {
            let diff_lines: Vec<Line> = file
                .lines
                .iter()
                .map(|diff_line| {
                    let style = match diff_line.kind {
                        DiffLineKind::Header => Style::default().fg(Color::Cyan),
                        DiffLineKind::FileMeta => Style::default().fg(Color::Magenta),
                        DiffLineKind::HunkHeader => Style::default().fg(Color::Yellow),
                        DiffLineKind::Addition => Style::default().fg(Color::Green),
                        DiffLineKind::Removal => Style::default().fg(Color::Red),
                        DiffLineKind::Context => Style::default().fg(Color::DarkGray),
                        DiffLineKind::Other => Style::default(),
                    };
                    Line::from(Span::styled(diff_line.content.clone(), style))
                })
                .collect();

            let scroll = file.scroll.min(u16::MAX as usize) as u16;
            let paragraph = Paragraph::new(diff_lines)
                .block(diff_block)
                .scroll((scroll, 0));
            f.render_widget(paragraph, layout[1]);
        } else {
            let diff_lines: Vec<Line> = vec![Line::raw("No diff available")];

            let paragraph = Paragraph::new(diff_lines)
                .block(diff_block)
                .style(theme.log_style);
            f.render_widget(paragraph, layout[1]);

            // Fill remaining space with blank lines to prevent artifacts
            if 1 < layout[1].height as usize {
                let blank_lines_needed = (layout[1].height as usize - 1).max(0);
                let blank_lines: Vec<Line> =
                    (0..blank_lines_needed).map(|_| Line::raw(" ")).collect();

                let blank_paragraph = Paragraph::new(blank_lines)
                    .style(Style::default().bg(theme.log_style.bg.unwrap_or(Color::Reset)))
                    .block(Block::default());

                let blank_area = Rect {
                    x: layout[1].x,
                    y: layout[1].y + 1,
                    width: layout[1].width,
                    height: blank_lines_needed as u16,
                };
                f.render_widget(blank_paragraph, blank_area);
            }
        }

        // instructions footer for diff
        let instructions = Paragraph::new(
            "Review changes: ↑/↓ scroll, PgUp/PgDn fast, ←/→ file, a accept, r reject, q dismiss",
        )
        .style(theme.footer_style)
        .block(Block::default().borders(Borders::ALL));
        f.render_widget(instructions, layout[2]);
    }

    fn render_session_list(
        &self,
        f: &mut Frame,
        area: Rect,
        session_list_state: &crate::tui::state::SessionListState,
        theme: &Theme,
    ) {
        // Clear dedicated session list area to avoid overlapping with previous content.
        f.render_widget(Clear, area);

        let items: Vec<ListItem> = session_list_state
            .sessions
            .iter()
            .enumerate()
            .map(|(i, session)| {
                let content = format!(
                    "{} ({}) - Created: {}",
                    session.title, session.id, session.created_at
                );
                let style = if i == session_list_state.selected_index {
                    theme.completion_selected_style
                } else {
                    theme.completion_style
                };
                ListItem::new(content).style(style)
            })
            .collect();

        let list =
            List::new(items.clone())
                .block(Block::default().borders(Borders::ALL).title(
                    "Sessions (↑↓ to navigate, Enter to switch, d to delete, q/ESC to close)",
                ))
                .highlight_style(theme.completion_selected_style)
                .highlight_symbol(">> ");

        f.render_widget(list, area);

        // Fill any remaining space with blank lines to prevent artifacts
        let list_height = items.len();
        let border_height = 2; // top and bottom border
        if list_height + border_height < area.height as usize {
            let blank_lines_needed = (area.height as usize - list_height - border_height).max(0);
            if blank_lines_needed > 0 {
                let blank_lines: Vec<Line> =
                    (0..blank_lines_needed).map(|_| Line::raw(" ")).collect();

                let blank_paragraph = Paragraph::new(blank_lines)
                    .style(Style::default().bg(theme.log_style.bg.unwrap_or(Color::Reset)));

                let blank_area = Rect {
                    x: area.x + 1,                       // account for left border
                    y: area.y + list_height as u16 + 1,  // account for items and top border
                    width: area.width.saturating_sub(2), // account for left and right borders
                    height: blank_lines_needed as u16,
                };
                f.render_widget(blank_paragraph, blank_area);
            }
        }
    }

    fn render_input_area(&mut self, f: &mut Frame, area: Rect) {
        // Clear input area fully to avoid artifacts when input shrinks or mode changes.
        f.render_widget(Clear, area);

        let input_style = if self.input_mode == crate::tui::state::InputMode::Shell {
            self.theme.shell_input_style
        } else {
            self.theme.input_style
        };

        self.textarea.set_style(input_style);

        if self.status == crate::tui::state::Status::ShellCommandRunning {
            self.textarea.set_style(input_style.fg(Color::DarkGray));
        }

        // Set the block title based on the input mode
        let block_title = if self.input_mode == crate::tui::state::InputMode::Shell {
            "Input (Shell Mode - Press ESC to exit)"
        } else {
            "Input"
        };

        self.textarea
            .set_block(Block::default().borders(Borders::ALL).title(block_title));

        f.render_widget(&self.textarea, area);

        if self.completion_active && !self.completion_candidates.is_empty() {
            let max_width = self
                .completion_candidates
                .iter()
                .map(|s| s.len())
                .max()
                .unwrap_or(10) as u16;
            let max_display_items = 20; // Limit the number of displayed completion items
            let list_height = (self.completion_candidates.len() as u16).min(max_display_items);

            // Calculate the height of the list block including borders and title
            // Borders: top (1) + bottom (1) = 2
            // Title: 1 line
            let block_height = 2 + 1; // 3 lines
            let total_height = list_height + block_height;

            // Position the popup above the input area, considering the full height of the block
            let popup_y = area.y.saturating_sub(total_height);

            let completion_area = Rect {
                x: area.x,
                y: popup_y,
                width: max_width + 2, // +2 for padding
                height: total_height,
            };

            let items: Vec<ListItem> = self
                .completion_candidates
                .iter()
                .skip(self.completion_scroll)
                .take(max_display_items as usize)
                .enumerate()
                .map(|(i, candidate)| {
                    let style = if (i + self.completion_scroll) == self.completion_index {
                        self.theme.completion_selected_style
                    } else {
                        self.theme.completion_style
                    };
                    ListItem::new(candidate.as_str()).style(style)
                })
                .collect();

            let title = match self.completion_type {
                crate::tui::state::CompletionType::Command => "Commands",
                crate::tui::state::CompletionType::FilePath => "Files",
                _ => "Completion", // Fallback, should not happen if completion_active is true
            };

            let list =
                List::new(items.clone()).block(Block::default().borders(Borders::ALL).title(title));

            f.render_widget(Clear, completion_area); // Clear the area behind the popup
            f.render_widget(list, completion_area);

            // Fill any remaining space in the completion popup with blank items to prevent artifacts
            let num_items = items.len();
            let max_displayable = completion_area.height.saturating_sub(2) as usize; // account for borders
            if num_items < max_displayable {
                let blank_items_needed = max_displayable - num_items;
                if blank_items_needed > 0 {
                    let blank_items: Vec<ListItem> = (0..blank_items_needed)
                        .map(|_| ListItem::new(" "))
                        .collect();

                    let blank_list = List::new(blank_items).block(Block::default());

                    let blank_area = Rect {
                        x: completion_area.x + 1,                       // account for left border
                        y: completion_area.y + 1 + num_items as u16, // account for title and items
                        width: completion_area.width.saturating_sub(2), // account for borders
                        height: blank_items_needed as u16,
                    };
                    f.render_widget(blank_list, blank_area);
                }
            }
        }
    }

    fn render_status_footer(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        // Clear footer area to prevent artifacts when content changes
        f.render_widget(Clear, area);

        // Single line footer: combine version, tokens, repomap, mode, elapsed
        let mut footer_text = String::with_capacity(150);
        footer_text.push_str("v0.1.0 | ");

        // Token usage
        write!(footer_text, "Prompt: {} ", self.tokens_prompt_used).unwrap();
        if let Some(total) = self.tokens_total_used {
            write!(footer_text, "Total: {} | ", total).unwrap();
        } else {
            footer_text.push_str("Total: N/A | ");
        }

        // Repomap status
        let repomap_str = match self.repomap_status {
            crate::tui::state::RepomapStatus::NotStarted => "NotStarted",
            crate::tui::state::RepomapStatus::Building => "Building",
            crate::tui::state::RepomapStatus::Ready => "Ready",
            crate::tui::state::RepomapStatus::Error => "Error",
        };
        footer_text.push_str(&format!("Repomap: {} | ", repomap_str));

        // Input mode
        let mode_str = match self.input_mode {
            crate::tui::state::InputMode::Normal => "Normal",
            crate::tui::state::InputMode::Shell => "Shell",
            crate::tui::state::InputMode::SessionList => "SessionList",
        };
        footer_text.push_str(&format!("Mode: {} | ", mode_str));

        // Elapsed time
        if let Some(start_time) = self.processing_start_time {
            let elapsed = start_time.elapsed();
            let elapsed_secs = elapsed.as_secs();
            let hours = elapsed_secs / 3600;
            let minutes = (elapsed_secs % 3600) / 60;
            let seconds = elapsed_secs % 60;
            let elapsed_str = format!("{:02}:{:02}:{:02}", hours, minutes, seconds);
            footer_text.push_str(&format!("Elapsed: {} ", elapsed_str));
        } else if let Some(last_elapsed) = &self.last_elapsed_time {
            footer_text.push_str(&format!("Elapsed: {} ", last_elapsed));
        }

        // Ensure the text is padded to fill the full width to prevent artifacts
        let max_width = area.width as usize;
        if footer_text.len() < max_width {
            footer_text.extend(std::iter::repeat_n(' ', max_width - footer_text.len()));
        } else if footer_text.len() > max_width {
            footer_text.truncate(max_width);
        }

        let footer_paragraph = Paragraph::new(footer_text)
            .style(theme.footer_style)
            .alignment(Alignment::Left);
        f.render_widget(footer_paragraph, area);

        // If area is taller than our content, fill remaining space
        if area.height > 1 {
            let blank_lines_needed = area.height - 1;
            if blank_lines_needed > 0 {
                let blank_lines: Vec<Line> =
                    (0..blank_lines_needed).map(|_| Line::raw(" ")).collect();

                let blank_paragraph = Paragraph::new(blank_lines)
                    .style(Style::default().bg(theme.footer_style.bg.unwrap_or(Color::Reset)));

                let blank_area = Rect {
                    x: area.x,
                    y: area.y + 1,
                    width: area.width,
                    height: blank_lines_needed,
                };
                f.render_widget(blank_paragraph, blank_area);
            }
        }
    }
}
