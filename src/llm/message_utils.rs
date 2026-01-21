pub fn truncate_tool_output(content: String, tool_name: &str) -> String {
    const DEFAULT_MAX_LEN: usize = 8000;
    const READ_MAX_LEN: usize = 40000; // Allow more context for reading files

    let max_len = if tool_name == "fs_read" || tool_name == "fs_read_many_files" {
        READ_MAX_LEN
    } else {
        DEFAULT_MAX_LEN
    };

    if content.len() > max_len {
        let truncated: String = content.chars().take(max_len).collect();
        format!(
            "{}... (Output truncated. Total length: {} chars. Refine your tool call to reduce output.)",
            truncated,
            content.len()
        )
    } else {
        content
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
