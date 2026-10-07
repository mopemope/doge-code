//! Bounded, content-free observations. Never consulted by loop control.
use serde_json::Value;
use std::collections::VecDeque;

const RECENT_READS: usize = 128;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AgentProgressUsage {
    pub read_tool_calls: usize,
    pub successful_read_tool_calls: usize,
    pub search_tool_calls: usize,
    pub repeated_read_ranges: usize,
    pub first_mutation_tool_call: Option<usize>,
    pub first_verification_tool_call: Option<usize>,
    pub verification_tool_calls: usize,
    pub successful_verification_tool_calls: usize,
}

#[derive(Default)]
pub(super) struct ProgressObserver {
    pub usage: AgentProgressUsage,
    recent_reads: VecDeque<blake3::Hash>,
}

impl ProgressObserver {
    pub fn observe(
        &mut self,
        index: usize,
        name: &str,
        arguments: &str,
        success: bool,
        output: Option<&Value>,
        mutated: bool,
    ) {
        if matches!(name, "fs_read" | "fs_read_many_files") {
            self.usage.read_tool_calls += 1;
            if success {
                self.usage.successful_read_tool_calls += 1;
            }
        }
        if matches!(
            name,
            "search_text"
                | "search_repomap"
                | "find_file"
                | "fs_list"
                | "search_memory"
                | "tool_search"
        ) {
            self.usage.search_tool_calls += 1;
        }
        if mutated {
            self.usage.first_mutation_tool_call.get_or_insert(index);
        }
        if name == "fs_read" && success {
            self.observe_read(output);
        }
        if name == "execute_process"
            && output
                .and_then(|v| v.get("status"))
                .and_then(Value::as_str)
                .is_some_and(|s| matches!(s, "completed" | "timed_out"))
            && let Ok(args) =
                serde_json::from_str::<crate::execution::ExecuteProcessParams>(arguments)
            && crate::provenance::classify_verification(&args.program, &args.args).is_some()
        {
            self.usage.first_verification_tool_call.get_or_insert(index);
            self.usage.verification_tool_calls += 1;
            if success {
                self.usage.successful_verification_tool_calls += 1;
            }
        }
    }

    fn observe_read(&mut self, output: Option<&Value>) {
        let Some(result) = output.and_then(|v| v.get("result")) else {
            return;
        };
        let (Some(path), Some(start), Some(end), Some(content)) = (
            result.get("path").and_then(Value::as_str),
            result.get("start_line").and_then(Value::as_u64),
            result.get("end_line").and_then(Value::as_u64),
            result.get("content").and_then(Value::as_str),
        ) else {
            return;
        };
        if content.is_empty() {
            return;
        }
        // Length-prefix the path; keep only fixed-size fingerprints, never raw content.
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&start.to_le_bytes());
        hasher.update(&end.to_le_bytes());
        hasher.update(content.as_bytes());
        let key = hasher.finalize();
        if let Some(position) = self
            .recent_reads
            .iter()
            .position(|previous| *previous == key)
        {
            self.usage.repeated_read_ranges += 1;
            self.recent_reads.remove(position);
        }
        if self.recent_reads.len() == RECENT_READS {
            self.recent_reads.pop_front();
        }
        self.recent_reads.push_back(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn progress_distinguishes_changed_content_noops_and_executed_failed_checks() {
        let mut observer = ProgressObserver::default();
        let read =
            json!({"result":{"path":"private.rs","start_line":1,"end_line":2,"content":"secret"}});
        observer.observe(1, "fs_read", "{}", true, Some(&read), false);
        observer.observe(2, "fs_read", r#"{"cursor":1}"#, true, Some(&read), false);
        let mut changed = read.clone();
        changed["result"]["content"] = json!("changed");
        observer.observe(3, "fs_read", "{}", true, Some(&changed), false);
        observer.observe(4, "fs_read", "{}", false, None, false);
        observer.observe(
            5,
            "edit",
            "{}",
            true,
            Some(&json!({"changed":false})),
            false,
        );
        let args = r#"{"program":"cargo","args":["test"]}"#;
        observer.observe(
            6,
            "execute_process",
            args,
            false,
            Some(&json!({"status":"policy_denied"})),
            false,
        );
        observer.observe(
            7,
            "execute_process",
            args,
            false,
            Some(&json!({"status":"completed"})),
            false,
        );
        observer.observe(8, "edit", "{}", true, None, true);
        observer.observe(
            9,
            "execute_process",
            args,
            true,
            Some(&json!({"status":"completed"})),
            false,
        );
        observer.observe(
            10,
            "execute_process",
            r#"{"program":"echo","args":["cargo","test"]}"#,
            true,
            Some(&json!({"status":"completed"})),
            false,
        );
        let usage = &observer.usage;
        assert_eq!(usage.read_tool_calls, 4);
        assert_eq!(usage.successful_read_tool_calls, 3);
        assert_eq!(usage.repeated_read_ranges, 1);
        assert_eq!(usage.first_mutation_tool_call, Some(8));
        assert_eq!(usage.first_verification_tool_call, Some(7));
        assert_eq!(usage.verification_tool_calls, 2);
        assert_eq!(usage.successful_verification_tool_calls, 1);
        let serialized = serde_json::to_string(usage).expect("usage");
        assert!(!serialized.contains("private.rs"));
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("cargo"));
    }

    #[test]
    fn read_fingerprints_are_bounded_and_empty_eof_is_not_repeated() {
        let mut observer = ProgressObserver::default();
        for i in 0..=RECENT_READS {
            observer.observe_read(Some(
                &json!({"result":{"path":i.to_string(),"start_line":1,"end_line":1,"content":"x"}}),
            ));
        }
        assert_eq!(observer.recent_reads.len(), RECENT_READS);
        observer.observe_read(Some(
            &json!({"result":{"path":"0","start_line":1,"end_line":1,"content":"x"}}),
        ));
        assert_eq!(observer.usage.repeated_read_ranges, 0);
        for _ in 0..2 {
            observer.observe_read(Some(
                &json!({"result":{"path":"eof","start_line":2,"end_line":1,"content":""}}),
            ));
        }
        assert_eq!(observer.usage.repeated_read_ranges, 0);
    }
}
