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
