//! Request-scoped runtime context overlay (v1).
//!
//! Recent-file / automatic-memory hints are bootstrap-only runtime hints,
//! not canonical conversation state. A [`RuntimeContextSnapshot`] is built
//! once per agent turn and overlaid onto the first LLM request only; it is
//! never pushed into [`crate::llm::tool_execution::history::HistoryManager`]
//! messages, session persistence, or compaction input.

use crate::tools::FsTools;

/// Hard upper bound for the whole rendered runtime context, in characters.
pub const RUNTIME_CONTEXT_MAX_CHARS: usize = 4_000;
/// Budget for the Recent Files portion, in characters.
pub const ACTIVE_CONTEXT_MAX_CHARS: usize = 3_000;
/// Budget for the automatic memory-hint portion, in characters.
pub const MEMORY_CONTEXT_MAX_CHARS: usize = 1_000;
/// Maximum query length forwarded to `search_memory`, in characters.
pub const MEMORY_QUERY_MAX_CHARS: usize = 1_000;

/// Marker appended when a runtime context component is truncated.
pub const RUNTIME_TRUNCATION_MARKER: &str = "[...runtime context truncated...]";

/// Bootstrap-only runtime hints for a single agent turn.
///
/// This is deliberately not a [`crate::llm::types::ChatMessage`]: runtime
/// hints are not canonical message history. Conversion to a system message
/// happens only at request-projection time (see [`RequestMessages`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeContextSnapshot {
    pub active_context: Option<String>,
    pub memory_context: Option<String>,
    pub truncated: bool,
}

impl RuntimeContextSnapshot {
    /// Build a snapshot from raw components, applying the hard budgets.
    ///
    /// Component budgets compose: active (<= 3,000) + memory (<= 1,000) +
    /// envelope (~37 chars) always fits within
    /// `RUNTIME_CONTEXT_MAX_CHARS` + marker overhead, so no further
    /// shrinking is needed. Active context keeps the larger share because
    /// workspace state is more direct than memory-key hints.
    pub fn new(active_context: Option<String>, memory_context: Option<String>) -> Self {
        let (active, active_truncated) = match active_context.filter(|s| !s.is_empty()) {
            Some(s) => {
                let (text, truncated) = truncate_component(&s, ACTIVE_CONTEXT_MAX_CHARS);
                (Some(text), truncated)
            }
            None => (None, false),
        };
        let (memory, memory_truncated) = match memory_context.filter(|s| !s.is_empty()) {
            Some(s) => {
                let (text, truncated) = truncate_component(&s, MEMORY_CONTEXT_MAX_CHARS);
                (Some(text), truncated)
            }
            None => (None, false),
        };
        Self {
            active_context: active,
            memory_context: memory,
            truncated: active_truncated || memory_truncated,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.active_context.is_none() && self.memory_context.is_none()
    }

    /// Render the snapshot as a single request-scoped system message body.
    pub fn render(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut parts = Vec::with_capacity(2);
        if let Some(active) = &self.active_context {
            parts.push(active.clone());
        }
        if let Some(memory) = &self.memory_context {
            parts.push(memory.clone());
        }
        Some(format!(
            "<RuntimeContext>\n{}\n</RuntimeContext>",
            parts.join("\n\n")
        ))
    }

    /// Rendered size in characters (0 when empty). Used for debug logging
    /// and follow-up token-governor budgeting; never logs the body itself.
    pub fn char_len(&self) -> usize {
        self.render().map(|s| s.chars().count()).unwrap_or(0)
    }
}

/// UTF-8-safe head truncation to `budget` chars with an explicit marker.
fn truncate_component(s: &str, budget: usize) -> (String, bool) {
    if s.chars().count() <= budget {
        return (s.to_string(), false);
    }
    let keep = budget
        .saturating_sub(RUNTIME_TRUNCATION_MARKER.chars().count() + 1)
        .max(1);
    let head = crate::tools::budget::safe_take_chars(s, keep);
    (format!("{head}\n{RUNTIME_TRUNCATION_MARKER}"), true)
}

/// Extract the latest user goal without cloning the whole history.
/// Only this string is passed to the snapshot builder.
pub fn last_user_goal(history: &[crate::llm::types::ChatMessage]) -> Option<String> {
    history
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.clone())
}

/// Builds a [`RuntimeContextSnapshot`] from live workspace state.
///
/// Failures never fail the agent turn: they warn and continue without
/// context, matching the previous `inject_context` behavior.
pub struct RuntimeContextBuilder<'a> {
    fs_tools: &'a FsTools,
}

impl<'a> RuntimeContextBuilder<'a> {
    pub fn new(fs_tools: &'a FsTools) -> Self {
        Self { fs_tools }
    }

    pub async fn build(&self, user_goal: Option<&str>) -> RuntimeContextSnapshot {
        let active_raw = {
            let cm = self.fs_tools.context_manager.read().await;
            let prompt = cm.get_context_prompt().await;
            if prompt.is_empty() {
                None
            } else {
                Some(prompt)
            }
        };

        let memory_raw = match truncate_query(user_goal) {
            Some(query) => match self.fs_tools.search_memory(Some(query), None).await {
                Ok(result) => {
                    if result.contains("No matching memories found")
                        || result.contains("No memories found")
                    {
                        None
                    } else {
                        Some(format!("<RelevantMemory>\n{result}\n</RelevantMemory>"))
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "runtime context memory search failed; continuing without memory hints");
                    None
                }
            },
            None => None,
        };

        RuntimeContextSnapshot::new(active_raw, memory_raw)
    }
}

/// Bound the memory-search query length (UTF-8 safe). Huge user prompts must
/// not flow unbounded into the substring search query.
fn truncate_query(user_goal: Option<&str>) -> Option<String> {
    let goal = user_goal?.trim();
    if goal.is_empty() {
        return None;
    }
    Some(
        crate::tools::budget::safe_take_chars(
            goal,
            MEMORY_QUERY_MAX_CHARS.min(goal.chars().count().max(1)),
        )
        .to_string(),
    )
}

/// Request projection: canonical history plus an optional one-shot runtime
/// overlay. The overlay is never written back to durable history.
#[derive(Debug)]
pub enum RequestMessages<'a> {
    Borrowed(&'a [crate::llm::types::ChatMessage]),
    Owned(Vec<crate::llm::types::ChatMessage>),
}

impl<'a> RequestMessages<'a> {
    pub fn borrowed(history: &'a [crate::llm::types::ChatMessage]) -> Self {
        Self::Borrowed(history)
    }

    /// Overlay the rendered snapshot before the last user message (the same
    /// semantic order the old history injection used). Tool-call pairing is
    /// never split: insertion happens before a user message or, when no
    /// user message exists, right after the leading system section.
    pub fn with_runtime_context(
        history: &'a [crate::llm::types::ChatMessage],
        runtime: &RuntimeContextSnapshot,
    ) -> Self {
        let Some(rendered) = runtime.render() else {
            return Self::Borrowed(history);
        };
        Self::with_rendered_context(history, rendered)
    }

    fn with_rendered_context(
        history: &'a [crate::llm::types::ChatMessage],
        rendered: String,
    ) -> Self {
        use crate::llm::types::ChatMessage;
        let overlay = ChatMessage {
            role: "system".into(),
            content: Some(rendered),
            tool_calls: vec![],
            tool_call_id: None,
        };
        if history.is_empty() {
            return Self::Owned(vec![overlay]);
        }
        let insert_at = history
            .iter()
            .rposition(|m| m.role == "user")
            .unwrap_or_else(|| history.iter().take_while(|m| m.role == "system").count());
        let mut projected = Vec::with_capacity(history.len() + 1);
        projected.extend_from_slice(&history[..insert_at]);
        projected.push(overlay);
        projected.extend_from_slice(&history[insert_at..]);
        Self::Owned(projected)
    }

    pub fn as_slice(&self) -> &[crate::llm::types::ChatMessage] {
        match self {
            Self::Borrowed(history) => history,
            Self::Owned(projected) => projected.as_slice(),
        }
    }

    /// Zero-allocation fast path indicator: true when no overlay was added.
    pub fn is_borrowed(&self) -> bool {
        matches!(self, Self::Borrowed(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{ChatMessage, ToolCall, ToolCallFunction};
    use std::sync::Arc;

    fn system(content: &str) -> ChatMessage {
        ChatMessage {
            role: "system".into(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn user(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".into(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn assistant(content: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn assistant_tool_call(id: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![ToolCall {
                id: Some(id.to_string()),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "fs_read".to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            tool_call_id: None,
        }
    }

    fn tool_result(id: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".into(),
            content: Some("{}".to_string()),
            tool_calls: vec![],
            tool_call_id: Some(id.to_string()),
        }
    }

    fn test_fs(project_root: &std::path::Path) -> FsTools {
        let cfg = Arc::new(crate::config::AppConfig {
            project_root: project_root.to_path_buf(),
            ..Default::default()
        });
        FsTools::new(Arc::new(tokio::sync::RwLock::new(None)), cfg)
    }

    #[test]
    fn test_snapshot_empty_when_no_components() {
        let snapshot = RuntimeContextSnapshot::new(None, None);
        assert!(snapshot.is_empty());
        assert!(snapshot.render().is_none());
        assert!(!snapshot.truncated);
    }

    #[test]
    fn test_snapshot_render_envelope() {
        let snapshot = RuntimeContextSnapshot::new(
            Some("## Active Context\n- a.rs".to_string()),
            Some("<RelevantMemory>\nMatching memories:\n- k\n</RelevantMemory>".to_string()),
        );
        let rendered = snapshot.render().expect("renders");
        assert!(rendered.starts_with("<RuntimeContext>"));
        assert!(rendered.ends_with("</RuntimeContext>"));
        assert!(rendered.contains("## Active Context"));
        assert!(rendered.contains("<RelevantMemory>"));
    }

    #[test]
    fn test_budget_enforced_with_marker() {
        let long_active = "a".repeat(10_000);
        let long_memory = "b".repeat(10_000);
        let snapshot = RuntimeContextSnapshot::new(Some(long_active), Some(long_memory));
        assert!(snapshot.truncated);
        let rendered = snapshot.render().expect("renders");
        let overhead = RUNTIME_TRUNCATION_MARKER.chars().count() + 64;
        assert!(
            rendered.chars().count() <= RUNTIME_CONTEXT_MAX_CHARS + overhead,
            "rendered {} chars exceeds budget",
            rendered.chars().count()
        );
        assert!(rendered.contains(RUNTIME_TRUNCATION_MARKER));
    }

    #[test]
    fn test_unicode_truncation_never_panics() {
        let active = format!("src/ユーザー管理.rs 認証処理\n{}", "あ".repeat(10_000));
        let memory = format!("キャッシュ設計\n{}", "い".repeat(10_000));
        let snapshot = RuntimeContextSnapshot::new(Some(active), Some(memory));
        assert!(snapshot.truncated);
        let rendered = snapshot.render().expect("renders");
        assert!(rendered.contains("ユーザー管理"));
        // Char-boundary safe: re-slicing by chars must round-trip.
        let collected: String = rendered.chars().collect();
        assert_eq!(collected, rendered);
    }

    #[test]
    fn test_component_budgets_compose_within_total() {
        // Both components at their individual caps must still fit the total
        // budget plus marker/envelope overhead, with neither dropped.
        let snapshot = RuntimeContextSnapshot::new(
            Some("x".repeat(ACTIVE_CONTEXT_MAX_CHARS)),
            Some("y".repeat(MEMORY_CONTEXT_MAX_CHARS)),
        );
        assert!(snapshot.active_context.is_some());
        assert!(snapshot.memory_context.is_some());
        let rendered = snapshot.render().expect("renders");
        let overhead = RUNTIME_TRUNCATION_MARKER.chars().count() + 64;
        assert!(
            rendered.chars().count() <= RUNTIME_CONTEXT_MAX_CHARS + overhead,
            "rendered {} chars exceeds budget",
            rendered.chars().count()
        );
    }

    #[test]
    fn test_request_projection_inserts_before_last_user() {
        let history = vec![
            system("prompt"),
            user("old"),
            assistant("old answer"),
            user("current"),
        ];
        let snapshot = RuntimeContextSnapshot::new(Some("## Active Context".to_string()), None);
        let projected = RequestMessages::with_runtime_context(&history, &snapshot);
        assert!(!projected.is_borrowed());
        let roles: Vec<&str> = projected
            .as_slice()
            .iter()
            .map(|m| m.role.as_str())
            .collect();
        assert_eq!(roles, vec!["system", "user", "assistant", "system", "user"]);
        assert!(
            projected.as_slice()[3]
                .content
                .as_deref()
                .unwrap()
                .contains("<RuntimeContext>")
        );
        assert_eq!(projected.as_slice()[4].content.as_deref(), Some("current"));
    }

    #[test]
    fn test_request_projection_never_splits_tool_pair() {
        let history = vec![assistant_tool_call("A"), tool_result("A"), user("next")];
        let snapshot = RuntimeContextSnapshot::new(Some("## Active Context".to_string()), None);
        let projected = RequestMessages::with_runtime_context(&history, &snapshot);
        let slice = projected.as_slice();
        assert_eq!(slice.len(), 4);
        assert_eq!(slice[0].role, "assistant");
        assert_eq!(slice[1].role, "tool");
        assert_eq!(slice[2].role, "system");
        assert!(
            slice[2]
                .content
                .as_deref()
                .unwrap()
                .contains("<RuntimeContext>")
        );
        assert_eq!(slice[3].role, "user");
    }

    #[test]
    fn test_request_projection_falls_back_after_leading_systems() {
        let history = vec![system("a"), system("b"), assistant_tool_call("A")];
        let snapshot = RuntimeContextSnapshot::new(Some("## Active Context".to_string()), None);
        let projected = RequestMessages::with_runtime_context(&history, &snapshot);
        let slice = projected.as_slice();
        assert_eq!(slice.len(), 4);
        assert_eq!(slice[0].role, "system");
        assert_eq!(slice[1].role, "system");
        assert_eq!(slice[2].role, "system");
        assert!(
            slice[2]
                .content
                .as_deref()
                .unwrap()
                .contains("<RuntimeContext>")
        );
        assert_eq!(slice[3].role, "assistant");
    }

    #[test]
    fn test_no_overlay_is_zero_allocation_borrow() {
        let history = vec![system("prompt"), user("hi")];
        let empty = RuntimeContextSnapshot::default();
        assert!(empty.is_empty());
        let projected = RequestMessages::with_runtime_context(&history, &empty);
        assert!(projected.is_borrowed());
        assert_eq!(projected.as_slice().len(), history.len());
        assert!(std::ptr::eq(
            projected.as_slice().as_ptr(),
            history.as_slice().as_ptr()
        ));
    }

    #[test]
    fn test_last_user_goal_extracts_only_latest() {
        let history = vec![user("first"), assistant("answer"), user("second")];
        assert_eq!(last_user_goal(&history).as_deref(), Some("second"));
        let empty: Vec<ChatMessage> = vec![system("only")];
        assert!(last_user_goal(&empty).is_none());
    }

    #[test]
    fn test_overlay_sent_only_on_first_projection() {
        // Unit-level first-request-only lifecycle: the first projection
        // carries the overlay, the second uses plain history with no clone.
        let history = vec![
            system("prompt"),
            user("do work"),
            assistant_tool_call("A"),
            tool_result("A"),
            user("continue"),
        ];
        let snapshot = RuntimeContextSnapshot::new(
            Some("## Active Context\n- a.rs".to_string()),
            Some("<RelevantMemory>\n- key-a\n</RelevantMemory>".to_string()),
        );
        assert!(!snapshot.is_empty());

        let first = RequestMessages::with_runtime_context(&history, &snapshot);
        assert!(!first.is_borrowed());
        let first_slice = first.as_slice();
        assert_eq!(first_slice.len(), history.len() + 1);
        let overlay_count = first_slice
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("<RuntimeContext>"))
            })
            .count();
        assert_eq!(overlay_count, 1, "exactly one overlay on the first request");

        // Second iteration: overlay consumed, plain history borrowed as-is.
        let second = RequestMessages::borrowed(&history);
        assert!(second.is_borrowed());
        assert!(std::ptr::eq(
            second.as_slice().as_ptr(),
            history.as_slice().as_ptr()
        ));
        assert!(
            !second.as_slice().iter().any(|m| m
                .content
                .as_deref()
                .is_some_and(|c| c.contains("<RuntimeContext>"))),
            "no overlay on subsequent requests"
        );
    }

    #[tokio::test]
    async fn test_builder_empty_without_files_or_memories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path());
        let snapshot = RuntimeContextBuilder::new(&fs).build(Some("cache")).await;
        assert!(snapshot.is_empty());
    }

    #[tokio::test]
    async fn test_builder_active_context_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path());
        fs.context_manager
            .write()
            .await
            .add_file(std::path::Path::new("src/ユーザー管理.rs"));
        let snapshot = RuntimeContextBuilder::new(&fs).build(None).await;
        assert!(snapshot.active_context.is_some());
        assert!(snapshot.memory_context.is_none());
        assert!(!snapshot.is_empty());
    }

    #[tokio::test]
    async fn test_builder_memory_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path());
        fs.write_memory("cache", "cache design notes", None, None)
            .await
            .expect("write memory");
        let snapshot = RuntimeContextBuilder::new(&fs).build(Some("cache")).await;
        assert!(snapshot.memory_context.is_some());
        assert!(
            snapshot
                .memory_context
                .as_deref()
                .unwrap()
                .contains("<RelevantMemory>")
        );
    }

    #[tokio::test]
    async fn test_builder_both_active_and_memory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path());
        fs.context_manager
            .write()
            .await
            .add_file(std::path::Path::new("src/auth.rs"));
        fs.write_memory("cache", "cache design notes", None, None)
            .await
            .expect("write memory");
        let snapshot = RuntimeContextBuilder::new(&fs).build(Some("cache")).await;
        assert!(snapshot.active_context.is_some());
        assert!(snapshot.memory_context.is_some());
    }

    #[tokio::test]
    async fn test_builder_ignores_no_match_memory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path());
        fs.write_memory("unrelated-key", "unrelated body", None, None)
            .await
            .expect("write memory");
        let snapshot = RuntimeContextBuilder::new(&fs)
            .build(Some("zzz-no-such-memory-qqq"))
            .await;
        assert!(snapshot.memory_context.is_none());
        assert!(snapshot.is_empty());
    }

    #[tokio::test]
    async fn test_builder_does_not_mutate_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path());
        fs.context_manager
            .write()
            .await
            .add_file(std::path::Path::new("src/a.rs"));
        let history = vec![system("prompt"), user("do work")];
        let before_len = history.len();
        let goal = last_user_goal(&history);
        let snapshot = RuntimeContextBuilder::new(&fs).build(goal.as_deref()).await;
        assert!(!snapshot.is_empty());
        // Canonical history is untouched: no overlay markers leak into it.
        assert_eq!(history.len(), before_len);
        assert!(
            !history.iter().any(|m| m
                .content
                .as_deref()
                .is_some_and(|c| c.contains("<RuntimeContext>")
                    || c.contains("Active Context")
                    || c.contains("RelevantMemory")))
        );
    }
}
