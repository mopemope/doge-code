//! Shared character-budget helpers for tool outputs.
//!
//! Tools must keep their own serialized output below the global truncation
//! caps (`src/llm/message_utils.rs`), because the global truncator is only a
//! safety net. These helpers implement the common head/tail truncation
//! patterns used across tools.

/// Default per-tool output budget in characters. Chosen so that a fully
/// serialized JSON payload (with overhead) stays safely under the 8,000-char
/// global cap.
pub const DEFAULT_TOOL_BUDGET_CHARS: usize = 6_000;

pub use crate::utils::text_budget::{
    BudgetedText, head_tail_truncate, head_truncate, safe_tail_chars, safe_take_chars,
};

/// Shared with the final conversation safety net; read results include JSON overhead.
pub const READ_TOOL_OUTPUT_MAX_CHARS: usize = 40_000;

pub(crate) fn read_result_fits<T: serde::Serialize>(result: &T) -> anyhow::Result<bool> {
    #[derive(serde::Serialize)]
    struct Envelope<'a, T> {
        ok: bool,
        result: &'a T,
    }
    Ok(serde_json::to_string(&Envelope { ok: true, result })?
        .chars()
        .count()
        <= READ_TOOL_OUTPUT_MAX_CHARS)
}

pub(crate) fn positive_read_option(name: &str, value: Option<usize>) -> anyhow::Result<()> {
    anyhow::ensure!(
        value != Some(0),
        "invalid argument: {name} must be greater than zero"
    );
    Ok(())
}

/// Measure the complete model-visible envelope, including JSON escaping.
pub(crate) fn serialized_tool_output_fits<T: serde::Serialize>(
    output: &T,
    tool: &str,
) -> anyhow::Result<bool> {
    Ok(serde_json::to_string(output)?.chars().count() <= crate::llm::max_output_chars(tool))
}

/// Keep read error JSON small even when an invalid path/pattern is enormous.
pub(crate) fn bounded_read_error(error: impl std::fmt::Display) -> anyhow::Error {
    let message = error.to_string();
    let prefix = safe_take_chars(&message, 512);
    anyhow::anyhow!(
        "{prefix}{}",
        if prefix.len() < message.len() {
            " [error detail shortened]"
        } else {
            ""
        }
    )
}
