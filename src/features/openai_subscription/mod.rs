//! ChatGPT-plan authentication and stateless Responses transport.
pub mod auth;
pub mod cli;
pub mod credentials;
pub mod models;
pub mod responses;
pub mod sse;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[default]
    OpenaiCompatible,
    OpenaiChatgpt,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderState {
    pub version: u32,
    pub account: String,
    pub model: String,
    pub output: Vec<serde_json::Value>,
    /// Append-only deferred activation names for this conversation turn.
    ///
    /// Empty for assistant state; non-empty only for developer activation
    /// markers (`role == "developer"`, `output` empty). Names only — schemas
    /// resolve from the trusted `ToolCatalog`. Optional for v1 session
    /// compatibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_tool_names: Vec<String>,
}

impl std::fmt::Debug for ProviderState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderState")
            .field("version", &self.version)
            .field("account", &self.account)
            .field("model", &self.model)
            .field("output_items", &self.output.len())
            .field("additional_tool_count", &self.additional_tool_names.len())
            .finish()
    }
}

impl ProviderState {
    /// True when the persisted output contains an opaque server-side
    /// compaction item. The ciphertext is never inspected.
    pub fn contains_compaction(&self) -> bool {
        self.output.iter().any(responses::is_compaction_item)
    }

    /// Index of the latest compaction item in the persisted output, if any.
    /// Centralizes the canonical-boundary scan so agent loop and history
    /// pruning share one definition.
    pub fn latest_compaction_index(&self) -> Option<usize> {
        responses::latest_compaction_index(&self.output)
    }

    /// True for developer activation markers (`output` empty,
    /// `additional_tool_names` non-empty).
    pub fn is_activation_marker(&self) -> bool {
        self.output.is_empty() && !self.additional_tool_names.is_empty()
    }

    /// True for assistant state (`output` non-empty, no activation names).
    pub fn is_assistant_state(&self) -> bool {
        !self.output.is_empty() && self.additional_tool_names.is_empty()
    }

    /// Assistant Responses output constructor.
    pub fn assistant(account: String, model: String, output: Vec<serde_json::Value>) -> Self {
        Self {
            version: 1,
            account,
            model,
            output,
            additional_tool_names: Vec::new(),
        }
    }

    /// Developer activation marker constructor. Names must already be
    /// sorted/deduped by the caller; validation rejects mixed states.
    pub fn activation(account: String, model: String, names: Vec<String>) -> Self {
        Self {
            version: 1,
            account,
            model,
            output: Vec::new(),
            additional_tool_names: names,
        }
    }

    /// Fail-closed validation for the role semantics:
    /// assistant XOR activation, never mixed, never tool/user roles
    /// (role checked by the caller holding the `ChatMessage`).
    pub fn validate_role_binding(&self, role: &str) -> anyhow::Result<()> {
        match role {
            "assistant" => {
                anyhow::ensure!(
                    self.is_assistant_state(),
                    "assistant provider state must carry output without additional tools"
                );
                Ok(())
            }
            "developer" => {
                anyhow::ensure!(
                    self.is_activation_marker(),
                    "developer provider state must carry additional tools without output"
                );
                // Sorted, unique, non-empty names.
                anyhow::ensure!(
                    !self.additional_tool_names.is_empty(),
                    "activation marker must name at least one tool"
                );
                let mut sorted = self.additional_tool_names.clone();
                sorted.sort();
                sorted.dedup();
                anyhow::ensure!(
                    sorted.len() == self.additional_tool_names.len()
                        && sorted == self.additional_tool_names,
                    "activation marker tool names must be sorted and unique"
                );
                for name in &self.additional_tool_names {
                    anyhow::ensure!(!name.is_empty(), "activation tool name must not be empty");
                }
                Ok(())
            }
            _ => anyhow::bail!("provider state is only valid on assistant or developer turns"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "ChatGPT request failed (HTTP {status:?}, code {code}, request {request_id:?}). {recovery}"
)]
pub struct ProviderError {
    pub status: Option<u16>,
    pub code: String,
    pub param: Option<String>,
    pub request_id: Option<String>,
    pub recovery: &'static str,
    pub retryable: bool,
    pub stream_started: bool,
    pub stream_completed: bool,
}

impl ProviderError {
    pub fn during_stream(mut self) -> Self {
        self.stream_started = true;
        self.retryable = false;
        self
    }
    pub fn from_body(
        status: Option<u16>,
        body: &serde_json::Value,
        request_id: Option<String>,
    ) -> Self {
        let error = body.get("error").unwrap_or(body);
        let code = error
            .get("code")
            .and_then(|v| v.as_str())
            .or_else(|| error.as_str())
            .unwrap_or("unknown_error")
            .to_owned();
        let (retryable, recovery) = match code.as_str() {
            "subscription_sharing_usage_limit_exceeded" => (
                false,
                "ChatGPT plan usage is paused. Review limits in ChatGPT Settings > Usage.",
            ),
            "subscription_sharing_user_not_eligible" => (
                false,
                "This account or workspace is not eligible for ChatGPT plan usage.",
            ),
            "subscription_sharing_unsupported_capability"
            | "subscription_sharing_route_not_supported" => {
                (false, "The selected capability or route is unsupported.")
            }
            "subscription_sharing_usage_unavailable" => (
                true,
                "Usage availability is temporarily unavailable. Try again later.",
            ),
            _ if matches!(status, Some(429 | 500 | 502 | 503 | 504)) => {
                (true, "Temporary service failure. Try again later.")
            }
            _ if matches!(status, Some(401 | 403)) => (
                false,
                "Check the selected ChatGPT account and granted permissions; sign in again if access was revoked.",
            ),
            _ => (
                false,
                "Check your account, model, and request. No other billing path was used.",
            ),
        };
        // Do not expose arbitrary server messages: they may echo submitted secrets.
        Self {
            status,
            code: safe_identifier(&code),
            param: error
                .get("param")
                .and_then(|v| v.as_str())
                .map(safe_identifier),
            request_id: request_id.map(|v| safe_identifier(&v)),
            recovery,
            retryable,
            stream_started: false,
            stream_completed: false,
        }
    }
}

fn safe_identifier(s: &str) -> String {
    crate::logging::safe_identifier(s)
}

#[cfg(test)]
mod tests;
