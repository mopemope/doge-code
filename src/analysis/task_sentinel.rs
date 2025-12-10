const STALL_THRESHOLD: usize = 15; // Warn after 15 steps without progress

#[derive(Debug, Clone)]
pub struct TaskSentinel {
    last_progress_step: usize,
    current_step: usize,
}

impl Default for TaskSentinel {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskSentinel {
    pub fn new() -> Self {
        Self {
            last_progress_step: 0,
            current_step: 0,
        }
    }

    /// Record a tool execution.
    /// `name`: Tool name
    /// `success`: Whether the tool execution was successful
    pub fn record_tool_call(&mut self, name: &str, success: bool) {
        self.current_step += 1;

        // heuristic: modification tools or plan updates count as progress
        let is_progress = success
            && (
                name == "fs_write"
                    || name == "edit"
                    || name == "apply_patch"
                    || name == "plan_write"
                    || name == "undo"
                // undo is also an action
            );

        if is_progress {
            self.last_progress_step = self.current_step;
        }
    }

    pub fn check_stalled(&self) -> Option<String> {
        let steps_since_progress = self.current_step.saturating_sub(self.last_progress_step);

        if steps_since_progress >= STALL_THRESHOLD {
            return Some(format!(
                "WARNING: No significant progress (file modification or plan update) detected for the last {} steps. You seem to be just reading or searching. Please reviewing your findings and take ACTION (edit code or update plan).",
                steps_since_progress
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stalled_detection() {
        let mut sentinel = TaskSentinel::new();

        // 14 reads - no warning
        for _ in 0..14 {
            sentinel.record_tool_call("fs_read", true);
            assert!(sentinel.check_stalled().is_none());
        }

        // 15th read - warning
        sentinel.record_tool_call("fs_read", true);
        assert!(sentinel.check_stalled().is_some());

        // write - reset
        sentinel.record_tool_call("fs_write", true);
        assert!(sentinel.check_stalled().is_none());

        // 1 step after write - no warning
        sentinel.record_tool_call("fs_read", true);
        assert!(sentinel.check_stalled().is_none());
    }
}
