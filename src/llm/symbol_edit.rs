use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::llm::{ChatMessage, ChatRequest};

/// 編集対象の範囲定義（シンボルまたは行範囲）
#[derive(Debug, Clone)]
pub struct EditTarget {
    pub file: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
    /// シンボル名（行範囲指定の場合はNoneまたは説明文）
    pub name: Option<String>,
    /// "function", "struct", "lines" など
    pub kind: String,
}

/// シンボル限定編集用のLLM入力を表すリクエスト。
#[derive(Debug, Clone)]
pub struct SymbolEditRequest {
    pub model: String,
    pub target: EditTarget,
    pub original_code: String,
    pub instruction: String,
    /// Stable symbol ID (`sym-v1-...`). Present for semantic-edit v1 prompts.
    pub symbol_id: Option<String>,
    /// Parent scope (e.g. impl name) if any.
    pub parent: Option<String>,
}

/// シンボル限定編集のLLM応答（解釈後）。
/// semantic-edit v1 では replacement のみを扱う。diff は拒否される。
#[derive(Debug, Clone)]
pub struct SymbolEditResponse {
    /// unified diff (legacy; semantic pathでは常に None)
    pub patch: Option<String>,
    /// シンボル定義全体の置換案
    pub replacement: Option<String>,
    /// 生レスポンス（パース失敗時のデバッグ用）
    pub raw: String,
}

/// ファイル拡張子からコードフェンス言語を決める。
pub fn fence_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts") => "typescript",
        Some("tsx") => "tsx",
        Some("go") => "go",
        Some("java") => "java",
        Some("c" | "h") => "c",
        Some("cpp" | "cxx" | "cc" | "hpp" | "hxx" | "hh") => "cpp",
        Some("cs") => "csharp",
        Some("md") => "markdown",
        _ => "",
    }
}

/// SymbolEditRequest から ChatRequest を構築する（非ストリーミング想定）。
/// semantic-edit v1: 対象Symbol全体の replacement のみを生成させる。
pub fn build_symbol_edit_chat_request(req: &SymbolEditRequest) -> ChatRequest {
    let system = ChatMessage {
        provider_state: None,
        role: "system".to_string(),
        content: Some(
            "You are editing exactly one semantic symbol. Return only its complete replacement."
                .to_string(),
        ),
        tool_calls: Vec::new(),
        tool_call_id: None,
    };

    let target_name = req.target.name.as_deref().unwrap_or("specified lines");
    let symbol_id = req.symbol_id.as_deref().unwrap_or("unknown");
    let parent = req.parent.as_deref().unwrap_or("-");
    let fence = fence_for_path(&req.target.file);
    let fence_open = if fence.is_empty() {
        "```".to_string()
    } else {
        format!("```{fence}")
    };

    let user_content = format!(
        "You are editing exactly one semantic symbol.\n\nSymbol ID:\n{symbol_id}\n\nFile:\n{file}\n\nKind:\n{kind}\n\nName:\n{name}\n\nParent:\n{parent}\n\nOriginal symbol:\n{fence_open}\n{code}\n```\n\nInstruction:\n{inst}\n\nReturn exactly one fenced code block containing the complete replacement\nfor this symbol.\n\nDo not return a diff.\nDo not edit surrounding code.\nDo not rename or move the target symbol in semantic-edit v1.\nDo not include prose outside the code block.",
        file = req.target.file.display(),
        kind = req.target.kind,
        name = target_name,
        code = req.original_code,
        inst = req.instruction,
    );

    let user = ChatMessage {
        provider_state: None,
        role: "user".to_string(),
        content: Some(user_content),
        tool_calls: Vec::new(),
        tool_call_id: None,
    };

    ChatRequest {
        model: req.model.clone(),
        messages: vec![system, user],
        temperature: Some(0.2),
        stream: Some(false),
    }
}

/// Semantic replacement parser (strict, fail-closed).
///
/// 成功条件: exactly one fenced block, non-empty, not diff.
/// 拒否: diff fence, 複数ブロック, 空, 認識不能, 非whitespace prose outside.
pub fn parse_semantic_replacement(raw: &str) -> Result<String> {
    let blocks = collect_fenced_blocks(raw);
    if blocks.is_empty() {
        anyhow::bail!("failed to parse symbol edit response: no recognizable code block");
    }
    if blocks.len() != 1 {
        anyhow::bail!(
            "failed to parse symbol edit response: expected exactly one code block, found {}",
            blocks.len()
        );
    }
    let (lang, body, start, end) = &blocks[0];
    if lang.eq_ignore_ascii_case("diff") {
        anyhow::bail!("failed to parse symbol edit response: diff blocks are not accepted");
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        anyhow::bail!("failed to parse symbol edit response: replacement is empty");
    }
    // Heuristic diff rejection: unified-diff markers without a fence language.
    if looks_like_diff(trimmed) {
        anyhow::bail!("failed to parse symbol edit response: diff content is not accepted");
    }
    // Prose outside the block is rejected (whitespace only allowed).
    let before = raw[..*start].trim();
    let after = raw[*end..].trim();
    if !before.is_empty() || !after.is_empty() {
        anyhow::bail!(
            "failed to parse symbol edit response: prose outside the code block is not allowed"
        );
    }
    Ok(trimmed.to_string())
}

fn looks_like_diff(body: &str) -> bool {
    let mut plus = 0;
    let mut minus = 0;
    for line in body.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            return true;
        }
        if line.starts_with("@@") {
            return true;
        }
        if line.starts_with('+') && !line.starts_with("++") {
            plus += 1;
        } else if line.starts_with('-') && !line.starts_with("--") {
            minus += 1;
        }
    }
    plus > 0 && minus > 0
}

fn collect_fenced_blocks(src: &str) -> Vec<(String, String, usize, usize)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut cursor = 0usize;
    while let Some(rel_start) = src[cursor..].find("```") {
        let start = cursor + rel_start;
        let after_open = start + 3;
        // Language tag: up to end of line.
        let rest = &src[after_open..];
        let line_end = rest.find('\n').map(|i| after_open + i).unwrap_or(src.len());
        let lang = src[after_open..line_end].trim().to_string();
        let body_start = if line_end < src.len() {
            line_end + 1
        } else {
            src.len()
        };
        let Some(rel_end) = src[body_start..].find("```") else {
            break;
        };
        let body_end = body_start + rel_end;
        let body = src[body_start..body_end].to_string();
        let end = body_end + 3;
        out.push((lang, body, start, end));
        cursor = end;
        if cursor >= bytes.len() {
            break;
        }
    }
    out
}

/// LLMレスポンス文字列から SymbolEditResponse を構築する。
///
/// semantic-edit v1 では replacement のみ。diff はエラーになる。
pub fn parse_symbol_edit_response(raw: &str) -> Result<SymbolEditResponse> {
    let replacement = parse_semantic_replacement(raw)?;
    Ok(SymbolEditResponse {
        patch: None,
        replacement: Some(replacement),
        raw: raw.to_string(),
    })
}

/// Legacy line-range parser for `/fix`.
///
/// Preserves the pre-semantic behavior (diff-first, lenient about surrounding
/// prose) so `/fix` keeps working for line-range edits while `/edit-symbol`
/// uses the strict semantic parser above.
pub fn parse_legacy_symbol_edit_response(raw: &str) -> Result<SymbolEditResponse> {
    if let Some(patch) = extract_code_block(raw, "diff") {
        return Ok(SymbolEditResponse {
            patch: Some(patch),
            replacement: None,
            raw: raw.to_string(),
        });
    }
    if let Some(code) = extract_code_block(raw, "rust").or_else(|| extract_code_block(raw, "")) {
        return Ok(SymbolEditResponse {
            patch: None,
            replacement: Some(code),
            raw: raw.to_string(),
        });
    }
    Err(anyhow::anyhow!(
        "failed to parse symbol edit response: no recognizable code block"
    ))
}

fn extract_code_block(src: &str, lang: &str) -> Option<String> {
    let fence = if lang.is_empty() {
        "```"
    } else {
        &format!("```{lang}")
    };
    let start = src.find(fence)?;
    let after_fence = &src[start + fence.len()..];
    let after_lang = if !lang.is_empty() {
        after_fence.strip_prefix('\n').unwrap_or(after_fence)
    } else {
        after_fence
    };
    let end = after_lang.find("```")?;
    Some(after_lang[..end].trim().to_string())
}

/// ヘルパー: ファイルとシンボル範囲から元コードを抽出する。
/// 呼び出し側でシンボル限定編集前に利用する想定。
/// ヘルパー: ファイルと範囲から元コードを抽出する。
pub fn read_target_source(path: &Path, start_line: u32, end_line: u32) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read file {}", path.display()))?;

    let mut result = String::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        if line_no >= start_line && line_no <= end_line {
            result.push_str(line);
            result.push('\n');
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_symbol_edit_response_rejects_diff() {
        let raw = "```diff\n- old\n+ new\n```";
        assert!(parse_symbol_edit_response(raw).is_err());
    }

    #[test]
    fn parse_symbol_edit_response_rejects_diff_without_fence_lang() {
        let raw = "```\n- old\n+ new\n```";
        assert!(parse_symbol_edit_response(raw).is_err());
    }

    #[test]
    fn parse_symbol_edit_response_rejects_multiple_blocks() {
        let raw = "```rust\nfn a() {}\n```\n```rust\nfn b() {}\n```";
        assert!(parse_symbol_edit_response(raw).is_err());
    }

    #[test]
    fn parse_symbol_edit_response_rejects_prose_outside() {
        let raw = "Here is patch:\n```rust\nfn foo() {}\n```";
        assert!(parse_symbol_edit_response(raw).is_err());
    }

    #[test]
    fn parse_symbol_edit_response_rejects_empty() {
        let raw = "```rust\n   \n```";
        assert!(parse_symbol_edit_response(raw).is_err());
    }

    #[test]
    fn parse_symbol_edit_response_accepts_single_replacement() {
        let raw = "```rust\nfn foo() {}\n```";
        let resp = parse_symbol_edit_response(raw).unwrap();
        assert!(resp.patch.is_none());
        assert_eq!(resp.replacement.as_deref(), Some("fn foo() {}"));
    }

    #[test]
    fn parse_symbol_edit_response_accepts_generic_fence() {
        let raw = "```python\ndef foo():\n    pass\n```";
        let resp = parse_symbol_edit_response(raw).unwrap();
        assert!(resp.replacement.unwrap().contains("def foo"));
    }

    #[test]
    fn parse_legacy_accepts_diff_for_fix() {
        let raw = "Here is patch:\n```diff\n- old\n+ new\n```";
        let resp = parse_legacy_symbol_edit_response(raw).unwrap();
        assert!(resp.patch.is_some());
    }

    #[test]
    fn build_request_contains_symbol_id_and_replacement_only() {
        let req = SymbolEditRequest {
            model: "m".to_string(),
            target: EditTarget {
                file: PathBuf::from("src/auth.rs"),
                start_line: 10,
                end_line: 20,
                name: Some("authenticate".to_string()),
                kind: "method".to_string(),
            },
            original_code: "fn authenticate() {}".to_string(),
            instruction: "add logging".to_string(),
            symbol_id: Some("sym-v1-abc".to_string()),
            parent: Some("AuthService".to_string()),
        };
        let chat = build_symbol_edit_chat_request(&req);
        let user = chat.messages.iter().find(|m| m.role == "user").unwrap();
        let content = user.content.as_deref().unwrap();
        assert!(content.contains("sym-v1-abc"));
        assert!(content.contains("Do not return a diff"));
        assert!(!content.contains("unified diff"));
    }

    #[test]
    fn fence_for_path_maps_extensions() {
        assert_eq!(fence_for_path(Path::new("a.rs")), "rust");
        assert_eq!(fence_for_path(Path::new("a.py")), "python");
        assert_eq!(fence_for_path(Path::new("a.unknown_ext_xyz")), "");
    }
}
