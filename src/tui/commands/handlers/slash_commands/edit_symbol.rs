use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

use anyhow::{Context, Result, anyhow};
use diffy::create_patch;
use tokio::fs;

use crate::analysis::{SymbolSpan, find_enclosing_symbol};
use crate::config::AppConfig;
use crate::diff_review::DiffReviewPayload;
use crate::llm::{
    SymbolEditRequest, SymbolEditResponse, build_symbol_edit_chat_request,
    parse_symbol_edit_response, read_symbol_source,
};
use crate::tools::apply_patch::{ApplyPatchParams, apply_patch as apply_patch_tool};
use crate::tui::commands::core::TuiExecutor;
use crate::tui::diff_review::{DiffFileState, DiffLineKind};
use crate::tui::view::TuiApp;

/// シンボル限定編集コマンドを処理する。
///
/// 現時点では最小実装として、以下を想定:
/// - 引数なし: カーソル位置のファイル/行に対してシンボルを特定
/// - 引数は将来拡張（明示的なファイル/行指定など）
pub fn handle_edit_symbol(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    let (file, line) = match current_file_and_line(ui) {
        Some(v) => v,
        None => {
            ui.push_log("No active file/line context for symbol edit.");
            return;
        }
    };

    let file_path = PathBuf::from(&file);

    let symbol = {
        let repomap_guard = executor.repomap.blocking_read();
        let repo_map = match repomap_guard.as_ref() {
            Some(map) => map,
            None => {
                ui.push_log("Repomap is not ready. Please run /rebuild-repomap first.");
                return;
            }
        };

        match find_enclosing_symbol(repo_map, &file_path, line) {
            Ok(Some(sym)) => sym,
            Ok(None) => {
                ui.push_log("No symbol found at current location.");
                return;
            }
            Err(e) => {
                ui.push_log(format!("Failed to find symbol: {e}"));
                return;
            }
        }
    };

    let original = match read_symbol_source(&file_path, &symbol) {
        Ok(code) => code,
        Err(e) => {
            ui.push_log(format!("Failed to read symbol source: {e}"));
            return;
        }
    };

    // ユーザの編集指示は現時点では直近の入力行を利用（本格UIは今後拡張）
    let instruction = match latest_instruction(ui) {
        Some(text) => text,
        None => {
            ui.push_log("Provide edit instruction before running /edit-symbol.");
            return;
        }
    };

    if executor.ui_tx.is_none() {
        executor.ui_tx = ui.sender();
    }

    let model = executor.cfg.model.clone();
    let req = SymbolEditRequest {
        model,
        symbol,
        original_code: original,
        instruction,
    };

    let target_display = make_relative_display(&req.symbol.file, &executor.cfg.project_root);
    ui.push_log(format!(
        "[edit-symbol] Targeting {} ({}) lines {}-{}",
        req.symbol.name, target_display, req.symbol.start_line, req.symbol.end_line
    ));

    let chat_req = build_symbol_edit_chat_request(&req);

    // 既存の LLM 実行パスを利用するため、ここではリクエスト構築までに留める。
    // 実際の送信とレスポンス処理への統合は今後のステップで行う。
    if let Err(e) = enqueue_symbol_edit_request(executor, req, chat_req) {
        ui.push_log(format!("Failed to enqueue symbol edit request: {e}"));
        return;
    }

    ui.push_log("Symbol edit request enqueued.");
}

fn current_file_and_line(ui: &TuiApp) -> Option<(String, u32)> {
    diff_review_cursor(ui).or_else(|| inline_path_reference(ui))
}

fn enqueue_symbol_edit_request(
    executor: &mut TuiExecutor,
    request: SymbolEditRequest,
    chat_req: crate::llm::ChatRequest,
) -> Result<()> {
    let client = executor
        .client
        .clone()
        .ok_or_else(|| anyhow!("LLM client is not configured. Use --api-key to set it."))?;

    let ui_tx = executor
        .ui_tx
        .clone()
        .ok_or_else(|| anyhow!("UI channel is not available yet"))?;

    let fs_tools = executor.tools.clone();
    let cfg = executor.cfg.clone();

    let crate::llm::types::ChatRequest {
        model, messages, ..
    } = chat_req;

    let symbol_label = format!(
        "{} ({})",
        request.symbol.name,
        make_relative_display(&request.symbol.file, &cfg.project_root)
    );

    tokio::runtime::Handle::current().spawn(async move {
        send_ui(
            &ui_tx,
            format!("[edit-symbol] Requesting LLM edit for {symbol_label}..."),
        );
        let _ = ui_tx.send("::status:processing".to_string());

        let response = match client.chat_once(&model, messages, None).await {
            Ok(choice) => choice.content,
            Err(e) => {
                send_ui(
                    &ui_tx,
                    format!("[edit-symbol][error] LLM request failed: {e}"),
                );
                let _ = ui_tx.send("::status:error".to_string());
                return;
            }
        };

        if response.trim().is_empty() {
            send_ui(
                &ui_tx,
                "[edit-symbol][error] LLM returned an empty response.",
            );
            let _ = ui_tx.send("::status:error".to_string());
            return;
        }

        let parsed = match parse_symbol_edit_response(&response) {
            Ok(resp) => resp,
            Err(e) => {
                send_ui(
                    &ui_tx,
                    format!("[edit-symbol][error] Failed to parse response: {e}"),
                );
                send_ui(
                    &ui_tx,
                    format!(
                        "[edit-symbol] Raw response snippet:\n{}",
                        truncate_for_log(&response)
                    ),
                );
                let _ = ui_tx.send("::status:error".to_string());
                return;
            }
        };

        match apply_symbol_edit_response(parsed, request, &cfg).await {
            Ok(changed_path) => {
                send_ui(
                    &ui_tx,
                    format!(
                        "[edit-symbol] Patch applied to {}",
                        make_relative_display(&changed_path, &cfg.project_root)
                    ),
                );

                let relative_for_session = make_relative_path(&changed_path, &cfg.project_root)
                    .unwrap_or(changed_path.clone());
                let _ = fs_tools.update_session_with_changed_file(relative_for_session);

                if cfg.show_diff
                    && let Err(e) = emit_diff_review(
                        &ui_tx,
                        vec![changed_path.clone()],
                        cfg.project_root.clone(),
                    )
                    .await
                {
                    send_ui(
                        &ui_tx,
                        format!("[edit-symbol][warn] Failed to build diff preview: {e}"),
                    );
                }

                let _ = ui_tx.send("::status:done".to_string());
            }
            Err(e) => {
                send_ui(
                    &ui_tx,
                    format!("[edit-symbol][error] Failed to apply suggestion: {e}"),
                );
                let _ = ui_tx.send("::status:error".to_string());
            }
        }
    });

    Ok(())
}

fn latest_instruction(ui: &TuiApp) -> Option<String> {
    ui.last_user_input.as_ref().and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn send_ui(tx: &Sender<String>, message: impl Into<String>) {
    let _ = tx.send(message.into());
}

fn truncate_for_log(raw: &str) -> String {
    const MAX_CHARS: usize = 2000;
    if raw.chars().count() <= MAX_CHARS {
        raw.to_string()
    } else {
        let truncated: String = raw.chars().take(MAX_CHARS).collect();
        format!("{truncated}…")
    }
}

async fn apply_symbol_edit_response(
    response: SymbolEditResponse,
    request: SymbolEditRequest,
    cfg: &AppConfig,
) -> Result<PathBuf> {
    let absolute_path = resolve_absolute_path(&request.symbol.file, &cfg.project_root);
    let file_content = fs::read_to_string(&absolute_path)
        .await
        .with_context(|| format!("failed to read {}", absolute_path.display()))?;

    let normalized_file = normalize_newlines(&file_content);
    let normalized_original = normalize_newlines(&request.original_code);
    let current_symbol = extract_symbol_block_from_content(&normalized_file, &request.symbol);

    if current_symbol != normalized_original {
        anyhow::bail!(
            "Symbol content changed on disk since the request was created. Please rerun /edit-symbol."
        );
    }

    let patch_content = if let Some(patch) = response.patch {
        normalize_llm_patch(&patch, &absolute_path, &cfg.project_root)
    } else if let Some(replacement) = response.replacement {
        build_patch_from_replacement(&normalized_file, &request.symbol, &replacement)?
    } else {
        anyhow::bail!("LLM response did not include a diff or replacement block.");
    };

    if patch_content.trim().is_empty() {
        anyhow::bail!("LLM response produced an empty patch.");
    }

    let params = ApplyPatchParams {
        file_path: absolute_path
            .canonicalize()
            .unwrap_or_else(|_| absolute_path.clone())
            .to_string_lossy()
            .to_string(),
        patch_content,
    };

    let result = apply_patch_tool(params, cfg)
        .await
        .context("failed to apply patch")?;

    if !result.success {
        anyhow::bail!(result.message);
    }

    Ok(absolute_path)
}

fn resolve_absolute_path(path: &Path, root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n")
}

fn extract_symbol_block_from_content(content: &str, span: &SymbolSpan) -> String {
    let mut buf = String::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = idx as u32 + 1;
        if line_no >= span.start_line && line_no <= span.end_line {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    buf
}

fn build_patch_from_replacement(
    normalized_file: &str,
    span: &SymbolSpan,
    replacement: &str,
) -> Result<String> {
    let mut normalized_replacement = normalize_newlines(replacement);
    if !normalized_replacement.ends_with('\n') {
        normalized_replacement.push('\n');
    }

    let (start, end) = symbol_byte_range(normalized_file, span);
    let mut updated = String::with_capacity(normalized_file.len());
    updated.push_str(&normalized_file[..start]);
    updated.push_str(&normalized_replacement);
    updated.push_str(&normalized_file[end..]);

    if updated == normalized_file {
        anyhow::bail!("Replacement produced no changes.");
    }

    Ok(create_patch(normalized_file, &updated).to_string())
}

fn symbol_byte_range(content: &str, span: &SymbolSpan) -> (usize, usize) {
    let mut start = None;
    let mut end = None;
    let mut cursor = 0usize;

    for (idx, segment) in content.split_inclusive('\n').enumerate() {
        let line_no = idx as u32 + 1;
        if line_no == span.start_line && start.is_none() {
            start = Some(cursor);
        }
        cursor += segment.len();
        if line_no == span.end_line {
            end = Some(cursor);
            break;
        }
    }

    let start_idx = start.unwrap_or(content.len());
    let end_idx = end.unwrap_or(content.len());
    (start_idx, end_idx)
}

fn normalize_llm_patch(raw_patch: &str, file_path: &Path, project_root: &Path) -> String {
    let mut body = normalize_newlines(raw_patch.trim());
    if !body.ends_with('\n') {
        body.push('\n');
    }

    let has_headers = body.lines().any(|line| line.starts_with("--- "));
    if has_headers && body.lines().any(|line| line.starts_with("+++ ")) {
        return body;
    }

    let rel = make_relative_display(file_path, project_root);
    format!("--- a/{rel}\n+++ b/{rel}\n{body}")
}

fn make_relative_path(path: &Path, root: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        path.strip_prefix(root).map(|p| p.to_path_buf()).ok()
    } else {
        Some(path.to_path_buf())
    }
}

fn make_relative_display(path: &Path, root: &Path) -> String {
    make_relative_path(path, root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

async fn emit_diff_review(
    tx: &Sender<String>,
    files: Vec<PathBuf>,
    project_root: PathBuf,
) -> Result<()> {
    let payload = tokio::task::spawn_blocking(move || collect_diff_for_files(files, project_root))
        .await
        .map_err(|e| anyhow!("failed to build diff payload: {e}"))??;

    if let Some(payload) = payload {
        let json = serde_json::to_string(&payload)?;
        let _ = tx.send(format!("::diff_review:{json}"));
    }
    Ok(())
}

fn collect_diff_for_files(
    files: Vec<PathBuf>,
    project_root: PathBuf,
) -> Result<Option<DiffReviewPayload>> {
    let mut combined = String::new();
    let mut listed = Vec::new();

    for file in files {
        let Some(rel_path) = make_relative_path(&file, &project_root) else {
            continue;
        };

        let output = Command::new("git")
            .arg("diff")
            .arg("--color=never")
            .arg("--")
            .arg(&rel_path)
            .current_dir(&project_root)
            .output()
            .with_context(|| {
                format!("failed to run git diff for {}", rel_path.to_string_lossy())
            })?;

        if output.stdout.is_empty() {
            continue;
        }

        let diff =
            String::from_utf8(output.stdout).context("git diff output was not valid UTF-8")?;
        combined.push_str(&diff);
        listed.push(rel_path.to_string_lossy().to_string());
    }

    if combined.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(DiffReviewPayload {
        diff: combined,
        files: listed,
    }))
}

fn diff_review_cursor(ui: &TuiApp) -> Option<(String, u32)> {
    let review = ui.diff_review.as_ref()?;
    let file = review.current_file()?;

    if file.lines.is_empty() {
        return Some((file.path.clone(), 1));
    }

    let idx = file.scroll.min(file.lines.len().saturating_sub(1));
    let line = line_number_from_diff(file, idx).unwrap_or(1);
    Some((file.path.clone(), line))
}

fn inline_path_reference(ui: &TuiApp) -> Option<(String, u32)> {
    let last_input = ui.last_user_input.as_deref()?;
    parse_inline_file_reference(last_input)
}

fn parse_inline_file_reference(input: &str) -> Option<(String, u32)> {
    let at_pos = input.rfind('@')?;
    let after = &input[at_pos + 1..];
    if after.is_empty() {
        return None;
    }

    let end = after.find(char::is_whitespace).unwrap_or(after.len());
    let token = after[..end].trim_start_matches(['(', '[', '{', '"', '\'', '`']);
    let token = token.trim_end_matches([',', '.', ';', ')', ']', '}', '"', '\'', '`']);

    extract_path_and_line(token)
}

fn extract_path_and_line(token: &str) -> Option<(String, u32)> {
    if token.is_empty() {
        return None;
    }

    let (path_part, marker) = match token.rfind([':', '#']) {
        Some(idx) => (&token[..idx], Some(&token[idx + 1..])),
        None => (token, None),
    };

    let mut path = path_part
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`'));
    if let Some(stripped) = path.strip_prefix("./") {
        path = stripped;
    }
    if path.is_empty() {
        return None;
    }

    let line = marker.and_then(parse_line_marker).unwrap_or(1);

    Some((path.to_string(), line))
}

fn parse_line_marker(marker: &str) -> Option<u32> {
    if marker.is_empty() {
        return None;
    }

    let trimmed = marker
        .trim_start_matches(['L', 'l'])
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();

    if trimmed.is_empty() {
        None
    } else {
        trimmed.parse().ok()
    }
}

fn line_number_from_diff(file: &DiffFileState, target_index: usize) -> Option<u32> {
    let mut current_line: Option<u32> = None;
    let mut last_mapped: Option<u32> = None;

    for (idx, line) in file.lines.iter().enumerate() {
        match line.kind {
            DiffLineKind::HunkHeader => {
                current_line = parse_new_file_line(&line.content);
                last_mapped = None;
                if idx == target_index {
                    return current_line;
                }
            }
            DiffLineKind::Addition | DiffLineKind::Context => {
                if let Some(line_no) = current_line {
                    if idx == target_index {
                        return Some(line_no);
                    }
                    last_mapped = Some(line_no);
                    current_line = Some(line_no.saturating_add(1));
                }
            }
            DiffLineKind::Removal => {
                if idx == target_index {
                    if let Some(prev) = last_mapped {
                        return Some(prev);
                    }
                    if let Some(line_no) = current_line {
                        let candidate = line_no.saturating_sub(1);
                        return Some(if candidate == 0 { 1 } else { candidate });
                    }
                }
            }
            _ => {
                if idx == target_index
                    && let Some(prev) = last_mapped
                {
                    return Some(prev);
                }
            }
        }
    }

    last_mapped.or(current_line)
}

fn parse_new_file_line(header: &str) -> Option<u32> {
    if !header.starts_with("@@") {
        return None;
    }

    let plus_pos = header.find('+')?;
    let mut rest = &header[plus_pos + 1..];

    if let Some(end) = rest.find('@') {
        rest = &rest[..end];
    }

    let mut digits = String::new();
    for ch in rest.chars().skip_while(|c| matches!(c, ' ' | '+')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if ch == ',' || ch.is_whitespace() {
            break;
        } else if digits.is_empty() && ch == 'L' {
            continue;
        } else {
            break;
        }
    }

    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::SymbolKind;
    use crate::tui::diff_review::{DiffLine, DiffLineKind};
    use std::path::{Path, PathBuf};

    #[test]
    fn parses_inline_reference_with_colon() {
        let text = "Apply change to @src/lib.rs:42 based on review.";
        assert_eq!(
            parse_inline_file_reference(text),
            Some(("src/lib.rs".into(), 42))
        );
    }

    #[test]
    fn parses_inline_reference_with_hash_marker() {
        let text = "Please inspect @src/main.rs#L120 while editing.";
        assert_eq!(
            parse_inline_file_reference(text),
            Some(("src/main.rs".into(), 120))
        );
    }

    #[test]
    fn maps_diff_scroll_to_line_numbers() {
        let file = DiffFileState {
            path: "src/lib.rs".into(),
            lines: vec![
                DiffLine {
                    content: "@@ -1,2 +10,4 @@ fn example()".into(),
                    kind: DiffLineKind::HunkHeader,
                },
                DiffLine {
                    content: " context".into(),
                    kind: DiffLineKind::Context,
                },
                DiffLine {
                    content: "+added".into(),
                    kind: DiffLineKind::Addition,
                },
                DiffLine {
                    content: "-removed".into(),
                    kind: DiffLineKind::Removal,
                },
            ],
            scroll: 0,
        };

        assert_eq!(line_number_from_diff(&file, 1), Some(10));
        assert_eq!(line_number_from_diff(&file, 2), Some(11));
        // Removal lines fall back to the last mapped line number
        assert_eq!(line_number_from_diff(&file, 3), Some(11));
    }

    #[test]
    fn normalize_llm_patch_inserts_headers_when_missing() {
        let patch = "+fn foo() {}\n";
        let normalized = normalize_llm_patch(patch, Path::new("src/lib.rs"), Path::new("/proj"));
        assert!(normalized.contains("--- a/src/lib.rs"));
        assert!(normalized.contains("+++ b/src/lib.rs"));
        assert!(normalized.ends_with('\n'));
    }

    #[test]
    fn build_patch_from_replacement_produces_diff() {
        let content = "fn foo() {}\nfn bar() {}\n";
        let symbol = SymbolSpan {
            file: PathBuf::from("src/lib.rs"),
            name: "foo".to_string(),
            kind: SymbolKind::Function,
            start_line: 1,
            end_line: 1,
            parent: None,
        };
        let replacement = "fn foo() { println!(\"ok\"); }\n";
        let patch = build_patch_from_replacement(content, &symbol, replacement).unwrap();
        assert!(patch.contains("-fn foo() {}"));
        assert!(patch.contains("+fn foo() { println!(\"ok\"); }"));
    }
}
