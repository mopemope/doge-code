const STALL_THRESHOLD: usize = 15; // Warn after 15 steps without progress
const PLAN_WRITE_NOOP_THRESHOLD: usize = 2; // Warn early for repeated no-op plan updates
const STALL_CRITICAL_THRESHOLD: usize = 3; // Escalate after repeated stalled warnings
const STALL_HARD_THRESHOLD: usize = 6; // Hard intervention after persistent stalling

#[derive(Debug, Clone)]
pub struct TaskSentinel {
    last_progress_step: usize,
    current_step: usize,
    consecutive_noop_plan_writes: usize,
    consecutive_stall_warnings: usize,
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
            consecutive_noop_plan_writes: 0,
            consecutive_stall_warnings: 0,
        }
    }

    /// Record a tool execution.
    /// `name`: Tool name
    /// `success`: Whether the tool execution was successful
    pub fn record_tool_call(&mut self, name: &str, success: bool) {
        self.record_tool_call_with_progress(name, success, None);
    }

    /// Record a tool execution with an optional explicit progress signal.
    /// `made_progress`: when `Some`, overrides the default heuristic for progress detection.
    pub fn record_tool_call_with_progress(
        &mut self,
        name: &str,
        success: bool,
        made_progress: Option<bool>,
    ) {
        self.current_step += 1;

        // Heuristic: only concrete state-changing actions count as progress.
        let default_is_progress = success
            && matches!(
                name,
                "fs_write" | "edit" | "apply_patch" | "undo" | "execute_bash"
            );
        let is_progress = made_progress.unwrap_or(default_is_progress);

        if name == "plan_write" {
            if success && matches!(made_progress, Some(false)) {
                self.consecutive_noop_plan_writes += 1;
            } else {
                self.consecutive_noop_plan_writes = 0;
            }
        } else {
            self.consecutive_noop_plan_writes = 0;
        }

        if is_progress {
            self.last_progress_step = self.current_step;
            self.consecutive_stall_warnings = 0;
        }
    }

    pub fn check_stalled(&mut self) -> Option<String> {
        if self.consecutive_noop_plan_writes >= PLAN_WRITE_NOOP_THRESHOLD {
            return Some(format!(
                "WARNING: `plan_write` returned unchanged results {} times in a row. Stop repeating no-op plan updates and either modify code, run a different tool, or answer the user directly.",
                self.consecutive_noop_plan_writes
            ));
        }

        let steps_since_progress = self.current_step.saturating_sub(self.last_progress_step);

        if steps_since_progress >= STALL_THRESHOLD {
            self.consecutive_stall_warnings += 1;

            if self.consecutive_stall_warnings >= STALL_HARD_THRESHOLD {
                return Some(format!(
                    "HARD INTERVENTION: No significant progress detected for the last {} steps ({} consecutive stalled warnings). Stop repeating the same tool calls.\n<MANDATORY_ACTION>\n1. Summarize why the current strategy is failing.\n2. Choose a materially different next action.\n3. If blocked, ask the user for clarification.\n</MANDATORY_ACTION>",
                    steps_since_progress, self.consecutive_stall_warnings
                ));
            }

            if self.consecutive_stall_warnings >= STALL_CRITICAL_THRESHOLD {
                return Some(format!(
                    "CRITICAL WARNING: No significant progress detected for the last {} steps ({} consecutive stalled warnings). You must change strategy now instead of repeating read/search loops.",
                    steps_since_progress, self.consecutive_stall_warnings
                ));
            }

            return Some(format!(
                "WARNING: No significant progress detected for the last {} steps. You seem to be stuck in a loop of reading or searching. Please review your findings and take ACTION (edit code, update plan, or execute a command).",
                steps_since_progress
            ));
        }
        self.consecutive_stall_warnings = 0;
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

    #[test]
    fn test_plan_write_is_not_progress() {
        let mut sentinel = TaskSentinel::new();

        for _ in 0..15 {
            sentinel.record_tool_call("plan_write", true);
        }

        assert!(sentinel.check_stalled().is_some());
    }

    #[test]
    fn test_plan_write_noop_streak_warns_early() {
        let mut sentinel = TaskSentinel::new();

        sentinel.record_tool_call_with_progress("plan_write", true, Some(false));
        assert!(sentinel.check_stalled().is_none());

        sentinel.record_tool_call_with_progress("plan_write", true, Some(false));
        let warning = sentinel
            .check_stalled()
            .expect("expected warning for repeated no-op plan_write");
        assert!(warning.contains("plan_write"));
        assert!(warning.contains("unchanged"));
    }

    #[test]
    fn test_plan_write_noop_streak_resets_on_progress() {
        let mut sentinel = TaskSentinel::new();

        sentinel.record_tool_call_with_progress("plan_write", true, Some(false));
        sentinel.record_tool_call_with_progress("plan_write", true, Some(true));
        assert!(sentinel.check_stalled().is_none());

        sentinel.record_tool_call_with_progress("plan_write", true, Some(false));
        assert!(sentinel.check_stalled().is_none());
    }

    #[test]
    fn test_stalled_warning_escalates_to_critical_and_hard() {
        let mut sentinel = TaskSentinel::new();

        for _ in 0..15 {
            sentinel.record_tool_call("fs_read", true);
        }
        let first = sentinel.check_stalled().expect("expected first warning");
        assert!(first.starts_with("WARNING:"));

        // Escalate to CRITICAL on repeated stalled checks.
        for _ in 0..2 {
            sentinel.record_tool_call("fs_read", true);
            let _ = sentinel.check_stalled();
        }
        sentinel.record_tool_call("fs_read", true);
        let critical = sentinel.check_stalled().expect("expected critical warning");
        assert!(critical.starts_with("CRITICAL WARNING:"));

        // Escalate to HARD INTERVENTION with persistent stalling.
        for _ in 0..2 {
            sentinel.record_tool_call("fs_read", true);
            let _ = sentinel.check_stalled();
        }
        sentinel.record_tool_call("fs_read", true);
        let hard = sentinel
            .check_stalled()
            .expect("expected hard intervention");
        assert!(hard.starts_with("HARD INTERVENTION:"));
    }
}
