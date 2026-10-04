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
use tokio::io::AsyncWriteExt;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::{AppConfig, IGNORE_FILE};
use crate::llm::client_core::OpenAIClient;
use crate::llm::types::ChatMessage;
use crate::utils;

use std::sync::{Arc, Mutex};

#[derive(Default)]
struct WatchEntry {
    active: bool,
    finished: Option<Instant>,
}

type WatchEntries = Arc<Mutex<HashMap<PathBuf, WatchEntry>>>;

/// Release single-flight ownership even after errors or a dropped task.
struct WatchReservation {
    entries: WatchEntries,
    path: PathBuf,
}

impl WatchReservation {
    fn acquire(entries: &WatchEntries, path: &Path, rate_limit: Duration) -> Result<Option<Self>> {
        let mut locked = utils::safe_std_lock(entries, "watch_reservations")?;
        let entry = locked.entry(path.to_path_buf()).or_default();
        if entry.active
            || entry
                .finished
                .is_some_and(|time| time.elapsed() < rate_limit)
        {
            return Ok(None);
        }
        entry.active = true;
        Ok(Some(Self {
            entries: entries.clone(),
            path: path.to_path_buf(),
        }))
    }
}

impl Drop for WatchReservation {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.entries.lock()
            && let Some(entry) = entries.get_mut(&self.path)
        {
            entry.active = false;
            entry.finished = Some(Instant::now());
        }
    }
}

fn watch_tools(cfg: &AppConfig) -> Result<crate::tools::FsTools> {
    let store = crate::session::SessionStore::new(cfg.project_root.join(".doge/sessions"))?;
    let mut manager = crate::session::SessionManager::with_store(store);
    manager.create_session(Some("watch".into()))?;
    Ok(crate::tools::FsTools::new(
        Arc::new(tokio::sync::RwLock::new(None)),
        Arc::new(cfg.clone()),
    )
    .with_session_manager(Arc::new(Mutex::new(manager))))
}

// doge: Please translate the Japanese code comments in this file to English.
pub async fn run_watch_mode(cfg: AppConfig) -> Result<()> {
    info!("Running in watch mode. Watching for file changes...");

    // Load and parse ignore files (.gitignore, .dogeignore)
    let gitignore = load_gitignore()?;

    let llm_client =
        OpenAIClient::from_config(&cfg)?.context("LLM authentication required for watch mode")?;
    let model = cfg.model.clone();

    // Use Arc<Mutex<>> for thread-safe access to file processing tracking
    let last_processed: WatchEntries = Arc::new(Mutex::new(HashMap::new()));
    let tools = watch_tools(&cfg)?;

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
                            tools.clone(),
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
        .unwrap_or(Duration::from_secs(0))
        .as_secs();
    let backup_file_name = format!("{}.bak.{}.{}", file_name, timestamp, uuid::Uuid::now_v7());
    let backup_path = backup_dir.join(backup_file_name);

    fs::create_dir_all(&backup_dir)
        .await
        .with_context(|| format!("create backup directory {}", backup_dir.display()))?;
    let mut backup = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup_path)
        .await?;
    backup.write_all(original_content.as_bytes()).await?;
    backup.sync_all().await?;

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
    last_processed: WatchEntries,
    cfg: AppConfig,
    tools: crate::tools::FsTools,
) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("watch target {}", path.display()))?;
    let rate_limit = Duration::from_millis(cfg.watch_config.rate_limit_duration_ms.unwrap_or(2000));
    let Some(_reservation) = WatchReservation::acquire(&last_processed, &path, rate_limit)? else {
        info!(file = %path.display(), "watch event coalesced; current edits remain protected");
        return Ok(());
    };
    // Use debounce delay from configuration
    let debounce_delay = cfg.watch_config.debounce_delay_ms.unwrap_or(500);
    // Small delay to allow for more changes to accumulate
    sleep(Duration::from_millis(debounce_delay)).await;

    handle_file_change(&llm_client, &model, path, &cfg, &tools).await
}

async fn handle_file_change(
    llm_client: &OpenAIClient,
    model: &str,
    path: PathBuf,
    cfg: &AppConfig,
    tools: &crate::tools::FsTools,
) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }

    crate::tools::scope::ensure_in_scope(&path, cfg, &[])?;
    let before = crate::tools::mutation::read_text_snapshot_async(&path).await?;
    let content = before.content_or_empty().to_owned();

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

                commit_watch_candidate(&path, &before, &new_content, cfg, tools).await?;
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

async fn commit_watch_candidate(
    path: &Path,
    before: &crate::tools::mutation::MutationSnapshot,
    candidate: &str,
    cfg: &AppConfig,
    tools: &crate::tools::FsTools,
) -> Result<()> {
    use crate::tools::mutation::{
        MutationTargetReceipt, build_receipt, commit_text_candidate, mutation_changed,
    };
    if !mutation_changed(before, candidate) {
        return Ok(());
    }
    // Reject an already stale response before creating its backup. The shared
    // writer repeats the check immediately before commit.
    let current = crate::tools::mutation::read_text_snapshot_async(path).await?;
    anyhow::ensure!(
        current.state_matches(before),
        "watch conflict: file changed while model was pending; manual edits preserved; save again to retry"
    );
    if let Some(backup_path) = write_watch_backup(path, before.content_or_empty(), cfg).await? {
        info!("Backup saved to {}", backup_path.display());
    }
    let after = commit_text_candidate(path, before, candidate).await?;
    let receipt = build_receipt(
        crate::provenance::ChangeKind::FileWrite,
        path.to_path_buf(),
        before.clone(),
        after,
        MutationTargetReceipt::File,
    );
    let report = tools
        .finalize_mutation(
            receipt,
            crate::tools::FinalizeMutationOptions {
                record_undo: true,
                ..Default::default()
            },
        )
        .await;
    for warning in report.warnings {
        warn!(%warning, "watch mutation tracking warning");
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
            provider_state: None,
            role: "system".to_string(),
            content: Some(system_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        },
        ChatMessage {
            provider_state: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mutation::read_text_snapshot_async;

    fn fixture() -> (tempfile::TempDir, AppConfig, crate::tools::FsTools) {
        let temp = tempfile::tempdir().expect("watch fixture");
        let cfg = AppConfig {
            project_root: temp.path().canonicalize().expect("root"),
            ..Default::default()
        };
        let tools = watch_tools(&cfg).expect("watch tools");
        (temp, cfg, tools)
    }

    #[test]
    fn native_watcher_observes_file_changes() {
        let directory = tempfile::tempdir().expect("watch directory");
        let path = directory.path().join("watched.rs");
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut watcher: RecommendedWatcher =
            Watcher::new(sender, notify::Config::default()).expect("native watcher");
        watcher
            .watch(directory.path(), RecursiveMode::Recursive)
            .expect("watch directory");
        std::fs::write(&path, "fn main() {}\n").expect("watched file");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let event = receiver
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("file event")
                .expect("watch result");
            if matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_))
                && event
                    .paths
                    .iter()
                    .any(|observed| observed.file_name() == path.file_name())
            {
                break;
            }
        }
    }

    #[test]
    fn test_single_flight_releases_on_drop_and_limits_feedback_events() {
        let entries = WatchEntries::default();
        let path = Path::new("fixture.rs");
        let reservation = WatchReservation::acquire(&entries, path, Duration::ZERO)
            .expect("acquire")
            .expect("owner");
        assert!(
            WatchReservation::acquire(&entries, path, Duration::ZERO)
                .expect("duplicate")
                .is_none()
        );
        drop(reservation);
        assert!(
            WatchReservation::acquire(&entries, path, Duration::from_secs(60))
                .expect("feedback")
                .is_none()
        );
        assert!(
            WatchReservation::acquire(&entries, path, Duration::ZERO)
                .expect("retry")
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_watch_conflict_preserves_manual_edit_without_backup_or_tracking() -> Result<()> {
        let (_temp, cfg, tools) = fixture();
        let path = cfg.project_root.join("source.rs");
        fs::write(&path, "original").await?;
        let before = read_text_snapshot_async(&path).await?;
        fs::write(&path, "manual edit during inference").await?;
        let error = commit_watch_candidate(&path, &before, "stale candidate", &cfg, &tools)
            .await
            .expect_err("stale response");
        assert!(error.to_string().contains("manual edits preserved"));
        assert_eq!(
            fs::read_to_string(&path).await?,
            "manual edit during inference"
        );
        assert!(tools.undo_stack.read().await.is_empty());
        assert!(!resolve_backup_root(&cfg).expect("backup root").exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_watch_commit_tracks_change_and_supports_undo_noop_is_untracked() -> Result<()> {
        let (_temp, cfg, tools) = fixture();
        let path = cfg.project_root.join("source.rs");
        fs::write(&path, "original").await?;
        let before = read_text_snapshot_async(&path).await?;
        commit_watch_candidate(&path, &before, "original", &cfg, &tools).await?;
        assert!(tools.undo_stack.read().await.is_empty());
        commit_watch_candidate(&path, &before, "candidate", &cfg, &tools).await?;
        assert_eq!(fs::read_to_string(&path).await?, "candidate");
        let entry = tools
            .undo_stack
            .read()
            .await
            .peek_last()
            .expect("undo receipt");
        assert!(entry.change_id.is_some(), "committed provenance");
        crate::tools::undo::undo(&tools).await?;
        assert_eq!(fs::read_to_string(&path).await?, "original");
        Ok(())
    }

    #[tokio::test]
    async fn test_watch_backups_are_unique_within_same_timestamp() -> Result<()> {
        let (_temp, mut cfg, _tools) = fixture();
        cfg.watch_config.backup_keep = Some(10);
        let path = cfg.project_root.join("source.rs");
        fs::write(&path, "fixture").await?;
        let first = write_watch_backup(&path, "first", &cfg)
            .await?
            .expect("first backup");
        let second = write_watch_backup(&path, "second", &cfg)
            .await?
            .expect("second backup");
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(first).await?, "first");
        assert_eq!(fs::read_to_string(second).await?, "second");
        Ok(())
    }

    #[tokio::test]
    async fn test_watch_text_completion_length_cannot_become_replacement() -> Result<()> {
        use httptest::{Expectation, ServerBuilder, matchers::*, responders::*};
        let (_temp, cfg, tools) = fixture();
        let path = cfg.project_root.join("source.rs");
        let source = "// AI!: update\nfn original() {}\n";
        fs::write(&path, source).await?;
        let server = ServerBuilder::new()
            .bind_addr(([127, 0, 0, 1], 0).into())
            .run()?;
        server.expect(Expectation::matching(request::method_path("POST", "/v1/chat/completions")).times(1)
            .respond_with(json_encoded(serde_json::json!({"choices":[{"index":0,"finish_reason":"length","message":{"role":"assistant","content":"fn partial() {}"}}]}))));
        let client = OpenAIClient::new(server.url_str("/v1"), "fixture-only")?;
        assert!(
            handle_file_change(&client, "fixture", path.clone(), &cfg, &tools)
                .await
                .is_err()
        );
        assert_eq!(fs::read_to_string(path).await?, source);
        assert!(tools.undo_stack.read().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_watch_refusal_in_extracted_json_preserves_source() -> Result<()> {
        use httptest::{Expectation, ServerBuilder, matchers::*, responders::*};
        for fenced in [false, true] {
            let (_temp, cfg, tools) = fixture();
            let path = cfg.project_root.join("source.rs");
            let original = "// AI!: update\nfn original() {}\n";
            fs::write(&path, original).await?;
            let server = ServerBuilder::new()
                .bind_addr(([127, 0, 0, 1], 0).into())
                .run()?;
            let json = serde_json::json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"fn replacement() {}","refusal":"declined"}}]}).to_string();
            let body = if fenced {
                format!("```json\n{json}\n```")
            } else {
                format!("Fixture prefix\n{json}")
            };
            server.expect(
                Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                    .times(1)
                    .respond_with(status_code(200).body(body)),
            );
            let client = OpenAIClient::new(server.url_str("/v1"), "fixture-only")?;
            let error = handle_file_change(&client, "fixture", path.clone(), &cfg, &tools)
                .await
                .expect_err("refusal must not become a replacement");
            assert_eq!(
                error.downcast_ref::<crate::llm::LlmErrorKind>(),
                Some(&crate::llm::LlmErrorKind::Incomplete)
            );
            assert_eq!(fs::read_to_string(&path).await?, original);
            assert!(tools.undo_stack.read().await.is_empty());
        }
        Ok(())
    }
}
