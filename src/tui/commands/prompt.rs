use crate::assets::Assets;
use chrono::Local;
use std::env;
use std::path::{Path, PathBuf};
use tera::{Context, Tera};
use tracing::error;

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
                error!("Failed to read {}: {}", path.display(), e);
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
pub(crate) fn build_system_prompt(cfg: &crate::config::AppConfig) -> String {
    let mut tera = Tera::default();
    let mut context = Context::new();

    let sys_prompt_template =
        String::from_utf8(Assets::get("system_prompt.md").unwrap().data.to_vec())
            .unwrap_or_default();

    context.insert("date", &Local::now().format("%Y-%m-%d %A").to_string());
    context.insert("os", &std::env::consts::OS);
    context.insert("project_dir", &cfg.project_root.to_string_lossy());
    context.insert(
        "shell",
        &env::var("SHELL").unwrap_or_else(|_| "unknown".to_string()),
    );

    let base_sys_prompt = tera
        .render_str(&sys_prompt_template, &context)
        .unwrap_or_else(|e| {
            error!("Failed to render system prompt: {e}");
            sys_prompt_template // fallback to the original template
        });

    let project_instructions = load_project_instructions(cfg);
    if let Some(instructions) = project_instructions {
        format!("{base_sys_prompt}\n\n# Project-Specific Instructions\n{instructions}")
    } else {
        base_sys_prompt
    }
}
