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
    /// 結果メッセージ
    pub message: String,
    /// 元のファイル内容（成功時も失敗時も含まれる）
    pub original_content: Option<String>,
    /// 変更後のファイル内容（成功時のみ）
    pub modified_content: Option<String>,
}

// ===== ツール定義 =====

/// apply_patchツールの定義を返す
pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "apply_patch".to_string(),
            description: create_tool_description(),
            strict: None,
            parameters: create_tool_parameters(),
        },
    }
}

fn create_tool_description() -> String {
    "Applies a unified diff patch to a file. \n\nREQUIRED PARAMETERS:\n- file_path: ABSOLUTE path to target file (e.g., '/home/user/project/src/main.rs')\n- patch_content: Unified diff content in proper format\n\nCRITICAL RULES:\n1. ALWAYS read current file content with fs_read first\n2. Context lines (starting with ' ') must EXACTLY match current file content  \n3. Use proper unified diff format with correct @@ line numbers\n4. NEVER use relative paths - always use absolute paths starting with project root\n\nWORKFLOW:\nfs_read → analyze current content → create precise diff → apply_patch\n\nEXAMPLE (CORRECT):\n```diff\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"Hello\");\n+    println!(\"Hello, world!\");\n }\n```\n\nFAILURE MODES & SOLUTIONS:\n- 'Context lines do not match': File content changed - re-read file and create new patch\n- 'File path must be absolute': Use absolute path like '/project/src/main.rs', not 'src/main.rs'\n- 'Failed to parse patch content': Check unified diff format syntax\n- 'Failed to write to file': Check file permissions and ensure write access\n\nCOMMON MISTAKES TO AVOID:\n- ❌ Using relative paths: 'src/main.rs'\n- ❌ Creating patch before reading current file content\n- ❌ Insufficient context lines (need 3-5 lines before/after change)\n- ❌ Incorrect line numbers in @@ headers\n- ❌ Whitespace differences in context lines".to_string()
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
    apply_patch_impl(params, config).await
}

/// apply_patchツールの主要インターフェース関数（エラー回復機能付き）
pub async fn apply_patch_with_recovery(
    params: ApplyPatchParams,
    config: &AppConfig,
) -> Result<ApplyPatchResult> {
    match apply_patch_impl(params.clone(), config).await {
        Ok(result) if result.success => Ok(result),
        Ok(result) => {
            if let Some(recovered) = attempt_recovery(&params, config, &result.message).await? {
                Ok(recovered)
            } else {
                Ok(result)
            }
        }
        Err(e) => {
            if let Some(recovered) = attempt_recovery(&params, config, &e.to_string()).await? {
                Ok(recovered)
            } else {
                Ok(ApplyPatchResult {
                    success: false,
                    message: format!("Patch application failed: {}", e),
                    original_content: None,
                    modified_content: None,
                })
            }
        }
    }
}

async fn attempt_recovery(
    params: &ApplyPatchParams,
    config: &AppConfig,
    error_message: &str,
) -> Result<Option<ApplyPatchResult>> {
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
                    .unwrap_or_else(|e| ApplyPatchResult {
                        success: false,
                        message: format!("Adjusted patch failed to apply: {}", e),
                        original_content: None,
                        modified_content: None,
                    }),
            )
        }
        FixResult::RequiresHumanIntervention { message } => Some(ApplyPatchResult {
            success: false,
            message,
            original_content: None,
            modified_content: None,
        }),
        FixResult::Failed { reason } => Some(ApplyPatchResult {
            success: false,
            message: reason,
            original_content: None,
            modified_content: None,
        }),
        FixResult::Success { message } => Some(ApplyPatchResult {
            success: true,
            message,
            original_content: None,
            modified_content: None,
        }),
    };

    Ok(recovered)
}

// ===== 実装関数群 =====

/// apply_patchの実際の実装
async fn apply_patch_impl(
    params: ApplyPatchParams,
    config: &AppConfig,
) -> Result<ApplyPatchResult> {
    let file_path = params.file_path;
    let patch_content = params.patch_content;

    // ===== 1. パス検証 =====
    validate_file_path_and_access(&file_path, config).await?;

    // ===== 2. ファイル存在と属性検証 =====
    let path = Path::new(&file_path);
    validate_file_exists_and_readable(path).await?;

    // ===== 3. ファイル内容の読み取り =====
    let original_content_raw = fs::read_to_string(path).await.with_context(|| {
        format!(
            "Failed to read file for an unknown reason: {}",
            path.display()
        )
    })?;

    // ===== 4. 改行コードの正規化 =====
    let (original_content, has_crlf) = normalize_line_endings(&original_content_raw);

    // ===== 5. パッチの解析 =====
    let patch = match parse_patch(&patch_content) {
        Ok(patch) => patch,
        Err(e) => {
            return Ok(ApplyPatchResult {
                success: false,
                message: format!("Failed to parse patch content: {}", e),
                original_content: Some(original_content_raw),
                modified_content: None,
            });
        }
    };

    // ===== 6. 空の変更チェック =====
    if is_empty_patch(&patch, &patch_content) {
        return Ok(ApplyPatchResult {
            success: false,
            message: "Patch content is invalid or results in no changes.".to_string(),
            original_content: Some(original_content_raw.clone()),
            modified_content: Some(original_content_raw.clone()),
        });
    }

    // ===== 7. パッチ適用 =====
    let patched_content_lf = match apply_patch_to_content(&original_content, &patch, path) {
        Ok(content) => content,
        Err(e) => {
            return Ok(ApplyPatchResult {
                success: false,
                message: format!("Failed to apply patch: {}", e),
                original_content: Some(original_content_raw),
                modified_content: None,
            });
        }
    };

    // ===== 8. 改行コードの復元 =====
    let patched_content = if has_crlf {
        patched_content_lf.replace('\n', "\r\n")
    } else {
        patched_content_lf
    };

    // ===== 9. パーミッションチェック =====
    validate_write_permissions(path).await?;

    // ===== 10. ファイル書き込み =====
    fs::write(path, &patched_content)
        .await
        .with_context(|| format!("Failed to write to file: {}", path.display()))?;

    // ===== 11. 書き込み検証 =====
    verify_file_content(path, &patched_content).await?;

    // ===== 12. セッション更新 =====
    update_session_with_changed_file(path).await;

    // ===== 13. 成功結果の返却 =====
    Ok(ApplyPatchResult {
        success: true,
        message: file_path,
        original_content: Some(original_content_raw),
        modified_content: Some(patched_content),
    })
}

// ===== ユーティリティ関数群 =====

/// パスの検証とアクセスチェック
async fn validate_file_path_and_access(file_path: &str, config: &AppConfig) -> Result<()> {
    let path = Path::new(file_path);

    // 絶対パスチェック
    if !path.is_absolute() {
        anyhow::bail!("File path must be absolute: {}", file_path);
    }

    // プロジェクトルート内または許可されたパス内のチェック
    let project_root = &config.project_root;
    let canonical_path = match path.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            // 正規化できない場合はパストラバーサルをチェック
            if path
                .components()
                .any(|comp| matches!(comp, Component::ParentDir))
            {
                anyhow::bail!(
                    "Path contains parent directory references which are not allowed: {}",
                    file_path
                );
            }
            path.to_path_buf()
        }
    };

    let is_allowed_path = config
        .allowed_paths
        .iter()
        .any(|allowed_path| canonical_path.starts_with(allowed_path));

    if !canonical_path.starts_with(project_root) && !is_allowed_path {
        anyhow::bail!(
            "Access to files outside the project root is not allowed: {}",
            file_path
        );
    }

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
    diffy::Patch::from_str(patch_content)
        .map_err(|e| anyhow::anyhow!("Failed to parse patch: {}", e))
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

/// ファイル内容の検証
async fn verify_file_content(path: &Path, expected_content: &str) -> Result<()> {
    let verification_content = fs::read_to_string(path).await.with_context(|| {
        format!(
            "Failed to read back file after patching: {}",
            path.display()
        )
    })?;

    if verification_content != expected_content {
        anyhow::bail!(
            "File content verification failed: content read back after patching does not match expected patched content for file: {}",
            path.display()
        );
    }

    Ok(())
}

/// セッションの更新
async fn update_session_with_changed_file(path: &Path) {
    if let Ok(current_dir) = std::env::current_dir()
        && let Ok(relative_path) = path.strip_prefix(current_dir)
    {
        let fs_tools = crate::tools::FsTools::default();
        let _ = fs_tools.update_session_with_changed_file(relative_path.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_utils::create_test_config_with_temp_dir;
    use diffy;
    use std::path::PathBuf;
    use tempfile::Builder;

    async fn apply_patch(params: ApplyPatchParams) -> anyhow::Result<ApplyPatchResult> {
        let config = create_test_config_with_temp_dir();
        super::apply_patch(params, &config).await
    }

    fn create_temp_file(content: &str) -> (PathBuf, String) {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = Builder::new()
            .prefix("test_")
            .suffix(".txt")
            .rand_bytes(5)
            .tempfile_in(&temp_dir)
            .unwrap()
            .into_temp_path()
            .to_path_buf();
        std::fs::write(&file_path, content).unwrap();
        let file_path_str = file_path.to_str().unwrap().to_string();
        (file_path, file_path_str)
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

        let (_temp_file, file_path) = create_temp_file(original_content);

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
        let (_temp_file, file_path) = create_temp_file(original_content);

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
        let (_temp_file, file_path) = create_temp_file(actual_content_in_file);

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

        let (_temp_file, file_path) = create_temp_file(actual_content_in_file);
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

        let (_temp_file, file_path) = create_temp_file(actual_content_in_file);
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

        let (_temp_file, file_path) = create_temp_file(&original_content);

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

        let (_temp_file, file_path) = create_temp_file(original_content);

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

        let (_temp_file, file_path) = create_temp_file(&original_content);

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
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("non_existent_file.txt");
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
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let params = ApplyPatchParams {
            file_path: temp_dir.to_str().unwrap().to_string(),
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
        let (_temp_file, file_path) = create_temp_file(original_content);

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
        let (_temp_file, file_path) = create_temp_file(original_content);

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
        let (_temp_file, file_path) = create_temp_file(original_content);

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
        let (_temp_file, file_path) = create_temp_file(original_content);

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
    async fn test_apply_patch_returns_content_in_success_case() {
        let original_content = "Hello, world!\nThis is the original file.\n";
        let modified_content = "Hello, Rust!\nThis is the modified file.\n";

        let (_temp_file, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);
        assert!(result.message.contains(&file_path));

        // Verify that content is returned in the success case (this was the fix)
        assert!(
            result.original_content.is_some(),
            "Original content should be returned in success case"
        );
        assert!(
            result.modified_content.is_some(),
            "Modified content should be returned in success case"
        );

        if let Some(orig_content) = &result.original_content {
            assert_eq!(
                orig_content, original_content,
                "Returned original content should match"
            );
        }

        if let Some(mod_content) = &result.modified_content {
            assert_eq!(
                mod_content, modified_content,
                "Returned modified content should match"
            );
        }

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_updates_session_with_changed_file() {
        let original_content = "Hello, world!\n";
        let modified_content = "Hello, Rust!\n";

        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let temp_file_path = temp_dir.join("test_apply_patch_session.txt");
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
        let (_temp_file, file_path) = create_temp_file(original_content);

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

        let (_temp_file, file_path) = create_temp_file(original_content);

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

        let (_temp_file, file_path) = create_temp_file(&actual_content);
        let patch_content = create_patch_content(&original_content, &modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let test_config = create_test_config_with_temp_dir();
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
        let (_temp_file, file_path) = create_temp_file(original_content);

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
}
