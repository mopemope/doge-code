use super::auth::{AuthHandle, json_response};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct Model {
    pub slug: String,
    #[serde(default)]
    pub display_name: String,
    pub visibility: String,
}

pub fn catalog(body: serde_json::Value) -> Result<Vec<Model>> {
    let models: Vec<Model> = serde_json::from_value(
        body.get("models")
            .context("invalid ChatGPT model catalog")?
            .clone(),
    )?;
    Ok(models
        .into_iter()
        .filter(|model| model.visibility == "list")
        .collect())
}

pub async fn fetch(handle: &AuthHandle, cancel: &CancellationToken) -> Result<Vec<Model>> {
    let bearer = handle.bearer(cancel).await?;
    tokio::select! {
        _ = cancel.cancelled() => bail!(crate::llm::LlmErrorKind::Cancelled),
        result = async {
            let response = handle.http.get(format!("{}/models", handle.resource.trim_end_matches('/')))
                .bearer_auth(bearer).send().await.map_err(|_| anyhow::anyhow!("ChatGPT model catalog request failed"))?;
            catalog(json_response(response).await?)
        } => result,
    }
}
