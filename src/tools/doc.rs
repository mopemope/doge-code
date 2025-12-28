use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::features::doc_skill::generator::DocGenerator;
use crate::llm::OpenAIClient;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "doc_generate".to_string(),
            strict: None,
            description: "Generates documentation for a specific symbol or file using LLM and RepoMap context.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path to the file"},
                    "symbol": {"type": "string", "description": "Name of the symbol to document (optional, if omitted documents the whole file)"}
                },
                "required": ["path"]
            }),
        },
    }
}

pub async fn doc_generate(
    path: &str,
    symbol: Option<&str>,
    config: &AppConfig,
    repomap: Arc<RwLock<Option<RepoMap>>>,
) -> Result<String> {
    // Check API key first
    let api_key = config
        .api_key
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("API key not set"))?;

    // Instantiate OpenAIClient with configured timeouts
    let client =
        OpenAIClient::new(config.base_url.clone(), api_key)?.with_llm_config(config.llm.clone());
    let client_arc = Arc::new(client);

    let generator = DocGenerator::new(
        repomap,
        client_arc,
        config.model.clone(),
        config.project_root.clone(),
    );
    let path_obj = std::path::Path::new(path);

    if let Some(sym) = symbol {
        generator.generate_doc_for_symbol(path_obj, sym).await
    } else {
        generator.generate_doc_for_file(path_obj).await
    }
}
