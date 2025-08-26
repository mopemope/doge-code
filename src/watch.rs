use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use notify_rust::Notification;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;
use tokio::fs;
use tracing::{error, info, warn};

use crate::config::AppConfig;
use crate::llm::client_core::OpenAIClient;
use crate::llm::types::ChatMessage;

// doge: Please translate the Japanese code comments in this file to English.
pub async fn run_watch_mode(cfg: AppConfig) -> Result<()> {
    info!("Running in watch mode. Watching for file changes...");

    // Load and parse .gitignore
    let gitignore = load_gitignore()?;

    let llm_client = OpenAIClient::new(&cfg.base_url, cfg.api_key.as_ref().unwrap())?
        .with_llm_config(cfg.llm.clone());
    let model = cfg.model.clone();

    let (tx, rx) = channel();

    let mut watcher: RecommendedWatcher = Watcher::new(
        tx,
        notify::Config::default().with_poll_interval(Duration::from_secs(2)),
    )?;

    watcher.watch(Path::new("."), RecursiveMode::Recursive)?;

    info!("Watcher initialized. Waiting for events.");

    for res in rx {
        match res {
            Ok(event) => {
                if let EventKind::Modify(_) = event.kind {
                    for path in event.paths {
                        // Check if the file is ignored by .gitignore
                        if gitignore.matched(&path, path.is_dir()).is_ignore() {
                            info!("Ignored file modified: {:?}", path);
                            continue;
                        }
                        info!("File modified: {:?}", path);
                        if let Err(e) = handle_file_change(&llm_client, &model, path).await {
                            error!("Error handling file change: {}", e);
                        }
                    }
                }
            }
            Err(e) => error!("Watch error: {:?}", e),
        }
    }

    Ok(())
}

fn load_gitignore() -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(".");
    // Add patterns from .gitignore
    if let Some(e) = builder.add(".gitignore") {
        warn!("Failed to add .gitignore to GitignoreBuilder: {}", e);
    }
    // Build the Gitignore object
    let gitignore = builder.build().map_err(|e| {
        error!("Failed to build Gitignore: {}", e);
        e
    })?;
    Ok(gitignore)
}

async fn handle_file_change(llm_client: &OpenAIClient, model: &str, path: PathBuf) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }

    let content = match fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(e) => {
            warn!("Failed to read file {}: {}", path.display(), e);
            return Ok(());
        }
    };

    let re = Regex::new(r"//\s*AI!:\s*(.+)")?;
    for (line_num, line) in content.lines().enumerate() {
        if let Some(caps) = re.captures(line)
            && let Some(instruction) = caps.get(1)
        {
            let instruction_text = instruction.as_str().trim().to_string();
            info!(
                "Found doge command in '{}' at line {}: -> {}",
                path.display(),
                line_num + 1,
                instruction_text
            );

            let new_content =
                execute_llm_task(llm_client, model, &content, &instruction_text, &path).await?;

            fs::write(&path, new_content).await?;
            info!("File {} updated.", path.display());

            Notification::new()
                .summary("Doge-Code Task Completed")
                .body(&format!("File {} was updated.", path.display()))
                .show()?;

            // Once one command is processed, break the loop for this file change event.
            break;
        }
    }

    Ok(())
}

async fn execute_llm_task(
    llm_client: &OpenAIClient,
    model: &str,
    file_content: &str,
    instruction: &str,
    file_path: &Path,
) -> Result<String> {
    info!("Executing LLM task for {}", file_path.display());

    let system_prompt = "You are an expert programmer. You will be given a file's content and an instruction to modify it. Your task is to return the entire file content with the requested modification. Do not add any extra explanations or markdown formatting. Just return the raw, updated file content.".to_string();

    let user_prompt = format!(
        "Please modify the following file based on the instruction.\n\nFile: `{}`\n\nInstruction: {}\n\n```rust\n{}\n```",
        file_path.display(),
        instruction,
        file_content
    );

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(system_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(user_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        },
    ];

    // Send a request to the LLM using the chat_once method
    // The first argument is the model name, the second is the messages, the third is the cancel token (None here)
    let res = llm_client.chat_once(model, messages, None).await?;

    let result_content = res.content;
    // Extract content from markdown code block if present
    let re = Regex::new(r"```(?:rust|)\n([\\s\\S]*?)\n```")?;
    if let Some(caps) = re.captures(&result_content)
        && let Some(code) = caps.get(1)
    {
        return Ok(code.as_str().to_string());
    }
    // Otherwise, return the whole content
    Ok(result_content)
}
