use crate::tui::diff_review::DiffLineKind;
use crate::tui::state::{RenderPlan, TuiApp, build_render_plan};
use crate::tui::theme::Theme;
use ansi_to_tui::IntoText;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use std::fmt::Write;
// use tracing::debug;

impl TuiApp {
    pub fn view(&mut self, f: &mut Frame, model: Option<&str>) {
        let size = f.area();

        // Apply theme background to entire screen
        let background_block = Block::default().style(self.theme.background_style);
        f.render_widget(background_block, size);

        // Use cyberpunk-specific layout if theme is cyberpunk
        if self.theme.name == "cyberpunk" {
            self.view_cyberpunk(f, model);
            return;
        }

        // Standard layout for other themes
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header
                Constraint::Min(1),    // Main content
                Constraint::Length(1), // Status Footer
                Constraint::Length(5), // Input area
            ])
            .split(size);

        let main_content_height = chunks[1].height;

        if self.window_width != size.width as usize {
            self.window_width = size.width as usize;
            self.recalculate_all_heights();
        }

        let params = crate::tui::state::BuildRenderPlanParams {
            title: &self.title,
            status: self.status,
            log: &self.log,
            width: size.width,
            main_content_height,
            model,
            spinner_state: self.spinner_state,
            scroll_state: &self.scroll_state,
            plan_list: &self.plan_list,
            theme: &self.theme,
            log_heights: &self.log_heights,
        };
        let plan = build_render_plan(params);

        self.render_header(f, chunks[0], &plan, &self.theme);
        self.render_main_content(f, chunks[1], &plan, &self.theme);
        self.render_status_footer(f, chunks[2], &self.theme);
        self.render_input_area(f, chunks[3]);
    }

    /// Cyberpunk-specific layout - Cyberdeck HUD style
    fn view_cyberpunk(&mut self, f: &mut Frame, model: Option<&str>) {
        let size = f.area();

        // Layout: Title bar, Status bar, Main content, System bar, Input
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Title bar (minimal)
                Constraint::Length(2), // Status indicators
                Constraint::Min(1),    // Main content
                Constraint::Length(2), // System status bar
                Constraint::Length(5), // Input area
            ])
            .split(size);

        let main_content_height = main_chunks[2].height;

        if self.window_width != size.width as usize {
            self.window_width = size.width as usize;
            self.recalculate_all_heights();
        }

        let params = crate::tui::state::BuildRenderPlanParams {
            title: &self.title,
            status: self.status,
            log: &self.log,
            width: size.width,
            main_content_height,
            model,
            spinner_state: self.spinner_state,
            scroll_state: &self.scroll_state,
            plan_list: &self.plan_list,
            theme: &self.theme,
            log_heights: &self.log_heights,
        };
        let plan = build_render_plan(params);

        // Render cyberdeck components
        self.render_cyber_title_bar(f, main_chunks[0], &self.theme);
        self.render_cyber_status_bar(f, main_chunks[1], &plan, &self.theme);
        self.render_main_content(f, main_chunks[2], &plan, &self.theme);
        self.render_cyber_system_bar(f, main_chunks[3], &self.theme);
        self.render_input_area(f, main_chunks[4]);
    }

    /// Minimal title bar with animated glitch-style decoration
    fn render_cyber_title_bar(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let width = area.width as usize;

        // Animated glitch blocks - cycle through different patterns
        let glitch_patterns = [
            "▓▓▓ DOGE//CODE ▓▓▓",
            "▓░▓ DOGE//CODE ▓░▓",
            "░▓░ DOGE//CODE ░▓░",
            "▓▓░ DOGE//CODE ░▓▓",
        ];
        let pattern_idx = (self.spinner_state / 3) % glitch_patterns.len();
        let title = glitch_patterns[pattern_idx];

        // Animated status indicator
        let status_frames = match self.status {
            crate::tui::state::Status::Idle => {
                ["◇ STANDBY ◇", "◈ STANDBY ◈", "◆ STANDBY ◆", "◈ STANDBY ◈"]
            }
            crate::tui::state::Status::Streaming => [
                "◇ STREAMING ◇",
                "◈ STREAMING ◈",
                "◆ STREAMING ◆",
                "◈ STREAMING ◈",
            ],
            crate::tui::state::Status::Processing => [
                "◇ PROCESSING ◇",
                "◈ PROCESSING ◈",
                "◆ PROCESSING ◆",
                "◈ PROCESSING ◈",
            ],
            crate::tui::state::Status::Waiting => {
                ["◇ WAITING ◇", "◈ WAITING ◈", "◆ WAITING ◆", "◈ WAITING ◈"]
            }
            _ => ["◇ ACTIVE ◇", "◈ ACTIVE ◈", "◆ ACTIVE ◆", "◈ ACTIVE ◈"],
        };
        let status_idx = (self.spinner_state / 2) % status_frames.len();
        let status_indicator = status_frames[status_idx];

        // Calculate padding
        let content_len = title.chars().count() + status_indicator.chars().count() + 4;
        let padding = width.saturating_sub(content_len);
        let left_pad = " ".to_string();
        let mid_pad = " ".repeat(padding);

        let line = format!("{}{}{}{} ", left_pad, title, mid_pad, status_indicator);

        let para = Paragraph::new(line).style(theme.title_style);
        f.render_widget(para, area);
    }

    /// Status bar with metrics and progress indicators
    fn render_cyber_status_bar(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        // Line 1: Animated scanline separator
        let sep_width = area.width as usize;
        let scan_pos = self.spinner_state % sep_width;
        let mut separator = String::with_capacity(sep_width * 3);
        for i in 0..sep_width {
            if i == scan_pos || i == (scan_pos + 1) % sep_width {
                separator.push('█');
            } else if i == (scan_pos + sep_width - 1) % sep_width {
                separator.push('▓');
            } else {
                separator.push('═');
            }
        }
        let sep_para = Paragraph::new(separator).style(theme.border_style);
        f.render_widget(sep_para, chunks[0]);

        // Line 2: Status indicators
        let scroll_info = if let Some(si) = &plan.scroll_info {
            format!("▸ LINES:{}/{}", si.current_line, si.total_lines)
        } else {
            "▸ LINES:0/0".to_string()
        };

        let tokens = format!("▸ TOKENS:{}", self.tokens_prompt_used);

        // Context usage progress bar (if available)
        let context_bar = if let (Some(remaining), Some(cfg)) =
            (self.remaining_context_tokens, self.cfg.as_ref())
        {
            if let Some(window) = cfg.get_context_window_size() {
                let used = window.saturating_sub(remaining);
                let pct = (used as f32 / window as f32 * 100.0) as u32;
                let filled = (pct / 10) as usize;
                let empty = 10 - filled;
                format!(
                    "▸ CTX:[{}{}] {}%",
                    "█".repeat(filled),
                    "░".repeat(empty),
                    pct
                )
            } else {
                "▸ CTX:[░░░░░░░░░░]".to_string()
            }
        } else {
            "▸ CTX:[░░░░░░░░░░]".to_string()
        };

        let elapsed = if let Some(start) = self.processing_start_time {
            format!("▸ T+{:.1}s", start.elapsed().as_secs_f32())
        } else if let Some(ref last) = self.last_elapsed_time {
            format!("▸ T:{}", last)
        } else {
            String::new()
        };

        let status_line = format!(
            " {} │ {} │ {} {}",
            scroll_info, tokens, context_bar, elapsed
        );
        let status_para = Paragraph::new(status_line).style(theme.footer_style);
        f.render_widget(status_para, chunks[1]);
    }

    /// System status bar at bottom
    fn render_cyber_system_bar(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        // Line 1: Animated scanline separator (moves opposite direction)
        let sep_width = area.width as usize;
        let scan_pos = (sep_width.saturating_sub(self.spinner_state % sep_width)) % sep_width;
        let mut separator = String::with_capacity(sep_width * 3);
        for i in 0..sep_width {
            if i == scan_pos || i == (scan_pos + 1) % sep_width {
                separator.push('█');
            } else if i == (scan_pos + 2) % sep_width {
                separator.push('▓');
            } else {
                separator.push('═');
            }
        }
        let sep_para = Paragraph::new(separator).style(theme.border_style);
        f.render_widget(sep_para, chunks[0]);

        // Line 2: System indicators with pulsing effect
        // Pulse indicator cycles through different symbols
        let pulse_icons = ['◉', '◎', '○', '◎'];
        let pulse_idx = (self.spinner_state / 2) % pulse_icons.len();
        let pulse_char = pulse_icons[pulse_idx];

        let uplink = format!("{} NEURAL-LINK:ACTIVE", pulse_char);

        let map_status = match self.repomap_status {
            crate::tui::state::RepomapStatus::NotStarted => format!("{} MAP:INIT", pulse_char),
            crate::tui::state::RepomapStatus::Building => format!("{} MAP:SYNC", pulse_char),
            crate::tui::state::RepomapStatus::Ready => "◉ MAP:READY".to_string(),
            crate::tui::state::RepomapStatus::Error => "◉ MAP:ERROR".to_string(),
        };

        let mode = match self.input_mode {
            crate::tui::state::InputMode::Normal => "◉ INPUT:NORMAL",
            crate::tui::state::InputMode::Shell => "◉ INPUT:SHELL",
            crate::tui::state::InputMode::SessionList => "◉ INPUT:SESSION",
            crate::tui::state::InputMode::HistorySearch => "◉ INPUT:HISTORY",
            crate::tui::state::InputMode::FileSearch => "◉ INPUT:FILES",
        };

        // Model indicator
        let model_info = if let Some(cfg) = &self.cfg {
            format!(
                "◉ MODEL:{}",
                cfg.model.split('/').next_back().unwrap_or(&cfg.model)
            )
        } else {
            "◉ MODEL:N/A".to_string()
        };

        let system_line = format!(" {} │ {} │ {} │ {}", uplink, map_status, mode, model_info);
        let system_para = Paragraph::new(system_line).style(theme.footer_style);
        f.render_widget(system_para, chunks[1]);
    }

    fn render_header(&self, f: &mut Frame, area: Rect, plan: &RenderPlan, theme: &Theme) {
        // Clear header area fully
        f.render_widget(Clear, area);

        // Cyberpunk Theme Special Header
        if theme.name == "cyberpunk" {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(40),
                    Constraint::Percentage(30),
                ])
                .split(area);

            // Left: Title/System
            let title_text = format!(" SYSTEM_ONLINE // {}", self.title.to_uppercase());
            let title = Paragraph::new(title_text)
                .style(theme.footer_style.add_modifier(Modifier::BOLD))
                .block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_type(theme.border_type)
                        .border_style(theme.border_style),
                );
            f.render_widget(title, chunks[0]);

            // Center: Scroll Info / Activity
            let center_text = if let Some(scroll_info) = &plan.scroll_info {
                format!(
                    "DATA_STREAM: {}/{}",
                    scroll_info.current_line, scroll_info.total_lines
                )
            } else {
                "DATA_STREAM: IDLE".to_string()
            };
            let center = Paragraph::new(center_text)
                .alignment(Alignment::Center)
                .style(theme.footer_style)
                .block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_type(theme.border_type)
                        .border_style(theme.border_style),
                );
            f.render_widget(center, chunks[1]);

            // Right: Fake Metrics (Aesthetic)
            let right_text = "CPU: [||||||  ]";
            let right = Paragraph::new(right_text)
                .alignment(Alignment::Right)
                .style(theme.footer_style)
                .block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_type(theme.border_type)
                        .border_style(theme.border_style),
                );
            f.render_widget(right, chunks[2]);
            return;
        }

        // Standard Header Logic (Existing)
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
        } else if self.input_mode == crate::tui::state::InputMode::HistorySearch
            && let Some(history_search_state) = &self.history_search_state
        {
            // Render log panel in the background first
            self.render_log_panel(f, area, plan, theme);
            // Render history search overlay on top
            self.render_history_search(f, area, history_search_state, theme);
        } else if self.input_mode == crate::tui::state::InputMode::FileSearch
            && let Some(file_search_state) = &self.file_search_state
        {
            // Render log panel in the background first
            self.render_log_panel(f, area, plan, theme);
            // Render file search overlay on top
            self.render_file_search(f, area, file_search_state, theme);
        } else if self.input_mode == crate::tui::state::InputMode::Shell {
            self.render_shell_view(f, area, theme);
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

        let paragraph = Paragraph::new(lines)
            .style(theme.log_style)
            .block(Block::default());
        f.render_widget(paragraph, area);
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
            .border_style(theme.border_style)
            .border_type(theme.border_type)
            .title("Changed Files (←/→ to focus)");
        let files_list = List::new(items).block(files_block);
        f.render_widget(files_list, layout[0]);

        // diff content
        let diff_block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .border_type(theme.border_type)
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border_style)
                .border_type(theme.border_type),
        );
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

        // Fill any remaining space with blank lines to prevent artifacts
        let list_height = items.len();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme.border_style)
                    .border_type(theme.border_type)
                    .title(
                        "Sessions (↑↓ to navigate, Enter to switch, d to delete, q/ESC to close)",
                    ),
            )
            .highlight_style(theme.completion_selected_style)
            .highlight_symbol(">> ");

        f.render_widget(list, area);

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

    fn render_shell_view(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        f.render_widget(Clear, area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style)
            .border_type(theme.border_type)
            .title("Shell Output")
            .style(theme.log_style);

        // Try to parse ANSI, fallback to raw text if it fails
        let text = match self.shell_output_buffer.as_bytes().into_text() {
            Ok(t) => t,
            Err(_) => Text::raw(&self.shell_output_buffer),
        };

        let height = area.height.saturating_sub(2) as usize; // remove borders
        let scroll = if text.lines.len() > height {
            (text.lines.len() - height) as u16
        } else {
            0
        };

        let paragraph = Paragraph::new(text).block(block).scroll((scroll, 0));

        f.render_widget(paragraph, area);
    }

    fn render_input_area(&mut self, f: &mut Frame, area: Rect) {
        // Clear input area fully to avoid artifacts when input shrinks or mode changes.
        f.render_widget(Clear, area);

        let input_style = if self.input_mode == crate::tui::state::InputMode::Shell {
            self.theme.shell_input_style
        } else if self.input_mode == crate::tui::state::InputMode::HistorySearch
            || self.input_mode == crate::tui::state::InputMode::FileSearch
        {
            // Use normal input style for search modes
            self.theme.input_style
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
        } else if self.input_mode == crate::tui::state::InputMode::HistorySearch {
            "Input (History Search)"
        } else if self.input_mode == crate::tui::state::InputMode::FileSearch {
            "Input (File Search)"
        } else {
            "Input"
        };

        self.textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(self.theme.border_style)
                .border_type(self.theme.border_type)
                .title(block_title),
        );

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

            let list = List::new(items.clone()).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.theme.border_style)
                    .border_type(self.theme.border_type)
                    .title(title),
            );

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

    fn render_history_search(
        &self,
        f: &mut Frame,
        area: Rect,
        state: &crate::tui::state::HistorySearchState,
        theme: &Theme,
    ) {
        // Create an overlay area centered or near input
        let overlay_height = (area.height as usize).clamp(5, 15) as u16;
        let overlay_width = (area.width as usize).clamp(40, 100) as u16;

        // Center the overlay
        let overlay_area = Rect {
            x: area.x + (area.width.saturating_sub(overlay_width)) / 2,
            y: area.y + (area.height.saturating_sub(overlay_height)) / 2,
            width: overlay_width,
            height: overlay_height,
        };

        // Clear area for overlay
        f.render_widget(Clear, overlay_area);

        let items: Vec<ListItem> = state
            .results
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let style = if i == state.selected_index {
                    theme.completion_selected_style
                } else {
                    theme.completion_style
                };
                ListItem::new(cmd.clone()).style(style)
            })
            .collect();

        let title = format!(
            "History Search: '{}' (Ctrl+R cycle, Enter select, ESC cancel)",
            state.query
        );
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme.border_style)
                    .border_type(theme.border_type)
                    .title(title),
            )
            .highlight_style(theme.completion_selected_style);

        f.render_widget(list, overlay_area);
    }

    fn render_file_search(
        &self,
        f: &mut Frame,
        area: Rect,
        state: &crate::tui::state::FileSearchState,
        theme: &Theme,
    ) {
        // Create an overlay area centered
        let overlay_height = (area.height as usize).clamp(10, 20) as u16;
        let overlay_width = (area.width as usize).clamp(60, 120) as u16;

        let overlay_area = Rect {
            x: area.x + (area.width.saturating_sub(overlay_width)) / 2,
            y: area.y + (area.height.saturating_sub(overlay_height)) / 2,
            width: overlay_width,
            height: overlay_height,
        };

        f.render_widget(Clear, overlay_area);

        let items: Vec<ListItem> = if state.loading && state.all_files.is_empty() {
            vec![ListItem::new("Scanning files...").style(theme.completion_style)]
        } else {
            state
                .results
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    let style = if i == state.selected_index {
                        theme.completion_selected_style
                    } else {
                        theme.completion_style
                    };
                    ListItem::new(path.as_str()).style(style)
                })
                .collect()
        };

        let title = if state.loading {
            format!("File Search: (Loading...) '{}'", state.query)
        } else {
            format!(
                "File Search: '{}' ({} found)",
                state.query,
                state.results.len()
            )
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme.border_style)
                    .border_type(theme.border_type)
                    .title(title),
            )
            .highlight_style(theme.completion_selected_style);

        f.render_widget(list, overlay_area);
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
            write!(footer_text, "Total: {} ", total).unwrap();
        } else {
            footer_text.push_str("Total: N/A ");
        }

        // Remaining context tokens
        if let Some(remaining) = self.remaining_context_tokens {
            write!(footer_text, "Remaining: {} | ", remaining).unwrap();
        } else {
            footer_text.push_str("Remaining: N/A | ");
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
            crate::tui::state::InputMode::HistorySearch => "HistorySearch",
            crate::tui::state::InputMode::FileSearch => "FileSearch",
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
