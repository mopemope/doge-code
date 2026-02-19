use crate::llm::types::ToolCall;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

const HISTORY_CAPACITY: usize = 10;
const REPEAT_THRESHOLD: usize = 3;
const PLAN_WRITE_NO_CHANGE_MARKER: &str = "plan_write_no_change";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopType {
    /// Same tool called with same arguments consecutively
    ConsecutiveRepetition(String),
    /// A repeating pattern A -> B -> A -> B
    CycleRepetition,
}

#[derive(Debug, Clone)]
struct ToolCallEntry {
    name: String,
    args_hash: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LoopDetector {
    history: VecDeque<ToolCallEntry>,
    intervention_count: usize,
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            intervention_count: 0,
        }
    }

    pub fn record_tool_call(&mut self, call: &ToolCall) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        call.function.arguments.hash(&mut hasher);
        let args_hash = hasher.finish();

        self.push_entry(ToolCallEntry {
            name: call.function.name.clone(),
            args_hash,
        });
    }

    pub fn record_plan_write_no_change(&mut self) {
        self.push_entry(ToolCallEntry {
            name: PLAN_WRITE_NO_CHANGE_MARKER.to_string(),
            args_hash: 0,
        });
    }

    fn push_entry(&mut self, entry: ToolCallEntry) {
        if self.history.len() >= HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(entry);
    }

    pub fn detect_loop(&mut self) -> Option<LoopType> {
        if self.history.len() < REPEAT_THRESHOLD {
            return None;
        }

        let loop_detected = {
            // 1. Check for immediate consecutive repetition
            let last = self.history.back()?;
            let mut count = 0;
            for entry in self.history.iter().rev() {
                if entry.name == last.name && entry.args_hash == last.args_hash {
                    count += 1;
                } else {
                    break;
                }
            }

            if count >= REPEAT_THRESHOLD {
                Some(LoopType::ConsecutiveRepetition(last.name.clone()))
            } else {
                // 2. Check for simple cycles (A -> B -> A -> B ...)
                if self.history.len() >= 4 {
                    let n = self.history.len();
                    let h = &self.history;

                    let a1 = &h[n - 4];
                    let b1 = &h[n - 3];
                    let a2 = &h[n - 2];
                    let b2 = &h[n - 1];

                    if is_same(a1, a2) && is_same(b1, b2) && !is_same(a1, b1) {
                        Some(LoopType::CycleRepetition)
                    } else if self.history.len() >= 6 {
                        // Check cycle length 3 (A, B, C, A, B, C)
                        if is_same(&h[n - 6], &h[n - 3])
                            && is_same(&h[n - 5], &h[n - 2])
                            && is_same(&h[n - 4], &h[n - 1])
                        {
                            Some(LoopType::CycleRepetition)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };

        if loop_detected.is_some() {
            self.intervention_count += 1;
        } else {
            self.intervention_count = 0;
        }

        loop_detected
    }

    pub fn loop_warning(&self, loop_type: &LoopType) -> String {
        let (base_msg, recommendation) = match loop_type {
            LoopType::ConsecutiveRepetition(name) => {
                if name == PLAN_WRITE_NO_CHANGE_MARKER {
                    (
                        "You are repeatedly calling `plan_write`, but the plan remains unchanged."
                            .to_string(),
                        "Stop repeating no-op `plan_write`. Update actual task state, use a different tool, or answer the user directly.",
                    )
                } else {
                    (
                        format!(
                            "You are repeatedly calling the tool '{}' with the same arguments.",
                            name
                        ),
                        "Try a DIFFERENT tool or approach (e.g., if `edit` fails, use `fs_read` to verify content first).",
                    )
                }
            }
            LoopType::CycleRepetition => (
                "You are in a repetitive loop (A -> B -> A -> B).".to_string(),
                "Stop repeating the same tool pattern. Either answer the user directly or choose a different next action based on the latest tool result.",
            ),
        };

        match self.intervention_count {
            1 => format!(
                "WARNING: {}\n<RECOMMENDED_ACTION>\n{}\n</RECOMMENDED_ACTION>",
                base_msg, recommendation
            ),
            2 => format!(
                "CRITICAL WARNING: {}. Action required! You MUST stop this cycle.\n<INSTRUCTION>\nAnalyze the previous errors carefully. Your current strategy is fundamentally flawed. Stop and THINK.\n</INSTRUCTION>",
                base_msg
            ),
            _ => format!(
                "HARD INTERVENTION: {}. LOOP DETECTED. DO NOT REPEAT.\n<MANDATORY_ACTION>\n1. STOP all current tool sequences.\n2. Summarize WHY you are stuck.\n3. PROPOSE a completely different path or ask the user for help.\n</MANDATORY_ACTION>",
                base_msg
            ),
        }
    }
}

fn is_same(a: &ToolCallEntry, b: &ToolCallEntry) -> bool {
    a.name == b.name && a.args_hash == b.args_hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{ToolCall, ToolCallFunction};

    fn make_call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: Some("test".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn test_consecutive_repetition_limit() {
        let mut detector = LoopDetector::new();
        let call = make_call("read", "{}");

        // 1st
        detector.record_tool_call(&call);
        assert!(detector.detect_loop().is_none());
        // 2nd
        detector.record_tool_call(&call);
        assert!(detector.detect_loop().is_none());
        // 3rd - threshold reached
        detector.record_tool_call(&call);
        match detector.detect_loop() {
            Some(LoopType::ConsecutiveRepetition(name)) => assert_eq!(name, "read"),
            _ => panic!("Expected ConsecutiveRepetition"),
        }
    }

    #[test]
    fn test_cycle_repetition_ab() {
        let mut detector = LoopDetector::new();
        let a = make_call("A", "{}");
        let b = make_call("B", "{}");

        // A -> B -> A -> B
        detector.record_tool_call(&a);
        detector.record_tool_call(&b);
        detector.record_tool_call(&a);
        assert!(detector.detect_loop().is_none());

        detector.record_tool_call(&b);
        match detector.detect_loop() {
            Some(LoopType::CycleRepetition) => {}
            _ => panic!("Expected CycleRepetition"),
        }
    }

    #[test]
    fn test_loop_escalation() {
        let mut detector = LoopDetector::new();
        let call = make_call("read", "{}");

        // 1st detection (3 calls total)
        for _ in 0..3 {
            detector.record_tool_call(&call);
        }
        let loop_type = detector.detect_loop().unwrap();
        let warning1 = detector.loop_warning(&loop_type);
        assert!(warning1.contains("WARNING"));
        assert!(!warning1.contains("CRITICAL"));

        // 2nd detection (same call again)
        detector.record_tool_call(&call);
        let loop_type = detector.detect_loop().unwrap();
        let warning2 = detector.loop_warning(&loop_type);
        assert!(warning2.contains("CRITICAL WARNING"));

        // 3rd detection
        detector.record_tool_call(&call);
        let loop_type = detector.detect_loop().unwrap();
        let warning3 = detector.loop_warning(&loop_type);
        assert!(warning3.contains("HARD INTERVENTION"));

        // Reset
        let call2 = make_call("write", "{}");
        detector.record_tool_call(&call2);
        assert!(detector.detect_loop().is_none());
        assert_eq!(detector.intervention_count, 0);
    }

    #[test]
    fn test_plan_write_no_change_marker_detection() {
        let mut detector = LoopDetector::new();

        detector.record_plan_write_no_change();
        assert!(detector.detect_loop().is_none());
        detector.record_plan_write_no_change();
        assert!(detector.detect_loop().is_none());
        detector.record_plan_write_no_change();

        let loop_type = detector
            .detect_loop()
            .expect("expected loop detection for no-change plan writes");
        match &loop_type {
            LoopType::ConsecutiveRepetition(name) => {
                assert_eq!(name, PLAN_WRITE_NO_CHANGE_MARKER);
            }
            _ => panic!("Expected ConsecutiveRepetition"),
        }

        let warning = detector.loop_warning(&loop_type);
        assert!(warning.contains("plan remains unchanged"));
    }
}
