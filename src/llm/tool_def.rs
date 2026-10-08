use crate::llm::types::ToolDef;
use crate::tools;

pub fn default_tools_def() -> Vec<ToolDef> {
    vec![
        tools::list::tool_def(),
        tools::read::tool_def(),
        tools::search_text::tool_def(),
        tools::write::tool_def(),
        tools::search_repomap::tool_def(),
        tools::execute::tool_def(),
        tools::process::tool_def(),
        tools::shell::tool_def(),
        tools::edit::tool_def(),
        tools::apply_patch::tool_def(),
        tools::find_file::tool_def(),
        tools::read_many::tool_def(),
        tools::plan::plan_write_tool_def(),
        tools::plan::plan_read_tool_def(),
        tools::requirements::requirements_write_tool_def(),
        tools::requirements::requirements_read_tool_def(),
        tools::undo::undo_tool_def(),
        tools::memory::read_memory_tool_def(),
        tools::memory::write_memory_tool_def(),
        tools::memory::list_memories_tool_def(),
        tools::memory::search_memory_tool_def(),
        tools::doc::tool_def(),
        tools::workflow::run_workflow_tool_def(),
        tools::task::tool_def(),
        tools::provenance::tool_def(),
        tools::impact::tool_def(),
        tools::observation::tool_def(),
    ]
}

/// Check the strict object contract in fixtures, including nested items.
/// This is deliberately not a complete provider JSON Schema validator.
#[cfg(test)]
pub(crate) fn strict_object_contract(schema: &serde_json::Value) -> bool {
    match schema {
        serde_json::Value::Object(object) => {
            let object_type = object.get("type").is_some_and(|kind| {
                kind.as_str() == Some("object")
                    || kind.as_array().is_some_and(|types| {
                        types.iter().any(|value| value.as_str() == Some("object"))
                    })
            });
            if object_type {
                let Some(properties) = object.get("properties").and_then(|v| v.as_object()) else {
                    return false;
                };
                let Some(required) = object.get("required").and_then(|v| v.as_array()) else {
                    return false;
                };
                if object.get("additionalProperties") != Some(&serde_json::Value::Bool(false))
                    || required.len() != properties.len()
                    || !properties
                        .keys()
                        .all(|key| required.iter().any(|v| v.as_str() == Some(key.as_str())))
                {
                    return false;
                }
            }
            // Walk schema positions only: examples/defaults may contain objects.
            ["properties", "$defs", "definitions"]
                .into_iter()
                .all(|key| {
                    object
                        .get(key)
                        .and_then(|v| v.as_object())
                        .is_none_or(|map| map.values().all(strict_object_contract))
                })
                && ["items", "additionalProperties"]
                    .into_iter()
                    .all(|key| object.get(key).is_none_or(strict_object_contract))
                && ["anyOf", "oneOf", "allOf"].into_iter().all(|key| {
                    object
                        .get(key)
                        .and_then(|v| v.as_array())
                        .is_none_or(|items| items.iter().all(strict_object_contract))
                })
        }
        serde_json::Value::Array(items) => items.iter().all(strict_object_contract),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_object_contract_checks_nullable_objects_inside_array_items() {
        let mut schema = serde_json::json!({"type":"array", "items":{
            "type":["object","null"], "properties":{"value":{"type":"string"}},
            "required":["value"], "additionalProperties":false,
            "default":{"type":"object","properties":{"ignored":{}}}
        }});
        assert!(strict_object_contract(&schema));
        schema["items"]["required"] = serde_json::json!([]);
        assert!(!strict_object_contract(&schema));
        schema["items"]["required"] = serde_json::json!(["value"]);
        schema["items"]["additionalProperties"] = serde_json::json!(true);
        assert!(!strict_object_contract(&schema));
    }

    #[test]
    fn strict_builtin_tools_require_all_properties_at_every_object() {
        let invalid: Vec<_> = default_tools_def()
            .into_iter()
            .filter(|tool| tool.function.strict == Some(true))
            .filter(|tool| !strict_object_contract(&tool.function.parameters))
            .map(|tool| tool.function.name)
            .collect();
        assert!(invalid.is_empty(), "invalid strict schemas: {invalid:?}");
    }

    #[test]
    fn test_observation_read_schema_is_small_and_stable() {
        // Fixed prompt ratchet: the new stable tool adds a small deliberate
        // cost. Keep the schema concise; fail if it grows unexpectedly.
        let def = crate::tools::observation::tool_def();
        let rendered = serde_json::to_string(&def).expect("serialize tool def");
        assert!(
            rendered.len() < 1_500,
            "observation_read schema grew: {} bytes",
            rendered.len()
        );
        // Full inventory stays parseable and observation_read is appended last.
        let all = default_tools_def();
        assert_eq!(all.last().unwrap().function.name, "observation_read");
        let all_json = serde_json::to_string(&all).expect("serialize all");
        assert!(all_json.contains("observation_read"));
    }
}
