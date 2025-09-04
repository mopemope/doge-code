use crate::planning::llm_decomposer::types::*;
use anyhow::Result;

pub fn extract_json_block(response: &str) -> Option<&str> {
    let json_start = response.find("```json").or_else(|| response.find('{'));
    let json_end = response.rfind("```").or_else(|| response.rfind('}'));
    match (json_start, json_end) {
        (Some(start), Some(end)) => {
            let start_pos = if response[start..].starts_with("```json") {
                start + 7
            } else {
                start
            };
            let end_pos =
                if response[..end].ends_with('}') && response.get(end..end + 3) == Some("```") {
                    end
                } else {
                    end + 1
                };
            Some(&response[start_pos..end_pos])
        }
        _ => None,
    }
}

pub fn parse_llm_json(response: &str) -> Result<LlmDecompositionResult> {
    let content = extract_json_block(response).unwrap_or(response);
    let parsed: LlmDecompositionResult = serde_json::from_str(content.trim())?;
    Ok(parsed)
}

pub fn extract_fallback_steps(response: &str) -> Result<LlmDecompositionResult> {
    let mut steps = Vec::new();
    let mut step_counter = 1;

    for line in response.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with(char::is_numeric)
            || line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("1.")
            || line.starts_with("2.")
        {
            let description = line
                .trim_start_matches(char::is_numeric)
                .trim_start_matches('.')
                .trim_start_matches('-')
                .trim_start_matches('*')
                .trim();
            if description.len() > 10 {
                steps.push(LlmTaskStep {
                    id: format!("fallback_step_{}", step_counter),
                    description: description.to_string(),
                    step_type: crate::planning::llm_decomposer::infer::infer_step_type(description)
                        .to_string(),
                    dependencies: if step_counter > 1 {
                        vec![format!("fallback_step_{}", step_counter - 1)]
                    } else {
                        vec![]
                    },
                    estimated_duration_minutes: 10,
                    required_tools: crate::planning::llm_decomposer::infer::infer_required_tools(
                        description,
                    ),
                    validation_criteria: vec!["Step is completed".to_string()],
                    detailed_instructions: description.to_string(),
                    potential_issues: vec!["Unexpected issues may occur".to_string()],
                });
                step_counter += 1;
            }
        }
    }

    if steps.is_empty() {
        anyhow::bail!("Could not extract any steps from LLM response");
    }

    Ok(LlmDecompositionResult {
        reasoning: "Steps automatically extracted from LLM response".to_string(),
        steps,
        complexity_assessment: "Estimated to be of moderate complexity".to_string(),
        risks: vec!["Possibility of insufficient detailed analysis".to_string()],
        prerequisites: vec!["Basic understanding of the project".to_string()],
    })
}
