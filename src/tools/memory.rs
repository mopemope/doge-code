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
        let key = crate::tools::budget::safe_take_chars(key, 128).to_string();
        tokio::task::spawn_blocking(move || {
            match read_memory_file(&path, MAX_MEMORY_FILE_BYTES) {
                Ok(content) => bounded_content(&content),
                Err(error) if error.downcast_ref::<crate::utils::bounded_file::ReadError>()
                    .is_some_and(|e| matches!(e, crate::utils::bounded_file::ReadError::Io(io) if io.kind() == std::io::ErrorKind::NotFound)) =>
                    bounded_content(&format!("Memory '{key}' not found.")),
                Err(error) => Err(error),
            }
        }).await.context("Memory read worker failed")?
            .map_err(crate::tools::budget::bounded_read_error)
    }

    pub async fn write_memory(
        &self,
        key: &str,
        content: &str,
        tags: Option<Vec<String>>,
        metadata: Option<Value>,
    ) -> Result<String> {
        let frontmatter = MemoryFrontmatter {
            tags: tags.unwrap_or_default(),
            metadata: metadata.unwrap_or(json!({})),
        };

        let yaml = serde_saphyr::to_string(&frontmatter)?;
        let full_content = format!("---\n{}---\n{}", yaml, content);
        anyhow::ensure!(
            full_content.len() <= MAX_MEMORY_FILE_BYTES,
            "Memory exceeds 1 MiB file limit; existing memory retained"
        );

        let dir = self.get_memory_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .await
                .context("Failed to create memory directory")?;
        }

        let path = self.get_memory_path(key);
        fs::write(&path, full_content)
            .await
            .context("Failed to write memory file")?;

        Ok(format!("Memory '{}' saved.", key))
    }

    pub async fn list_memories(&self) -> Result<String> {
        let dir = self.get_memory_dir();
        tokio::task::spawn_blocking(move || {
            let keys = memory_inventory(&dir)?;
            complete_keys("Available memories", "No memories found.", &keys)
        })
        .await
        .context("Memory inventory worker failed")?
        .map_err(crate::tools::budget::bounded_read_error)
    }

    pub async fn search_memory(
        &self,
        query: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<String> {
        let dir = self.get_memory_dir();
        tokio::task::spawn_blocking(move || {
            search_memories(&dir, query.as_deref(), tags.as_deref())
        })
        .await
        .context("Memory search worker failed")?
        .map_err(crate::tools::budget::bounded_read_error)
    }
}

const MAX_MEMORY_FILE_BYTES: usize = 1024 * 1024;
const MAX_MEMORY_SCAN_BYTES: usize = 8 * 1024 * 1024;
const MAX_MEMORY_ENTRIES: usize = 1024;
const MAX_MEMORY_OUTPUT_CHARS: usize = 6000;

fn read_memory_file(path: &std::path::Path, limit: usize) -> Result<String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let name = crate::tools::budget::safe_take_chars(&name, 64);
    let bytes = crate::utils::bounded_file::read_regular_bytes(
        path,
        limit,
        crate::utils::bounded_file::SymlinkPolicy::Follow,
    )
    .map_err(|error| {
        let message = format!("Memory file {name:?} unavailable: {error}");
        anyhow::Error::new(error).context(message)
    })?;
    String::from_utf8(bytes).with_context(|| format!("Memory file {name:?} is not valid UTF-8"))
}

/// Complete inventory, bounded even when entries are not memory files.
fn memory_inventory(dir: &std::path::Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).context("Memory inventory unavailable"),
    };
    let mut keys = Vec::new();
    for (index, entry) in entries.enumerate() {
        anyhow::ensure!(
            index < MAX_MEMORY_ENTRIES,
            "Memory inventory exceeds 1024 entries; reduce the directory before retrying"
        );
        let entry = entry.context("Memory inventory entry unavailable")?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        anyhow::ensure!(
            path.metadata()
                .context("Memory input unavailable")?
                .is_file(),
            "Memory input is not a regular file"
        );
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Memory key is not valid UTF-8"))?;
        keys.push(name[..name.len() - 3].to_string());
    }
    keys.sort();
    Ok(keys)
}

fn search_memories(
    dir: &std::path::Path,
    query: Option<&str>,
    tags: Option<&[String]>,
) -> Result<String> {
    let keys = memory_inventory(dir)?;
    if keys.is_empty() {
        return Ok("No memories found.".into());
    }
    let query = query.map(str::to_lowercase);
    let mut consumed = 0;
    let mut matches = Vec::new();
    for key in keys {
        let limit = MAX_MEMORY_FILE_BYTES.min(MAX_MEMORY_SCAN_BYTES - consumed);
        let content = read_memory_file(&dir.join(format!("{key}.md")), limit)?;
        consumed += content.len();
        let (frontmatter, body) = parse_memory_file(&content);
        if tags.is_some_and(|wanted| !wanted.iter().any(|t| frontmatter.tags.contains(t))) {
            continue;
        }
        if query
            .as_ref()
            .is_some_and(|q| !key.to_lowercase().contains(q) && !body.to_lowercase().contains(q))
        {
            continue;
        }
        matches.push(key);
    }
    complete_keys("Matching memories", "No matching memories found.", &matches)
}

fn fits_memory_output(text: &str) -> bool {
    // `content` is the larger of the unchanged dispatch content/result envelopes.
    json!({"content": text}).to_string().chars().count() <= MAX_MEMORY_OUTPUT_CHARS
}
fn complete_keys(label: &str, empty: &str, keys: &[String]) -> Result<String> {
    let text = if keys.is_empty() {
        empty.to_string()
    } else {
        format!("{label}:\n- {}", keys.join("\n- "))
    };
    anyhow::ensure!(
        fits_memory_output(&text),
        "Complete memory keys exceed the output limit; use search_memory with a narrower query or read_memory with a known key"
    );
    Ok(text)
}
fn bounded_content(content: &str) -> Result<String> {
    let mut limit = MAX_MEMORY_OUTPUT_CHARS;
    loop {
        let bounded = crate::tools::budget::head_truncate(content, limit);
        if fits_memory_output(&bounded.text) {
            return Ok(bounded.text);
        }
        limit = (limit / 2).max(1);
    }
}

fn parse_memory_file(content: &str) -> (MemoryFrontmatter, String) {
    if content.starts_with("---")
        && let Some(end_idx) = content[3..].find("---")
    {
        let yaml_str = &content[3..end_idx + 3];
        let body = &content[end_idx + 6..]; // 3 for start --- + 3 for end --- + yaml len
        if let Ok(fm) = serde_saphyr::from_str::<MemoryFrontmatter>(yaml_str) {
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
            // Optional fields retain omission/default semantics; strict requires all properties.
            strict: Some(false),
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
                "required": [],
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
            // Optional fields retain omission/default semantics; strict requires all properties.
            strict: Some(false),
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
    async fn unreadable_memory_is_not_reported_as_a_successful_search() -> Result<()> {
        let root = TempDir::new()?;
        let dir = root.path().join(".doge/memory");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("broken.md"), [0xff, 0xfe])?;
        let tools = MemoryTools::new(Arc::new(AppConfig {
            project_root: root.path().into(),
            ..Default::default()
        }));
        assert!(
            tools.search_memory(None, None).await.is_err(),
            "invalid UTF-8 must not become a normal matching key"
        );
        assert!(
            tools
                .search_memory(Some("anything".into()), None)
                .await
                .is_err(),
            "read failure must not become no matches"
        );
        assert_eq!(std::fs::read(dir.join("broken.md"))?, [0xff, 0xfe]);
        Ok(())
    }

    #[test]
    fn legacy_yaml_frontmatter_preserves_metadata_and_roundtrips() {
        let legacy = "---\ntags:\n- architecture\n- 'on'\nmetadata:\n  enabled: true\n  count: 2\n  label: '001'\n  empty: null\n  note: |\n    first line\n    second line\n---\n日本語 body\n";
        let (frontmatter, body) = parse_memory_file(legacy);
        assert_eq!(frontmatter.tags, ["architecture", "on"]);
        assert_eq!(
            frontmatter.metadata,
            json!({"enabled":true,"count":2,"label":"001","empty":null,"note":"first line\nsecond line\n"})
        );
        assert_eq!(body, "日本語 body");
        let encoded = serde_saphyr::to_string(&frontmatter).expect("serialize metadata");
        let restored: MemoryFrontmatter =
            serde_saphyr::from_str(&encoded).expect("reopen metadata");
        assert_eq!(restored.metadata, frontmatter.metadata);
        assert_eq!(restored.tags, frontmatter.tags);
    }

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

#[cfg(test)]
mod bounded_tests;
