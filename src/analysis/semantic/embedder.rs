use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::fmt;

pub struct Embedder {
    model: TextEmbedding,
}

impl fmt::Debug for Embedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Embedder {{ model: ... }}")
    }
}

impl Embedder {
    pub fn new() -> Result<Self> {
        let mut options = InitOptions::new(EmbeddingModel::AllMiniLML6V2);
        options.show_download_progress = false;

        let model = TextEmbedding::try_new(options)?;
        Ok(Self { model })
    }

    pub fn embed(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.model.embed(texts, None)
    }
}
