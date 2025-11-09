use std::path::Path;

use anyhow::{Context, Result};

use crate::analysis::SymbolSpan;
use crate::llm::{ChatMessage, ChatRequest};

/// シンボル限定編集用のLLM入力を表すリクエスト。
#[derive(Debug, Clone)]
pub struct SymbolEditRequest {
    pub model: String,
    pub symbol: SymbolSpan,
    pub original_code: String,
    pub instruction: String,
}

/// シンボル限定編集のLLM応答（解釈後）。
/// 現時点では最小限で、将来必要になればフィールド追加する。
#[derive(Debug, Clone)]
pub struct SymbolEditResponse {
    /// unified diff (推奨)
    pub patch: Option<String>,
    /// シンボル定義全体の置換案
    pub replacement: Option<String>,
    /// 生レスポンス（パース失敗時のデバッグ用）
    pub raw: String,
}

/// SymbolEditRequest から ChatRequest を構築する（非ストリーミング想定）。
/// 実際の送信は既存のクライアントに委譲する。
pub fn build_symbol_edit_chat_request(req: &SymbolEditRequest) -> ChatRequest {
    let system = ChatMessage {
        role: "system".to_string(),
        content: Some(
            "You are an AI code editor. Edit ONLY the specified symbol in the given Rust code. \
             Respond with either a unified diff or a full replacement of that symbol. \
             Do not modify any other parts of the file."
                .to_string(),
        ),
        tool_calls: Vec::new(),
        tool_call_id: None,
    };

    let user_content = format!(
        "Target file: {file}\nTarget symbol: {name} ({kind}) lines {start}-{end}\n\nOriginal symbol code:\n```rust\n{code}\n```\n\nInstruction:\n{inst}\n\nOutput format:\n- Prefer unified diff starting with '```diff' and including only changes for this file; or\n- A full replacement of the symbol definition inside '```rust' code fences.\n- Do not include explanations outside code blocks.",
        file = req.symbol.file.display(),
        name = req.symbol.name,
        kind = format_symbol_kind(&req.symbol),
        start = req.symbol.start_line,
        end = req.symbol.end_line,
        code = req.original_code,
        inst = req.instruction,
    );

    let user = ChatMessage {
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

fn format_symbol_kind(symbol: &SymbolSpan) -> &'static str {
    use crate::analysis::SymbolKind;
    match symbol.kind {
        SymbolKind::Function => "function",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Impl => "impl",
        SymbolKind::Method => "method",
        SymbolKind::AssocFn => "assoc_fn",
        SymbolKind::Mod => "mod",
        SymbolKind::Variable => "var",
        SymbolKind::Comment => "comment",
    }
}

/// LLMレスポンス文字列から SymbolEditResponse を構築する簡易パーサ。
///
/// 優先順位:
/// 1. ```diff ... ``` ブロックを unified diff として採用
/// 2. ```rust ... ``` または ``` ... ``` 内の単一コードブロックを replacement として採用
pub fn parse_symbol_edit_response(raw: &str) -> Result<SymbolEditResponse> {
    // diff ブロック検出
    if let Some(patch) = extract_code_block(raw, "diff") {
        return Ok(SymbolEditResponse {
            patch: Some(patch),
            replacement: None,
            raw: raw.to_string(),
        });
    }

    // rust または無指定コードブロックを replacement として扱う
    if let Some(code) = extract_code_block(raw, "rust").or_else(|| extract_code_block(raw, "")) {
        return Ok(SymbolEditResponse {
            patch: None,
            replacement: Some(code),
            raw: raw.to_string(),
        });
    }

    // コードブロックが見つからない場合は、そのまま返す（呼び出し側で再試行・確認用）
    Err(anyhow::anyhow!(
        "failed to parse symbol edit response: no recognizable code block"
    ))
}

fn extract_code_block(src: &str, lang: &str) -> Option<String> {
    let fence = if lang.is_empty() {
        "```"
    } else {
        // 例: ```diff, ```rust
        &format!("```{}", lang)
    };

    let start = src.find(fence)?;
    let after_fence = &src[start + fence.len()..];

    // lang付きの場合は改行を1つスキップ
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
pub fn read_symbol_source(path: &Path, span: &SymbolSpan) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read file {}", path.display()))?;

    let mut result = String::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        if line_no >= span.start_line && line_no <= span.end_line {
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
    fn parse_symbol_edit_response_prefers_diff() {
        let raw = "Here is patch:\n```diff\n- old\n+ new\n```";
        let resp = parse_symbol_edit_response(raw).unwrap();
        assert!(resp.patch.is_some());
        assert!(resp.replacement.is_none());
    }

    #[test]
    fn parse_symbol_edit_response_uses_rust_block_as_replacement() {
        let raw = "```rust\nfn foo() {}\n```";
        let resp = parse_symbol_edit_response(raw).unwrap();
        assert!(resp.patch.is_none());
        assert_eq!(resp.replacement.as_deref(), Some("fn foo() {}"));
    }

    #[test]
    fn extract_code_block_works_for_plain_block() {
        let raw = "before```\ncode\n```after";
        let code = extract_code_block(raw, "").unwrap();
        assert_eq!(code, "code");
    }
}
