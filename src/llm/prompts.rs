// Legacy raw-template export. Runtime requests render this same authoritative
// resource through build_system_prompt; do not maintain a second workflow.
pub const SYSTEM_PROMPT: &str = include_str!("../../resources/system_prompt.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stale_prompt_has_no_visible_thinking_requirement() {
        assert!(!SYSTEM_PROMPT.contains("<thinking>"));
        assert!(!SYSTEM_PROMPT.contains("Mandatory Thinking Process"));
        assert!(!SYSTEM_PROMPT.contains("ALWAYS output your reasoning"));
    }
}
