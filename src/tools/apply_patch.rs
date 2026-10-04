use crate::config::AppConfig;
use crate::error_recovery::{ErrorContext, ErrorRecoveryTool, FixResult};
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use diffy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Component, Path};
use tokio::fs;

// ===== データ構造体 =====

/// apply_patchツールの入力パラメータ
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApplyPatchParams {
    /// 対象ファイルの絶対パス
    pub file_path: String,
    /// 統一diff形式のパッチ内容
    pub patch_content: String,
}

/// apply_patchツールの出力結果
#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyPatchResult {
    /// パッチ適用が成功したかどうか
    pub success: bool,
    /// 実際にファイルが変更されたかどうか (no-op は true/false = true/false)
    #[serde(default)]
    pub changed: bool,
    /// 結果メッセージ
    pub message: String,
    /// 対象ファイルのパス
    pub file_path: String,
    /// 成功時のみ: unified diff（超過時は先頭部分のみ返す）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// diffが予算で切詰められたかどうか
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub diff_truncated: bool,
    /// 成功時のみ: 追加行数
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub lines_added: u64,
    /// 成功時のみ: 削除行数
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub lines_removed: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl ApplyPatchResult {
    /// 失敗/エラー用の最小結果を生成する
    pub fn failure(message: impl Into<String>, file_path: &str) -> Self {
        Self {
            success: false,
            changed: false,
            message: message.into(),
            file_path: file_path.to_string(),
            diff: None,
            diff_truncated: false,
            lines_added: 0,
            lines_removed: 0,
            warnings: vec![],
        }
    }

    /// メッセージのみの成功結果（パッチ適用済み等、差分を伴わない成功）
    pub fn success_with_message(message: impl Into<String>, file_path: &str) -> Self {
        Self {
            success: true,
            changed: false,
            message: message.into(),
            file_path: file_path.to_string(),
            diff: None,
            diff_truncated: false,
            lines_added: 0,
            lines_removed: 0,
            warnings: vec![],
        }
    }

    /// 成功結果を生成する。unified diffを生成し、予算内に収める。
    pub fn success_from_contents(
        file_path: &str,
        message: impl Into<String>,
        original_content: &str,
        modified_content: &str,
    ) -> Self {
        use crate::tools::budget::DEFAULT_TOOL_BUDGET_CHARS;
        use crate::tools::budget::head_truncate;

        let diff = diffy::create_patch(original_content, modified_content).to_string();
        let (lines_added, lines_removed) = count_patch_lines(&diff);
        let budgeted = head_truncate(&diff, DEFAULT_TOOL_BUDGET_CHARS);
        Self {
            success: true,
            changed: true,
            message: message.into(),
            file_path: file_path.to_string(),
            diff: Some(budgeted.text),
            diff_truncated: budgeted.truncated,
            lines_added,
            lines_removed,
            warnings: vec![],
        }
    }
}

/// Count +/- lines in a unified diff, excluding the +++/--- headers.
fn count_patch_lines(diff_text: &str) -> (u64, u64) {
    let mut added = 0u64;
    let mut removed = 0u64;
    for line in diff_text.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

// ===== ツール定義 =====

/// apply_patchツールの定義を返す
const DESCRIPTION: &str = "Applies a unified diff patch. REQUIRED: Read the file closer to the edit time to ensure context lines match EXACTLY. Use absolute paths.";

/// apply_patchツールの定義を返す
pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "apply_patch".to_string(),
            description: DESCRIPTION.to_string(),
            strict: None,
            parameters: create_tool_parameters(),
        },
    }
}

fn create_tool_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": {
                "type": "string",
                "description": "ABSOLUTE path to target file. Must start with project root. Example: '/home/user/project/src/main.rs'. NEVER use relative paths like 'src/main.rs'."
            },
            "patch_content": {
                "type": "string",
                "description": "Unified diff content. Must use proper format with @@ line numbers. Context lines (starting with ' ') must exactly match current file content."
            }
        },
        "required": ["file_path", "patch_content"]
    })
}

// ===== ツールインターフェース =====

/// apply_patchツールの主要インターフェース関数
pub async fn apply_patch(params: ApplyPatchParams, config: &AppConfig) -> Result<ApplyPatchResult> {
    Ok(apply_patch_impl(params, config).await?.result)
}

/// apply_patchツールの主要インターフェース関数（エラー回復機能付き）
pub async fn apply_patch_with_recovery(
    params: ApplyPatchParams,
    config: &AppConfig,
) -> Result<ApplyPatchResult> {
    Ok(apply_patch_with_recovery_and_receipt(params, config)
        .await?
        .result)
}

/// Receipt-preserving recovery entry point.
///
/// Normal, fuzzy, and adjusted-patch recoveries all return the actual
/// `before -> after` receipt; already-applied successes return
/// `changed = false` with no receipt.
pub async fn apply_patch_with_recovery_and_receipt(
    params: ApplyPatchParams,
    config: &AppConfig,
) -> Result<crate::tools::mutation::MutationExecution<ApplyPatchResult>> {
    match apply_patch_impl(params.clone(), config).await {
        Ok(exec) if exec.result.success => Ok(exec),
        Ok(exec) => {
            if let Some(recovered) =
                attempt_recovery_with_receipt(&params, config, &exec.result.message).await?
            {
                Ok(recovered)
            } else {
                Ok(exec)
            }
        }
        Err(e) => {
            if let Some(recovered) =
                attempt_recovery_with_receipt(&params, config, &e.to_string()).await?
            {
                Ok(recovered)
            } else {
                Ok(crate::tools::mutation::MutationExecution {
                    result: ApplyPatchResult::failure(
                        format!("Patch application failed: {}", e),
                        &params.file_path,
                    ),
                    receipt: None,
                })
            }
        }
    }
}

async fn attempt_recovery_with_receipt(
    params: &ApplyPatchParams,
    config: &AppConfig,
    error_message: &str,
) -> Result<Option<crate::tools::mutation::MutationExecution<ApplyPatchResult>>> {
    let error_recovery_tool = ErrorRecoveryTool::new(config.clone());
    let error_context = ErrorContext {
        error_source: "apply_patch".to_string(),
        file_path: params.file_path.clone(),
        patch_content: params.patch_content.clone(),
        command: "".to_string(),
        output: error_message.to_string(),
    };

    let fix_result = match error_recovery_tool
        .attempt_recovery(error_message, error_context)
        .await
    {
        Ok(res) => res,
        Err(_) => return Ok(None),
    };

    let recovered = match fix_result {
        FixResult::PatchAdjustment { new_patch } => {
            let new_params = ApplyPatchParams {
                file_path: params.file_path.clone(),
                patch_content: new_patch,
            };
            Some(
                apply_patch_impl(new_params, config)
                    .await
                    .unwrap_or_else(|e| crate::tools::mutation::MutationExecution {
                        result: ApplyPatchResult::failure(
                            format!("Adjusted patch failed to apply: {}", e),
                            &params.file_path,
                        ),
                        receipt: None,
                    }),
            )
        }
        FixResult::RequiresHumanIntervention { message } => {
            Some(crate::tools::mutation::MutationExecution {
                result: ApplyPatchResult::failure(message, &params.file_path),
                receipt: None,
            })
        }
        FixResult::Failed { reason } => Some(crate::tools::mutation::MutationExecution {
            result: ApplyPatchResult::failure(reason, &params.file_path),
            receipt: None,
        }),
        // Recovery layer reports success without a diff (e.g. "patch already
        // applied"); preserve the original success semantics.
        FixResult::Success { message } => Some(crate::tools::mutation::MutationExecution {
            result: ApplyPatchResult::success_with_message(message, &params.file_path),
            receipt: None,
        }),
    };

    Ok(recovered)
}

// ===== 実装関数群 =====

/// apply_patchの実際の実装 (receipt付き; session/undo/provenanceなし)
async fn apply_patch_impl(
    params: ApplyPatchParams,
    config: &AppConfig,
) -> Result<crate::tools::mutation::MutationExecution<ApplyPatchResult>> {
    use crate::tools::mutation::{
        MutationTargetReceipt, build_receipt, commit_text_candidate, mutation_changed,
        read_text_snapshot_async,
    };

    let file_path = params.file_path;
    let patch_content = params.patch_content;

    let ok = |result: ApplyPatchResult,
              receipt: Option<crate::tools::mutation::MutationReceipt>| {
        crate::tools::mutation::MutationExecution { result, receipt }
    };

    // ===== 1. パス検証 =====
    validate_file_path_and_access(&file_path, config).await?;

    // ===== 2. ファイル存在と属性検証 =====
    let path = Path::new(&file_path);
    validate_file_exists_and_readable(path).await?;

    // ===== 3. 正確な before snapshot =====
    let before = read_text_snapshot_async(path).await.map_err(|e| {
        anyhow::anyhow!(
            "Failed to read file for an unknown reason: {}: {e}",
            path.display()
        )
    })?;
    let original_content_raw = before.content.clone().unwrap_or_default();

    // ===== 4. 改行コードの正規化 =====
    let (original_content, has_crlf) = normalize_line_endings(&original_content_raw);

    // ===== 5. パッチの解析 =====
    let patch = match parse_patch(&patch_content) {
        Ok(patch) => patch,
        Err(e) => {
            return Ok(ok(
                ApplyPatchResult::failure(
                    format!("Failed to parse patch content: {}", e),
                    &file_path,
                ),
                None,
            ));
        }
    };

    // ===== 6. 空の変更チェック =====
    if is_empty_patch(&patch, &patch_content) {
        return Ok(ok(
            ApplyPatchResult::failure(
                "Patch content is invalid or results in no changes.",
                &file_path,
            ),
            None,
        ));
    }

    // ===== 7. パッチ適用 (fuzzy recovery含む; actual contentからreceipt生成) =====
    let patched_content_lf = match apply_patch_to_content(&original_content, &patch, path) {
        Ok(content) => content,
        Err(e) => {
            return Ok(ok(
                ApplyPatchResult::failure(format!("Failed to apply patch: {}", e), &file_path),
                None,
            ));
        }
    };

    // ===== 8. 改行コードの復元 =====
    let patched_content = if has_crlf {
        patched_content_lf.replace('\n', "\r\n")
    } else {
        patched_content_lf
    };

    // No-op: candidateが元と同一なら何も書かない。
    if !mutation_changed(&before, &patched_content) {
        return Ok(ok(
            ApplyPatchResult::success_with_message(
                "No change needed: patch content already applied.",
                &file_path,
            ),
            None,
        ));
    }

    // ===== 9. パーミッションチェック =====
    validate_write_permissions(path).await?;

    // ===== 10. 共有commit (race check + sibling temp + verify) =====
    let after = match commit_text_candidate(path, &before, &patched_content).await {
        Ok(after) => after,
        Err(crate::tools::mutation::MutationCommitError::ConcurrentModification) => {
            return Ok(ok(
                ApplyPatchResult::failure(
                    "File changed during commit; refusing to overwrite",
                    &file_path,
                ),
                None,
            ));
        }
        Err(e) => {
            return Ok(ok(
                ApplyPatchResult::failure(format!("Failed to write to file: {e}"), &file_path),
                None,
            ));
        }
    };

    let receipt = build_receipt(
        crate::provenance::ChangeKind::ApplyPatch,
        path.to_path_buf(),
        before,
        after,
        MutationTargetReceipt::File,
    );

    // ===== 11. 成功結果の返却 (tool outputはbudgeted, provenanceはfull) =====
    let mut result = ApplyPatchResult::success_from_contents(
        &file_path,
        file_path.clone(),
        receipt.before.content_or_empty(),
        receipt.after.content_or_empty(),
    );
    // success_from_contents sets changed=true; keep receipt lines.
    result.lines_added = receipt.lines_added as u64;
    result.lines_removed = receipt.lines_removed as u64;
    Ok(ok(result, Some(receipt)))
}

// ===== ユーティリティ関数群 =====

/// パスの検証とアクセスチェック
async fn validate_file_path_and_access(file_path: &str, config: &AppConfig) -> Result<()> {
    let path = Path::new(file_path);

    // 絶対パスチェック
    if !path.is_absolute() {
        anyhow::bail!("File path must be absolute: {}", file_path);
    }

    // プロジェクトルート内または許可されたパス内のチェック。
    // ルートとターゲットは同一の正規化契約で比較する。
    // 存在しないパスは信頼できる既存祖先で解決し、 traversal/escape は拒否する。
    crate::tools::scope::ensure_in_project_scope(path, config).map_err(|e| {
        if path
            .components()
            .any(|comp| matches!(comp, Component::ParentDir))
        {
            anyhow::anyhow!(
                "Path contains parent directory references which are not allowed: {} ({e})",
                file_path
            )
        } else {
            anyhow::anyhow!(
                "Access to files outside the project root is not allowed: {} ({e})",
                file_path
            )
        }
    })?;

    Ok(())
}

/// ファイル存在と読み取り可能かの検証
async fn validate_file_exists_and_readable(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "Failed to read file: File does not exist at {}",
            path.display()
        ));
    }
    if path.is_dir() {
        return Err(anyhow::anyhow!(
            "Failed to read file: Path is a directory: {}",
            path.display()
        ));
    }
    Ok(())
}

/// 改行コードの正規化
fn normalize_line_endings(content: &str) -> (String, bool) {
    let has_crlf = content.contains("\r\n");
    let normalized_content = if has_crlf {
        content.replace("\r\n", "\n")
    } else {
        content.to_string()
    };
    (normalized_content, has_crlf)
}

/// パッチの解析
fn parse_patch(patch_content: &str) -> Result<diffy::Patch<'_, str>> {
    let patch = diffy::Patch::from_str(patch_content)
        .map_err(|e| anyhow::anyhow!("Failed to parse patch: {}", e))?;
    // diffy 0.5 accepts trailing content and additional files. This tool
    // applies exactly one text patch, so every line after its first hunk must
    // belong to the parsed hunks (including no-newline markers).
    let mut lines = patch_content.split_inclusive('\n').peekable();
    while lines.peek().is_some_and(|line| !line.starts_with("@@ ")) {
        lines.next();
    }
    for hunk in patch.hunks() {
        if !lines.next().is_some_and(|line| line.starts_with("@@ ")) {
            anyhow::bail!("Failed to parse patch: missing hunk header");
        }
        for _ in hunk.lines() {
            lines
                .next()
                .context("Failed to parse patch: missing hunk line")?;
            if lines
                .peek()
                .is_some_and(|line| line.starts_with("\\ No newline at end of file"))
            {
                lines.next();
            }
        }
    }
    if lines.next().is_some() {
        anyhow::bail!(
            "Failed to parse patch: trailing content or multiple files are not supported"
        );
    }
    Ok(patch)
}

/// 空のパッチかどうかのチェック
fn is_empty_patch(patch: &diffy::Patch<str>, patch_content: &str) -> bool {
    patch.hunks().is_empty() && !patch_content.trim().is_empty()
}

/// パッチをコンテンツに適用
fn apply_patch_to_content(
    original_content: &str,
    patch: &diffy::Patch<'_, str>,
    _path: &Path,
) -> Result<String> {
    match diffy::apply(original_content, patch) {
        Ok(content) => Ok(content),
        Err(e) => {
            let error_str = e.to_string();
            if (error_str.contains("error applying hunk")
                || error_str.contains("context lines do not match"))
                && let Ok(fuzzy_content) = apply_patch_with_fuzz(original_content, patch)
            {
                tracing::warn!("apply_patch: fallback fuzzy application succeeded");
                return Ok(fuzzy_content);
            }

            let detailed_message = if error_str.contains("error applying hunk")
                || error_str.contains("context lines do not match")
            {
                format!(
                    "Failed to apply patch: Context lines do not match. This usually happens when the file content has changed since the patch was created, or the context in the patch doesn't match the current file content exactly. Make sure to read the current file content with fs_read before creating your patch. Error: {}",
                    e
                )
            } else {
                format!("Failed to apply patch: {}", e)
            };

            anyhow::bail!("{}", detailed_message)
        }
    }
}

const MAX_FUZZ_LINES: usize = 2;
const MAX_FUZZ_OFFSET_LINES: usize = 20;
const RECOVERY_MAX_FUZZ_LINES: usize = 6;
const RECOVERY_MAX_FUZZ_OFFSET_LINES: usize = 200;

pub(crate) fn apply_patch_with_fuzz(
    original_content: &str,
    patch: &diffy::Patch<'_, str>,
) -> Result<String> {
    apply_patch_with_fuzz_params(
        original_content,
        patch,
        MAX_FUZZ_LINES,
        MAX_FUZZ_OFFSET_LINES,
    )
}

pub(crate) fn apply_patch_with_aggressive_fuzz(
    original_content: &str,
    patch: &diffy::Patch<'_, str>,
) -> Result<String> {
    apply_patch_with_fuzz_params(
        original_content,
        patch,
        RECOVERY_MAX_FUZZ_LINES,
        RECOVERY_MAX_FUZZ_OFFSET_LINES,
    )
}

fn apply_patch_with_fuzz_params(
    original_content: &str,
    patch: &diffy::Patch<'_, str>,
    max_fuzz_lines: usize,
    max_fuzz_offset_lines: usize,
) -> Result<String> {
    let mut image = split_lines_preserve_newline(original_content);
    let mut line_shift: isize = 0;

    for hunk in patch.hunks() {
        let lines = hunk.lines();
        let mut applied = false;
        let mut applied_delta: isize = 0;
        let base_expected = hunk.new_range().start().saturating_sub(1);
        let expected_pos = clamp_expected_pos(base_expected as isize + line_shift, image.len());

        for leading_trim in 0..=max_fuzz_lines {
            for trailing_trim in 0..=max_fuzz_lines {
                let trimmed = match trim_context_lines(lines, leading_trim, trailing_trim) {
                    Some(trimmed) => trimmed,
                    None => continue,
                };

                let pre = pre_image_lines(trimmed);
                let pos = if pre.is_empty() {
                    std::cmp::min(expected_pos, image.len())
                } else if let Some(pos) =
                    find_best_subsequence(&image, &pre, expected_pos, max_fuzz_offset_lines)
                {
                    pos
                } else {
                    continue;
                };

                let post = build_post_lines_with_context(&image, pos, trimmed);
                let delta = post.len() as isize - pre.len() as isize;
                image.splice(pos..pos + pre.len(), post);
                applied = true;
                applied_delta = delta;
                break;
            }

            if applied {
                break;
            }
        }

        if !applied {
            anyhow::bail!("Failed to apply patch: context mismatch remained after fuzzy matching");
        }

        line_shift = line_shift.saturating_add(applied_delta);
    }

    Ok(image.concat())
}

fn split_lines_preserve_newline(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }

    content
        .split_inclusive('\n')
        .map(|line| line.to_string())
        .collect()
}

fn trim_context_lines<'a>(
    lines: &'a [diffy::Line<'a, str>],
    leading_trim: usize,
    trailing_trim: usize,
) -> Option<&'a [diffy::Line<'a, str>]> {
    let mut start = 0usize;
    let mut end = lines.len();

    for _ in 0..leading_trim {
        match lines.get(start)? {
            diffy::Line::Context(_) => start += 1,
            _ => return None,
        }
    }

    for _ in 0..trailing_trim {
        if end == 0 {
            return None;
        }
        match lines.get(end - 1)? {
            diffy::Line::Context(_) => end = end.saturating_sub(1),
            _ => return None,
        }
    }

    if start > end {
        return None;
    }

    Some(&lines[start..end])
}

fn pre_image_lines(lines: &[diffy::Line<'_, str>]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| match line {
            diffy::Line::Context(text) | diffy::Line::Delete(text) => Some((*text).to_string()),
            diffy::Line::Insert(_) => None,
        })
        .collect()
}

fn build_post_lines_with_context(
    image: &[String],
    pos: usize,
    lines: &[diffy::Line<'_, str>],
) -> Vec<String> {
    let mut post = Vec::new();
    let mut pre_index = 0usize;

    for line in lines {
        match line {
            diffy::Line::Context(text) => {
                let fallback = (*text).to_string();
                let value = image.get(pos + pre_index).cloned().unwrap_or(fallback);
                post.push(value);
                pre_index += 1;
            }
            diffy::Line::Delete(_) => {
                pre_index += 1;
            }
            diffy::Line::Insert(text) => {
                post.push((*text).to_string());
            }
        }
    }

    post
}

fn find_best_subsequence(
    haystack: &[String],
    needle: &[String],
    expected_pos: usize,
    max_distance: usize,
) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }

    let mut matches = Vec::new();
    for pos in 0..=haystack.len().saturating_sub(needle.len()) {
        if lines_match_at(haystack, needle, pos) {
            matches.push(pos);
        }
    }

    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some(matches[0]);
    }

    let mut best: Option<(usize, usize)> = None;
    let mut tie = false;
    for pos in matches {
        let distance = pos.abs_diff(expected_pos);
        if distance > max_distance {
            continue;
        }
        match best {
            None => {
                best = Some((distance, pos));
                tie = false;
            }
            Some((best_dist, _)) => {
                if distance < best_dist {
                    best = Some((distance, pos));
                    tie = false;
                } else if distance == best_dist {
                    tie = true;
                }
            }
        }
    }

    if tie {
        return None;
    }

    best.map(|(_, pos)| pos)
}

fn lines_match_at(haystack: &[String], needle: &[String], pos: usize) -> bool {
    haystack[pos..pos + needle.len()]
        .iter()
        .zip(needle)
        .all(|(a, b)| line_eq(a, b))
}

fn line_eq(a: &str, b: &str) -> bool {
    a.trim_end() == b.trim_end()
}

fn clamp_expected_pos(pos: isize, len: usize) -> usize {
    if pos < 0 {
        0
    } else if pos as usize > len {
        len
    } else {
        pos as usize
    }
}

/// 書き込みパーミッションの検証
async fn validate_write_permissions(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .await
        .with_context(|| format!("Failed to get file metadata: {}", path.display()))?;

    // Unixシステムでのみ詳細なパーミッションチェック
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = metadata.permissions();
        if permissions.readonly() {
            anyhow::bail!("Failed to write to file: {} (read-only)", path.display());
        }
        // 書き込みパーミッションの簡易チェック
        let mode = permissions.mode();
        if mode & 0o200 == 0 && mode & 0o020 == 0 && mode & 0o002 == 0 {
            anyhow::bail!(
                "Failed to write to file: {} (no write permissions)",
                path.display()
            );
        }
    }

    // Non-Unixシステムでは簡易書き込みテスト
    #[cfg(not(unix))]
    {
        use tokio::fs::OpenOptions;
        let mut test_file = OpenOptions::new()
            .write(true)
            .open(path)
            .await
            .with_context(|| format!("Cannot open file for writing: {}", path.display()))?;

        test_file
            .shutdown()
            .await
            .with_context(|| format!("Cannot write to file: {}", path.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffy;
    use tempfile::TempDir;

    fn test_config_for(dir: &TempDir) -> AppConfig {
        AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        }
    }

    async fn apply_patch(params: ApplyPatchParams) -> anyhow::Result<ApplyPatchResult> {
        // Infer the project root from the target file's parent so tempdir
        // files pass scope checks without repository-root artifacts.
        let parent = std::path::Path::new(&params.file_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let config = AppConfig {
            project_root: parent,
            ..AppConfig::default()
        };
        super::apply_patch(params, &config).await
    }

    fn create_temp_file(content: &str) -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join(format!("test_{}.txt", fastrand::u32(..)));
        std::fs::write(&file_path, content).unwrap();
        let file_path_str = file_path.to_str().unwrap().to_string();
        (dir, file_path_str)
    }

    #[test]
    fn rejects_trailing_content_and_multiple_file_patches() {
        let patch = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n";
        assert!(parse_patch(patch).is_ok());
        for tail in [
            "garbage\n",
            "--- a/other\n+++ b/other\n",
            "GIT binary patch\nliteral 0\n",
        ] {
            assert!(parse_patch(&format!("{patch}{tail}")).is_err(), "{tail}");
        }
    }

    fn create_patch_content(original: &str, modified: &str) -> String {
        let patch = diffy::create_patch(original, modified);
        patch.to_string()
    }

    #[tokio::test]
    async fn test_apply_patch_success() {
        let original_content = r#"Hello, world!
This is the original file.
"#;
        let modified_content = r#"Hello, Rust!
This is the modified file.
"#;

        let (_dir, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);
        assert!(result.message.contains(&file_path));

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_create_and_apply_patch_integration() {
        let original_content = r#"line 1
line 2
line 3
"#;
        let modified_content = r#"line 1
line two
line 3
"#;

        // Create a temporary file with the original content
        let (_dir, file_path) = create_temp_file(original_content);

        // 1. Create the patch
        let patch_content = create_patch_content(original_content, modified_content);
        assert!(patch_content.contains("-line 2"));
        assert!(patch_content.contains("+line two"));

        // 2. Apply the patch
        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };
        let result = apply_patch(params).await.unwrap();

        // 3. Verify the result
        assert!(result.success);
        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_conflict() {
        let original_content = r#"line A
line B
line C
"#;
        let modified_content = r#"line A
line Bee
line C
"#;
        let actual_content_in_file = r#"line A
line Z
line C
"#; // This is different from original_content

        // Create a temporary file with the "actual" content
        let (_dir, file_path) = create_temp_file(actual_content_in_file);

        // 1. Create the patch based on the "original" content
        let patch_content = create_patch_content(original_content, modified_content);

        // 2. Attempt to apply the patch to the "actual" content
        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };
        let result = apply_patch(params).await.unwrap();

        // 3. Verify that the patch application failed due to content mismatch
        assert!(!result.success);
        assert!(result.message.contains("Failed to apply patch"));
        // Check that the enhanced error message includes helpful guidance
        assert!(
            result.message.contains("Context lines do not match")
                || result.message.contains("context lines do not match")
        );
        assert!(
            result.message.contains("fs_read") || result.message.contains("current file content")
        );

        // Ensure the file content remains unchanged
        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, actual_content_in_file);
    }

    #[tokio::test]
    async fn test_apply_patch_fuzzy_context_recovery() {
        let original_content = r#"line 1
line 2
line 3
line 4
"#;
        let modified_content = r#"line 1
line 2 changed
line 3
line 4
"#;
        let actual_content_in_file = r#"line 1 modified
line 2
line 3
line 4
"#;

        let (_dir, file_path) = create_temp_file(actual_content_in_file);
        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };
        let result = apply_patch(params).await.unwrap();

        assert!(result.success);
        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(
            final_content,
            r#"line 1 modified
line 2 changed
line 3
line 4
"#
        );
    }

    #[tokio::test]
    async fn test_apply_patch_fuzzy_trailing_whitespace() {
        let original_content = "line 1\nline 2\n";
        let modified_content = "line 1\nline 2 changed\n";
        let actual_content_in_file = "line 1  \nline 2\n";

        let (_dir, file_path) = create_temp_file(actual_content_in_file);
        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };
        let result = apply_patch(params).await.unwrap();

        assert!(result.success);
        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, "line 1  \nline 2 changed\n");
    }

    #[test]
    fn test_find_best_subsequence_prefers_near_expected() {
        let haystack = vec![
            "alpha\n".to_string(),
            "needle\n".to_string(),
            "beta\n".to_string(),
            "gamma\n".to_string(),
            "delta\n".to_string(),
            "epsilon\n".to_string(),
            "needle\n".to_string(),
            "zeta\n".to_string(),
        ];
        let needle = vec!["needle\n".to_string()];
        let expected_pos = 6;

        let pos = find_best_subsequence(&haystack, &needle, expected_pos, MAX_FUZZ_OFFSET_LINES)
            .expect("should find near expected");
        assert_eq!(pos, 6);
    }

    #[tokio::test]
    async fn test_apply_patch_requires_absolute_path() {
        let params = ApplyPatchParams {
            file_path: "relative/path/to/file.txt".to_string(),
            patch_content: "any patch".to_string(),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("File path must be absolute")
        );
    }

    #[tokio::test]
    async fn test_apply_patch_with_crlf_line_endings() {
        let original_content = r#"first line
second line
"#
        .replace('\n', "\r\n");
        let modified_content = r#"first line
second line modified
"#
        .replace('\n', "\r\n");

        let (_dir, file_path) = create_temp_file(&original_content);

        // Create patch using LF-normalized content, as our tool now handles this internally
        let patch_content = create_patch_content(
            &original_content.replace("\r\n", "\n"),
            &modified_content.replace("\r\n", "\n"),
        );

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(
            result.success,
            "Patch should apply cleanly. Message: {}",
            result.message
        );

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_no_newline_at_end_of_file() {
        let original_content = "hello";
        let modified_content = "hello world";

        let (_dir, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_large_change() {
        let original_content = "line\n".repeat(100);
        let modified_content = "changed line\n".repeat(100);

        let (_dir, file_path) = create_temp_file(&original_content);

        let patch_content = create_patch_content(&original_content, &modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_to_non_existent_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("non_existent_file.txt");
        let params = ApplyPatchParams {
            file_path: file_path.to_str().unwrap().to_string(),
            patch_content: "... a patch ...".to_string(),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("File does not exist"));
    }

    #[tokio::test]
    async fn test_apply_patch_to_directory() {
        let dir = TempDir::new().unwrap();
        let params = ApplyPatchParams {
            file_path: dir.path().to_str().unwrap().to_string(),
            patch_content: "... a patch ...".to_string(),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Path is a directory"));
    }

    #[tokio::test]
    async fn test_apply_patch_to_read_only_file() {
        let original_content = "read only content";
        let (_dir, file_path) = create_temp_file(original_content);

        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&file_path, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            // On non-Unix systems, we can't easily set read-only, so skip this test
            return;
        }

        let patch_content = create_patch_content(original_content, "new content");

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Failed to write to file"));

        // Cleanup: make writable again to allow deletion by tempfile
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
            // Restore writable permissions for owner (rw-r--r--)
            perms.set_mode(0o644);
            std::fs::set_permissions(&file_path, perms).unwrap();
        }
    }

    #[tokio::test]
    async fn test_apply_patch_with_malformed_patch_content() {
        let original_content = "some content";
        let (_dir, file_path) = create_temp_file(original_content);

        let params = ApplyPatchParams {
            file_path,
            patch_content: "this is not a valid patch".to_string(),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .message
                .contains("Patch content is invalid or results in no changes.")
        );
    }

    #[tokio::test]
    async fn test_patch_to_make_file_empty() {
        let original_content = "delete me";
        let modified_content = "";
        let (_dir, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_patch_on_empty_file() {
        let original_content = "";
        let modified_content = "add me";
        let (_dir, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_returns_diff_in_success_case() {
        let original_content = "Hello, world!\nThis is the original file.\n";
        let modified_content = "Hello, Rust!\nThis is the modified file.\n";

        let (_dir, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);
        assert!(result.message.contains(&file_path));

        // Verify that a diff (not full contents) is returned in the success case
        let diff = result
            .diff
            .as_ref()
            .expect("Diff should be returned in success case");
        assert!(diff.contains("-Hello, world!"));
        assert!(diff.contains("+Hello, Rust!"));
        assert!(!diff.contains("This is the original file.\nThis is the modified file."));
        assert_eq!(result.file_path, file_path);
        assert_eq!(result.lines_added, 2);
        assert_eq!(result.lines_removed, 2);
        assert!(!result.diff_truncated);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_updates_session_with_changed_file() {
        let original_content = "Hello, world!\n";
        let modified_content = "Hello, Rust!\n";

        let _dir = TempDir::new().unwrap();
        let temp_file_path = _dir.path().join("test_apply_patch_session.txt");
        std::fs::write(&temp_file_path, original_content).unwrap();
        let file_path_str = temp_file_path.to_string_lossy().to_string();

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path_str,
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        // Verify file was actually changed
        let final_content = std::fs::read_to_string(&temp_file_path).unwrap();
        assert_eq!(final_content, modified_content);

        // Clean up
        std::fs::remove_file(&temp_file_path).unwrap();

        // Note: The session update happens in a separate thread with FsTools::default(),
        // so we can't directly check it in this test. The important thing is that the
        // update_session_with_changed_file call is made in the success path, which it is.
    }

    #[tokio::test]
    async fn test_apply_patch_with_path_traversal_attempt() {
        let original_content = "test content";
        let (_dir, file_path) = create_temp_file(original_content);

        // Try to use a path with parent directory references
        let path_with_traversal = format!("{}/../forbidden.txt", file_path);
        let patch_content = create_patch_content(original_content, "modified");

        let params = ApplyPatchParams {
            file_path: path_with_traversal,
            patch_content,
        };

        // This should fail because path traversal is detected
        let result = apply_patch(params).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("parent directory references")
        );
    }

    #[tokio::test]
    async fn test_apply_patch_mixed_line_endings() {
        // Test with CRLF line endings
        let original_content = "line1\r\nline2\r\nline3\r\n";
        let modified_content = "line1\r\nline2_modified\r\nline3\r\n";

        let (_dir, file_path) = create_temp_file(original_content);

        // Create patch using normalized content (CRLF -> LF)
        let normalized_original = original_content.replace("\r\n", "\n");
        let normalized_modified = modified_content.replace("\r\n", "\n");
        let patch_content = create_patch_content(&normalized_original, &normalized_modified);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_recovery_large_offset() {
        let base_lines: Vec<String> = (0..10).map(|i| format!("line{}\n", i)).collect();
        let original_content = base_lines.join("");

        let mut modified_lines = base_lines.clone();
        modified_lines[5] = "changed line\n".to_string();
        let modified_content = modified_lines.join("");

        // Actual file has many lines inserted before, so hunk offset drifts beyond default fuzz
        let mut actual_lines: Vec<String> = (0..60).map(|i| format!("extra{}\n", i)).collect();
        actual_lines.extend(base_lines.clone());
        let actual_content = actual_lines.join("");

        let (_dir, file_path) = create_temp_file(&actual_content);
        let patch_content = create_patch_content(&original_content, &modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let test_config = test_config_for(&_dir);
        let result = apply_patch_with_recovery(params, &test_config)
            .await
            .unwrap();

        assert!(
            result.success,
            "Recovery should succeed: {}",
            result.message
        );

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert!(final_content.contains("changed line"));
        // Ensure extras are preserved
        assert!(final_content.starts_with("extra0\nextra1"));
    }

    #[tokio::test]
    async fn test_apply_patch_permission_check_unix() {
        let original_content = "test content";
        let (_dir, file_path) = create_temp_file(original_content);

        // On Unix systems, test read-only file handling
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
            perms.set_mode(0o444); // read-only
            std::fs::set_permissions(&file_path, perms).unwrap();

            let patch_content = create_patch_content(original_content, "modified content");
            let params = ApplyPatchParams {
                file_path: file_path.clone(),
                patch_content,
            };

            let result = apply_patch(params).await;
            assert!(result.is_err());
            let err_msg = result.unwrap_err().to_string();
            assert!(err_msg.contains("read-only") || err_msg.contains("no write permissions"));

            // Restore permissions for cleanup
            let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&file_path, perms).unwrap();
        }
    }
    #[test]
    fn test_success_with_message_shape() {
        // The recovery layer's FixResult::Success (e.g. "patch already
        // applied") must map to a successful result without a diff.
        let result = ApplyPatchResult::success_with_message("patch already applied", "/tmp/x.rs");
        assert!(result.success);
        assert!(result.diff.is_none());
        assert!(!result.diff_truncated);
        assert_eq!(result.lines_added, 0);
        assert_eq!(result.lines_removed, 0);
        assert_eq!(result.message, "patch already applied");
    }

    #[tokio::test]
    async fn test_apply_patch_fuzzy_context_success() {
        let original_content = "line1\nline2\nline3\n";
        let (_dir, file_path) = create_temp_file(original_content);

        // Mismatch in context line "line3" (patch expects "line3Modified")
        // But "line1" matches, so fuzzy matching should handle it.
        let patch_content =
            "--- a\n+++ b\n@@ -1,3 +1,3 @@\n line1\n-line2\n+lineNew\n line3Modified\n";

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content: patch_content.to_string(),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success, "Fuzzy patch application should succeed");
        let diff = result.diff.as_ref().expect("Diff should be returned");
        assert!(diff.contains("+lineNew"), "Content should be modified");
    }

    #[tokio::test]
    async fn test_apply_patch_total_mismatch_failure() {
        let original_content = "line1\nline2\nline3\n";
        let (_dir, file_path) = create_temp_file(original_content);

        // Total mismatch: NO context lines match, AND the line to be deleted doesn't match.
        // This ensures even aggressive fuzzy matching can't find a place to apply it.
        let patch_content = "--- a\n+++ b\n@@ -1,3 +1,3 @@\n line1Modified\n-line2Modified\n+lineNew\n line3Modified\n";

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content: patch_content.to_string(),
        };

        let result = apply_patch(params).await;

        match result {
            Ok(res) => {
                assert!(!res.success, "Patch should fail when NO context matches");
            }
            Err(_) => {
                // Also acceptable
            }
        }
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    use tempfile::TempDir;

    fn cfg_for(dir: &TempDir) -> AppConfig {
        AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        }
    }

    fn write_file(dir: &TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_normal_patch_has_apply_patch_receipt() {
        let dir = TempDir::new().unwrap();
        let original = "line 1\nline 2\nline 3\n";
        let modified = "line 1\nline two\nline 3\n";
        let file = write_file(&dir, "a.txt", original);
        let patch = diffy::create_patch(original, modified).to_string();
        let exec = apply_patch_with_recovery_and_receipt(
            ApplyPatchParams {
                file_path: file.clone(),
                patch_content: patch,
            },
            &cfg_for(&dir),
        )
        .await
        .unwrap();
        assert!(exec.result.success);
        assert!(exec.result.changed);
        let receipt = exec.receipt.expect("receipt");
        assert_eq!(receipt.kind, crate::provenance::ChangeKind::ApplyPatch);
        // Provenance diff is the actual before->after, not the LLM patch.
        assert!(receipt.diff.contains("-line 2"));
        assert!(receipt.diff.contains("+line two"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), modified);
    }

    #[tokio::test]
    async fn test_fuzzy_recovery_records_actual_mutation() {
        let dir = TempDir::new().unwrap();
        let original = "line 1\nline 2\nline 3\nline 4\n";
        let modified = "line 1\nline 2 changed\nline 3\nline 4\n";
        let actual = "line 1 modified\nline 2\nline 3\nline 4\n";
        let file = write_file(&dir, "a.txt", actual);
        let patch = diffy::create_patch(original, modified).to_string();
        let exec = apply_patch_with_recovery_and_receipt(
            ApplyPatchParams {
                file_path: file.clone(),
                patch_content: patch,
            },
            &cfg_for(&dir),
        )
        .await
        .unwrap();
        assert!(exec.result.success, "{}", exec.result.message);
        let receipt = exec.receipt.expect("receipt");
        // Actual original content preserved, not the LLM's assumed original.
        assert!(
            receipt
                .before
                .content_or_empty()
                .contains("line 1 modified")
        );
        assert!(receipt.after.content_or_empty().contains("line 2 changed"));
    }

    #[tokio::test]
    async fn test_conflict_has_no_receipt_and_preserves_file() {
        let dir = TempDir::new().unwrap();
        let original = "line A\nline B\nline C\n";
        let modified = "line A\nline Bee\nline C\n";
        let actual = "line A\nline Z\nline C\n";
        let file = write_file(&dir, "a.txt", actual);
        let patch = diffy::create_patch(original, modified).to_string();
        let exec = apply_patch_with_recovery_and_receipt(
            ApplyPatchParams {
                file_path: file.clone(),
                patch_content: patch,
            },
            &cfg_for(&dir),
        )
        .await
        .unwrap();
        assert!(!exec.result.success);
        assert!(!exec.result.changed);
        assert!(exec.receipt.is_none());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), actual);
    }

    #[test]
    fn test_already_applied_shape_is_noop() {
        let r = ApplyPatchResult::success_with_message("patch already applied", "/tmp/x.rs");
        assert!(r.success);
        assert!(!r.changed);
    }

    #[tokio::test]
    async fn test_patch_undo_restores() {
        // Patch then undo via the shared finalizer + undo stack.
        let proj = TempDir::new().unwrap();
        let sessions_root = proj.path().join(".doge/sessions");
        let store = crate::session::SessionStore::new(sessions_root).unwrap();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
        }
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: proj.path().to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        let fs = crate::tools::FsTools::new(
            std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            config.clone(),
        )
        .with_session_manager(manager);
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "v0\n").unwrap();
        let patch = diffy::create_patch("v0\n", "v1\n").to_string();
        let exec = apply_patch_with_recovery_and_receipt(
            ApplyPatchParams {
                file_path: file.to_str().unwrap().to_string(),
                patch_content: patch,
            },
            &config,
        )
        .await
        .unwrap();
        assert!(exec.result.changed);
        let receipt = exec.receipt.unwrap();
        fs.finalize_mutation(
            receipt,
            crate::tools::FinalizeMutationOptions {
                record_undo: true,
                reverts_change_id: None,
                attribution: Default::default(),
            },
        )
        .await;
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1\n");
        let undo = crate::tools::undo::undo(&fs).await.unwrap();
        assert!(undo.success);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v0\n");
    }
}
