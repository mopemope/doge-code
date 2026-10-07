use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
#[cfg(test)]
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::{Block, Borders};
use ratatui_textarea::{CursorMove, Input, TextArea};
use tracing::debug;

use crate::tui::state::{CompletionType, TuiApp, save_input_history};

#[cfg(test)]
type TerminalType = Terminal<CrosstermBackend<std::io::Stdout>>;

/// Only control commands bypass the ordinary instruction queue.
fn is_immediate_control_command(line: &str) -> bool {
    let line = line.trim();
    line == "/jobs" || line.split_whitespace().next() == Some("/cancel")
}

fn apply_selected_completion(app: &mut TuiApp) {
    let Some(completed) = app.completion_candidates.get(app.completion_index).cloned() else {
        return;
    };
    let cursor = app.textarea.cursor();
    let (row, col) = (cursor.0, cursor.1);
    let current = app.textarea.lines()[row].clone();
    let cursor_byte = current
        .char_indices()
        .nth(col)
        .map_or(current.len(), |(byte, _)| byte);
    let range = match app.completion_type {
        CompletionType::FilePath => {
            let Some(at) = current[..cursor_byte].rfind('@') else {
                return;
            };
            if current[at + 1..cursor_byte].contains(char::is_whitespace) {
                return;
            }
            let end = current[cursor_byte..]
                .find(char::is_whitespace)
                .map_or(current.len(), |offset| cursor_byte + offset);
            at + 1..end
        }
        CompletionType::Command => {
            if !current.starts_with('/') {
                return;
            }
            0..current.find(char::is_whitespace).unwrap_or(current.len())
        }
        CompletionType::None => return,
    };
    let mut replacement = current.clone();
    replacement.replace_range(range.clone(), &completed);
    let cursor_chars = replacement[..range.start + completed.len()].chars().count();
    // Delete only this line's contents. Deleting again at its empty end would
    // remove the following newline and join a different draft line.
    app.textarea.cancel_selection();
    app.textarea.move_cursor(CursorMove::End);
    if !current.is_empty() {
        app.textarea.delete_line_by_head();
    }
    app.textarea.insert_str(&replacement);
    // Jump uses u16 coordinates; unusually large drafts still retain their
    // contents and keep the cursor at the edited line's end.
    if let (Ok(row), Ok(col)) = (u16::try_from(row), u16::try_from(cursor_chars)) {
        app.textarea.move_cursor(CursorMove::Jump(row, col));
    }
    if app.completion_type == CompletionType::Command && range.end == current.len() {
        app.textarea.insert_str(" ");
    }
}

/// Handle keys when in Normal input mode. Returns Ok(true) if the caller should exit the event loop.
pub fn handle_normal_mode_key(
    app: &mut TuiApp,
    k: KeyEvent,
    _terminal: &mut Terminal<impl ratatui::backend::Backend>,
) -> Result<bool> {
    match k {
        KeyEvent {
            code: KeyCode::Enter,
            modifiers: m,
            ..
        } if m.contains(KeyModifiers::ALT) => {
            app.textarea.insert_newline();
            app.completion_active = false;
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::Enter,
            ..
        } => {
            debug!(
                "Enter key pressed. completion_active: {}",
                app.completion_active
            );
            let mut submit = false;
            if app.completion_active && !app.completion_candidates.is_empty() {
                let current_input = app.textarea.lines()[app.textarea.cursor().0].trim();
                // If the user has typed a command that is in the completion list, submit it directly.
                if app.completion_type == CompletionType::Command
                    && app.completion_candidates.iter().any(|c| c == current_input)
                {
                    submit = true;
                } else {
                    apply_selected_completion(app);
                }
                app.completion_active = false;
                app.dirty = true;
            } else {
                submit = true;
            }

            if submit {
                debug!("Submitting line: '{}'", app.textarea.lines().join("\n"));
                let line = app.textarea.lines().join("\n");
                if !line.trim().is_empty() {
                    if app.input_history.last().map(|s| s.as_str()) != Some(line.as_str()) {
                        app.input_history.push(line.clone());
                        save_input_history(&app.input_history);
                    }
                    app.history_index = app.input_history.len();
                    app.draft.clear();
                }

                let immediate_control = is_immediate_control_command(&line);

                let quitting = line.trim() == "/quit";
                if !quitting {
                    if immediate_control {
                        app.dispatch(&line);
                    } else {
                        app.pending_instructions.push_back(line);
                    }
                }

                app.textarea = TextArea::default();
                app.textarea
                    .set_block(Block::default().borders(Borders::ALL).title("Input"));
                app.textarea.set_placeholder_text("Enter your message...");
                app.dirty = true;
                app.spinner_state = 0;
                if quitting {
                    return Ok(true);
                }
            }
        }

        KeyEvent {
            code: KeyCode::Esc, ..
        } => {
            if app.completion_active {
                app.completion_active = false;
                app.dirty = true;
            } else {
                app.dispatch("/cancel");
                app.dirty = true;
            }
        }

        KeyEvent {
            code: KeyCode::Char('r'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.enter_history_search();
            app.completion_active = false;
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::Char('p'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.enter_file_search();
            app.completion_active = false;
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::Char('d'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            if app.view_mode == crate::tui::state::ViewMode::Log {
                app.view_mode = crate::tui::state::ViewMode::Dashboard;
            } else {
                app.view_mode = crate::tui::state::ViewMode::Log;
            }
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::PageUp,
            ..
        } => {
            let visible_lines = app.main_content_height.saturating_sub(1).max(1);
            app.page_up(visible_lines);
        }

        KeyEvent {
            code: KeyCode::PageDown,
            ..
        } => {
            let visible_lines = app.main_content_height.saturating_sub(1).max(1);
            app.page_down(visible_lines);
        }

        KeyEvent {
            code: KeyCode::Home,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_top();
        }

        KeyEvent {
            code: KeyCode::Char('v'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            // Handle Ctrl+V to paste clipboard content
            app.completion_active = false;
            app.dirty = true;
            match arboard::Clipboard::new() {
                Ok(mut clipboard) => match clipboard.get_text() {
                    Ok(contents) => {
                        app.textarea.insert_str(&contents);
                        app.dirty = true;
                    }
                    Err(e) => {
                        app.push_log(format!("[Clipboard] Failed to read clipboard: {}", e));
                        app.dirty = true;
                    }
                },
                Err(e) => {
                    app.push_log(format!("[Clipboard] Failed to access clipboard: {}", e));
                    app.dirty = true;
                }
            }
        }

        KeyEvent {
            code: KeyCode::End,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_bottom();
        }

        KeyEvent {
            code: KeyCode::Up,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_up(1);
        }

        KeyEvent {
            code: KeyCode::Down,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_down(1);
        }

        KeyEvent {
            code: KeyCode::Char('l'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_bottom();
        }

        KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            ..
        } => {
            if app.completion_active && !app.completion_candidates.is_empty() {
                app.completion_index = app.completion_index.saturating_sub(1);

                // Scroll up if necessary
                if app.completion_index < app.completion_scroll {
                    app.completion_scroll = app.completion_index;
                } else if app.completion_index
                    >= app.completion_scroll + crate::tui::state::MAX_COMPLETION_DISPLAY_ITEMS
                {
                    // Prevent index from going completely out of bounds visually if jumping
                    app.completion_scroll =
                        app.completion_index - crate::tui::state::MAX_COMPLETION_DISPLAY_ITEMS + 1;
                }

                app.dirty = true;
            } else if app.textarea.lines().len() > 1 {
                app.textarea.move_cursor(CursorMove::Up);
                app.dirty = true;
            } else if !app.input_history.is_empty() && app.history_index > 0 {
                if app.history_index == app.input_history.len() {
                    app.draft = app.textarea.lines().join("\n");
                }
                app.history_index -= 1;
                app.textarea = TextArea::from(app.input_history[app.history_index].lines());
                app.textarea
                    .set_block(Block::default().borders(Borders::ALL).title("Input"));
                app.textarea.set_placeholder_text("Enter your message...");
                app.dirty = true;
            }
        }

        KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            ..
        } => {
            if app.completion_active && !app.completion_candidates.is_empty() {
                app.completion_index = (app.completion_index + 1)
                    .min(app.completion_candidates.len().saturating_sub(1));

                // Scroll down if necessary
                let max_display_items = crate::tui::state::MAX_COMPLETION_DISPLAY_ITEMS;
                if app.completion_index >= app.completion_scroll + max_display_items {
                    app.completion_scroll = app.completion_index - max_display_items + 1;
                }

                app.dirty = true;
            } else if app.textarea.lines().len() > 1 {
                app.textarea.move_cursor(CursorMove::Down);
                app.dirty = true;
            } else if !app.input_history.is_empty() && app.history_index < app.input_history.len() {
                app.history_index += 1;
                if app.history_index == app.input_history.len() {
                    app.textarea = TextArea::from(app.draft.lines());
                } else {
                    app.textarea = TextArea::from(app.input_history[app.history_index].lines());
                }
                app.textarea
                    .set_block(Block::default().borders(Borders::ALL).title("Input"));
                app.textarea.set_placeholder_text("Enter your message...");
                app.dirty = true;
            }
        }

        KeyEvent {
            code: KeyCode::Tab, ..
        } => {
            if app.completion_active && !app.completion_candidates.is_empty() {
                apply_selected_completion(app);
                app.completion_active = false;
                app.dirty = true;
            }
        }

        // Keep Shift+Tab as a no-op, matching the pre-migration textarea behavior.
        KeyEvent {
            code: KeyCode::BackTab,
            ..
        } => {}

        KeyEvent {
            code: KeyCode::Left,
            ..
        } => {
            app.textarea.move_cursor(CursorMove::Back);
            app.completion_active = false;
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::Right,
            ..
        } => {
            app.textarea.move_cursor(CursorMove::Forward);
            app.completion_active = false;
            app.dirty = true;
        }

        other => {
            // Completely block all mouse-related input from reaching textarea
            // to prevent conflicts with our custom mouse scroll handling
            match other.code {
                KeyCode::Char(_)
                | KeyCode::Tab
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Enter
                | KeyCode::Esc
                | KeyCode::F(_) => {
                    let handled_by_textarea = app.textarea.input(Input::from(other));
                    if handled_by_textarea {
                        app.dirty = true;
                        let row = app.textarea.cursor().0;
                        let current_line = &app.textarea.lines()[row];
                        let cursor_byte = current_line
                            .char_indices()
                            .nth(app.textarea.cursor().1)
                            .map_or(current_line.len(), |(byte, _)| byte);
                        let input_str = current_line[..cursor_byte].to_string();
                        if input_str.starts_with('/') {
                            app.update_completion_candidates(&input_str);
                        } else if input_str.contains('@') {
                            app.update_file_path_completion_candidates(&input_str);
                        } else {
                            app.completion_active = false;
                        }
                    }
                }
                _ => {
                    // Block all other events (including any mouse events that might sneak through)
                    tracing::debug!("Blocked non-key event from textarea: {:?}", other.code);
                }
            }
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn multiline_cursor_move_does_not_apply_stale_completion_to_other_row() {
        let mut app = TuiApp::new_for_test("multiline", None, "default");
        app.textarea = TextArea::from(vec!["@alpha".to_string(), "@beta".to_string()]);
        app.textarea.move_cursor(CursorMove::End);
        app.completion_active = true;
        app.completion_type = crate::tui::state::CompletionType::FilePath;
        app.completion_candidates = vec!["alpha".to_string()];
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        for _ in 0..3 {
            handle_normal_mode_key(
                &mut app,
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                &mut terminal,
            )
            .unwrap();
        }
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(app.textarea.lines(), ["@alpha", "@beta"]);
    }

    #[test]
    fn multiline_vertical_navigation_keeps_draft() {
        let mut app = TuiApp::new_for_test("multiline", None, "default");
        app.textarea = TextArea::from(vec!["first".to_string(), "second".to_string()]);
        app.textarea.move_cursor(CursorMove::Bottom);
        app.input_history = vec!["old history".to_string()];
        app.history_index = 1;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(app.textarea.lines(), ["first", "second"]);
        assert_eq!(app.textarea.cursor().0, 0);
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(app.textarea.cursor().0, 1);
        assert_eq!(app.history_index, 1);
    }

    #[test]
    fn multiline_command_completion_preserves_spacing_and_other_rows() {
        let mut app = TuiApp::new_for_test("multiline", None, "default");
        app.textarea = TextArea::from(vec![
            "keep first".to_string(),
            "/he  arguments".to_string(),
            "keep last".to_string(),
        ]);
        app.textarea.move_cursor(CursorMove::Jump(1, 3));
        app.completion_active = true;
        app.completion_type = crate::tui::state::CompletionType::Command;
        app.completion_candidates = vec!["/help".to_string()];
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(
            app.textarea.lines(),
            ["keep first", "/help  arguments", "keep last"]
        );
    }

    #[test]
    fn multiline_completion_preserves_other_lines_and_suffix() {
        for code in [KeyCode::Tab, KeyCode::Enter] {
            let mut app = TuiApp::new_for_test("multiline", None, "default");
            app.textarea = TextArea::from(vec![
                "keep first".to_string(),
                "日本語 @pa keep suffix".to_string(),
                "keep last".to_string(),
            ]);
            app.textarea.move_cursor(CursorMove::Jump(1, 7));
            app.completion_active = true;
            app.completion_type = crate::tui::state::CompletionType::FilePath;
            app.completion_candidates = vec!["path.rs".to_string()];
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
            handle_normal_mode_key(
                &mut app,
                KeyEvent::new(code, KeyModifiers::NONE),
                &mut terminal,
            )
            .unwrap();
            assert_eq!(
                app.textarea.lines(),
                ["keep first", "日本語 @path.rs keep suffix", "keep last"]
            );
        }
    }

    #[test]
    fn multiline_typing_updates_completion_from_current_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.rs"), "").unwrap();
        let mut app = TuiApp::new_for_test("multiline", None, "default");
        app.cfg = Some(crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        });
        app.textarea = TextArea::from(vec!["keep first".to_string(), "review @".to_string()]);
        app.textarea.move_cursor(CursorMove::Bottom);
        app.textarea.move_cursor(CursorMove::End);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut terminal,
        )
        .unwrap();
        assert!(app.completion_active);
        assert!(app.completion_candidates.contains(&"alpha.rs".to_string()));
    }

    #[test]
    fn multiline_input_view_grows_to_show_draft_rows() {
        let mut app = TuiApp::new_for_test("multiline", None, "default");
        app.textarea = TextArea::from((0..6).map(|n| format!("INPUT{n}")).collect::<Vec<_>>());
        app.textarea.move_cursor(CursorMove::Bottom);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| app.view(f, None)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        for n in 0..6 {
            assert!(screen.contains(&format!("INPUT{n}")));
        }
        app.textarea = TextArea::from((0..30).map(|n| format!("INPUT{n}")).collect::<Vec<_>>());
        app.textarea.move_cursor(CursorMove::Bottom);
        terminal.draw(|f| app.view(f, None)).unwrap();
        assert_eq!(app.main_content_height, 21);
        assert_eq!(app.textarea.lines().len(), 30);
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(screen.contains("INPUT29"));
        terminal.backend_mut().resize(1, 1);
        terminal.draw(|f| app.view(f, None)).unwrap();
        assert_eq!(app.textarea.lines().len(), 30);
    }

    use super::*;
    use crate::tui::state::CompletionType;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{TerminalOptions, Viewport, layout::Rect};
    use ratatui_textarea::{CursorMove, TextArea};

    fn test_terminal() -> anyhow::Result<TerminalType> {
        let backend = CrosstermBackend::new(std::io::stdout());
        Ok(Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
            },
        )?)
    }

    #[test]
    fn enter_dispatches_controls_while_busy_without_resetting_timer() -> anyhow::Result<()> {
        use crate::tui::commands::core::CommandHandler;
        use std::sync::{Arc, Mutex};
        struct Recorder(Arc<Mutex<Vec<String>>>);
        impl CommandHandler for Recorder {
            fn handle(&mut self, line: &str, _: &mut TuiApp) {
                self.0.lock().unwrap().push(line.trim().into());
            }
            fn get_custom_commands(&self) -> Vec<String> {
                vec![]
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        let mut terminal = test_terminal()?;
        for status in [
            crate::tui::state::Status::Thinking,
            crate::tui::state::Status::Running,
        ] {
            for line in ["/jobs", " /cancel ", "/cancel job-2", "/cancel\tbad-id"] {
                let calls = Arc::new(Mutex::new(Vec::new()));
                let mut app = TuiApp::new_for_test("controls", None, "default");
                app.handler = Some(Box::new(Recorder(calls.clone())));
                app.status = status;
                let started = std::time::Instant::now();
                app.processing_start_time = Some(started);
                app.pending_instructions.push_back("next prompt".into());
                app.textarea = TextArea::from(vec![line.to_string()]);
                // Avoid touching persistent input history in this isolated test.
                app.input_history.push(line.into());
                handle_normal_mode_key(
                    &mut app,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    &mut terminal,
                )?;
                assert_eq!(*calls.lock().unwrap(), [line.trim()]);
                assert_eq!(app.pending_instructions, ["next prompt"]);
                assert_eq!(app.processing_start_time, Some(started));
                assert_eq!(app.textarea.lines(), [""]);
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn enter_controls_target_current_job_and_preserve_prompt_queue() -> anyhow::Result<()> {
        use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, JobStatus, WorkspaceAccess};
        use crate::tui::commands::core::TuiExecutor;
        use crate::tui::state::{LogEntry, Status};
        use std::sync::{Arc, Mutex};
        let dir = tempfile::tempdir()?;
        let cfg = crate::config::AppConfig {
            project_root: dir.path().into(),
            no_repomap: true,
            ..Default::default()
        };
        let repomap = Arc::new(tokio::sync::RwLock::new(None));
        let tools = crate::tools::FsTools::new(repomap.clone(), Arc::new(cfg.clone()));
        let manager = Arc::new(Mutex::new(crate::session::SessionManager::with_store(
            crate::session::SessionStore::new(dir.path().join("sessions"))?,
        )));
        let executor = TuiExecutor::construct_with_session_manager(cfg, repomap, tools, manager)?;
        let jobs = executor.jobs.clone();
        let spawn = || {
            jobs.spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::None,
                    "blocking",
                ),
                |ctx| async move {
                    ctx.cancellation.cancelled().await;
                    JobRunOutcome::Cancelled
                },
            )
            .expect("blocking job")
        };
        let mut app = TuiApp::new_for_test("controls", None, "default");
        app.handler = Some(Box::new(executor));
        let mut terminal = test_terminal()?;
        for (status, cancel) in [
            (Status::Thinking, " /cancel "),
            (Status::Running, "/cancel ID"),
            (Status::Thinking, "/cancel\tID"),
        ] {
            let id = spawn();
            app.status = status;
            for line in [
                " /jobs ",
                "/cancel bad-id",
                "/cancellation",
                "/jobs-extra",
                "ordinary prompt",
            ] {
                app.textarea = TextArea::from(vec![line.to_string()]);
                app.input_history.push(line.into());
                handle_normal_mode_key(
                    &mut app,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    &mut terminal,
                )?;
            }
            assert!(app.log.iter().any(|entry| match entry {
                LogEntry::Plain(text) | LogEntry::Markdown(text) => text.contains(&id.to_string()),
            }));
            assert!(app.log.iter().any(|entry| match entry {
                LogEntry::Plain(text) | LogEntry::Markdown(text) =>
                    text.contains("Unknown job id: bad-id"),
            }));
            assert_eq!(
                app.pending_instructions,
                ["/cancellation", "/jobs-extra", "ordinary prompt"]
            );
            let line = cancel.replace("ID", &id.to_string());
            app.textarea = TextArea::from(vec![line.clone()]);
            app.input_history.push(line);
            handle_normal_mode_key(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut terminal,
            )?;
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while jobs.foreground_id().is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await?;
            assert_eq!(jobs.get_snapshot(id).unwrap().status, JobStatus::Cancelled);
            // Immediate cancels cannot remain queued and target the next job.
            assert_eq!(
                app.pending_instructions,
                ["/cancellation", "/jobs-extra", "ordinary prompt"]
            );
            app.pending_instructions.clear();
        }
        let id = spawn();
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut terminal,
        )?;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while jobs.foreground_id().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert_eq!(jobs.get_snapshot(id).unwrap().status, JobStatus::Cancelled);
        assert!(app.pending_instructions.is_empty());
        app.textarea = TextArea::from(vec![" /quit ".to_string()]);
        app.input_history.push(" /quit ".into());
        assert!(handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut terminal
        )?);
        assert!(
            app.textarea.lines().iter().all(|line| line.is_empty()),
            "a refused quit must allow a fresh recovery command"
        );
        Ok(())
    }

    #[test]
    fn normal_mode_keeps_shift_tab_as_a_noop() -> anyhow::Result<()> {
        let mut app = TuiApp::new_for_test("key-routing", None, "default");
        app.textarea = TextArea::from(vec!["abc".to_string()]);
        app.dirty = false;
        let mut terminal = test_terminal()?;

        let should_exit = handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            &mut terminal,
        )?;

        assert!(!should_exit);
        assert_eq!(app.textarea.lines(), ["abc"]);
        assert_eq!(app.textarea.cursor(), (0, 0));
        assert!(!app.dirty);
        Ok(())
    }

    #[test]
    fn normal_mode_routes_textarea_input_and_completion() -> anyhow::Result<()> {
        let mut app = TuiApp::new_for_test("key-routing", None, "default");
        let mut terminal = test_terminal()?;

        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut terminal,
        )?;
        assert_eq!(app.textarea.lines(), ["x"]);

        app.textarea = TextArea::from(vec!["abc".to_string()]);
        app.textarea.move_cursor(CursorMove::End);
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            &mut terminal,
        )?;
        assert_eq!(app.textarea.cursor(), (0, 0));

        app.textarea = TextArea::from(vec!["/he".to_string()]);
        app.completion_active = true;
        app.completion_candidates = vec!["/help".to_string()];
        app.completion_type = CompletionType::Command;
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut terminal,
        )?;
        assert_eq!(app.textarea.lines(), ["/help "]);
        assert!(!app.completion_active);

        Ok(())
    }
}
