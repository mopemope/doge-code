use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use notify_rust::Notification;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::{AppConfig, IGNORE_FILE};
use crate::llm::client_core::OpenAIClient;
use crate::llm::types::ChatMessage;
use crate::utils;

use std::sync::{Arc, Mutex};

// doge: Please translate the Japanese code comments in this file to English.
pub async fn run_watch_mode(cfg: AppConfig) -> Result<()> {
    info!("Running in watch mode. Watching for file changes...");

    // Load and parse ignore files (.gitignore, .dogeignore)
    let gitignore = load_gitignore()?;

    let api_key = cfg
        .api_key
        .as_ref()
        .context("OPENAI_API_KEY is required for watch mode")?;
    let llm_client = OpenAIClient::new(&cfg.base_url, api_key)?.with_llm_config(cfg.llm.clone());
    let model = cfg.model.clone();

    // Use Arc<Mutex<>> for thread-safe access to file processing tracking
    let last_processed = Arc::new(Mutex::new(HashMap::<PathBuf, Instant>::new()));

    let (tx, rx) = channel();

    let mut watcher: RecommendedWatcher = Watcher::new(
        tx,
        notify::Config::default().with_poll_interval(Duration::from_millis(500)), // Reduced from 2s to 500ms for more responsive watching
    )?;

    // Watch only source code files to limit scope
    watcher.watch(Path::new("."), RecursiveMode::Recursive)?;

    info!("Watcher initialized. Waiting for events.");

    // Clone the Arc for the loop
    let last_processed_clone = last_processed.clone();
    let cfg_clone = cfg.clone(); // Clone config to pass to spawned tasks

    for res in rx {
        match res {
            Ok(event) => {
                if let EventKind::Modify(_) = event.kind {
                    for path in event.paths {
                        // Check if the file is ignored by configured ignore files
                        if gitignore.matched(&path, path.is_dir()).is_ignore() {
                            info!("Ignored file modified: {:?}", path);
                            continue;
                        }

                        // Apply additional filtering to limit scope to source files
                        if !should_watch_file(&path, &cfg) {
                            continue;
                        }

                        info!("File modified: {:?}", path);

                        // Debounce: wait a short time before processing to avoid multiple rapid triggers
                        let debounce_task = debounce_file_change(
                            path.clone(),
                            llm_client.clone(),
                            model.clone(),
                            last_processed_clone.clone(),
                            cfg_clone.clone(),
                        );
                        tokio::spawn(async move {
                            if let Err(e) = debounce_task.await {
                                error!("Error handling debounced file change: {}", e);
                            }
                        });
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
    // Add patterns from .dogeignore
    if let Some(e) = builder.add(IGNORE_FILE) {
        warn!("Failed to add {} to GitignoreBuilder: {}", IGNORE_FILE, e);
    }
    // Build the Gitignore object
    let gitignore = builder.build().map_err(|e| {
        error!("Failed to build Gitignore: {}", e);
        e
    })?;
    Ok(gitignore)
}

// Check if file should be watched based on configuration to limit scope
fn should_watch_file(path: &Path, cfg: &AppConfig) -> bool {
    let path_str = normalize_path_for_match(path);

    if path_str.starts_with(".doge/") || path_str.contains("/.doge/") {
        return false;
    }

    if let Some(backup_root) = resolve_backup_root(cfg)
        && is_path_under_dir(path, &backup_root, &cfg.project_root)
    {
        return false;
    }

    // Check exclude patterns first
    if let Some(ref exclude_patterns) = cfg.watch_config.exclude_patterns {
        for pattern in exclude_patterns {
            if glob_match(pattern, &path_str) {
                return false; // Skip this file if it matches an exclude pattern
            }
        }
    }

    // Then check include patterns
    if let Some(ref include_patterns) = cfg.watch_config.include_patterns {
        for pattern in include_patterns {
            if glob_match(pattern, &path_str) {
                return true; // Process this file if it matches an include pattern
            }
        }
    }

    // Default to false if no patterns match
    false
}

fn normalize_path_for_match(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_string()
}

fn resolve_backup_root(cfg: &AppConfig) -> Option<PathBuf> {
    if cfg.watch_config.backup_enabled == Some(false) {
        return None;
    }

    let backup_dir = cfg
        .watch_config
        .backup_dir
        .as_deref()
        .unwrap_or(".doge/backup");
    let backup_path = Path::new(backup_dir);

    if backup_path.is_absolute() {
        Some(backup_path.to_path_buf())
    } else {
        Some(cfg.project_root.join(backup_path))
    }
}

fn is_path_under_dir(path: &Path, dir: &Path, project_root: &Path) -> bool {
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let abs_dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        project_root.join(dir)
    };

    abs_path.starts_with(&abs_dir)
}

// Simple glob matching function to check if a path matches a pattern
fn glob_match(pattern: &str, path: &str) -> bool {
    // Convert glob pattern to regex
    let re_pattern = regex::escape(pattern)
        .replace(r"\*\*", ".*") // ** matches any number of directories
        .replace(r"\*", "[^/]*") // * matches any number of non-slash characters
        .replace(r"\?", "."); // ? matches any single non-slash character

    if let Ok(re) = regex::Regex::new(&format!("^{}$", re_pattern)) {
        re.is_match(path)
    } else {
        false
    }
}

async fn write_watch_backup(
    path: &Path,
    original_content: &str,
    cfg: &AppConfig,
) -> Result<Option<PathBuf>> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let backup_root = match resolve_backup_root(cfg) {
        Some(root) => root,
        None => return Ok(None),
    };
    let relative = canonical.strip_prefix(&cfg.project_root).ok();

    let (backup_dir, file_name) = match relative {
        Some(rel) => {
            let parent = rel.parent().unwrap_or(Path::new(""));
            (backup_root.join(parent), rel.file_name())
        }
        None => (backup_root.clone(), canonical.file_name()),
    };

    let file_name = match file_name {
        Some(name) => name.to_string_lossy().to_string(),
        None => return Ok(None),
    };

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_file_name = format!("{}.bak.{}", file_name, timestamp);
    let backup_path = backup_dir.join(backup_file_name);

    fs::create_dir_all(&backup_dir)
        .await
        .with_context(|| format!("create backup directory {}", backup_dir.display()))?;
    fs::write(&backup_path, original_content)
        .await
        .with_context(|| format!("write backup {}", backup_path.display()))?;

    let keep = cfg.watch_config.backup_keep.unwrap_or(5);
    if keep > 0 {
        prune_watch_backups(&backup_dir, &file_name, keep).await?;
    }

    Ok(Some(backup_path))
}

async fn prune_watch_backups(backup_dir: &Path, file_name: &str, keep: usize) -> Result<()> {
    let prefix = format!("{}.bak.", file_name);
    let mut entries = match fs::read_dir(backup_dir).await {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };

    let mut candidates = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let entry_name = entry.file_name();
        let entry_name = entry_name.to_string_lossy();
        if !entry_name.starts_with(&prefix) {
            continue;
        }

        let modified = entry
            .metadata()
            .await
            .and_then(|meta| meta.modified())
            .unwrap_or(UNIX_EPOCH);
        candidates.push((modified, entry.path()));
    }

    if candidates.len() <= keep {
        return Ok(());
    }

    candidates.sort_by_key(|(modified, _)| *modified);
    let remove_count = candidates.len() - keep;
    for (_, path) in candidates.into_iter().take(remove_count) {
        let _ = fs::remove_file(&path).await;
    }

    Ok(())
}

// Debounce function to prevent rapid multiple calls for the same file
async fn debounce_file_change(
    path: PathBuf,
    llm_client: OpenAIClient,
    model: String,
    last_processed: Arc<Mutex<HashMap<PathBuf, Instant>>>,
    cfg: AppConfig,
) -> Result<()> {
    // Use debounce delay from configuration
    let debounce_delay = cfg.watch_config.debounce_delay_ms.unwrap_or(500);
    // Small delay to allow for more changes to accumulate
    sleep(Duration::from_millis(debounce_delay)).await;

    // Check rate limiting to avoid excessive API calls
    let rate_limit_duration = cfg.watch_config.rate_limit_duration_ms.unwrap_or(2000);
    {
        let last_processed_lock = utils::safe_std_lock(&*last_processed, "last_processed")?;
        if let Some(last_time) = last_processed_lock.get(&path)
            && last_time.elapsed() < Duration::from_millis(rate_limit_duration)
        {
            // Use configurable rate limit
            info!("Rate limited: skipping processing for {:?}", path);
            return Ok(());
        }
    }

    // Actually handle the file change
    if let Err(e) = handle_file_change(&llm_client, &model, path.clone(), &cfg).await {
        error!("Error handling file change: {}", e);
    } else {
        // Update the last processed time
        {
            let mut last_processed_lock = utils::safe_std_lock(&*last_processed, "last_processed")?;
            last_processed_lock.insert(path, Instant::now());
        }
    }

    Ok(())
}

async fn handle_file_change(
    llm_client: &OpenAIClient,
    model: &str,
    path: PathBuf,
    cfg: &AppConfig,
) -> Result<()> {
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

    // Use AI comment pattern from configuration (as a literal string, not regex)
    let ai_comment_pattern = cfg
        .watch_config
        .ai_comment_pattern
        .as_deref()
        .unwrap_or("// AI!:"); // Default to literal string instead of regex

    for (line_num, line) in content.lines().enumerate() {
        if let Some(instruction_start) = line.find(ai_comment_pattern) {
            // Extract the instruction after the pattern
            let instruction_part = &line[instruction_start + ai_comment_pattern.len()..];
            let instruction_text = instruction_part.trim().to_string();

            if !instruction_text.is_empty() {
                // Only process if there's actual instruction text
                info!(
                    "Found doge command in '{}' at line {}: -> {}",
                    path.display(),
                    line_num + 1,
                    instruction_text
                );

                let new_content =
                    execute_llm_task(llm_client, model, &content, &instruction_text, &path).await?;

                if new_content == content {
                    info!("No changes detected for {}", path.display());
                    return Ok(());
                }

                if let Some(backup_path) = write_watch_backup(&path, &content, cfg).await? {
                    info!("Backup saved to {}", backup_path.display());
                }
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

    // Determine language for syntax highlighting based on file extension
    let language = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    let user_prompt = format!(
        "Please modify the following file based on the instruction.\n\nFile: `{}`\n\nInstruction: {}\n\n```{}\n{}\n```",
        file_path.display(),
        instruction,
        language,
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
    // Extract content from markdown code block if present - match any language
    let re = Regex::new(r"```(?:[a-zA-Z0-9_-]*|)\n([\s\S]*?)\n```")?;
    if let Some(caps) = re.captures(&result_content)
        && let Some(code) = caps.get(1)
    {
        return Ok(code.as_str().to_string());
    }
    // Otherwise, return the whole content
    Ok(result_content)
}
