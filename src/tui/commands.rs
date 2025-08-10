use crate::analysis::Analyzer;
use crate::assets::Assets;
use crate::llm::OpenAIClient;
use crate::tools::FsTools;
use crate::tui::theme::Theme; // 新規追加
use crate::tui::view::TuiApp;
use anyhow::Result;
use chrono::Local;
use std::env;
use std::path::{Path, PathBuf};
use tera::{Context, Tera};
use tokio::sync::watch;

/// Finds the project instructions file based on a priority list.
/// Checks for AGENTS.md, QWEN.md, or GEMINI.md in that order within the project root.
pub(crate) fn find_project_instructions_file(project_root: &Path) -> Option<PathBuf> {
    let priority_files = ["AGENTS.md", "QWEN.md", "GEMINI.md"];
    for file_name in &priority_files {
        let path = project_root.join(file_name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// Inner function for loading project instructions, allowing for mocking the file reader.
pub(crate) fn load_project_instructions_inner<F>(
    project_root: &Path,
    read_file: F,
) -> Option<String>
where
    F: Fn(&Path) -> std::io::Result<String>,
{
    if let Some(path) = find_project_instructions_file(project_root) {
        match read_file(&path) {
            Ok(content) => Some(content),
            Err(e) => {
                eprintln!("Failed to read {}: {}", path.display(), e);
                None
            }
        }
    } else {
        None
    }
}

/// Load project-specific instructions from a file.
/// Checks for AGENTS.md, QWEN.md, or GEMINI.md in that order.
fn load_project_instructions(cfg: &crate::config::AppConfig) -> Option<String> {
    load_project_instructions_inner(&cfg.project_root, |p: &Path| std::fs::read_to_string(p))
}

/// Combine the base system prompt with project-specific instructions.
fn build_system_prompt(cfg: &crate::config::AppConfig) -> String {
    let mut tera = Tera::default();
    let mut context = Context::new();

    let sys_prompt_template =
        String::from_utf8(Assets::get("system_prompt.md").unwrap().data.to_vec())
            .unwrap_or_default();

    context.insert("date", &Local::now().format("%Y年%m月%d日 %A").to_string());
    context.insert("os", &std::env::consts::OS);
    context.insert("project_dir", &cfg.project_root.to_string_lossy());
    context.insert(
        "shell",
        &env::var("SHELL").unwrap_or_else(|_| "unknown".to_string()),
    );

    let base_sys_prompt = tera
        .render_str(&sys_prompt_template, &context)
        .unwrap_or_else(|e| {
            eprintln!("Failed to render system prompt: {e}");
            sys_prompt_template // fallback to the original template
        });

    let project_instructions = load_project_instructions(cfg);
    if let Some(instructions) = project_instructions {
        format!("{base_sys_prompt}\n\n# Project-Specific Instructions\n{instructions}")
    } else {
        base_sys_prompt
    }
}

pub trait CommandHandler {
    fn handle(&mut self, line: &str, ui: &mut TuiApp);
}

pub struct TuiExecutor {
    pub(crate) cfg: crate::config::AppConfig,
    pub(crate) tools: FsTools,
    pub(crate) analyzer: Analyzer,
    pub(crate) client: Option<OpenAIClient>,
    #[allow(dead_code)]
    pub(crate) history: crate::llm::ChatHistory,
    pub(crate) ui_tx: Option<std::sync::mpsc::Sender<String>>,
    pub(crate) cancel_tx: Option<watch::Sender<bool>>,
    pub(crate) last_user_prompt: Option<String>,
}

impl TuiExecutor {
    pub fn new(cfg: crate::config::AppConfig) -> Result<Self> {
        let tools = FsTools::new();
        let analyzer = Analyzer::new(&cfg.project_root)?;
        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };
        // Load system prompt
        let sys_prompt = build_system_prompt(&cfg);
        let mut history = crate::llm::ChatHistory::new(12_000, Some(sys_prompt));
        history.append_system_once();

        Ok(Self {
            cfg,
            tools,
            analyzer,
            client,
            history,
            ui_tx: None,
            cancel_tx: None,
            last_user_prompt: None,
        })
    }
}

impl CommandHandler for TuiExecutor {
    fn handle(&mut self, line: &str, ui: &mut TuiApp) {
        if self.ui_tx.is_none() {
            self.ui_tx = ui.sender();
        }
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        match line {
            "/help" => {
                ui.push_log(
                    "/help, /map, /tools, /clear, /open <path>, /quit, /retry, /theme <name>",
                );
            }
            "/tools" => ui.push_log("Available tools: fs_search, fs_read, fs_write"),
            "/clear" => {
                ui.log.clear();
            }
            "/cancel" => {
                if let Some(tx) = &self.cancel_tx {
                    let _ = tx.send(true);
                    if let Some(tx) = &self.ui_tx {
                        let _ = tx.send("::status:cancelled".into());
                    }
                    ui.push_log("[Cancelled]");
                    self.cancel_tx = None;
                } else {
                    ui.push_log("[no running task]");
                }
            }
            "/retry" => {
                if self.cancel_tx.is_some() {
                    ui.push_log("[busy] streaming in progress; use /cancel first");
                    return;
                }
                match self.last_user_prompt.clone() {
                    Some(prompt) => {
                        // Re-dispatch as if user typed it
                        self.handle(&prompt, ui);
                    }
                    None => ui.push_log("[no previous prompt]"),
                }
            }
            "/map" => match self.analyzer.build() {
                Ok(map) => {
                    ui.push_log(format!("RepoMap: {} symbols", map.symbols.len()));
                    for s in map.symbols.iter().take(50) {
                        ui.push_log(format!(
                            "{} {}  @{}:{}",
                            s.kind.as_str(),
                            s.name,
                            s.file.display(),
                            s.start_line
                        ));
                    }
                }
                Err(e) => ui.push_log(format!("map error: {e}")),
            },
            // /theme コマンドの処理を追加
            line if line.starts_with("/theme ") => {
                let theme_name = line[7..].trim(); // "/theme " の後の文字列を取得
                match theme_name.to_lowercase().as_str() {
                    "dark" => {
                        ui.theme = Theme::dark();
                        ui.push_log("[Theme switched to dark]");
                    }
                    "light" => {
                        ui.theme = Theme::light();
                        ui.push_log("[Theme switched to light]");
                    }
                    _ => {
                        ui.push_log(format!(
                            "[Unknown theme: {theme_name}. Available themes: dark, light]"
                        ));
                    }
                }
                // テーマ変更後に再描画
                if let Err(e) = ui.draw_with_model(Some(&self.cfg.model)) {
                    ui.push_log(format!("Failed to redraw after theme change: {e}"));
                }
            }
            _ => {
                if let Some(rest) = line.strip_prefix("/open ") {
                    let path_arg = rest.trim();
                    if path_arg.is_empty() {
                        ui.push_log("usage: /open <path>");
                        return;
                    }
                    // Resolve to absolute path; allow project-internal paths and absolute paths
                    let p = std::path::Path::new(path_arg);
                    let abs = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        self.cfg.project_root.join(p)
                    };
                    if !abs.exists() {
                        ui.push_log(format!("not found: {}", abs.display()));
                        return;
                    }
                    // Leave TUI alt screen temporarily while spawning editor in blocking mode
                    use crossterm::{cursor, execute, terminal};
                    let mut stdout = std::io::stdout();
                    let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
                    let _ = terminal::disable_raw_mode();

                    // Choose editor from $EDITOR, then $VISUAL, else fallback list
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .or_else(|| std::env::var("VISUAL").ok())
                        .unwrap_or_else(|| "vi".to_string());
                    let status = std::process::Command::new(&editor).arg(&abs).status();

                    // Re-enter TUI
                    let _ = terminal::enable_raw_mode();
                    let _ = execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide);

                    match status {
                        Ok(s) if s.success() => ui.push_log(format!("opened: {}", abs.display())),
                        Ok(s) => ui.push_log(format!("editor exited with status {s}")),
                        Err(e) => ui.push_log(format!("failed to launch editor: {e}")),
                    }
                    return;
                }
                if !line.starts_with('/') {
                    let rest = line;
                    self.last_user_prompt = Some(rest.to_string());
                    ui.push_log(format!("> {rest}"));
                    match self.client.as_ref() {
                        Some(c) => {
                            let rt = tokio::runtime::Handle::current();
                            let model = self.cfg.model.clone();
                            let content = rest.to_string();
                            let c = c.clone();
                            let tx = self.ui_tx.clone();
                            // Prepare a fresh line for the final output
                            ui.push_log(String::new());
                            let (cancel_tx, cancel_rx) = watch::channel(false);
                            self.cancel_tx = Some(cancel_tx);
                            // Build initial messages with optional system prompt + user
                            let mut msgs = Vec::new();
                            // Load system prompt
                            let sys_prompt = build_system_prompt(&self.cfg);
                            msgs.push(crate::llm::ChatMessage {
                                role: "system".into(),
                                content: Some(sys_prompt),
                                tool_calls: vec![],
                                tool_call_id: None,
                            });
                            msgs.push(crate::llm::ChatMessage {
                                role: "user".into(),
                                content: Some(content.clone()),
                                tool_calls: vec![],
                                tool_call_id: None,
                            });
                            let fs = self.tools.clone();
                            rt.spawn(async move {
                                if *cancel_rx.borrow() {
                                    if let Some(tx) = tx {
                                        let _ = tx.send("::status:cancelled".into());
                                        let _ = tx.send("[Cancelled]".into());
                                    }
                                    return;
                                }
                                let res =
                                    crate::llm::run_agent_loop(&c, &model, &fs, msgs, tx.clone())
                                        .await;
                                match res {
                                    Ok(msg) => {
                                        if let Some(tx) = tx {
                                            let _ = tx.send(msg.content);
                                            let _ = tx.send("::status:done".into());
                                        }
                                    }
                                    Err(e) => {
                                        if let Some(tx) = tx {
                                            let _ = tx.send(format!("LLM error: {e}"));
                                            let _ = tx.send("::status:error".into());
                                        }
                                    }
                                }
                            });
                        }
                        None => ui.push_log("OPENAI_API_KEY not set; cannot call LLM."),
                    }
                } else {
                    ui.push_log(format!("> {line}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // use std::io; // 削除
    // use std::path::PathBuf; // 削除

    #[test]
    fn test_find_project_instructions_file_found_agents_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let agents_md_path = project_root.join("AGENTS.md");
        let qwen_md_path = project_root.join("QWEN.md");
        let gemini_md_path = project_root.join("GEMINI.md");

        // Create AGENTS.md
        std::fs::write(&agents_md_path, "Agents instructions content").unwrap();
        // Also create QWEN.md and GEMINI.md to show AGENTS.md has priority
        std::fs::write(&qwen_md_path, "Qwen instructions content").unwrap();
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(agents_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_found_qwen_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let agents_md_path = project_root.join("AGENTS.md"); // 追加: AGENTS.mdが存在しないことを明示
        let qwen_md_path = project_root.join("QWEN.md");
        let gemini_md_path = project_root.join("GEMINI.md");

        // AGENTS.md should not exist for this test to be valid
        assert!(!agents_md_path.exists());

        // Create QWEN.md (AGENTS.md does not exist)
        std::fs::write(&qwen_md_path, "Qwen instructions content").unwrap();
        // Also create GEMINI.md to show QWEN.md has priority
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(qwen_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_found_gemini_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let agents_md_path = project_root.join("AGENTS.md"); // 追加: AGENTS.mdが存在しないことを明示
        let qwen_md_path = project_root.join("QWEN.md"); // 追加: QWEN.mdが存在しないことを明示
        let gemini_md_path = project_root.join("GEMINI.md");

        // AGENTS.md and QWEN.md should not exist for this test to be valid
        assert!(!agents_md_path.exists());
        assert!(!qwen_md_path.exists());

        // Create GEMINI.md (AGENTS.md and QWEN.md do not exist)
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(gemini_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, None);
    }
}
