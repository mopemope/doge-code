use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::time::Duration;

use crate::tui::state::{Status, TuiApp, save_input_history}; // TuiAppとsave_input_historyをインポート

// TuiAppにイベントループのロジックを実装
impl TuiApp {
    pub fn event_loop(&mut self) -> Result<()> {
        // Sync history index to end if out of range (e.g., after loading)
        if self.history_index > self.input_history.len() {
            self.history_index = self.input_history.len();
        }
        let mut last_ctrl_c_at: Option<std::time::Instant> = None;
        let mut dirty = true; // initial full render
        let mut is_streaming = false; // 新規: ストリーミング状態を追跡
        loop {
            // Drain inbox; mark dirty on any state change
            if let Some(rx) = self.inbox_rx.as_ref() {
                let mut drained = Vec::new();
                while let Ok(msg) = rx.try_recv() {
                    drained.push(msg);
                }
                for msg in drained {
                    match msg.as_str() {
                        "::status:done" => {
                            if is_streaming {
                                // 変更: マーカーにスペースを追加
                                self.push_log(" --- LLM Response End --- ".to_string());
                                // 新規: ストリーム完了時に構造化レスポンスをログに追加
                                self.finalize_and_append_llm_response();
                                is_streaming = false;
                            }
                            self.status = Status::Done;
                            dirty = true;
                        }
                        "::status:cancelled" => {
                            if is_streaming {
                                // 変更: マーカーにスペースを追加
                                self.push_log(" --- LLM Response End (Cancelled) --- ".to_string());
                                // 新規: ストリーム完了時に構造化レスポンスをログに追加
                                self.finalize_and_append_llm_response();
                                is_streaming = false;
                            }
                            self.status = Status::Cancelled;
                            dirty = true;
                        }
                        "::status:streaming" => {
                            if !is_streaming {
                                // 変更: マーカーにスペースを追加
                                self.push_log(" --- LLM Response Start --- ".to_string());
                                // 新規: ストリーミング開始時に current_llm_response と解析バッファを初期化
                                self.current_llm_response = Some(Vec::new());
                                self.llm_parsing_buffer.clear();
                                is_streaming = true;
                            }
                            self.status = Status::Streaming;
                            dirty = true;
                        }
                        "::status:error" => {
                            if is_streaming {
                                // 変更: マーカーにスペースを追加
                                self.push_log(" --- LLM Response End (Error) --- ".to_string());
                                // 新規: ストリーム完了時に構造化レスポンスをログに追加
                                self.finalize_and_append_llm_response();
                                is_streaming = false;
                            }
                            self.status = Status::Error;
                            dirty = true;
                        }

                        _ if msg.starts_with("::append:") => {
                            let payload = &msg["::append:".len()..];
                            // 変更: LLM応答内容を構造化して蓄積
                            self.append_stream_token_structured(payload);
                            dirty = true;
                        }
                        _ => {
                            self.push_log(msg);
                            dirty = true;
                        }
                    }
                }
            }

            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(k) => match k.code {
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            let now = std::time::Instant::now();
                            if let Some(prev) = last_ctrl_c_at
                                && now.duration_since(prev) <= std::time::Duration::from_secs(3)
                            {
                                return Ok(());
                            }
                            last_ctrl_c_at = Some(now);
                            self.dispatch("/cancel");
                            self.push_log("[Press Ctrl+C again within 3s to exit]");
                            dirty = true;
                        }
                        KeyCode::Esc => {
                            self.dispatch("/cancel");
                            dirty = true;
                        }
                        KeyCode::Enter => {
                            if self.compl.visible {
                                self.apply_completion();
                                dirty = true;
                                continue;
                            }
                            let line = std::mem::take(&mut self.input);
                            // record history if non-empty and not duplicate of last
                            if !line.trim().is_empty() {
                                if self.input_history.last().map(|s| s.as_str())
                                    != Some(line.as_str())
                                {
                                    self.input_history.push(line.clone());
                                    save_input_history(&self.input_history);
                                }
                                self.history_index = self.input_history.len();
                                self.draft.clear();
                            }
                            if line.trim() == "/quit" {
                                return Ok(());
                            }
                            self.dispatch(&line);
                            dirty = true;
                        }
                        KeyCode::Backspace => {
                            self.input.pop();
                            if self.history_index == self.input_history.len() {
                                self.draft = self.input.clone();
                            }
                            self.update_completion();
                            dirty = true;
                        }
                        KeyCode::Up => {
                            if self.compl.visible {
                                if !self.compl.items.is_empty() {
                                    self.compl.selected =
                                        (self.compl.selected + 1) % self.compl.items.len();
                                    dirty = true;
                                }
                            } else if self.history_index > 0 {
                                if self.history_index == self.input_history.len() {
                                    self.draft = self.input.clone();
                                }
                                self.history_index -= 1;
                                self.input = self.input_history[self.history_index].clone();
                                dirty = true;
                            }
                        }
                        KeyCode::Down => {
                            if self.compl.visible {
                                if !self.compl.items.is_empty() {
                                    if self.compl.selected == 0 {
                                        self.compl.selected = self.compl.items.len() - 1;
                                    } else {
                                        self.compl.selected -= 1;
                                    }
                                    dirty = true;
                                }
                            } else if self.history_index < self.input_history.len() {
                                self.history_index += 1;
                                if self.history_index == self.input_history.len() {
                                    self.input = self.draft.clone();
                                } else {
                                    self.input = self.input_history[self.history_index].clone();
                                }
                                dirty = true;
                            }
                        }
                        KeyCode::Char(c) => {
                            // If completion popup is visible and space is pressed, close the popup and suppress reopening once.
                            if c == ' ' && self.compl.visible {
                                self.compl.reset();
                                self.compl.suppress_once = true;
                                self.input.push(c);
                                if self.history_index == self.input_history.len() {
                                    self.draft = self.input.clone();
                                }
                                dirty = true;
                                continue;
                            }
                            self.input.push(c);
                            // If user typed a new '@', enable completion again regardless of previous suppression.
                            if c == '@' {
                                self.compl.suppress_once = false;
                            }
                            if self.history_index == self.input_history.len() {
                                self.draft = self.input.clone();
                            }
                            self.update_completion();
                            dirty = true;
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {
                        dirty = true;
                    }
                    _ => {}
                }
            }

            if dirty {
                self.draw_with_model(self.model.as_deref())?;
                dirty = false;
            }
        }
    }
}
