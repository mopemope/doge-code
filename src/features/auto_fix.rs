//! Generic Auto-Fix Logic
use crate::config::AppConfig;
use crate::llm::{self, OpenAIClient};
use crate::tools::FsTools;
use anyhow::{Context, Result};
use std::future::Future;
use std::pin::Pin;
use tracing::{info, warn};

/// Result of a check that can be auto-fixed
#[derive(Debug, Clone)]
pub struct FixableResult {
    /// Whether the check passed
    pub success: bool,
    /// Standard output from the check command
    pub stdout: String,
    /// Standard error from the check command
    pub stderr: String,
    /// Exit code, if applicable
    pub exit_code: Option<i32>,
    /// Optional context prompt to be included in the fix request
    pub context_prompt: Option<String>,
}

/// Trait for the agent that performs fixes
pub trait FixerAgent: Send + Sync {
    fn fix<'a>(&'a self, prompt: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

/// Default implementation using the Project's LLM Agent
pub struct DefaultFixerAgent {
    client: Option<OpenAIClient>,
    tools: FsTools,
    config: AppConfig,
}

impl DefaultFixerAgent {
    pub fn new(tools: FsTools, client: Option<OpenAIClient>, config: AppConfig) -> Self {
        Self {
            tools,
            client,
            config,
        }
    }
}

impl FixerAgent for DefaultFixerAgent {
    fn fix<'a>(&'a self, prompt: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if self.client.is_none() {
                return Err(anyhow::anyhow!("OpenAI client not initialized"));
            }

            let sys_prompt = crate::tui::commands::prompt::build_system_prompt(&self.config);
            let msgs = vec![
                llm::types::ChatMessage {
                    role: "system".into(),
                    content: Some(sys_prompt),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                llm::types::ChatMessage {
                    role: "user".into(),
                    content: Some(prompt.to_string()),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ];

            // We act as a "fresh" agent for the fix.
            // run_agent_loop returns (updated_msgs, final_response)
            // We discard them because we rely on the side effects (tools)
            // and loop until check passes.
            let _ = llm::run_agent_loop(
                self.client.as_ref().unwrap(),
                &self.config.model,
                &self.tools,
                msgs,
                None,
                None,
                &self.config,
                None,
            )
            .await?;

            Ok(())
        })
    }
}

/// Generic Fixer that runs a check command, analyzes failure, fixes, and repeats
pub struct AutoFixer<F: FixerAgent> {
    fixer: F,
    max_iterations: usize,
}

impl<F: FixerAgent> AutoFixer<F> {
    pub fn new(fixer: F, max_iterations: usize) -> Self {
        Self {
            fixer,
            max_iterations,
        }
    }

    /// Runs a fix loop
    pub async fn run_fix_loop<CheckFn, Fut, PromptFn>(
        &mut self,
        mut check_fn: CheckFn,
        prompt_fn: PromptFn,
    ) -> Result<(FixableResult, usize)>
    where
        CheckFn: FnMut() -> Fut + Send,
        Fut: std::future::Future<Output = FixableResult> + Send,
        PromptFn: Fn(&FixableResult, usize) -> String + Send,
    {
        let mut iteration = 0;

        // Initial run
        let mut last_result = check_fn().await;
        if last_result.success {
            return Ok((last_result, 0));
        }

        while iteration < self.max_iterations {
            iteration += 1;
            info!("Auto-fix iteration {}/{}", iteration, self.max_iterations);

            // Build prompt
            let prompt = prompt_fn(&last_result, iteration);

            // LLM Analysis & Fix
            info!("Sending failure context to LLM for analysis...");
            let fix_res: Result<()> = self.fixer.fix(&prompt).await;
            fix_res.context("Failed to run LLM analysis")?;

            // Re-run check
            info!("Re-running check after LLM fix...");
            last_result = check_fn().await;

            if last_result.success {
                info!("Check passed after {} iteration(s)!", iteration);
                return Ok((last_result, iteration));
            }
        }

        warn!(
            "Auto-fix loop reached max iterations ({}) without success",
            self.max_iterations
        );
        Ok((last_result, iteration))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockFixer {
        fix_calls: Arc<Mutex<Vec<String>>>,
    }

    impl FixerAgent for MockFixer {
        fn fix<'a>(
            &'a self,
            prompt: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            let prompts = self.fix_calls.clone();
            let p = prompt.to_string();
            Box::pin(async move {
                prompts.lock().unwrap().push(p);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn test_auto_fixer_success_first_try() {
        let fixer = MockFixer {
            fix_calls: Arc::new(Mutex::new(vec![])),
        };
        let mut auto_fixer = AutoFixer::new(fixer, 3);

        let check_fn = || async {
            FixableResult {
                success: true,
                stdout: "".into(),
                stderr: "".into(),
                exit_code: Some(0),
                context_prompt: None,
            }
        };

        let prompt_fn = |_: &FixableResult, _: usize| "prompt".to_string();

        let (res, iters) = auto_fixer.run_fix_loop(check_fn, prompt_fn).await.unwrap();
        assert!(res.success);
        assert_eq!(iters, 0);
    }

    #[tokio::test]
    async fn test_auto_fixer_retry_success() {
        let fix_calls = Arc::new(Mutex::new(vec![]));
        let fixer = MockFixer {
            fix_calls: fix_calls.clone(),
        };
        let mut auto_fixer = AutoFixer::new(fixer, 3);

        // Fail once, then succeed
        let attempt = Arc::new(Mutex::new(0));
        let check_fn = || {
            let attempt = attempt.clone();
            async move {
                let mut a = attempt.lock().unwrap();
                *a += 1;
                if *a == 1 {
                    FixableResult {
                        success: false,
                        ..result_fail()
                    }
                } else {
                    result_success()
                }
            }
        };

        // Helper for constructing results
        fn result_fail() -> FixableResult {
            FixableResult {
                success: false,
                stdout: "fail".into(),
                stderr: "".into(),
                exit_code: Some(1),
                context_prompt: None,
            }
        }
        fn result_success() -> FixableResult {
            FixableResult {
                success: true,
                stdout: "ok".into(),
                stderr: "".into(),
                exit_code: Some(0),
                context_prompt: None,
            }
        }

        let prompt_fn = |_: &FixableResult, i: usize| format!("fix {}", i);

        let (res, iters) = auto_fixer.run_fix_loop(check_fn, prompt_fn).await.unwrap();
        assert!(res.success);
        assert_eq!(iters, 1);
        assert_eq!(fix_calls.lock().unwrap().len(), 1);
        assert_eq!(fix_calls.lock().unwrap()[0], "fix 1");
    }

    #[tokio::test]
    async fn test_auto_fixer_max_iters() {
        let fix_calls = Arc::new(Mutex::new(vec![]));
        let fixer = MockFixer {
            fix_calls: fix_calls.clone(),
        };
        let mut auto_fixer = AutoFixer::new(fixer, 2);

        // Always fail
        let check_fn = || async {
            FixableResult {
                success: false,
                stdout: "fail".into(),
                stderr: "".into(),
                exit_code: Some(1),
                context_prompt: None,
            }
        };

        let prompt_fn = |_: &FixableResult, i: usize| format!("fix {}", i);

        let (res, iters) = auto_fixer.run_fix_loop(check_fn, prompt_fn).await.unwrap();
        assert!(!res.success);
        assert_eq!(iters, 2);
        assert_eq!(fix_calls.lock().unwrap().len(), 2);
    }
}
