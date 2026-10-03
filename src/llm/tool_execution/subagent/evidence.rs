use super::budget::SubagentStopReason;
use crate::llm::tool_execution::dispatch::ToolOutput;
use crate::tools::budget::head_truncate;
use crate::tools::task::SUBAGENT_SUMMARY_BUDGET_CHARS;
use std::collections::HashSet;

pub(super) const MAX_REPORTED_FILES: usize = 32;
const MAX_EVIDENCE_ENTRIES: usize = 20;
const EVIDENCE_CHARS: usize = 240;
const FILE_CHARS: usize = 300;

#[derive(Default)]
pub(super) struct SubagentEvidenceLedger {
    entries: Vec<String>,
    observed_files: HashSet<String>,
    pub files: Vec<String>,
    pub files_truncated: bool,
}

impl SubagentEvidenceLedger {
    pub fn record(&mut self, name: &str, success: bool, summary: &str) {
        if self.entries.len() < MAX_EVIDENCE_ENTRIES {
            self.entries.push(
                head_truncate(
                    &format!(
                        "{name} ({}): {summary}",
                        if success { "ok" } else { "failed" }
                    ),
                    EVIDENCE_CHARS,
                )
                .text,
            );
        }
    }

    pub fn file_count(&self) -> usize {
        self.observed_files.len()
    }

    fn file(&mut self, path: &str) {
        if self.observed_files.insert(path.to_string()) {
            if self.files.len() < MAX_REPORTED_FILES {
                let truncated = head_truncate(path, FILE_CHARS);
                self.files_truncated |= truncated.text != path;
                self.files.push(truncated.text);
            } else {
                self.files_truncated = true;
            }
        }
    }

    /// Paths come exclusively from the dispatcher's current structured outputs.
    pub fn record_files(&mut self, tool: &str, output: &ToolOutput) {
        let value = &output.value;
        match tool {
            "fs_read" => {
                if let Some(path) = value.pointer("/result/path").and_then(|v| v.as_str()) {
                    self.file(path);
                }
            }
            "fs_read_many_files" => self.path_array(value.pointer("/result/files"), Some("path")),
            "find_file" => self.path_array(value.get("files"), None),
            "search_text" => self.path_array(value.get("results"), Some("path")),
            "search_repomap" => self.path_array(value.pointer("/results/results"), Some("file")),
            _ => {}
        }
    }

    fn path_array(&mut self, list: Option<&serde_json::Value>, key: Option<&str>) {
        if let Some(list) = list.and_then(|v| v.as_array()) {
            for item in list {
                if let Some(path) = key
                    .map_or(Some(item), |key| item.get(key))
                    .and_then(|v| v.as_str())
                {
                    self.file(path);
                }
            }
        }
    }

    pub fn fallback(&self, reason: SubagentStopReason) -> String {
        let facts = if self.entries.is_empty() {
            "- No tool evidence collected; findings remain unknown.".into()
        } else {
            self.entries
                .iter()
                .map(|v| format!("- {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        // Reserve space for Files and Recommendation even with a full ledger.
        let facts = head_truncate(&facts, 2000).text;
        let files = if self.files.is_empty() {
            "- No files examined.".into()
        } else {
            head_truncate(
                &self
                    .files
                    .iter()
                    .map(|v| format!("- {v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                1000,
            )
            .text
        };
        head_truncate(&format!("Sub-agent stopped early: {}.\n\nFacts:\n{facts}\n\nFiles:\n{files}\n\nRecommendation:\n- Continue investigation from the files/evidence above.\n- Results are partial because {}; unresolved findings remain unknown.", reason.as_str(), reason.as_str()), SUBAGENT_SUMMARY_BUDGET_CHARS).text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_structured_file_tracking_all_read_tools() {
        let mut ledger = SubagentEvidenceLedger::default();
        for (tool, value) in [
            ("fs_read", serde_json::json!({"result":{"path":"a"}})),
            (
                "fs_read_many_files",
                serde_json::json!({"result":{"files":[{"path":"b"}]}}),
            ),
            ("find_file", serde_json::json!({"files":["c"]})),
            ("search_text", serde_json::json!({"results":[{"path":"d"}]})),
            (
                "search_repomap",
                serde_json::json!({"results":{"results":[{"file":"e"}]}}),
            ),
        ] {
            ledger.record_files(
                tool,
                &ToolOutput {
                    value,
                    is_success: true,
                    result_summary: String::new(),
                },
            );
        }
        assert_eq!(ledger.files, ["a", "b", "c", "d", "e"]);
    }
    #[test]
    fn test_files_and_evidence_bounded_unicode_fallback() {
        let mut ledger = SubagentEvidenceLedger::default();
        for i in 0..100 {
            ledger.file(&format!("file-{i}"));
            ledger.file(&format!("file-{i}"));
            ledger.record("fs_read", true, &"調査".repeat(1000));
        }
        assert_eq!(ledger.files.len(), MAX_REPORTED_FILES);
        assert_eq!(ledger.file_count(), 100);
        assert!(ledger.files_truncated);
        assert_eq!(ledger.entries.len(), MAX_EVIDENCE_ENTRIES);
        assert!(
            ledger
                .entries
                .iter()
                .all(|v| v.chars().count() <= EVIDENCE_CHARS)
        );
        let summary = ledger.fallback(SubagentStopReason::TokenBudget);
        assert!(summary.chars().count() <= SUBAGENT_SUMMARY_BUDGET_CHARS);
        for part in [
            "Facts:",
            "Files:",
            "Recommendation:",
            "token_budget",
            "file-0",
            "fs_read",
        ] {
            assert!(summary.contains(part), "{part}");
        }
    }
}
