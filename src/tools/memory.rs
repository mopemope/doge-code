use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;

#[derive(Debug, Clone)]
pub struct MemoryTools {
    config: Arc<AppConfig>,
}

impl MemoryTools {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }

    fn get_memory_dir(&self) -> PathBuf {
        self.config.project_root.join(".doge/memory")
    }

    fn get_memory_path(&self, key: &str) -> PathBuf {
        // Sanitize key to avoid path traversal?
        // Simple alphanumeric check?
        let sane_key = key.replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "_");
        self.get_memory_dir().join(format!("{}.md", sane_key))
    }

    pub async fn read_memory(&self, key: &str) -> Result<String> {
        let path = self.get_memory_path(key);
        if !path.exists() {
            return Ok(format!("Memory '{}' not found.", key));
        }
        let content = fs::read_to_string(path)
            .await
            .context("Failed to read memory file")?;
        Ok(content)
    }

    pub async fn write_memory(&self, key: &str, content: &str) -> Result<String> {
        let dir = self.get_memory_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .await
                .context("Failed to create memory directory")?;
        }
        let path = self.get_memory_path(key);
        fs::write(&path, content)
            .await
            .context("Failed to write memory file")?;
        Ok(format!("Memory '{}' saved.", key))
    }

    pub async fn list_memories(&self) -> Result<String> {
        let dir = self.get_memory_dir();
        if !dir.exists() {
            return Ok("No memories found.".to_string());
        }
        let mut entries = fs::read_dir(dir)
            .await
            .context("Failed to read memory directory")?;
        let mut memories = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(file_name) = entry.file_name().into_string()
                && file_name.ends_with(".md")
            {
                memories.push(file_name.trim_end_matches(".md").to_string());
            }
        }
        if memories.is_empty() {
            Ok("No memories found.".to_string())
        } else {
            Ok(format!("Available memories:\n- {}", memories.join("\n- ")))
        }
    }
}

pub fn read_memory_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "read_memory".to_string(),
            description: "Read content from a persistent memory (markdown file).".to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The identifier/name of the memory to read."
                    }
                },
                "required": ["key"],
                "additionalProperties": false
            }),
        },
    }
}

pub fn write_memory_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "write_memory".to_string(),
            description:
                "Write content to a persistent memory (markdown file). Overwrites existing memory."
                    .to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The identifier/name of the memory to write."
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to store (markdown format supported)."
                    }
                },
                "required": ["key", "content"],
                "additionalProperties": false
            }),
        },
    }
}

pub fn list_memories_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "list_memories".to_string(),
            description: "List all available memory keys.".to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
    }
}
