use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReviewPayload {
    pub diff: String,
    pub files: Vec<String>,
}
