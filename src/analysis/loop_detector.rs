use crate::llm::types::ToolCall;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

const HISTORY_CAPACITY: usize = 10;
const REPEAT_THRESHOLD: usize = 3;

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
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
        }
    }

    pub fn record_tool_call(&mut self, call: &ToolCall) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        call.function.arguments.hash(&mut hasher);
        let args_hash = hasher.finish();

        let entry = ToolCallEntry {
            name: call.function.name.clone(),
            args_hash,
        };

        if self.history.len() >= HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(entry);
    }

    pub fn detect_loop(&self) -> Option<LoopType> {
        if self.history.len() < REPEAT_THRESHOLD {
            return None;
        }

        // 1. Check for immediate consecutive repetition
        // Check if the last N entries are identical
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
            return Some(LoopType::ConsecutiveRepetition(last.name.clone()));
        }

        // 2. Check for simple cycles (A -> B -> A -> B ...)
        // Min cycle length 2, max cycle length hardcoded for simplicity (e.g. 2 or 3)
        // Check cycle length 2 (A, B, A, B)
        if self.history.len() >= 4 {
            let n = self.history.len();
            let h = &self.history;

            // Look at last 4: [..., A, B, A, B]
            // Indexes: n-4, n-3, n-2, n-1
            let a1 = &h[n - 4];
            let b1 = &h[n - 3];
            let a2 = &h[n - 2];
            let b2 = &h[n - 1];

            if is_same(a1, a2) && is_same(b1, b2) && !is_same(a1, b1) {
                return Some(LoopType::CycleRepetition);
            }
        }

        // Check cycle length 3 (A, B, C, A, B, C)
        if self.history.len() >= 6 {
            let n = self.history.len();
            let h = &self.history;
            if is_same(&h[n - 6], &h[n - 3])
                && is_same(&h[n - 5], &h[n - 2])
                && is_same(&h[n - 4], &h[n - 1])
            {
                return Some(LoopType::CycleRepetition);
            }
        }

        None
    }

    pub fn loop_warning(&self, loop_type: &LoopType) -> String {
        match loop_type {
            LoopType::ConsecutiveRepetition(name) => {
                format!(
                    "WARNING: You are repeatedly calling the tool '{}' with the same arguments. STOP. This strategy is NOT working.\n\
                     <RECOMMENDED_ACTION>\n\
                     1. Analyze WHY it is failing.\n\
                     2. Read the error message carefully.\n\
                     3. Try a DIFFERENT tool or approach (e.g., if `edit` fails, use `fs_read` to verify the file content first).\n\
                     </RECOMMENDED_ACTION>",
                    name
                )
            }
            LoopType::CycleRepetition => {
                "WARNING: You are in a repetitive loop (A -> B -> A -> B). Your current mental model is likely incorrect. STOP.\n\
                 <RECOMMENDED_ACTION>\n\
                 1. Reset your plan.\n\
                 2. Use `plan_write` to outline a NEW approach.\n\
                 3. Double check file paths and contents.\n\
                 </RECOMMENDED_ACTION>".to_string()
            }
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
}
