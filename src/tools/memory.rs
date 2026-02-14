use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;

#[derive(Debug, Clone)]
pub struct MemoryTools {
    config: Arc<AppConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MemoryFrontmatter {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    metadata: Value,
}

impl MemoryTools {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }

    fn get_memory_dir(&self) -> PathBuf {
        self.config.project_root.join(".doge/memory")
    }

    fn get_memory_path(&self, key: &str) -> PathBuf {
        // Sanitize key to avoid path traversal
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

    pub async fn write_memory(
        &self,
        key: &str,
        content: &str,
        tags: Option<Vec<String>>,
        metadata: Option<Value>,
    ) -> Result<String> {
        let dir = self.get_memory_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .await
                .context("Failed to create memory directory")?;
        }

        let frontmatter = MemoryFrontmatter {
            tags: tags.unwrap_or_default(),
            metadata: metadata.unwrap_or(json!({})),
        };

        let yaml = serde_yaml::to_string(&frontmatter)?;
        let full_content = format!("---\n{}---\n{}", yaml, content);

        let path = self.get_memory_path(key);
        fs::write(&path, full_content)
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

    pub async fn search_memory(
        &self,
        query: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<String> {
        let dir = self.get_memory_dir();
        if !dir.exists() {
            return Ok("No memories found.".to_string());
        }

        let mut matched_files = Vec::new();
        let mut entries = fs::read_dir(dir)
            .await
            .context("Failed to read memory dir")?;

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                let content = fs::read_to_string(&path).await.unwrap_or_default();
                let key = path.file_stem().unwrap().to_string_lossy().to_string();

                let (frontmatter, body) = parse_memory_file(&content);

                // Filter by tags
                if let Some(target_tags) = &tags {
                    let file_tags = &frontmatter.tags;
                    if !target_tags.iter().any(|t| file_tags.contains(t)) {
                        continue;
                    }
                }

                // Filter by query (simple string match)
                if let Some(q) = &query {
                    let q_lower = q.to_lowercase();
                    if !key.to_lowercase().contains(&q_lower)
                        && !body.to_lowercase().contains(&q_lower)
                    {
                        continue;
                    }
                }

                matched_files.push(key);
            }
        }

        if matched_files.is_empty() {
            Ok("No matching memories found.".to_string())
        } else {
            Ok(format!(
                "Matching memories:\n- {}",
                matched_files.join("\n- ")
            ))
        }
    }
}

fn parse_memory_file(content: &str) -> (MemoryFrontmatter, String) {
    if content.starts_with("---")
        && let Some(end_idx) = content[3..].find("---")
    {
        let yaml_str = &content[3..end_idx + 3];
        let body = &content[end_idx + 6..]; // 3 for start --- + 3 for end --- + yaml len
        if let Ok(fm) = serde_yaml::from_str::<MemoryFrontmatter>(yaml_str) {
            return (fm, body.trim().to_string());
        }
    }
    // Fallback
    (
        MemoryFrontmatter {
            tags: vec![],
            metadata: json!({}),
        },
        content.to_string(),
    )
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
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional tags for categorization."
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Optional arbitrary metadata."
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

pub fn search_memory_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "search_memory".to_string(),
            description: "Search memories by query string (content/key) and/or tags.".to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "String to search for in memory key or content."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter memories that have any of these tags."
                    }
                },
                "additionalProperties": false
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_memory_lifecycle() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let config = Arc::new(AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        });
        let tools = MemoryTools::new(config);

        // 1. Write memory with tags
        let tags = Some(vec!["arch".to_string(), "database".to_string()]);
        let metadata = Some(json!({ "version": 1 }));
        tools
            .write_memory("db_schema", "Schema design...", tags, metadata)
            .await?;

        // 2. Read memory
        let content = tools.read_memory("db_schema").await?;
        assert!(content.contains("Schema design..."));
        assert!(content.contains("tags:"));
        assert!(content.contains("arch"));

        // 3. Search by query
        let result = tools
            .search_memory(Some("Schema".to_string()), None)
            .await?;
        assert!(result.contains("db_schema"));

        // 4. Search by tag
        let result = tools
            .search_memory(None, Some(vec!["database".to_string()]))
            .await?;
        assert!(result.contains("db_schema"));

        // 5. Search by non-matching tag
        let result = tools
            .search_memory(None, Some(vec!["frontend".to_string()]))
            .await?;
        assert!(result.contains("No matching memories"));

        Ok(())
    }
}
