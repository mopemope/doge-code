use crate::analysis::{ContextManager, RepoMap};
use crate::config::AppConfig;
use crate::session::{SessionData, SessionManager};
use crate::tools::execute;
use crate::tools::find_file;
use crate::tools::list;
use crate::tools::plan;
use crate::tools::read;
use crate::tools::read_many;
use crate::tools::remote_tools::RemoteToolManager;
use crate::tools::search_repomap;
use crate::tools::search_text;
use crate::tools::session_manager::SessionManagerWrapper;
use crate::tools::shell::{self, SharedShellSession};
use crate::tools::write;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

use crate::tools::memory::MemoryTools;
// ... imports ...

/// Options for [`FsTools::finalize_mutation`].
#[derive(Debug, Clone, Default)]
pub struct FinalizeMutationOptions {
    /// Push an undo entry for the committed receipt. Normal mutations use
    /// `true`; undo itself uses `false` so undo never re-pushes.
    pub record_undo: bool,
    /// Provenance `reverts_change_id` (set by undo to link the reverted
    /// change; `None` for ordinary mutations).
    pub reverts_change_id: Option<String>,
    /// Per-turn provenance attribution. Agent tool paths pass
    /// `runtime.attribution`; non-agent paths use the default (`None`).
    pub attribution: crate::provenance::ProvenanceAttribution,
}

/// Post-commit bookkeeping outcome.
///
/// Source mutation already succeeded; failures here are warnings only.
#[derive(Debug, Clone, Default)]
pub struct MutationFinalizeReport {
    pub change_id: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FsTools {
    search_repomap_tools: search_repomap::RepomapSearchTools,
    memory_tools: MemoryTools,
    repomap: Arc<RwLock<Option<RepoMap>>>,
    // ...
    session_manager_wrapper: SessionManagerWrapper,
    pub config: Arc<AppConfig>,
    remote_tool_manager: RemoteToolManager,
    execution_policy: crate::execution::ExecutionPolicy,
    pub context_manager: Arc<RwLock<ContextManager>>,
    pub(crate) review_registry: Arc<Mutex<Option<Arc<Mutex<crate::tools::review::ReviewCapture>>>>>,
    pub(crate) review_capture: Option<Arc<Mutex<crate::tools::review::ReviewCapture>>>,
    pub undo_stack: Arc<RwLock<crate::tools::undo::UndoStack>>,
    pub shell_session: SharedShellSession,
}

impl Default for FsTools {
    fn default() -> Self {
        Self::new(Arc::new(RwLock::new(None)), Arc::new(AppConfig::default()))
    }
}

impl FsTools {
    pub fn new(repomap: Arc<RwLock<Option<RepoMap>>>, config: Arc<AppConfig>) -> Self {
        Self {
            search_repomap_tools: search_repomap::RepomapSearchTools::new(),
            memory_tools: MemoryTools::new(config.clone()),
            context_manager: Arc::new(RwLock::new(ContextManager::new(repomap.clone()))),
            repomap,
            session_manager_wrapper: SessionManagerWrapper::new(None),
            config: config.clone(),
            remote_tool_manager: RemoteToolManager::new(config.clone()),
            execution_policy: crate::execution::ExecutionPolicy::new(config.clone()),
            review_registry: Arc::new(Mutex::new(None)),
            review_capture: None,
            undo_stack: Arc::new(RwLock::new(crate::tools::undo::UndoStack::new())),
            shell_session: SharedShellSession::new(
                config.project_root.clone(),
                config.command_timeout_ms,
            ),
        }
    }

    pub fn with_session_manager(mut self, session_manager: Arc<Mutex<SessionManager>>) -> Self {
        self.session_manager_wrapper = SessionManagerWrapper::new(Some(session_manager));
        self
    }

    /// Update the current session with tool call count
    pub fn update_session_with_tool_call_count(&self) -> Result<()> {
        self.session_manager_wrapper
            .update_session_with_tool_call_count()
    }

    /// Record a successful tool call in the current session
    pub fn record_tool_call_success(&self, tool_name: &str) -> Result<()> {
        self.session_manager_wrapper
            .record_tool_call_success(tool_name)
    }

    /// Record a failed tool call in the current session
    pub fn record_tool_call_failure(&self, tool_name: &str) -> Result<()> {
        self.session_manager_wrapper
            .record_tool_call_failure(tool_name)
    }

    /// Update the current session with lines edited count
    pub fn update_session_with_lines_edited(&self, lines_edited: u64) -> Result<()> {
        self.session_manager_wrapper
            .update_session_with_lines_edited(lines_edited)
    }

    /// Update the current session with a changed file path
    pub fn update_session_with_changed_file(&self, path: std::path::PathBuf) -> Result<()> {
        self.session_manager_wrapper
            .update_session_with_changed_file(path)
    }

    /// Update session with changed file if it is within the project root
    pub fn update_session_if_changed(&self, path: &std::path::Path) -> Result<()> {
        if let Ok(relative_path) = path.strip_prefix(&self.config.project_root) {
            self.update_session_with_changed_file(relative_path.to_path_buf())?;
        }
        Ok(())
    }

    /// Get current session data
    pub fn get_current_session(&self) -> Option<SessionData> {
        self.session_manager_wrapper.get_current_session()
    }

    /// Get files changed by the agent in the current session (project-root relative)
    pub fn get_session_changed_files(&self) -> Vec<std::path::PathBuf> {
        self.get_current_session()
            .map(|s| {
                s.changed_files
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get session info string
    pub fn get_session_info(&self) -> Option<String> {
        self.session_manager_wrapper.get_session_info()
    }

    pub fn ensure_current_session_id(&self) -> Result<String> {
        if let Some(session) = self.get_current_session() {
            return Ok(session.meta.id);
        }

        let session_manager = self
            .session_manager_wrapper
            .get_session_manager()
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No session manager"))?;

        let mut mgr = session_manager.lock().unwrap();
        if mgr.current_session.is_none() {
            mgr.create_session(None)?;
        }

        mgr.current_session
            .as_ref()
            .map(|s| s.meta.id.clone())
            .ok_or_else(|| anyhow::anyhow!("No current session"))
    }

    /// Get reference to remote tool manager
    pub fn get_remote_tool_manager(&self) -> &RemoteToolManager {
        &self.remote_tool_manager
    }

    /// Get reference to session manager wrapper
    pub fn get_session_manager_wrapper(&self) -> &SessionManagerWrapper {
        &self.session_manager_wrapper
    }

    /// Storage context for provenance and other per-session durable state.
    pub fn current_session_storage_context(&self) -> Option<crate::session::SessionStorageContext> {
        self.session_manager_wrapper
            .current_session_storage_context()
    }

    /// Mark a provenance recording failure on the current session.
    pub fn mark_current_session_provenance_failure(&self) -> Result<()> {
        self.session_manager_wrapper
            .mark_current_session_provenance_failure()
    }

    /// Persist the conversation-owned Observation Store snapshot.
    pub fn update_session_with_observations(
        &self,
        observations: crate::llm::observation::ObservationStore,
        unseen: std::collections::BTreeSet<String>,
    ) -> Result<()> {
        self.session_manager_wrapper
            .update_session_with_observations(observations, unseen)
    }

    /// Load the persisted Observation Store snapshot, if any.
    pub fn load_current_observations(
        &self,
    ) -> Option<(
        crate::llm::observation::ObservationStore,
        std::collections::BTreeSet<String>,
    )> {
        self.session_manager_wrapper.load_current_observations()
    }

    /// Legacy shell-gate compatibility shim (used by tests).
    ///
    /// Backed by `ExecutionPolicy`: under the deprecated `allowed_commands`
    /// fallback, shell operators/expansions deny the command instead of
    /// passing a prefix match. Under the new `[execution]` policy this only
    /// reflects whether shell execution itself is enabled (`allow_shell`),
    /// NOT whether any particular command string is safe — do not reuse this
    /// as a security check for new code. Prefer structured `execute_process`.
    pub fn is_command_allowed(&self, command: &str) -> bool {
        if self.config.allowed_commands.is_empty() && !self.config.execution_configured {
            return true;
        }
        self.execution_policy
            .check_legacy_shell_command(command)
            .is_ok()
    }

    pub fn execution_policy(&self) -> &crate::execution::ExecutionPolicy {
        &self.execution_policy
    }

    /// Update context with accessed file
    pub fn update_context(&self, path: std::path::PathBuf) {
        let cm = self.context_manager.clone();
        tokio::spawn(async move {
            cm.write().await.add_file(&path);
        });
    }

    /// Shared post-commit bookkeeping for every workspace text mutation.
    ///
    /// Must only be called after the source file mutation already succeeded.
    /// Never rolls back the source change: provenance/session/context
    /// failures are collected as warnings. Outside-project allowed paths
    /// skip session + provenance (tracking scope, not a failure) but still
    /// push undo entries.
    pub async fn finalize_mutation(
        &self,
        receipt: crate::tools::mutation::MutationReceipt,
        options: FinalizeMutationOptions,
    ) -> MutationFinalizeReport {
        let mut warnings: Vec<String> = Vec::new();
        let absolute = receipt.path.clone();

        tracing::info!(
            kind = ?receipt.kind,
            file = %absolute.display(),
            before_hash = receipt.before.content_hash.as_deref().unwrap_or("missing"),
            after_hash = receipt.after.content_hash.as_deref().unwrap_or("missing"),
            "mutation.commit"
        );

        // Outside-project scope check on canonical paths: receipts carry
        // the raw user-supplied path, which may contain `..` or symlinked
        // components that a lexical comparison would misclassify.
        let project_root = &self.config.project_root;
        let canonical_absolute = crate::tools::mutation::canonicalize_for_scope(&absolute);
        let canonical_root = crate::tools::mutation::canonicalize_for_scope(project_root);
        let outside_tracked_scope = !canonical_absolute.starts_with(&canonical_root);

        if outside_tracked_scope {
            self.capture_receipt(&receipt, None);
            // Allowed outside paths are a supported scope gap, not a failure.
            if options.record_undo {
                self.push_undo_for_receipt(&receipt, None).await;
            }
            warnings.push(
                "Mutation succeeded outside project root; provenance not recorded.".to_string(),
            );
            // Session + context stay project-scoped.
            self.update_context(absolute);
            return MutationFinalizeReport {
                change_id: None,
                warnings,
            };
        }

        // Inside project: provenance -> undo -> session -> context.
        let change_id = match crate::tools::provenance::record_committed_mutation_with_attribution(
            self,
            &receipt,
            options.reverts_change_id.clone(),
            &options.attribution,
        ) {
            Ok(Some(envelope)) => Some(envelope.event_id.clone()),
            Ok(None) => {
                // Relativization failed despite the canonical scope check
                // (e.g. an unresolvable path). Warn instead of silently
                // skipping so the gap stays visible.
                warnings.push(
                    "Source change committed but provenance was skipped: path could not be relativized under the project root.".to_string(),
                );
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "provenance.record_failed");
                let _ = self.mark_current_session_provenance_failure();
                warnings
                    .push("Source change committed but provenance recording failed.".to_string());
                None
            }
        };

        self.capture_receipt(&receipt, change_id.clone());
        if options.record_undo {
            self.push_undo_for_receipt(&receipt, change_id.clone())
                .await;
        }

        // Session bookkeeping (project-relative only).
        if let Ok(rel) =
            crate::analysis::normalize_relative_path(&canonical_root, &canonical_absolute)
        {
            if let Err(e) = self.update_session_with_changed_file(std::path::PathBuf::from(&rel)) {
                warnings.push(format!("Session changed-file update failed: {e}"));
            }
            let lines = (receipt.lines_added + receipt.lines_removed) as u64;
            if lines > 0
                && let Err(e) = self.update_session_with_lines_edited(lines)
            {
                warnings.push(format!("Session lines-edited update failed: {e}"));
            }
        }
        self.update_context(absolute);

        MutationFinalizeReport {
            change_id,
            warnings,
        }
    }

    async fn push_undo_for_receipt(
        &self,
        receipt: &crate::tools::mutation::MutationReceipt,
        change_id: Option<String>,
    ) {
        let before_state = match (&receipt.before.exists, &receipt.before.content) {
            (false, _) => crate::tools::undo::UndoFileState::Missing,
            (true, Some(content)) => crate::tools::undo::UndoFileState::Text {
                content: content.clone(),
            },
            (true, None) => crate::tools::undo::UndoFileState::Text {
                content: String::new(),
            },
        };
        let expected_after = crate::provenance::FileStateEvidence {
            exists: receipt.after.exists,
            content_hash: receipt.after.content_hash.clone(),
            byte_len: receipt.after.byte_len,
        };
        let entry =
            crate::tools::undo::BackupEntry {
                entry_id: uuid::Uuid::now_v7().to_string(),
                path: receipt.path.clone(),
                before: before_state,
                expected_after,
                expected_path: receipt.after.resolved_path.clone().unwrap_or_else(|| {
                    crate::tools::mutation::canonicalize_for_scope(&receipt.path)
                }),
                change_id,
            };
        self.undo_stack.write().await.push_entry(entry);
    }

    pub fn fs_list(
        &self,
        path: &str,
        max_depth: Option<usize>,
        pattern: Option<&str>,
        options: list::FsListOptions,
    ) -> Result<list::FsListResponse> {
        list::fs_list(path, max_depth, pattern, &self.config, options)
    }

    async fn update_read_context(
        &self,
        paths: impl IntoIterator<Item = PathBuf>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        let mut context = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled)),
            context = self.context_manager.write() => context,
        };
        super::async_io::check(Some(cancel))?;
        for path in paths {
            context.add_file(&path);
        }
        Ok(())
    }

    pub async fn fs_read_async(
        &self,
        path: String,
        opts: read::FsReadOptions,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<read::FsReadResult> {
        let result =
            read::fs_read_async(path.clone(), opts, self.config.clone(), cancel.clone()).await?;
        self.update_read_context([PathBuf::from(path)], &cancel)
            .await?;
        Ok(result)
    }

    pub async fn fs_read_many_files_async(
        &self,
        paths: Vec<String>,
        exclude: Option<Vec<String>>,
        recursive: Option<bool>,
        options: read_many::FsReadManyOptions,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<read_many::FsReadManyResponse> {
        let result = read_many::fs_read_many_files_async(
            paths,
            exclude,
            recursive,
            self.config.clone(),
            options,
            cancel.clone(),
        )
        .await?;
        self.update_read_context(
            result.files.iter().map(|file| PathBuf::from(&file.path)),
            &cancel,
        )
        .await?;
        Ok(result)
    }

    pub fn fs_read(&self, path: &str, opts: read::FsReadOptions) -> Result<read::FsReadResult> {
        match read::fs_read(path, opts, &self.config) {
            Ok(result) => {
                self.update_context(PathBuf::from(path));
                Ok(result)
            }
            Err(e) => Err(e),
        }
    }

    pub fn fs_read_many_files(
        &self,
        paths: Vec<String>,
        exclude: Option<Vec<String>>,
        recursive: Option<bool>,
        options: read_many::FsReadManyOptions,
    ) -> Result<read_many::FsReadManyResponse> {
        match read_many::fs_read_many_files(paths, exclude, recursive, &self.config, options) {
            Ok(result) => {
                for file in &result.files {
                    self.update_context(PathBuf::from(&file.path));
                }
                Ok(result)
            }
            Err(e) => Err(e),
        }
    }

    pub fn search_text(
        &self,
        search_pattern: &str,
        file_glob: Option<&str>,
    ) -> Result<Vec<(PathBuf, usize, String)>> {
        self.search_text_with_options(
            search_pattern,
            file_glob,
            search_text::SearchTextOptions::default(),
        )
        .map(|result| result.rows)
    }

    pub async fn search_text_with_options_async(
        &self,
        search_pattern: String,
        file_glob: Option<String>,
        options: search_text::SearchTextOptions,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<search_text::SearchTextResult> {
        search_text::search_text_with_options_async(
            search_pattern,
            file_glob,
            options,
            self.config.clone(),
            cancel,
        )
        .await
    }

    pub fn search_text_with_options(
        &self,
        search_pattern: &str,
        file_glob: Option<&str>,
        options: search_text::SearchTextOptions,
    ) -> Result<search_text::SearchTextResult> {
        search_text::search_text_with_options(search_pattern, file_glob, options, &self.config)
    }

    pub async fn fs_write(&self, path: &str, content: &str) -> Result<write::FsWriteResult> {
        self.fs_write_with_attribution(
            path,
            content,
            &crate::provenance::ProvenanceAttribution::none(),
        )
        .await
    }

    pub async fn fs_write_with_attribution(
        &self,
        path: &str,
        content: &str,
        attribution: &crate::provenance::ProvenanceAttribution,
    ) -> Result<write::FsWriteResult> {
        let mut exec = write::fs_write_with_receipt(path, content, &self.config)?;
        if let Some(receipt) = exec.receipt.take() {
            let report = self
                .finalize_mutation(
                    receipt,
                    FinalizeMutationOptions {
                        record_undo: true,
                        reverts_change_id: None,
                        attribution: attribution.clone(),
                    },
                )
                .await;
            exec.result.warnings.extend(report.warnings);
        }
        Ok(exec.result)
    }

    /// Structured process execution (no shell). Returns the serialized
    /// `ProcessResult` JSON (with `ok == success`).
    pub async fn execute_process(
        &self,
        params: crate::execution::ExecuteProcessParams,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<String> {
        crate::execution::warn_if_dual_config(
            self.config.execution_configured,
            !self.config.allowed_commands.is_empty(),
        );
        match crate::execution::run_process(params.into_request(), &self.config, cancel).await {
            Ok(result) => Ok(serde_json::to_string(&result.to_json())?),
            Err(e) => Err(e),
        }
    }

    pub async fn execute_bash(&self, command: &str) -> Result<String> {
        self.execute_bash_with_cancel(command, None).await
    }

    pub async fn execute_bash_with_cancel(
        &self,
        command: &str,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<String> {
        if cancel
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
        }
        crate::execution::warn_if_dual_config(
            self.config.execution_configured,
            !self.config.allowed_commands.is_empty(),
        );
        // Policy gate (shell on/off + legacy allowlist without shell bypass).
        match self.execution_policy.check_legacy_shell_command(command) {
            Err(denial) => {
                tracing::warn!("Command denied by execution policy: {}", denial.message());
                let result = execute::ExecuteBashResult::simple(
                    String::new(),
                    denial.message(),
                    None,
                    false,
                );
                return Ok(serde_json::to_string(&result)?);
            }
            Ok(Some(fast_req)) => {
                // Safe simple command: run without `bash -c` via the new runner.
                match crate::execution::run_process(fast_req, &self.config, cancel).await {
                    Ok(result) => {
                        let legacy = execute::ExecuteBashResult {
                            stdout: result.stdout,
                            stderr: result.stderr,
                            exit_code: result.exit_code,
                            success: result.success,
                            output_truncated: result.output_truncated,
                            warnings: result.warnings,
                            timed_out: result.status == crate::execution::ProcessStatus::TimedOut,
                        };
                        return Ok(serde_json::to_string(&legacy)?);
                    }
                    Err(error) => {
                        if error.downcast_ref::<crate::llm::LlmErrorKind>().is_some() {
                            return Err(error);
                        }
                        let result = execute::ExecuteBashResult::simple(
                            String::new(),
                            error.to_string(),
                            None,
                            false,
                        );
                        return Ok(serde_json::to_string(&result)?);
                    }
                }
            }
            Ok(None) => {}
        }

        match execute::execute_bash_with_cancel(command, &self.config, cancel).await {
            Ok(result) => Ok(serde_json::to_string(&result)?),
            Err(error) => {
                if error.downcast_ref::<crate::llm::LlmErrorKind>().is_some() {
                    return Err(error);
                }
                // Return a structured result with the error details.
                let result = execute::ExecuteBashResult::simple(
                    String::new(),
                    error.to_string(),
                    None,
                    false,
                );
                Ok(serde_json::to_string(&result)?)
            }
        }
    }

    pub async fn execute_shell(&self, command: &str) -> Result<String> {
        self.execute_shell_with_cancel(command, None).await
    }

    pub async fn execute_shell_with_cancel(
        &self,
        command: &str,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<String> {
        if cancel
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
        }
        crate::execution::warn_if_dual_config(
            self.config.execution_configured,
            !self.config.allowed_commands.is_empty(),
        );
        // Single gate for both policies: shell on/off, plus — under the
        // legacy `allowed_commands` fallback — only provably-simple matching
        // commands run. A `cargo …; rm …` string must not pass on a `cargo`
        // prefix, so shell operators/expansions are denied there just like
        // for `execute_bash`. The fast-path conversion (if any) is
        // intentionally unused: the command runs in the persistent shell
        // session to preserve `cd`/`export`/state.
        if let Err(denial) = self.execution_policy.check_legacy_shell_command(command) {
            tracing::warn!(
                "Shell command denied by execution policy: {}",
                denial.message()
            );
            let result =
                shell::ExecuteShellResult::simple(String::new(), denial.message(), None, false);
            return Ok(serde_json::to_string(&result)?);
        }

        match self.shell_session.exec_with_cancel(command, cancel).await {
            Ok(result) => Ok(result),
            Err(error) => {
                if error.downcast_ref::<crate::llm::LlmErrorKind>().is_some() {
                    return Err(error);
                }
                let result = shell::ExecuteShellResult::simple(
                    String::new(),
                    error.to_string(),
                    None,
                    false,
                );
                Ok(serde_json::to_string(&result)?)
            }
        }
    }

    /// Finds files in the project based on a filename or pattern.
    ///
    /// This method allows the LLM agent to search for files within the project
    /// directory. It supports searching by full filename, partial name, or glob
    /// patterns.
    ///
    /// # Arguments
    ///
    /// * `filename` - The filename or pattern to search for.
    ///
    /// # Returns
    ///
    /// A `Result` containing:
    /// - `Ok(find_file::FindFileResult)`: A struct with a list of matching file paths.
    /// - `Err(anyhow::Error)`: An error if the search could not be completed.
    ///
    /// # Examples
    ///
    /// To find a file by its exact name:
    /// ```ignore
    /// let result = fs_tools.find_file("main.rs").await?;
    /// ```
    ///
    /// To find files matching a glob pattern:
    /// ```ignore
    /// let result = fs_tools.find_file("*.rs").await?;
    /// ```
    ///
    /// To find files with a partial name match:
    /// ```ignore
    /// let result = fs_tools.find_file("main").await?;
    /// ```
    pub async fn find_file(&self, filename: &str) -> Result<find_file::FindFileResult> {
        self.find_file_with_options(filename, find_file::FindFileOptions::default())
            .await
    }

    pub async fn find_file_with_options(
        &self,
        filename: &str,
        options: find_file::FindFileOptions,
    ) -> Result<find_file::FindFileResult> {
        find_file::find_file_with_options(
            find_file::FindFileArgs {
                filename: filename.to_string(),
            },
            &self.config,
            options,
        )
        .await
    }

    pub async fn search_repomap(
        &self,
        args: search_repomap::SearchRepomapArgs,
    ) -> Result<search_repomap::SearchRepomapResponse> {
        // Use a more robust approach to handle potential RwLock poisoning
        let repomap_guard = self.repomap.read().await;
        if let Some(map) = &*repomap_guard {
            return self
                .search_repomap_tools
                .search_repomap(map, args, &self.config.project_root)
                .await;
        }
        drop(repomap_guard);

        // The shared map is not populated yet (initial build still running).
        // Fall back to an on-demand build (cache-backed) instead of failing.
        tracing::info!("shared repomap unavailable; building on demand");
        let map = crate::analysis::ensure_repomap_ready(&self.repomap, &self.config.project_root)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "repomap is not ready yet (initial build still running and on-demand build failed: {e}); retry in a few seconds or use `search_text`/`fs_list` meanwhile"
                )
            })?;
        self.search_repomap_tools
            .search_repomap(&map, args, &self.config.project_root)
            .await
    }

    pub fn plan_write(
        &self,
        items: Vec<plan::PlanItem>,
        mode: plan::PlanWriteMode,
    ) -> Result<plan::PlanWriteResult> {
        self.plan_write_with_attribution(
            items,
            mode,
            &crate::provenance::ProvenanceAttribution::none(),
        )
    }

    pub fn plan_write_with_attribution(
        &self,
        items: Vec<plan::PlanItem>,
        mode: plan::PlanWriteMode,
        attribution: &crate::provenance::ProvenanceAttribution,
    ) -> Result<plan::PlanWriteResult> {
        let session_id = self.ensure_current_session_id()?;
        let changed_files = self
            .get_current_session()
            .map(|s| s.changed_files)
            .unwrap_or_default();
        let before_items = plan::plan_read(&session_id, &self.config)
            .map(|p| p.items)
            .unwrap_or_default();
        // Current requirement ids for validation (event history is source).
        // Single snapshot reused for validation + warnings (avoids duplicate I/O).
        let loaded_requirements = match crate::tools::provenance::load_current_events(self) {
            Ok(Some(loaded)) => Some(loaded),
            _ => None,
        };
        let (valid_requirement_ids, withdrawn_ids) = match &loaded_requirements {
            Some(loaded) => {
                let state = crate::provenance::requirements::current_requirements(&loaded.events);
                let valid = state.items.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
                let withdrawn = state
                    .items
                    .iter()
                    .filter(|r| r.status == crate::provenance::types::RequirementStatus::Withdrawn)
                    .map(|r| r.id.clone())
                    .collect::<Vec<_>>();
                (valid, withdrawn)
            }
            None => (Vec::new(), Vec::new()),
        };
        // Core validates requirement links before persisting (unknown ids fail).
        let mut result = plan::plan_write_from_base_path(
            items,
            mode,
            &session_id,
            &self.config.project_root,
            &self.config,
            Some(&changed_files),
            Some(&valid_requirement_ids),
        )?;

        let mut warnings = Vec::new();
        if self
            .get_current_session()
            .is_some_and(|s| s.provenance_incomplete)
        {
            warnings.push(
                "Provenance trace is incomplete because one or more event writes failed."
                    .to_string(),
            );
        }

        // Soft warning for withdrawn links (never blocks).
        warnings.extend(plan::withdrawn_requirement_warnings(
            &result.plan.items,
            &withdrawn_ids,
        ));

        if result.changed {
            let transitions =
                crate::tools::provenance::diff_plan_transitions(&before_items, &result.plan.items);
            if !transitions.is_empty() {
                match crate::tools::provenance::record_plan_changed_with_attribution(
                    self,
                    transitions.clone(),
                    attribution,
                ) {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "provenance.record_failed");
                        let _ = self.mark_current_session_provenance_failure();
                        warnings.push(
                            "Plan was saved, but provenance recording failed; trace is incomplete."
                                .to_string(),
                        );
                    }
                }
            }
            // Completion warnings: newly completed items with active linked
            // changes that no successful verification has observed. Never
            // blocks completion; research-only items with no linked changes
            // produce no warning.
            warnings.extend(crate::tools::provenance::plan_completion_warnings(
                self,
                &transitions,
            ));
            // Soft warning: committed changes without a requirement link.
            warnings.extend(crate::tools::provenance::plan_requirement_link_warnings(
                self,
                &transitions,
            ));
        }

        result.warnings = warnings;
        // Canonical semantic delta, computed from the same before/after
        // snapshots used for provenance transitions.
        result.delta = plan::summarize_plan_changes(&before_items, &result.plan.items);
        Ok(result)
    }

    pub fn plan_read(&self) -> Result<plan::PlanList> {
        let session_id = self.ensure_current_session_id()?;
        plan::plan_read(&session_id, &self.config)
    }

    pub async fn call_remote_tool(
        &self,
        alias: &str,
        args: &serde_json::Value,
        cancel_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<Option<crate::tools::remote_tools::RemoteToolOutcome>> {
        self.remote_tool_manager
            .call_remote_tool(alias, args, cancel_token)
            .await
    }
    pub async fn read_memory(&self, key: &str) -> Result<String> {
        self.memory_tools.read_memory(key).await
    }

    pub async fn write_memory(
        &self,
        key: &str,
        content: &str,
        tags: Option<Vec<String>>,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        self.memory_tools
            .write_memory(key, content, tags, metadata)
            .await
    }

    pub async fn search_memory(
        &self,
        query: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<String> {
        self.memory_tools.search_memory(query, tags).await
    }

    pub async fn list_memories(&self) -> Result<String> {
        self.memory_tools.list_memories().await
    }
    pub async fn doc_generate(
        &self,
        path: &str,
        symbol: Option<&str>,
        client: crate::llm::OpenAIClient,
        model: &str,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<String> {
        crate::tools::doc::doc_generate(
            path,
            symbol,
            client,
            model,
            &self.config.project_root,
            self.repomap.clone(),
            cancel,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use anyhow::Result;

    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_execute_bash_with_permissions_allowed() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["echo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

        // This should succeed because "echo" is in the allowed list
        let result = fs_tools.execute_bash("echo 'hello world'").await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_with_permissions_not_allowed() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["echo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

        // This should return a JSON string with success = false because "rm" is not in the allowed list
        let result_str = fs_tools.execute_bash("rm -rf /").await.unwrap();
        let result: execute::ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
        assert!(!result.success);
        assert!(result.stderr.contains("not allowed"));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_with_permissions_no_config() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config without allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec![], // Empty list means all commands are allowed
            ..Default::default()
        };

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

        // This should be allowed because the allowed_commands list is empty
        let result = fs_tools.execute_bash("echo 'hello world'").await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_exact_match() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Exact match should be allowed
        assert!(fs_tools.is_command_allowed("cargo"));

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_prefix_match() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Prefix match should be allowed
        assert!(fs_tools.is_command_allowed("cargo build"));

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_not_allowed() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Command not in the allowed list should not be allowed
        assert!(!fs_tools.is_command_allowed("rm"));

        Ok(())
    }

    // Additional tests for edge cases in allowed_commands functionality
    #[tokio::test]
    async fn test_is_command_allowed_partial_match_edge_case() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // "carg" should not match "cargo" (partial match without space should not be allowed)
        assert!(!fs_tools.is_command_allowed("carg"));

        // "cargox" should not match "cargo" (extra characters without space should not be allowed)
        assert!(!fs_tools.is_command_allowed("cargox"));

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_space_separation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["git".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Valid commands with proper space separation should be allowed
        assert!(fs_tools.is_command_allowed("git status"));
        assert!(fs_tools.is_command_allowed("ls -la"));

        // Commands with no space after should not be allowed
        assert!(!fs_tools.is_command_allowed("gitstatus"));

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_complex_commands() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with complex allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo build".to_string(), "git status".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Commands matching the specific allowed commands should be allowed
        assert!(fs_tools.is_command_allowed("cargo build"));
        assert!(fs_tools.is_command_allowed("git status"));

        // Different commands should not be allowed
        assert!(!fs_tools.is_command_allowed("cargo test"));
        assert!(!fs_tools.is_command_allowed("git commit"));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_complex_allowed_command() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with a complex allowed command
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["echo 'hello world'".to_string()],
            ..Default::default()
        };

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

        // This should succeed because the exact command is allowed
        let result = fs_tools.execute_bash("echo 'hello world'").await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_with_empty_allowed_commands() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with no allowed commands (should allow all)
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec![],
            ..Default::default()
        };

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

        // All commands should be allowed when allowed_commands list is empty
        let result = fs_tools.execute_bash("echo 'test'").await;
        assert!(result.is_ok());

        let result = fs_tools.execute_bash("ls -la").await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_update_session_if_changed() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().join("project");
        std::fs::create_dir(&project_root)?;

        let cfg = AppConfig {
            project_root: project_root.clone(),
            ..Default::default()
        };

        // Ensure session directory exists
        let session_dir = project_root.join(".doge/sessions");
        std::fs::create_dir_all(&session_dir)?;

        let store = crate::session::SessionStore::new(session_dir)?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));

        // Initialize a session
        {
            let mut mgr = session_manager.lock().unwrap();
            mgr.create_session(None)?;
        }

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager.clone());

        // Test file inside project root
        let file_path = project_root.join("src/main.rs");
        if let Err(e) = fs_tools.update_session_if_changed(&file_path) {
            panic!("Failed inside project root update: {}", e);
        }

        // Verify session updated
        {
            let mgr = session_manager.lock().unwrap();
            let session = mgr.current_session.as_ref().unwrap();
            assert!(session.changed_files.contains(&"src/main.rs".to_string()));
        }

        // Test file outside project root
        let outside_path = temp_dir.path().join("outside.rs");
        // This should pass silently (Ok(())) but NOT update session
        if let Err(e) = fs_tools.update_session_if_changed(&outside_path) {
            panic!("Failed outside project root update: {}", e);
        }

        // Verify session NOT updated with outside path
        {
            let mgr = session_manager.lock().unwrap();
            let session = mgr.current_session.as_ref().unwrap();
            assert_eq!(session.changed_files.len(), 1);
            assert!(session.changed_files.contains(&"src/main.rs".to_string()));
        }

        Ok(())
    }
}
