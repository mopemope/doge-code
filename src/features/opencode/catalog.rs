//! Offline endpoint snapshot. Routing and CLI listings use these same entries.
use super::{ApiKind, ProviderKind, model_id};
use anyhow::{Result, bail};

pub const SNAPSHOT: &str = "2026-10-09";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayApi {
    ChatCompletions,
    Responses,
    Messages,
    Google,
    SystemOne,
}
impl GatewayApi {
    pub fn name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "Chat Completions",
            Self::Responses => "Responses",
            Self::Messages => "Messages",
            Self::Google => "Google",
            Self::SystemOne => "SystemOne",
        }
    }
    pub fn adapter(self) -> Option<ApiKind> {
        match self {
            Self::ChatCompletions => Some(ApiKind::ChatCompletions),
            Self::Responses => Some(ApiKind::Responses),
            _ => None,
        }
    }
    pub fn reason(self) -> &'static str {
        match self {
            Self::ChatCompletions | Self::Responses => {
                "implemented adapter; live availability not checked"
            }
            _ => "unsupported API adapter in dgc; no request or fallback",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub api: GatewayApi,
}
const OPENCODEGO: &[ModelSpec] = &[
    ModelSpec {
        id: "grok-4.7",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "grok-4.6",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-6-luna",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.6-luna",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "muse-spark-1.3-contributor",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "muse-spark-1.2-contributor",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "kimi-k3",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "kimi-k2.7-code",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "kimi-k2.6",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "longcat-2.0",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "longcat-2.5-preview-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "step-5-preview-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "mimo-v2.6-flash",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "mimo-v2.6-pro",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "mimo-v2.5",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "mimo-v2.5-pro",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "hy4-preview",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "hy3",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "space-bunny",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "glm-5.3-flash",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "glm-5.3",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "glm-5.2",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "deepseek-v4.1-flash",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "deepseek-v4-pro",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "deepseek-v4-flash",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "deepseek-v4-flash-vision-exp",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "claude-haiku-5-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "minimax-m3",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "minimax-m2.7",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "qwen3.8-max",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "qwen3.8-flash",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "qwen3.7-plus",
        api: GatewayApi::Messages,
    },
];
const OPENCODEZEN: &[ModelSpec] = &[
    ModelSpec {
        id: "gpt-6-astra",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-6-sol",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-6.1-sol",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-6-luna",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.6-sol",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.6-terra",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.6-luna",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.5",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.5-pro",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.4",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.4-pro",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.4-mini",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.4-nano",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.3-codex",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.3-codex-spark",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.2",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.2-codex",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.1",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.1-codex",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.1-codex-max",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5.1-codex-mini",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5-codex",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "gpt-5-nano",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "grok-4.7",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "grok-4.6",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "grok-4.5",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "grok-build-0.1",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "muse-spark-1.3",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "muse-spark-1.2",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "muse-spark-1.3-contributor-free",
        api: GatewayApi::Responses,
    },
    ModelSpec {
        id: "qwen3.8-max",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "minimax-m3",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "minimax-m2.7",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "minimax-m2.5",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "glm-5.1",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "glm-5",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "kimi-k2.5",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "kimi-k2.6",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "kimi-k2.7-code",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "kimi-k3",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "mistral-large-4",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "big-pickle",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "space-bunny-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "longcat-2.5-preview-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "step-5-preview-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "exo-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "mimo-v2.6-flash-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "mimo-v2.5-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "ling-3.1-flash-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "ling-3.0-flash-fin-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "nemotron-3-ultra-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "nemotron-3.5-lightning-free",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "glm-5.3-flash",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "glm-5.3",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "glm-5.2",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "deepseek-v4.1-flash",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "deepseek-v4-pro",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "deepseek-v4-flash",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "deepseek-v4-flash-vision-exp",
        api: GatewayApi::ChatCompletions,
    },
    ModelSpec {
        id: "claude-fable-5-1",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-fable-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-opus-5-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-opus-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-opus-4-8",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-opus-4-7",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-opus-4-6",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-opus-4-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-sonnet-5-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-sonnet-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-sonnet-4-6",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-sonnet-4-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-haiku-5-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "claude-haiku-4-5",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "qwen3.8-flash",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "qwen3.7-max",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "qwen3.7-plus",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "qwen3.6-plus",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "qwen3.5-plus",
        api: GatewayApi::Messages,
    },
    ModelSpec {
        id: "gemini-3.8-flash",
        api: GatewayApi::Google,
    },
    ModelSpec {
        id: "gemini-3.7-flash",
        api: GatewayApi::Google,
    },
    ModelSpec {
        id: "gemini-3.6-flash",
        api: GatewayApi::Google,
    },
    ModelSpec {
        id: "gemini-3.5-flash",
        api: GatewayApi::Google,
    },
    ModelSpec {
        id: "gemini-3.5-flash-lite",
        api: GatewayApi::Google,
    },
    ModelSpec {
        id: "gemini-3.1-pro",
        api: GatewayApi::Google,
    },
    ModelSpec {
        id: "gemini-3-flash",
        api: GatewayApi::Google,
    },
    ModelSpec {
        id: "jev-1.13",
        api: GatewayApi::SystemOne,
    },
    ModelSpec {
        id: "jev-1.13-free",
        api: GatewayApi::SystemOne,
    },
];
pub fn catalog(provider: ProviderKind) -> &'static [ModelSpec] {
    match provider {
        ProviderKind::OpencodeGo => OPENCODEGO,
        ProviderKind::OpencodeZen => OPENCODEZEN,
        _ => &[],
    }
}
pub fn lookup(provider: ProviderKind, model: &str) -> Result<&'static ModelSpec> {
    let id = model_id(provider, model)?;
    catalog(provider).iter().find(|entry| entry.id == id).ok_or_else(|| anyhow::anyhow!(
        "Unknown OpenCode model {id:?} in offline snapshot {SNAPSHOT}; run dgc models --provider {}", super::provider_name(provider)))
}
pub fn print_catalog(provider: ProviderKind, model: Option<&str>) -> Result<()> {
    if catalog(provider).is_empty() {
        bail!("No offline catalog for this provider");
    }
    println!(
        "{} documented gateway models — offline snapshot {}",
        super::provider_name(provider),
        SNAPSHOT
    );
    println!(
        "Source: https://docs.opencode.ai/docs/{}/",
        if provider == ProviderKind::OpencodeGo {
            "go"
        } else {
            "zen"
        }
    );
    println!(
        "No config, credentials or network access; availability and account access are not checked."
    );
    println!("MODEL ID | API | DGC SUPPORT | REASON");
    let selected = model.map(|id| lookup(provider, id)).transpose()?;
    for entry in catalog(provider)
        .iter()
        .filter(|e| selected.is_none_or(|s| e.id == s.id))
    {
        println!(
            "{} | {} | {} | {}",
            entry.id,
            entry.api.name(),
            if entry.api.adapter().is_some() {
                "yes"
            } else {
                "no"
            },
            entry.api.reason()
        );
    }
    if let Some(spec) = selected {
        println!("Canonical model ID: {}", spec.id);
        if spec.api.adapter().is_none() {
            bail!("Selected model is documented but unsupported by dgc");
        }
    }
    Ok(())
}
