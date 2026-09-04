/// Minimum characters kept for a truncated string field before replacing it
/// with a bare placeholder.
const MIN_STRING_KEEP: usize = 200;

/// Head-room reserved inside a string budget for the truncation marker.
const TRUNCATION_MARKER_RESERVE: usize = 60;

/// Budget a single string field, keeping the head and appending a marker.
fn budget_string_field(value: &str, budget: usize) -> String {
    let total_chars = value.chars().count();
    if total_chars <= budget {
        return value.to_string();
    }
    if budget <= MIN_STRING_KEEP + TRUNCATION_MARKER_RESERVE {
        return format!("[truncated {} chars]", total_chars);
    }
    let keep = budget - TRUNCATION_MARKER_RESERVE;
    let mut cut = keep.min(value.len());
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut == 0 {
        return format!("[truncated {} chars]", total_chars);
    }
    let kept_chars = value[..cut].chars().count();
    format!(
        "{}\n[...truncated {} of {} chars]",
        &value[..cut],
        total_chars - kept_chars,
        total_chars
    )
}

/// Shrink a JSON value in place so its serialized form fits in `budget` chars.
///
/// Strategy: shorten oversized string fields first (head-biased), then drop
/// array items from the tail. Returns true when the serialized value fits.
fn budget_json_value(value: &mut serde_json::Value, budget: usize) -> bool {
    let fits = |v: &serde_json::Value| {
        serde_json::to_string(v)
            .map(|s| s.chars().count() <= budget)
            .unwrap_or(false)
    };
    match value {
        serde_json::Value::String(s) => {
            *s = budget_string_field(s, budget);
            fits(value)
        }
        serde_json::Value::Array(_) => {
            loop {
                if serialized_len(value) <= budget {
                    break;
                }
                let can_pop = match value {
                    serde_json::Value::Array(items) => items.len() > 1,
                    _ => false,
                };
                if !can_pop {
                    break;
                }
                if let serde_json::Value::Array(items) = value {
                    items.pop();
                }
            }
            if let serde_json::Value::Array(items) = value
                && let Some(last) = items.last_mut()
            {
                budget_json_value(last, budget);
            }
            fits(value)
        }
        serde_json::Value::Object(map) => {
            for (_, field) in map.iter_mut() {
                if field.is_string() || field.is_array() || field.is_object() {
                    budget_json_value(field, budget);
                }
            }
            fits(value)
        }
        _ => true,
    }
}

fn serialized_len(value: &serde_json::Value) -> usize {
    serde_json::to_string(value)
        .map(|s| s.chars().count())
        .unwrap_or(usize::MAX)
}

/// Character budget applied to serialized tool output before it is fed back to
/// the model.
pub fn max_output_chars(tool_name: &str) -> usize {
    const DEFAULT_MAX_LEN: usize = 8000;
    const READ_MAX_LEN: usize = 40000; // Allow more context for reading files

    if tool_name == "fs_read"
        || tool_name == "fs_read_many_files"
        || tool_name == "plan_write"
        || tool_name == "plan_read"
    {
        READ_MAX_LEN
    } else {
        DEFAULT_MAX_LEN
    }
}

/// Truncates tool output while keeping it valid JSON.
///
/// Slicing the serialized payload can land mid-structure and hand the model
/// malformed JSON. Instead, the output is parsed and oversized string fields
/// are shortened (and array tails dropped) so the result stays parseable.
/// Non-JSON payloads fall back to a head slice wrapped in a JSON object.
pub fn truncate_tool_output(content: String, tool_name: &str) -> String {
    let max_len = max_output_chars(tool_name);

    if content.chars().count() <= max_len {
        return content;
    }

    let total_chars = content.chars().count();
    let note = format!(
        "... (Output truncated. Total length: {} chars. Refine your tool call to reduce output.)",
        total_chars
    );
    let json_budget = max_len.saturating_sub(note.chars().count() + 2);

    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&content) {
        budget_json_value(&mut value, json_budget);
        if let Ok(serialized) = serde_json::to_string(&value)
            && serialized.chars().count() <= max_len
        {
            return serialized;
        }
    }

    // Fallback: non-JSON (or still oversized) payload wrapped in valid JSON.
    // JSON escaping of quotes/backslashes can inflate the serialized size
    // beyond the budget, so shrink the kept head until the envelope fits
    // (an empty head always fits).
    let mut keep = json_budget.saturating_sub(80);
    loop {
        let mut cut = keep.min(content.len());
        while cut > 0 && !content.is_char_boundary(cut) {
            cut -= 1;
        }
        let head = if cut > 0 { &content[..cut] } else { "" };
        let wrapped = serde_json::json!({
            "truncated_raw": head,
            "note": note.clone(),
        });
        let serialized = serde_json::to_string(&wrapped).unwrap_or_else(|_| note.clone());
        if serialized.chars().count() <= max_len || keep == 0 {
            return serialized;
        }
        keep /= 2;
    }
}

pub fn clean_json_text(text: &str) -> String {
    let text = text.trim();
    if text.starts_with("```json") {
        if let Some(end) = text.rfind("```") {
            return text[7..end].trim().to_string();
        }
    } else if text.starts_with("```")
        && let Some(end) = text.rfind("```")
    {
        return text[3..end].trim().to_string();
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_output_unchanged() {
        let short = "short output";
        assert_eq!(truncate_tool_output(short.to_string(), "any_tool"), short);
    }

    #[test]
    fn test_non_json_output_uses_legacy_slice() {
        let long = "na".repeat(5000); // 10000 chars, not JSON
        let truncated = truncate_tool_output(long.clone(), "any_tool");
        assert!(truncated.contains("truncated"));
        assert!(truncated.chars().count() < long.chars().count());
    }

    #[test]
    fn test_read_tier_not_truncated_under_40k() {
        let content = format!("{{\"content\":\"{}\"}}", "na".repeat(15000)); // ~30k chars JSON
        let result = truncate_tool_output(content.clone(), "fs_read");
        assert_eq!(result.chars().count(), content.chars().count());
        assert!(!result.contains("truncated"));
    }

    #[test]
    fn test_json_output_stays_valid_json() {
        let content = format!(
            "{{\"stdout\":\"{}\",\"exit_code\":0,\"success\":true}}",
            "x".repeat(20_000)
        );
        let result = truncate_tool_output(content, "execute_bash");
        assert!(result.chars().count() <= 8000);
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("truncated output must be valid JSON");
        assert_eq!(parsed["exit_code"], 0);
        assert!(parsed["stdout"].as_str().unwrap().contains("truncated"));
    }

    #[test]
    fn test_json_error_output_stays_valid_json() {
        let content = serde_json::json!({ "error": "e".repeat(20_000) }).to_string();
        let result = truncate_tool_output(content, "any_tool");
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("truncated error output must be valid JSON");
        assert!(parsed["error"].as_str().unwrap().contains("truncated"));
    }

    #[test]
    fn test_json_huge_array_drops_tail_but_keeps_head() {
        let items: Vec<String> = (0..1000).map(|i| format!("entry-{i:04}")).collect();
        let content = serde_json::json!({ "entries": items }).to_string();
        assert!(content.chars().count() > 8000);
        let result = truncate_tool_output(content, "fs_list");
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("truncated array output must be valid JSON");
        let entries = parsed["entries"].as_array().expect("entries kept");
        assert!(entries.len() < 1000);
        assert!(entries.len() > 1);
        assert_eq!(entries[0], "entry-0000");
    }

    #[test]
    fn test_json_multibyte_strings_survive() {
        let content = serde_json::json!({ "content": "日本語のテキスト".repeat(2000) }).to_string();
        assert!(content.chars().count() > 8000);
        let result = truncate_tool_output(content, "any_tool");
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("multibyte truncation must produce valid JSON");
        assert!(parsed["content"].as_str().unwrap().contains("日本語"));
    }

    #[test]
    fn test_nested_object_strings_budgeted() {
        let payload = serde_json::json!({
            "result": {
                "files": [{ "path": "a.rs", "content": "y".repeat(20_000) }],
                "warnings": [],
            }
        })
        .to_string();
        let result = truncate_tool_output(payload, "any_tool");
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("nested truncation must produce valid JSON");
        assert!(
            parsed["result"]["files"][0]["content"]
                .as_str()
                .unwrap()
                .contains("truncated")
        );
        assert_eq!(parsed["result"]["files"][0]["path"], "a.rs");
    }

    #[test]
    fn test_fallback_envelope_survives_json_escaping_inflation() {
        // Backslash-heavy non-JSON content roughly doubles in size when
        // JSON-escaped; the fallback must shrink until the envelope fits.
        let content = "\\".repeat(40_000);
        let result = truncate_tool_output(content, "any_tool");
        assert!(
            result.chars().count() <= 8000,
            "escaped envelope exceeded the cap: {} chars",
            result.chars().count()
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("fallback envelope must be valid JSON");
        assert!(parsed["truncated_raw"].is_string());
        assert!(parsed["note"].as_str().unwrap().contains("truncated"));
    }
}
