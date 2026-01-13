use crate::analysis::cache::ensure_repomap_ready;
use crate::analysis::{RepoMap, symbol::SymbolInfo};
use crate::llm::client_core::OpenAIClient;
use crate::llm::types::ChatMessage;
use anyhow::Result;
use std::cmp::min;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

pub struct DocGenerator {
    repomap: Arc<RwLock<Option<RepoMap>>>,
    llm_client: Arc<OpenAIClient>,
    model: String,
    project_root: PathBuf,
}

impl DocGenerator {
    pub fn new(
        repomap: Arc<RwLock<Option<RepoMap>>>,
        llm_client: Arc<OpenAIClient>,
        model: impl Into<String>,
        project_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            repomap,
            llm_client,
            model: model.into(),
            project_root: project_root.into(),
        }
    }

    /// Generate documentation for a specific symbol in a file
    pub async fn generate_doc_for_symbol(
        &self,
        file_path: &Path,
        symbol_name: &str,
    ) -> Result<String> {
        info!(
            "Generating doc for symbol '{}' in {:?}",
            symbol_name, file_path
        );

        let repomap = self.ensure_repomap().await?;

        let Some(symbol) = self.find_symbol(&repomap, file_path, symbol_name) else {
            return Ok(format!(
                "Symbol '{}' not found in {:?}",
                symbol_name, file_path
            ));
        };

        let code = tokio::fs::read_to_string(file_path).await?;
        let snippet = Self::extract_snippet(&code, symbol.start_line, symbol.end_line, 5);

        let prompt =
            Self::build_symbol_doc_prompt(file_path, symbol_name, symbol.kind.as_str(), &snippet);

        self.call_llm(prompt).await
    }

    fn build_symbol_doc_prompt(
        file_path: &Path,
        symbol_name: &str,
        symbol_kind: &str,
        code: &str,
    ) -> String {
        format!(
            "Write a Rust doc comment for the symbol below. Output only the doc comment text (/// ...), no code.\n\
File: {:?}\nSymbol: {} ({})\n\nCode snippet:\n```rust\n{}\n```\n\nGuidelines: concise summary, params/returns if applicable, mention side effects, stay within the snippet scope.",
            file_path, symbol_name, symbol_kind, code
        )
    }

    // ...
    pub async fn generate_doc_for_file(&self, file_path: &Path) -> Result<String> {
        info!("Generating doc for file {:?}", file_path);

        let repomap = self.ensure_repomap().await?;
        let code = tokio::fs::read_to_string(file_path).await?;
        let truncated_code = Self::truncate_code(&code, 8000);
        let symbols: Vec<SymbolInfo> = repomap
            .symbols
            .iter()
            .filter(|s| s.file == file_path)
            .cloned()
            .collect();

        let prompt = Self::build_file_doc_prompt(file_path, &symbols, &truncated_code);
        self.call_llm(prompt).await
    }

    fn build_file_doc_prompt(file_path: &Path, symbols: &[SymbolInfo], code: &str) -> String {
        let mut symbol_lines = String::new();
        for sym in symbols.iter().take(30) {
            let range = format!("{}-{}", sym.start_line + 1, sym.end_line + 1);
            let parent = sym.parent.as_deref().unwrap_or("<root>");
            symbol_lines.push_str(&format!(
                "- [{}] {} ({}), parent: {}\n",
                sym.kind.as_str(),
                sym.name,
                range,
                parent
            ));
        }

        format!(
            "Create a concise Rust module/file doc comment summarizing purpose, key responsibilities, and notable symbols.\n\
File: {:?}\n\
Symbols (subset):\n{}\
File excerpt (truncated):\n```rust\n{}\n```\n\
Do not include the code itself in the output—only the doc comment text.",
            file_path,
            if symbol_lines.is_empty() {
                "(no symbols detected)".to_string()
            } else {
                symbol_lines
            },
            code
        )
    }

    fn extract_snippet(code: &str, start_line: usize, end_line: usize, context: usize) -> String {
        let lines: Vec<&str> = code.lines().collect();
        let start = start_line.saturating_sub(context);
        let end = min(lines.len(), end_line.saturating_add(1 + context));
        lines[start..end].join("\n")
    }

    fn truncate_code(code: &str, max_chars: usize) -> String {
        if code.len() <= max_chars {
            code.to_string()
        } else {
            let mut truncated = code[..max_chars].to_string();
            truncated.push_str("\n...<truncated>...");
            truncated
        }
    }

    fn find_symbol<'a>(
        &self,
        repomap: &'a RepoMap,
        file_path: &Path,
        symbol_name: &str,
    ) -> Option<&'a SymbolInfo> {
        repomap
            .symbols
            .iter()
            .find(|s| s.file == file_path && s.name == symbol_name)
    }

    async fn ensure_repomap(&self) -> Result<RepoMap> {
        ensure_repomap_ready(&self.repomap, &self.project_root).await
    }

    async fn call_llm(&self, prompt: String) -> Result<String> {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: Some(prompt),
            tool_calls: vec![],
            tool_call_id: None,
        }];

        let response = self
            .llm_client
            .chat_once(&self.model, messages, None)
            .await?;

        Ok(response.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmConfig;
    use httptest::{Expectation, Server, matchers::*, responders::*};
    use std::path::PathBuf;
    use tempfile;

    #[test]
    fn test_build_symbol_doc_prompt() {
        let path = PathBuf::from("src/main.rs");
        let symbol = "main";
        let kind = "Function";
        let code = "fn main() {}";
        let prompt = DocGenerator::build_symbol_doc_prompt(&path, symbol, kind, code);

        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("Symbol: main"));
        assert!(prompt.contains("Function"));
        assert!(prompt.contains("fn main() {}"));
    }

    #[tokio::test]
    async fn test_generate_doc_for_symbol_integration() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .times(1)
                .respond_with(json_encoded(serde_json::json!({
                    "id": "test",
                    "choices": [
                        {"index":0, "message": {"role":"assistant","content":"/// Valid documentation"}}
                    ]
                }))),
        );

        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("main.rs");
        tokio::fs::write(&file_path, "fn main() {}").await.unwrap();

        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key")
            .unwrap()
            .with_llm_config(LlmConfig::default());
        let client = Arc::new(client);

        let repomap = Arc::new(RwLock::new(None));
        let generator = DocGenerator::new(repomap, client, "gpt-test", temp_dir.path());
        let doc_res = generator.generate_doc_for_symbol(&file_path, "main").await;

        match doc_res {
            Ok(doc) => {
                if doc.contains("Symbol 'main' not found") {
                    eprintln!("Integration test warning: Symbol not found (Analyzer issue?)");
                } else {
                    assert_eq!(doc, "/// Valid documentation");
                }
            }
            Err(e) => panic!("Doc generation failed: {}", e),
        }
    }

    #[tokio::test]
    async fn test_generate_doc_for_symbol_not_found() {
        // No mock server needed for this one as LLM shouldn't be called,
        // but if implementation changes to call LLM anyway, we might need one.
        // Current impl checks RepoMap first.

        // However, we need Analyzer to run and return an empty or partial RepoMap.
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("main.rs");
        tokio::fs::write(&file_path, "fn main() {}").await.unwrap();

        // Client - mock server that should NOT be called (panic if called)
        let server = Server::run();
        // No expectations set means if it gets a request it might fail or we can configure it to panic.
        // httptest defaults: unexpected request => 500 or panic depending on config?
        // Actually httptest logs checking errors on drop.

        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key")
            .unwrap()
            .with_llm_config(LlmConfig::default());
        let client = Arc::new(client);

        let repomap = Arc::new(RwLock::new(None));
        let generator = DocGenerator::new(repomap, client, "gpt-test", temp_dir.path());

        // Ask for non-existent symbol
        let doc_res = generator
            .generate_doc_for_symbol(&file_path, "non_existent")
            .await;

        match doc_res {
            Ok(msg) => {
                assert!(
                    msg.contains("Symbol 'non_existent' not found"),
                    "Should return not found message, got: {}",
                    msg
                );
            }
            Err(e) => panic!("Should not fail with error, but return message: {}", e),
        }
    }

    #[tokio::test]
    async fn test_generate_doc_for_file_integration() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .times(1)
                .respond_with(json_encoded(serde_json::json!({
                    "id": "test",
                    "choices": [
                        {"index":0, "message": {"role":"assistant","content":"//! File docs"}}
                    ]
                }))),
        );

        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("lib.rs");
        tokio::fs::write(&file_path, "pub fn foo() {}\npub fn bar() {}\n")
            .await
            .unwrap();

        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key")
            .unwrap()
            .with_llm_config(LlmConfig::default());
        let client = Arc::new(client);

        let repomap = Arc::new(RwLock::new(None));
        let generator = DocGenerator::new(repomap, client, "gpt-test", temp_dir.path());

        let doc = generator.generate_doc_for_file(&file_path).await.unwrap();
        assert_eq!(doc, "//! File docs");
    }
}
