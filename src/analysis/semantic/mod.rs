pub mod embedder;

use super::database::entities::{action_log, symbol_embedding, symbol_info};
use crate::analysis::semantic::embedder::Embedder;
use crate::config::RagConfig;
use anyhow::{Context, Result};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, JoinType, PaginatorTrait, QueryFilter,
    QuerySelect, RelationTrait, Set,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

#[derive(Clone, Debug)]
pub struct SemanticService {
    db_conn: DatabaseConnection,
    embedder: Arc<Mutex<Option<Embedder>>>,
    config: RagConfig,
}

impl SemanticService {
    pub fn new(db_conn: DatabaseConnection, config: RagConfig) -> Self {
        Self {
            db_conn,
            embedder: Arc::new(Mutex::new(None)),
            config,
        }
    }

    /// Initializes the embedder if not already done.
    async fn ensure_embedder_ready(&self) -> Result<()> {
        let mut guard = self.embedder.lock().await;
        if guard.is_none() {
            info!("Initializing semantic embedder model...");
            let embedder = Embedder::new().context("Failed to initialize embedder")?;
            *guard = Some(embedder);
            info!("Semantic embedder model initialized.");
        }
        Ok(())
    }

    /// Generates embeddings for symbols that don't have them yet.
    pub async fn update_embeddings(&self, project_root: &Path) -> Result<()> {
        if !self.config.enabled {
            debug!("Semantic embeddings disabled; skipping update.");
            return Ok(());
        }

        let project_root_str = project_root.to_str().unwrap_or_default();

        // 1. Find symbols without embeddings
        // We do a LEFT JOIN and filter where embedding is NULL
        // sea-orm approach:
        let symbols = symbol_info::Entity::find()
            .filter(symbol_info::Column::ProjectRoot.eq(project_root_str))
            .find_also_related(symbol_embedding::Entity)
            .all(&self.db_conn)
            .await
            .context("Failed to fetch symbols for embedding check")?;

        let mut missing_embeddings = Vec::new();
        for (symbol, embedding_opt) in symbols {
            if embedding_opt.is_none() {
                missing_embeddings.push(symbol);
            }
        }

        if missing_embeddings.is_empty() {
            debug!("No symbols missing embeddings.");
            return Ok(());
        }

        info!(
            "Found {} symbols missing embeddings. generating...",
            missing_embeddings.len()
        );

        self.ensure_embedder_ready().await?;
        let mut guard = self.embedder.lock().await;
        let embedder = guard.as_mut().unwrap();

        // 2. Prepare text for embedding
        // We need to group by file to avoid reading the same file multiple times
        let mut symbols_by_file: HashMap<String, Vec<symbol_info::Model>> = HashMap::new();
        for sym in missing_embeddings {
            symbols_by_file
                .entry(sym.file_path.clone())
                .or_default()
                .push(sym);
        }

        let mut texts = Vec::new();
        let mut symbol_ids = Vec::new();

        for (file_path, syms) in symbols_by_file {
            let path = PathBuf::from(&file_path);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to read file {}: {}", file_path, e);
                    continue;
                }
            };
            let lines: Vec<&str> = content.lines().collect();

            for sym in syms {
                // Extract code snippet
                let start = sym.start_line as usize;
                let end = sym.end_line as usize;
                if start < lines.len() {
                    let end = std::cmp::min(end + 1, lines.len()); // inclusive? tree-sitter is 0-indexed
                    let code = lines[start..end].join("\n");

                    // Format: "name: kind\ncode"
                    // Or just code? "kind name\ncode" is better.
                    // docstrings are usually above the start_line in tree-sitter if captured?
                    // The `SymbolInfo` doesn't strictly guarantee docstrings are inside start/end.
                    // But we'll use what we have.
                    let text = format!(
                        "{} {}
{}",
                        sym.kind, sym.name, code
                    );

                    // Truncate to avoid token limits? fastembed handles truncation usually.
                    // But excessively long functions might dilute the vector.
                    // Let's limit to ~50 lines of code roughly or 2000 chars.
                    // TODO: Consider a more sophisticated chunking strategy (e.g., overlapping windows or AST-based semantic chunking).
                    let truncated_text = if text.len() > 2000 {
                        text.chars().take(2000).collect()
                    } else {
                        text
                    };

                    texts.push(truncated_text);
                    symbol_ids.push(sym.id);
                }
            }
        }

        if texts.is_empty() {
            return Ok(());
        }

        // 3. Generate Embeddings (Batch)
        // Batches of 32 or 64
        let batch_size = self.config.batch_size;
        for (chunk_texts, chunk_ids) in texts.chunks(batch_size).zip(symbol_ids.chunks(batch_size))
        {
            let embeddings = embedder.embed(chunk_texts.to_vec())?;

            // 4. Save to DB
            let mut active_models = Vec::new();
            for (id, emb) in chunk_ids.iter().zip(embeddings.into_iter()) {
                // Serialize embedding
                let blob: Vec<u8> = Self::serialize_embedding(&emb);

                active_models.push(symbol_embedding::ActiveModel {
                    symbol_id: Set(*id),
                    embedding: Set(blob),
                    model_version: Set("all-MiniLM-L6-v2".to_string()),
                    ..Default::default()
                });
            }

            // Bulk insert
            if !active_models.is_empty() {
                symbol_embedding::Entity::insert_many(active_models)
                    .exec(&self.db_conn)
                    .await
                    .context("Failed to insert embeddings")?;
            }
        }

        info!("Successfully generated and saved embeddings.");
        Ok(())
    }

    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        project_root: &Path,
    ) -> Result<Vec<(symbol_info::Model, f32)>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        self.ensure_embedder_ready().await?;
        let mut guard = self.embedder.lock().await;
        let embedder = guard.as_mut().unwrap();

        // 1. Embed Query
        let query_embedding = embedder
            .embed(vec![query.to_string()])?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Failed to embed query"))?;

        // 2. Fetch all embeddings for project
        // Note: For large repos, this should use an index or vector DB.
        // For < 10k symbols, bruteforce is acceptable.
        // TODO: Performance bottle-neck. Loading ALL embeddings into memory for every search is inefficient for large codebases.
        // Consider integrating `sqlite-vec` or using an external vector store (e.g., Qdrant, Chroma) if scaling is needed.
        let project_root_str = project_root.to_str().unwrap_or_default();

        let results = symbol_embedding::Entity::find()
            .find_also_related(symbol_info::Entity)
            .filter(symbol_info::Column::ProjectRoot.eq(project_root_str))
            .all(&self.db_conn)
            .await?;

        // 3. Calculate Cosine Similarity
        let mut scored_results: Vec<(symbol_info::Model, f32)> = Vec::new();

        for (emb_model, info_opt) in results {
            if let Some(info_model) = info_opt {
                let vec = Self::deserialize_embedding(&emb_model.embedding);
                let score = cosine_similarity(&query_embedding, &vec);
                scored_results.push((info_model, score));
            }
        }

        // 4. Sort and Limit
        scored_results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored_results.truncate(limit);

        Ok(scored_results)
    }

    pub async fn has_embeddings(&self, project_root: &Path) -> Result<bool> {
        if !self.config.enabled {
            return Ok(false);
        }

        let project_root_str = project_root.to_str().unwrap_or_default();
        let count = symbol_embedding::Entity::find()
            .join(
                JoinType::InnerJoin,
                symbol_embedding::Relation::SymbolInfo.def(),
            )
            .filter(symbol_info::Column::ProjectRoot.eq(project_root_str))
            .count(&self.db_conn)
            .await
            .context("Failed to count symbol embeddings")?;

        Ok(count > 0)
    }

    pub fn auto_update_enabled(&self) -> bool {
        self.config.auto_update
    }

    fn serialize_embedding(vec: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(vec.len() * 4);
        for &f in vec {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        bytes
    }

    fn deserialize_embedding(bytes: &[u8]) -> Vec<f32> {
        let mut vec = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks(4) {
            if let Ok(bytes) = chunk.try_into() {
                vec.push(f32::from_le_bytes(bytes));
            }
        }
        vec
    }
    pub async fn log_action(
        &self,
        session_id: &str,
        action_type: &str,
        content: &str,
        metadata: serde_json::Value,
    ) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        self.ensure_embedder_ready().await?;
        let mut guard = self.embedder.lock().await;
        let embedder = guard.as_mut().unwrap();

        // Embed content (and maybe thought from metadata?)
        // For now, embed the content + metadata string representation for context
        let text_to_embed = format!("{}\n{}", content, metadata);
        let embedding = embedder
            .embed(vec![text_to_embed])?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Failed to embed action"))?;

        let blob = Self::serialize_embedding(&embedding);
        let now = chrono::Utc::now().to_rfc3339();

        let active_model = action_log::ActiveModel {
            session_id: Set(session_id.to_string()),
            timestamp: Set(now),
            action_type: Set(action_type.to_string()),
            content: Set(content.to_string()),
            metadata: Set(metadata),
            embedding: Set(blob),
            ..Default::default()
        };

        action_log::Entity::insert(active_model)
            .exec(&self.db_conn)
            .await
            .context("Failed to insert action log")?;

        Ok(())
    }

    pub async fn search_action_history(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(action_log::Model, f32)>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        self.ensure_embedder_ready().await?;
        let mut guard = self.embedder.lock().await;
        let embedder = guard.as_mut().unwrap();

        let query_embedding = embedder
            .embed(vec![query.to_string()])?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Failed to embed query"))?;

        // Fetch all action logs (Optimization: Filter by session? Or search global knowledge?)
        // For "learning", global search is better.
        // TODO: Optimization needed for large history.
        let logs = action_log::Entity::find().all(&self.db_conn).await?;

        let mut scored_results = Vec::new();
        for log in logs {
            let vec = Self::deserialize_embedding(&log.embedding);
            let score = cosine_similarity(&query_embedding, &vec);
            scored_results.push((log, score));
        }

        scored_results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored_results.truncate(limit);

        Ok(scored_results)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot_product: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot_product / (norm_a * norm_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        // Same vector = 1.0
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < f32::EPSILON);

        let c = vec![0.0, 1.0, 0.0];
        // Orthogonal vectors = 0.0
        assert!((cosine_similarity(&a, &c)).abs() < f32::EPSILON);

        let d = vec![-1.0, 0.0, 0.0];
        // Opposite vectors = -1.0
        assert!((cosine_similarity(&a, &d) - -1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_serialization_round_trip() {
        let original = vec![0.123, -0.456, 1.0, 0.0];
        let serialized = SemanticService::serialize_embedding(&original);
        let deserialized = SemanticService::deserialize_embedding(&serialized);

        assert_eq!(original.len(), deserialized.len());
        for (o, d) in original.iter().zip(deserialized.iter()) {
            assert!((o - d).abs() < f32::EPSILON);
        }
    }
}

#[cfg(test)]
mod test_history;
