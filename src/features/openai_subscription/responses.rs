use super::{
    ProviderError, ProviderState,
    auth::AuthHandle,
    sse::{Decoder, InferenceEvent},
};
use crate::{
    config::ReasoningEffort,
    llm::{ChatMessage, ChoiceMessageWithTools, ToolCall, ToolCallFunction, ToolDef, Usage},
};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Serialize)]
pub struct Request {
    model: String,
    pub(crate) input: Vec<Value>,
    pub(crate) tools: Vec<Value>,
    store: bool,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_management: Option<Vec<ContextManagement>>,
}

/// Server-side native compaction directive for `POST /responses`.
///
/// Sent as `context_management: [{type: "compaction", compact_threshold}]`
/// with `store:false` / `stream:true` preserved. Never interpret, summarize,
/// or rewrite the opaque compaction state server-side produces.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ContextManagement {
    #[serde(rename = "type")]
    kind: &'static str,
    compact_threshold: u32,
}

impl ContextManagement {
    pub fn compaction(compact_threshold: u32) -> Self {
        Self {
            kind: "compaction",
            compact_threshold,
        }
    }
}

/// Minimum `compact_threshold` accepted by the Responses API.
pub const RESPONSES_MIN_COMPACT_THRESHOLD: u32 = 1_000;

/// Validate a resolved native-compaction threshold without silent clamping.
pub fn validate_compact_threshold(threshold: u32) -> Result<()> {
    if threshold < RESPONSES_MIN_COMPACT_THRESHOLD {
        bail!(
            "Responses native compaction threshold {threshold} is below the API minimum {}; lower the configured auto-compaction threshold is not supported for openai-chatgpt",
            RESPONSES_MIN_COMPACT_THRESHOLD
        );
    }
    Ok(())
}

/// True when a Responses output item is an opaque server-side compaction item.
pub fn is_compaction_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("compaction")
}

fn validate_compaction_item(item: &Value) -> Result<()> {
    if item.get("type").and_then(Value::as_str) != Some("compaction") {
        bail!("Responses compaction item must have type compaction");
    }
    let encrypted = item.get("encrypted_content").and_then(Value::as_str);
    if encrypted.is_none_or(|s| s.is_empty()) {
        bail!("Responses compaction item missing encrypted_content");
    }
    if let Some(id) = item.get("id")
        && !id.is_string()
    {
        bail!("Responses compaction item id must be a string");
    }
    Ok(())
}

/// Index of the latest compaction item in a Responses output array, if any.
pub fn latest_compaction_index(output: &[Value]) -> Option<usize> {
    output.iter().rposition(is_compaction_item)
}

/// Canonical continuation output: everything from the latest compaction item
/// onward (inclusive). Items before the boundary are not needed for
/// continuation and must not be replayed or dispatched from.
pub fn canonical_output(output: &[Value]) -> &[Value] {
    match latest_compaction_index(output) {
        Some(index) => &output[index..],
        None => output,
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let additional_items = self
            .input
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("additional_tools"))
            .count();
        let additional_tools: usize = self
            .input
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                    item.get("tools").and_then(Value::as_array).map(|v| v.len())
                } else {
                    None
                }
            })
            .sum();
        f.debug_struct("ResponsesRequest")
            .field("model", &self.model)
            .field("input_items", &self.input.len())
            .field("tool_namespaces", &self.tools.len())
            .field("base_tool_count", &base_tool_count(&self.tools))
            .field("additional_tool_count", &additional_tools)
            .field("provider_input_state_count", &additional_items)
            .finish_non_exhaustive()
    }
}

fn base_tool_count(tools: &[Value]) -> usize {
    tools
        .iter()
        .filter_map(|ns| ns.get("tools").and_then(Value::as_array))
        .map(|v| v.len())
        .sum()
}

/// Wire exposure for Responses append-only validation.
///
/// `base` is the immutable initial top-level namespace; `additional` is the
/// set of deferred tools already appended via `additional_tools` input
/// items. Provider visibility never bypasses local `ToolCatalog` checks.
#[derive(Debug, Clone, Default)]
pub struct ResponseToolExposure {
    pub base: std::collections::BTreeSet<String>,
    pub additional: std::collections::BTreeSet<String>,
}

impl ResponseToolExposure {
    pub fn from_defs(base_tools: &[ToolDef], active_tools: &[ToolDef]) -> Self {
        let base: std::collections::BTreeSet<String> =
            base_tools.iter().map(|t| t.function.name.clone()).collect();
        let active: std::collections::BTreeSet<String> = active_tools
            .iter()
            .map(|t| t.function.name.clone())
            .collect();
        let additional: std::collections::BTreeSet<String> =
            active.difference(&base).cloned().collect();
        Self { base, additional }
    }

    pub fn contains_base(&self, name: &str) -> bool {
        self.base.contains(name)
    }

    pub fn contains_additional(&self, name: &str) -> bool {
        self.additional.contains(name)
    }
}

fn tool_function_json(tool: &ToolDef) -> Value {
    json!({"type":"function", "name":tool.function.name, "description":tool.function.description, "parameters":tool.function.parameters, "strict":tool.function.strict.unwrap_or(false)})
}

fn additional_tools_item(tools: Vec<Value>) -> Value {
    json!({"type":"additional_tools","role":"developer","tools":tools})
}

pub fn build(
    model: &str,
    account: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    effort: Option<ReasoningEffort>,
    compact_threshold: Option<u32>,
) -> Result<Request> {
    // Legacy lenient projection for existing callers/tests: top-level is
    // `tools`, history replays verbatim without append-only exposure checks.
    // Production Responses path uses `build_with_activation`.
    if let Some(threshold) = compact_threshold {
        validate_compact_threshold(threshold)?;
    }
    let mut input = Vec::new();
    let mut calls = std::collections::HashSet::new();
    let mut outputs = std::collections::HashSet::new();
    for message in messages {
        if let Some(state) = &message.provider_state {
            if state.version != 1 || state.account != account || state.model != model {
                bail!(
                    "Responses session belongs to a different account or model; start a new session or restore its original selection"
                );
            }
            if message.role != "assistant" {
                bail!("provider state is only valid on assistant turns");
            }
            for item in &state.output {
                validate_output(item)?;
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .context("function call ID missing")?;
                    if !calls.insert(id.to_owned()) {
                        bail!("duplicate function call ID");
                    }
                }
                input.push(item.clone());
            }
            continue;
        }
        match message.role.as_str() {
            "system" | "developer" | "user" | "assistant" => {
                if let Some(content) = &message.content {
                    input.push(json!({"role": if message.role == "system" { "developer" } else { &message.role }, "content": content}));
                }
                for call in &message.tool_calls {
                    let id = call
                        .id
                        .as_deref()
                        .context("tool call ID missing in history")?;
                    if !calls.insert(id.to_owned()) {
                        bail!("duplicate tool call ID");
                    }
                    input.push(json!({"type":"function_call","name":call.function.name,"namespace":"dgc","arguments":call.function.arguments,"call_id":id}));
                }
            }
            "tool" => {
                let id = message
                    .tool_call_id
                    .as_deref()
                    .context("tool result call ID missing")?;
                if !calls.contains(id) || !outputs.insert(id.to_owned()) {
                    bail!("orphan or duplicate tool result");
                }
                input.push(json!({"type":"function_call_output","call_id":id,"output":message.content.as_deref().unwrap_or("")}));
            }
            _ => bail!("unsupported conversation role"),
        }
    }
    let functions: Vec<_> = tools.iter().map(tool_function_json).collect();
    Ok(Request {
        model: model.to_owned(),
        input,
        tools: if functions.is_empty() {
            vec![]
        } else {
            vec![
                json!({"type":"namespace","name":"dgc","description":"doge-code local tools","tools":functions}),
            ]
        },
        store: false,
        stream: true,
        reasoning: effort.map(|e| json!({"effort":e.as_api_str()})),
        context_management: compact_threshold
            .map(|threshold| vec![ContextManagement::compaction(threshold)]),
    })
}

/// Append-only Responses request projection.
///
/// Top-level `tools` is exactly the stable `base_tools` namespace.
/// Deferred activations appear only as `additional_tools` input items
/// appended after their `tool_search` result, never by rewriting the
/// prefix. Schemas resolve solely from `active_tools` (trusted catalog
/// snapshot); unknown or inactive names fail closed.
pub fn build_with_activation(
    model: &str,
    account: &str,
    messages: &[ChatMessage],
    base_tools: &[ToolDef],
    active_tools: &[ToolDef],
    effort: Option<ReasoningEffort>,
    compact_threshold: Option<u32>,
) -> Result<Request> {
    if let Some(threshold) = compact_threshold {
        validate_compact_threshold(threshold)?;
    }
    let base_map: std::collections::BTreeMap<&str, &ToolDef> = base_tools
        .iter()
        .map(|t| (t.function.name.as_str(), t))
        .collect();
    let active_map: std::collections::BTreeMap<&str, &ToolDef> = active_tools
        .iter()
        .map(|t| (t.function.name.as_str(), t))
        .collect();
    let base_names: std::collections::BTreeSet<String> =
        base_map.keys().map(|s| (*s).to_string()).collect();
    let active_names: std::collections::BTreeSet<String> =
        active_map.keys().map(|s| (*s).to_string()).collect();
    // Fail closed on ambiguous wiring input: duplicate names with
    // conflicting schemas would collapse silently in the maps above.
    if base_tools.len() != base_map.len() {
        bail!("duplicate tool names in base tools");
    }
    if active_tools.len() != active_map.len() {
        bail!("duplicate tool names in active tools");
    }
    let mut input = Vec::new();
    let mut calls = std::collections::HashSet::new();
    let mut outputs = std::collections::HashSet::new();
    let mut wire_available: std::collections::BTreeSet<String> = base_names.clone();
    // Helper: emit a deterministic `additional_tools` item for names that
    // are active non-base, validating catalog membership and fail-closed
    // duplicates. Updates `wire_available`.
    let emit_additional = |names: &[String],
                           wire_available: &mut std::collections::BTreeSet<String>,
                           input: &mut Vec<Value>|
     -> Result<()> {
        if names.is_empty() {
            return Ok(());
        }
        let mut sorted = names.to_vec();
        sorted.sort();
        sorted.dedup();
        // Callers pass sorted-unique lists; a length mismatch means the
        // persisted batch contained duplicates.
        anyhow::ensure!(
            sorted.len() == names.len(),
            "duplicate tool names in activation batch"
        );
        for name in &sorted {
            anyhow::ensure!(
                active_names.contains(name),
                "activation references unknown or inactive tool; refusing to wire session-tampered capability"
            );
            anyhow::ensure!(
                !base_names.contains(name),
                "activation must not re-expose base namespace tools"
            );
            if !wire_available.insert(name.clone()) {
                bail!("duplicate additional_tools exposure for '{name}'");
            }
        }
        let mut functions = Vec::new();
        for name in &sorted {
            let def = active_map.get(name.as_str()).with_context(|| {
                format!("trusted catalog has no schema for activated tool '{name}'")
            })?;
            functions.push(tool_function_json(def));
        }
        input.push(additional_tools_item(functions));
        Ok(())
    };
    // Legacy repair helper: ensure a historical call name is wired before
    // its first usage. Inserts a deterministic single-tool marker when the
    // name is active non-base but not yet exposed.
    let ensure_wired_for_historical_call =
        |name: &str,
         wire_available: &mut std::collections::BTreeSet<String>,
         input: &mut Vec<Value>|
         -> Result<()> {
            if wire_available.contains(name) {
                return Ok(());
            }
            if base_names.contains(name) {
                wire_available.insert(name.to_string());
                return Ok(());
            }
            anyhow::ensure!(
                active_names.contains(name),
                "historical call references unknown tool '{name}'"
            );
            // Active non-base without a marker: legacy session without provider
            // activation state. Insert the deterministic compatibility item.
            emit_additional(
                std::slice::from_ref(&name.to_string()),
                wire_available,
                input,
            )
        };
    for message in messages {
        if let Some(state) = &message.provider_state {
            if state.version != 1 || state.account != account || state.model != model {
                bail!(
                    "Responses session belongs to a different account or model; start a new session or restore its original selection"
                );
            }
            state.validate_role_binding(&message.role)?;
            if message.role == "developer" {
                // Developer activation marker: account/model/version already
                // checked; validate names against the trusted active set.
                anyhow::ensure!(
                    state.output.is_empty(),
                    "developer activation state must not carry provider output"
                );
                anyhow::ensure!(
                    !state.additional_tool_names.is_empty(),
                    "developer activation state must name tools"
                );
                emit_additional(
                    &state.additional_tool_names,
                    &mut wire_available,
                    &mut input,
                )?;
                continue;
            }
            if message.role != "assistant" {
                bail!("provider state is only valid on assistant turns");
            }
            for item in &state.output {
                validate_output(item)?;
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .context("function call ID missing")?;
                    if !calls.insert(id.to_owned()) {
                        bail!("duplicate function call ID");
                    }
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .context("function call name missing")?;
                    let namespace = item.get("namespace").and_then(Value::as_str);
                    match namespace {
                        Some("dgc") => {
                            if !base_names.contains(name) {
                                // Legacy wire: older sessions stored deferred
                                // calls under `dgc`. Repair exposure before
                                // replaying verbatim (no canonical rewrite).
                                ensure_wired_for_historical_call(
                                    name,
                                    &mut wire_available,
                                    &mut input,
                                )?;
                            } else if !wire_available.contains(name) {
                                wire_available.insert(name.to_string());
                            }
                        }
                        None => {
                            // New additional call: must already be exposed.
                            // Legacy missing-marker case self-heals here.
                            ensure_wired_for_historical_call(
                                name,
                                &mut wire_available,
                                &mut input,
                            )?;
                        }
                        Some(other) => {
                            bail!("unrecognized Responses function namespace '{other}'");
                        }
                    }
                    if !active_names.contains(name) && !base_names.contains(name) {
                        bail!("historical call references tool outside the active catalog");
                    }
                }
                input.push(item.clone());
            }
            continue;
        }
        match message.role.as_str() {
            "system" | "developer" | "user" | "assistant" => {
                if let Some(content) = &message.content {
                    input.push(json!({"role": if message.role == "system" { "developer" } else { &message.role }, "content": content}));
                }
                for call in &message.tool_calls {
                    let id = call
                        .id
                        .as_deref()
                        .context("tool call ID missing in history")?;
                    if !calls.insert(id.to_owned()) {
                        bail!("duplicate tool call ID");
                    }
                    let name = call.function.name.as_str();
                    if base_names.contains(name) {
                        wire_available.insert(name.to_string());
                        input.push(json!({"type":"function_call","name":call.function.name,"namespace":"dgc","arguments":call.function.arguments,"call_id":id}));
                    } else if active_names.contains(name) {
                        // Historical additional call without provider state:
                        // repair exposure, then emit unnamespaced.
                        ensure_wired_for_historical_call(name, &mut wire_available, &mut input)?;
                        input.push(json!({"type":"function_call","name":call.function.name,"arguments":call.function.arguments,"call_id":id}));
                    } else {
                        bail!("historical call references unknown tool '{name}'");
                    }
                }
            }
            "tool" => {
                let id = message
                    .tool_call_id
                    .as_deref()
                    .context("tool result call ID missing")?;
                if !calls.contains(id) || !outputs.insert(id.to_owned()) {
                    bail!("orphan or duplicate tool result");
                }
                input.push(json!({"type":"function_call_output","call_id":id,"output":message.content.as_deref().unwrap_or("")}));
            }
            _ => bail!("unsupported conversation role"),
        }
    }
    // Top-level is exactly the stable base namespace; never the live active set.
    let functions: Vec<_> = base_tools.iter().map(tool_function_json).collect();
    // Responses-native `tool_search` (`{"type":"tool_search"}`) is never
    // emitted on the ChatGPT plan route. Doge local `tool_search` remains a
    // normal `type == "function"` entry inside the namespace.
    debug_assert!(
        !functions
            .iter()
            .any(|f| f.get("type").and_then(Value::as_str) == Some("tool_search")),
        "Responses-native tool_search must never be emitted"
    );
    if tracing::enabled!(tracing::Level::DEBUG) {
        let additional_items = input
            .iter()
            .filter(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("additional_tools")
            })
            .count();
        let additional_tool_defs: usize = input
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(serde_json::Value::as_str) == Some("additional_tools")
                {
                    item.get("tools")
                        .and_then(serde_json::Value::as_array)
                        .map(|v| v.len())
                } else {
                    None
                }
            })
            .sum();
        tracing::debug!(
            base_tool_count = functions.len(),
            additional_tool_count = additional_tool_defs,
            provider_input_state_count = additional_items,
            "responses append-only wire projection"
        );
    }
    Ok(Request {
        model: model.to_owned(),
        input,
        tools: if functions.is_empty() {
            vec![]
        } else {
            vec![
                json!({"type":"namespace","name":"dgc","description":"doge-code local tools","tools":functions}),
            ]
        },
        store: false,
        stream: true,
        reasoning: effort.map(|e| json!({"effort":e.as_api_str()})),
        context_management: compact_threshold
            .map(|threshold| vec![ContextManagement::compaction(threshold)]),
    })
}

fn validate_output(item: &Value) -> Result<()> {
    // Opaque server-side compaction items carry no `status`; they must be
    // accepted without interpreting the ciphertext.
    if is_compaction_item(item) {
        return validate_compaction_item(item);
    }
    if item
        .get("status")
        .is_some_and(|status| status != "completed")
    {
        bail!("Responses output item was not completed; no tools were executed");
    }
    match item.get("type").and_then(Value::as_str) {
        Some("message") => {
            if item.get("role").and_then(Value::as_str) != Some("assistant") {
                bail!("Responses output message must have assistant role");
            }
            Ok(())
        }
        Some("reasoning") => Ok(()),
        Some("function_call") => {
            if item
                .get("namespace")
                .and_then(Value::as_str)
                .is_some_and(|v| v != "dgc")
            {
                bail!("unrecognized Responses function namespace");
            }
            if item
                .get("call_id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .is_none()
            {
                bail!("Responses function call ID missing");
            }
            Ok(())
        }
        _ => bail!("unsupported Responses output item; no tools were executed"),
    }
}

/// Legacy single-slice completion validation: namespace-agnostic,
/// name-in-tools check. Preserved for existing callers and tests;
/// production Responses traffic uses [`completed_with_activation`].
pub fn completed(
    response: &Value,
    account: &str,
    model: &str,
    tools: &[ToolDef],
) -> Result<(ChoiceMessageWithTools, Option<Usage>)> {
    if response.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("Responses terminal status was not completed");
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .context("Responses output missing")?;
    // The latest compaction item is the canonical continuation boundary.
    // Only items at/after it are validated, dispatched, and persisted.
    // Pre-boundary tool calls must never dispatch from this response.
    let canonical: Vec<Value> = canonical_output(output).to_vec();
    let mut text = String::new();
    let mut refusal = None;
    let mut tool_calls = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for item in &canonical {
        validate_output(item)?;
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for part in content {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => text
                                .push_str(part.get("text").and_then(Value::as_str).unwrap_or("")),
                            Some("refusal") => {
                                refusal = Some(
                                    part.get("refusal")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_owned(),
                                )
                            }
                            _ => bail!("unsupported Responses message content"),
                        }
                    }
                }
            }
            Some("function_call") => {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .context("Responses tool name missing")?;
                if !tools.iter().any(|t| t.function.name == name) {
                    bail!("Responses requested a tool outside the active catalog");
                }
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .context("Responses call ID missing")?;
                if !ids.insert(id) {
                    bail!("duplicate Responses call ID");
                }
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .context("Responses arguments missing")?;
                let value: Value = serde_json::from_str(arguments)
                    .map_err(|_| anyhow::anyhow!("invalid Responses function arguments"))?;
                if !value.is_object() {
                    bail!("Responses function arguments must be an object");
                }
                tool_calls.push(ToolCall {
                    id: Some(id.to_owned()),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                    },
                });
            }
            _ => {}
        }
    }
    let usage = response_usage(response)?;
    Ok((
        ChoiceMessageWithTools {
            refusal,
            role: "assistant".into(),
            content: Some(text),
            tool_calls,
            provider_state: Some(ProviderState {
                version: 1,
                account: account.to_owned(),
                model: model.to_owned(),
                output: canonical,
                additional_tool_names: Vec::new(),
            }),
        },
        usage,
    ))
}

/// Namespace-aware Responses completion validation.
///
/// Base tools must arrive with `namespace == "dgc"`; appended deferred
/// tools must arrive unnamespaced (`namespace` absent) with `name` in the
/// active-minus-base set. Foreign namespaces and unknown unnamespaced
/// names are rejected without dispatch.
pub fn completed_with_activation(
    response: &Value,
    account: &str,
    model: &str,
    base_tools: &[ToolDef],
    active_tools: &[ToolDef],
) -> Result<(ChoiceMessageWithTools, Option<Usage>)> {
    if response.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("Responses terminal status was not completed");
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .context("Responses output missing")?;
    // The latest compaction item is the canonical continuation boundary.
    // Only items at/after it are validated, dispatched, and persisted.
    // Pre-boundary tool calls must never dispatch from this response.
    let canonical: Vec<Value> = canonical_output(output).to_vec();
    // Fail fast here: persisting an empty assistant state would poison
    // history, since the append-only replay requires non-empty output.
    if canonical.is_empty() {
        bail!("Responses output was empty; no tools were executed");
    }
    let exposure = ResponseToolExposure::from_defs(base_tools, active_tools);
    let mut text = String::new();
    let mut refusal = None;
    let mut tool_calls = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for item in &canonical {
        validate_output(item)?;
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for part in content {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => text
                                .push_str(part.get("text").and_then(Value::as_str).unwrap_or("")),
                            Some("refusal") => {
                                refusal = Some(
                                    part.get("refusal")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_owned(),
                                )
                            }
                            _ => bail!("unsupported Responses message content"),
                        }
                    }
                }
            }
            Some("function_call") => {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .context("Responses tool name missing")?;
                match item.get("namespace").and_then(Value::as_str) {
                    Some("dgc") => {
                        if !exposure.contains_base(name) {
                            bail!("Responses requested a tool outside the base namespace");
                        }
                    }
                    None => {
                        if !exposure.contains_additional(name) {
                            bail!("Responses requested an unknown unnamespaced tool");
                        }
                    }
                    Some(other) => {
                        bail!("unrecognized Responses function namespace '{other}'");
                    }
                }
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .context("Responses call ID missing")?;
                if !ids.insert(id) {
                    bail!("duplicate Responses call ID");
                }
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .context("Responses arguments missing")?;
                let value: Value = serde_json::from_str(arguments)
                    .map_err(|_| anyhow::anyhow!("invalid Responses function arguments"))?;
                if !value.is_object() {
                    bail!("Responses function arguments must be an object");
                }
                tool_calls.push(ToolCall {
                    id: Some(id.to_owned()),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                    },
                });
            }
            _ => {}
        }
    }
    let usage = response_usage(response)?;
    Ok((
        ChoiceMessageWithTools {
            refusal,
            role: "assistant".into(),
            content: Some(text),
            tool_calls,
            provider_state: Some(ProviderState::assistant(
                account.to_owned(),
                model.to_owned(),
                canonical,
            )),
        },
        usage,
    ))
}

fn response_usage(response: &Value) -> Result<Option<Usage>> {
    response.get("usage").filter(|v| !v.is_null()).map(|value| {
        let mapped = json!({"prompt_tokens":value.get("input_tokens"),"completion_tokens":value.get("output_tokens"),"total_tokens":value.get("total_tokens"),"prompt_tokens_details":value.get("input_tokens_details"),"completion_tokens_details":value.get("output_tokens_details")});
        serde_json::from_value::<Usage>(mapped).map_err(|error| {
            let summary = crate::llm::telemetry::JsonErrorSummary::from_error(&error);
            tracing::error!(category = summary.category, line = summary.line,
                column = summary.column, "Responses usage deserialize error");
            anyhow::anyhow!("invalid Responses usage: {summary}")
        })
    }).transpose()
}

pub async fn infer(
    client: &crate::llm::OpenAIClient,
    auth: &AuthHandle,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    effort: Option<ReasoningEffort>,
    cancel: CancellationToken,
) -> Result<ChoiceMessageWithTools> {
    // Legacy lenient path (namespace-agnostic completion check) for
    // existing callers; production agent traffic uses infer_with_activation.
    let compact_threshold = client.responses_compact_threshold();
    let request = build(
        model,
        &auth.account,
        messages,
        tools,
        effort,
        compact_threshold,
    )?;
    if tracing::enabled!(tracing::Level::DEBUG) {
        tracing::debug!(
            responses_native_compaction_enabled = compact_threshold.is_some(),
            responses_compact_threshold = compact_threshold.unwrap_or(0),
            "responses request contract"
        );
    }
    execute_infer_request(client, auth, &request, cancel, |response| {
        completed(response, &auth.account, model, tools)
    })
    .await
}

/// Shared POST + SSE loop. Request projection and completion validation
/// differ per caller (lenient legacy vs append-only strict) and are
/// injected; transport, retry, timeout, and usage accounting are common.
async fn execute_infer_request(
    client: &crate::llm::OpenAIClient,
    auth: &AuthHandle,
    request: &Request,
    cancel: CancellationToken,
    validate_completed: impl Fn(&Value) -> Result<(ChoiceMessageWithTools, Option<Usage>)>,
) -> Result<ChoiceMessageWithTools> {
    let attempts = client.llm_cfg.max_retries.min(3) + 1;
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(client.llm_cfg.request_timeout_ms.max(1));
    let result = async {
        for attempt in 0..attempts {
            let bearer = auth.bearer(&cancel).await?;
            client.record_request_attempt();
            let response = auth
                .http
                .post(format!("{}/responses", auth.resource))
                // Override the shorter OAuth timeout for long-running inference.
                // The outer deadline includes refresh and all retry attempts.
                .timeout(Duration::from_millis(
                    client.llm_cfg.request_timeout_ms.max(1),
                ))
                .bearer_auth(bearer)
                .json(&request)
                .send()
                .await
                .map_err(|_| {
                    anyhow::anyhow!("Responses connection failed; no automatic replay was made")
                })?;
            let request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if !response.status().is_success() {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(30);
                let error = match super::auth::json_response(response).await {
                    Err(e) => e,
                    Ok(_) => anyhow::anyhow!("unexpected Responses HTTP status"),
                };
                if error
                    .downcast_ref::<ProviderError>()
                    .is_some_and(|e| e.retryable)
                    && attempt + 1 < attempts
                {
                    tokio::time::sleep(Duration::from_secs(
                        retry_after.saturating_mul(attempt as u64 + 1).min(30),
                    ))
                    .await;
                    continue;
                }
                return Err(error);
            }
            let mut stream = response.bytes_stream();
            let mut decoder = Decoder::default();
            let mut total = 0usize;
            while let Some(chunk) = tokio::time::timeout(
                Duration::from_millis(client.llm_cfg.read_idle_timeout_ms.max(1)),
                stream.next(),
            )
            .await
            .context("Responses stream idle timeout")?
            {
                let bytes = chunk.map_err(|_| {
                    anyhow::anyhow!("Responses stream interrupted; no automatic replay was made")
                })?;
                total = total.saturating_add(bytes.len());
                if total > 32 * 1024 * 1024 {
                    bail!("Responses stream exceeds size limit");
                }
                for event in decoder.push(&bytes)? {
                    let event: InferenceEvent = serde_json::from_value(event)
                        .map_err(|_| anyhow::anyhow!("invalid Responses event fields"))?;
                    match event {
                        InferenceEvent::Completed { response } => {
                            let (result, usage) = validate_completed(&response)?;
                            if let Some(usage) = usage {
                                client.record_usage(&usage);
                            }
                            return Ok(result);
                        }
                        InferenceEvent::Failed { response } => {
                            return Err(ProviderError::from_body(
                                None,
                                &response,
                                request_id.clone(),
                            )
                            .during_stream()
                            .into());
                        }
                        InferenceEvent::Error { body } => {
                            return Err(ProviderError::from_body(
                                None,
                                &Value::Object(body),
                                request_id.clone(),
                            )
                            .during_stream()
                            .into());
                        }
                        InferenceEvent::Incomplete => {
                            bail!("Responses generation incomplete; no tools were executed")
                        }
                        _ => {} // Deltas are advisory. Only completed output authorizes tool dispatch.
                    }
                }
            }
            bail!("Responses stream ended without response.completed; no tools were executed");
        }
        bail!("Responses retry budget exhausted")
    };
    tokio::select! { biased; _ = cancel.cancelled() => bail!(crate::llm::LlmErrorKind::Cancelled), result = tokio::time::timeout_at(deadline, result) => result.context("Responses request deadline exceeded")? }
}

#[allow(clippy::too_many_arguments)]
pub async fn infer_with_activation(
    client: &crate::llm::OpenAIClient,
    auth: &AuthHandle,
    model: &str,
    messages: &[ChatMessage],
    base_tools: &[ToolDef],
    active_tools: &[ToolDef],
    effort: Option<ReasoningEffort>,
    cancel: CancellationToken,
) -> Result<ChoiceMessageWithTools> {
    let compact_threshold = client.responses_compact_threshold();
    let request = build_with_activation(
        model,
        &auth.account,
        messages,
        base_tools,
        active_tools,
        effort,
        compact_threshold,
    )?;
    if tracing::enabled!(tracing::Level::DEBUG) {
        tracing::debug!(
            responses_native_compaction_enabled = compact_threshold.is_some(),
            responses_compact_threshold = compact_threshold.unwrap_or(0),
            "responses request contract"
        );
    }
    execute_infer_request(client, auth, &request, cancel, |response| {
        completed_with_activation(response, &auth.account, model, base_tools, active_tools)
    })
    .await
}
