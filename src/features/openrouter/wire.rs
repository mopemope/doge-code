use super::{DecodedResponse, SelectedModelMetadata};
use crate::config::{ReasoningEffort, ReasoningMode};
use anyhow::Result;

fn exact_metadata<'a>(
    metadata: Option<&'a SelectedModelMetadata>,
    model: &str,
) -> Option<&'a SelectedModelMetadata> {
    metadata.filter(|m| {
        m.provider == crate::features::openai_subscription::ProviderKind::Openrouter
            && m.model.id == model
    })
}

pub fn resolve_effort(
    metadata: Option<&SelectedModelMetadata>,
    model: &str,
    mode: ReasoningMode,
    requested: Option<ReasoningEffort>,
) -> Result<Option<ReasoningEffort>> {
    use super::EffortSupport;
    if mode == ReasoningMode::Off {
        return Ok(None);
    }
    let Some(requested) = requested else {
        return Ok(None);
    };
    let support = exact_metadata(metadata, model).map(|m| &m.model.reasoning.supported_efforts);
    let supported = match support {
        Some(EffortSupport::GatewayAccepted) => Some(true),
        Some(EffortSupport::Listed(values)) => {
            Some(values.iter().any(|s| s == requested.as_api_str()))
        }
        _ => None,
    };
    match (mode, supported) {
        (ReasoningMode::Fixed, Some(false)) => anyhow::bail!(crate::llm::LlmErrorKind::Client),
        (ReasoningMode::Fixed, _) | (ReasoningMode::Auto, Some(true)) => Ok(Some(requested)),
        _ => Ok(None),
    }
}

pub fn adapt_request_value(
    request: &mut serde_json::Value,
    metadata: Option<&SelectedModelMetadata>,
    model: &str,
    mode: ReasoningMode,
    requested: Option<ReasoningEffort>,
    has_tools: bool,
) -> Result<()> {
    super::model_id(model)?;
    if let Some(meta) = exact_metadata(metadata, model) {
        anyhow::ensure!(
            meta.model
                .output_modalities
                .as_ref()
                .is_none_or(|values| values.iter().any(|s| s == "text")),
            crate::llm::LlmErrorKind::Client
        );
        if has_tools {
            anyhow::ensure!(
                meta.model
                    .supported_parameters
                    .as_ref()
                    .is_none_or(|values| values.iter().any(|s| s == "tools")),
                crate::llm::LlmErrorKind::Client
            );
        }
    }
    let effort = resolve_effort(metadata, model, mode, requested)?;
    let object = request
        .as_object_mut()
        .ok_or(crate::llm::LlmErrorKind::Client)?;
    object.remove("reasoning_effort");
    object.remove("reasoning");
    object.remove("models");
    object.insert("model".into(), model.into());
    object.insert("stream".into(), false.into());
    if let Some(effort) = effort {
        object.insert(
            "reasoning".into(),
            serde_json::json!({"effort":effort.as_api_str()}),
        );
    }
    if has_tools {
        object.insert(
            "provider".into(),
            serde_json::json!({"require_parameters":true}),
        );
    }
    anyhow::ensure!(
        !object
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| messages
                .iter()
                .any(|m| m.get("provider_state").is_some_and(|v| !v.is_null()))),
        crate::llm::LlmErrorKind::Client
    );
    Ok(())
}

pub fn decode_response(value: serde_json::Value) -> Result<DecodedResponse> {
    use crate::llm::LlmErrorKind;
    anyhow::ensure!(
        value.is_object() && !value.get("error").is_some_and(|v| !v.is_null()),
        LlmErrorKind::Client
    );
    let choices = value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .ok_or(LlmErrorKind::Deserialize)?;
    anyhow::ensure!(choices.len() == 1, LlmErrorKind::Deserialize);
    let choice = &choices[0];
    let mut message = choice
        .get("message")
        .cloned()
        .ok_or(LlmErrorKind::Deserialize)?;
    anyhow::ensure!(
        message.get("role").and_then(serde_json::Value::as_str) == Some("assistant"),
        LlmErrorKind::Client
    );
    anyhow::ensure!(
        !message.get("provider_state").is_some_and(|v| !v.is_null()),
        LlmErrorKind::Client
    );
    anyhow::ensure!(
        !message.get("refusal").is_some_and(|v| !v.is_null())
            && !choice.get("error").is_some_and(|v| !v.is_null()),
        LlmErrorKind::Incomplete
    );
    if message
        .get("reasoning_content")
        .is_none_or(serde_json::Value::is_null)
        && let Some(reasoning) = message.get("reasoning").cloned()
    {
        message["reasoning_content"] = reasoning;
    }
    if let Some(details) = message.get("reasoning_details") {
        anyhow::ensure!(
            details.is_null() || details.is_array(),
            LlmErrorKind::Deserialize
        );
    }
    if let Some(calls) = message.get("tool_calls").filter(|v| !v.is_null()) {
        let calls = calls.as_array().ok_or(LlmErrorKind::Deserialize)?;
        for call in calls {
            anyhow::ensure!(
                call.pointer("/function/arguments")
                    .and_then(serde_json::Value::as_str)
                    .is_some(),
                LlmErrorKind::Client
            );
        }
    }
    let message: crate::llm::ChoiceMessageWithTools =
        serde_json::from_value(message).map_err(|_| LlmErrorKind::Deserialize)?;
    let has_tools = !message.tool_calls.is_empty();
    let finish = choice
        .get("finish_reason")
        .and_then(serde_json::Value::as_str)
        .ok_or(LlmErrorKind::Incomplete)?;
    crate::llm::types::validate_completion(Some(finish), has_tools)?;
    anyhow::ensure!(
        has_tools
            || message
                .content
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
        LlmErrorKind::Incomplete
    );
    let prompt_tokens = value
        .pointer("/usage/prompt_tokens")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    let usage = value
        .get("usage")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok());
    Ok(DecodedResponse {
        message,
        usage,
        prompt_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn openrouter_effort_policy_does_not_guess_or_escalate() {
        assert_eq!(
            resolve_effort(
                None,
                "publisher/model",
                ReasoningMode::Auto,
                Some(ReasoningEffort::High)
            )
            .unwrap(),
            None
        );
        assert_eq!(
            resolve_effort(
                None,
                "publisher/model",
                ReasoningMode::Fixed,
                Some(ReasoningEffort::Low)
            )
            .unwrap(),
            Some(ReasoningEffort::Low)
        );
        assert_eq!(
            resolve_effort(
                None,
                "publisher/model",
                ReasoningMode::Off,
                Some(ReasoningEffort::High)
            )
            .unwrap(),
            None
        );
    }
    #[test]
    fn openrouter_request_uses_chat_and_required_parameters() {
        let mut value =
            json!({"model":"publisher/model:free", "messages":[], "reasoning_effort":"high"});
        adapt_request_value(
            &mut value,
            None,
            "publisher/model:free",
            ReasoningMode::Fixed,
            Some(ReasoningEffort::Low),
            true,
        )
        .unwrap();
        assert_eq!(value["model"], "publisher/model:free");
        assert_eq!(value["reasoning"]["effort"], "low");
        assert_eq!(value["provider"]["require_parameters"], true);
        assert!(value.get("reasoning_effort").is_none());
        assert!(value.get("models").is_none());
    }
    fn response() -> serde_json::Value {
        json!({"choices":[{"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}]})
    }
    #[test]
    fn openrouter_response_rejects_error_refusal_and_incomplete() {
        for patch in [json!({"error":{"message":"SECRET"}}), json!({"choices":[]})] {
            assert!(decode_response(patch).is_err());
        }
        for reason in [
            serde_json::Value::Null,
            json!("length"),
            json!("content_filter"),
            json!("error"),
            json!("refusal"),
        ] {
            let mut v = response();
            v["choices"][0]["finish_reason"] = reason;
            assert!(decode_response(v).is_err());
        }
        for (key, value) in [
            ("role", json!("user")),
            ("content", json!("  ")),
            ("refusal", json!("SECRET")),
            ("provider_state", json!({"SECRET":true})),
        ] {
            let mut v = response();
            v["choices"][0]["message"][key] = value;
            let e = decode_response(v).unwrap_err();
            assert!(!format!("{e:#?}").contains("SECRET"));
        }
        let mut v = response();
        v["usage"] = json!({"prompt_tokens":12});
        let d = decode_response(v).unwrap();
        assert!(d.usage.is_none());
        assert_eq!(d.prompt_tokens, Some(12));
    }
    #[test]
    fn openrouter_response_preserves_reasoning_and_validates_before_dispatch() {
        let details = json!([{"type":"reasoning.encrypted","data":"opaque-secret","index":0},{"text":"keep order"}]);
        let mut v = response();
        v["choices"][0]["message"]["reasoning"] = json!("alias");
        v["choices"][0]["message"]["reasoning_content"] = json!("preferred");
        v["choices"][0]["message"]["reasoning_details"] = details.clone();
        let d = decode_response(v).unwrap();
        assert_eq!(
            d.message.reasoning.reasoning_content.as_deref(),
            Some("preferred")
        );
        assert_eq!(d.message.reasoning.reasoning_details, Some(details));
    }

    #[test]
    fn openrouter_effort_exact_metadata_and_known_capabilities() {
        use super::super::{
            CatalogPrices, EffortSupport, OpenRouterModel, ReasoningMetadata, SelectedModelMetadata,
        };
        let mut meta = SelectedModelMetadata {
            provider: crate::features::openai_subscription::ProviderKind::Openrouter,
            fetched_at: chrono::Utc::now(),
            model: OpenRouterModel {
                id: "pub/model".into(),
                name: None,
                input_modalities: None,
                output_modalities: None,
                supported_parameters: None,
                context_window: None,
                prices: CatalogPrices::default(),
                reasoning: ReasoningMetadata {
                    supported_efforts: EffortSupport::Listed(vec!["low".into()]),
                    mandatory: Some(true),
                },
            },
        };
        assert_eq!(
            resolve_effort(
                Some(&meta),
                "pub/model",
                ReasoningMode::Auto,
                Some(ReasoningEffort::Low)
            )
            .unwrap(),
            Some(ReasoningEffort::Low)
        );
        assert_eq!(
            resolve_effort(
                Some(&meta),
                "pub/model",
                ReasoningMode::Auto,
                Some(ReasoningEffort::High)
            )
            .unwrap(),
            None
        );
        assert!(
            resolve_effort(
                Some(&meta),
                "pub/model",
                ReasoningMode::Fixed,
                Some(ReasoningEffort::High)
            )
            .is_err()
        );
        assert_eq!(
            resolve_effort(
                Some(&meta),
                "pub/other",
                ReasoningMode::Fixed,
                Some(ReasoningEffort::High)
            )
            .unwrap(),
            Some(ReasoningEffort::High)
        );
        meta.model.reasoning.supported_efforts = EffortSupport::GatewayAccepted;
        assert_eq!(
            resolve_effort(
                Some(&meta),
                "pub/model",
                ReasoningMode::Auto,
                Some(ReasoningEffort::High)
            )
            .unwrap(),
            Some(ReasoningEffort::High)
        );
        meta.model.supported_parameters = Some(vec![]);
        assert!(
            adapt_request_value(
                &mut json!({}),
                Some(&meta),
                "pub/model",
                ReasoningMode::Off,
                None,
                true
            )
            .is_err()
        );
    }
    #[test]
    fn openrouter_tool_only_response_and_reasoning_round_trip() {
        let details = json!([{"type":"reasoning.encrypted","data":"opaque","index":1}]);
        let v = json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"reasoning":"thinking","reasoning_details":details,"tool_calls":[{"id":"call1","type":"function","function":{"name":"find_file","arguments":"{}"}}]}}]});
        let d = decode_response(v).unwrap();
        let saved = serde_json::to_value(&d.message).unwrap();
        let restored: crate::llm::ChoiceMessageWithTools =
            serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(restored.reasoning.reasoning_details, Some(details.clone()));
        assert_eq!(saved["reasoning_details"], details);
        assert_eq!(saved["reasoning_content"], "thinking");
    }
}
