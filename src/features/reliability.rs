use crate::config::AppConfig;
use crate::features::auto_fix::{AutoFixer, FixableResult};
use crate::features::verification::AutoVerifier;
use crate::llm::client_core::OpenAIClient;
use crate::llm::tool_execution::ui_rendering::truncate_string_with_graphemes;
use anyhow::Result;
use std::path::Path;
use tracing::{error, info};

pub struct ReliabilityLayer {
    verifier: AutoVerifier,
    client: Option<OpenAIClient>,
    fs_tools: crate::tools::FsTools,
    config: AppConfig,
}

impl ReliabilityLayer {
    pub fn new(
        config: AppConfig,
        client: Option<OpenAIClient>,
        fs_tools: crate::tools::FsTools,
    ) -> Self {
        let verifier = AutoVerifier::new(&config);
        Self {
            verifier,
            client,
            fs_tools,
            config,
        }
    }

    pub async fn verify_and_fix(
        &self,
        path: &Path,
        ui_tx: Option<&std::sync::mpsc::Sender<String>>,
    ) -> Result<String> {
        let mut tool_message_content = String::new();

        if let Some(mut verification_result) = self.verifier.verify_path(path).await {
            let mut fixed = false;
            let mut fixed_iters = 0;

            // Attempt Auto-Fix if configured
            if self.config.test_fix.max_iterations > 0 {
                info!(
                    "Verification failed for {}. Attempting auto-fix...",
                    path.display()
                );
                if let Some(tx) = ui_tx {
                    let _ = tx.send(format!(
                        "::status:working:Verification failed. Attempting auto-fix (max {} iters)...",
                        self.config.test_fix.max_iterations
                    ));
                }

                if let Some(client) = &self.client {
                    let fixer = crate::features::auto_fix::DefaultFixerAgent::new(
                        self.fs_tools.clone(),
                        Some(client.clone()),
                        self.config.clone(),
                    );
                    let mut auto_fixer = AutoFixer::new(fixer, self.config.test_fix.max_iterations);

                    // We need to clone specific fields for the async closure
                    let verifier_clone = self.verifier.clone();
                    let path_clone = path.to_path_buf();
                    let path_str = path.to_string_lossy().to_string();

                    // Define check function for the loop
                    let check_fn = move || {
                        let v = verifier_clone.clone();
                        let p = path_clone.clone();
                        Box::pin(async move {
                            match v.verify_path(&p).await {
                                Some(res) => FixableResult {
                                    success: false,
                                    stdout: res.stdout,
                                    stderr: res.stderr,
                                    exit_code: res.exit_code,
                                    context_prompt: None,
                                },
                                None => FixableResult {
                                    success: true,
                                    stdout: String::new(),
                                    stderr: String::new(),
                                    exit_code: Some(0),
                                    context_prompt: None,
                                },
                            }
                        })
                    };

                    // Define prompt function
                    let prompt_fn = move |res: &FixableResult, iter: usize| {
                        format!(
                            "<VERIFICATION_FAILURE iteration=\"{}\">\nThe verification command for {} failed.\n\nSTDOUT:\n{}\n\nSTDERR:\n{}\n\nPlease fix the code to resolve these errors.</VERIFICATION_FAILURE>",
                            iter,
                            path_str,
                            truncate_string_with_graphemes(&res.stdout, 2000),
                            truncate_string_with_graphemes(&res.stderr, 4000)
                        )
                    };

                    // Run the fix loop
                    match auto_fixer.run_fix_loop(check_fn, prompt_fn).await {
                        Ok((fixed_res, iters)) => {
                            if fixed_res.success {
                                fixed = true;
                                fixed_iters = iters;
                            } else {
                                // Update verification result with the final failure
                                verification_result.stdout = fixed_res.stdout;
                                verification_result.stderr = fixed_res.stderr;
                                verification_result.exit_code = fixed_res.exit_code;
                                verification_result.message = format!(
                                    "<verification_error>\nVerification Failed after {} auto-fix attempts:\n{}{}\n</verification_error>",
                                    iters, verification_result.stdout, verification_result.stderr
                                );
                            }
                        }
                        Err(e) => {
                            error!("Auto-fix execution error: {}", e);
                            if let Some(tx) = ui_tx {
                                let _ = tx.send(format!("::status:error:Auto-fix error: {}", e));
                            }
                        }
                    }
                } else {
                    info!("Client not available for auto-fix.");
                }
            }

            if fixed {
                info!("Auto-fix successful after {} iterations", fixed_iters);
                tool_message_content.push_str(&format!(
                    "\n\n<AUTO_FIX>\nVerification failed initially, but was automatically fixed after {} iterations.\n</AUTO_FIX>",
                    fixed_iters
                ));
                if let Some(tx) = ui_tx {
                    let _ = tx.send(format!(
                        "::status:fixed:Auto-fixed after {} iterations.",
                        fixed_iters
                    ));
                }
            } else {
                // Auto-fix failed or was disabled. Proceed with failure reporting/reverting.
                self.handle_verification_failure(
                    &mut tool_message_content,
                    verification_result,
                    ui_tx,
                )
                .await;
            }
        } else {
            // Verification passed initially
            let verification_note = r#"
    
    <SYSTEM_NOTE>
    File modification detected. Verification passed.
    </SYSTEM_NOTE>"#;
            tool_message_content.push_str(verification_note);
        }

        Ok(tool_message_content)
    }

    async fn handle_verification_failure(
        &self,
        tool_message_content: &mut String,
        verification_result: crate::features::verification::VerificationResult,
        ui_tx: Option<&std::sync::mpsc::Sender<String>>,
    ) {
        // Auto-revert logic (soft revert)
        let should_revert = self.config.verification.auto_revert;

        if should_revert {
            // Attempt to revert the change using the undo stack
            let reverted = {
                let mut stack = self.fs_tools.undo_stack.write().await;
                if let Some(entry) = stack.pop() {
                    // Write back the original content
                    crate::tools::write::fs_write(
                        entry.path.to_str().unwrap(),
                        &entry.content,
                        &self.config,
                    )
                    .is_ok()
                } else {
                    false
                }
            };

            let warning = if reverted {
                format!(
                    r#"
    
    <AUTOMATED_VERIFICATION_FAILURE>
    The tool execution succeeded, but an automated check FAILED with strict mode enabled.
    Exit Code: {:?}
    
    STDOUT:
    {}
    
    STDERR:
    {}
    </AUTOMATED_VERIFICATION_FAILURE>
    
    <DIRECTIVE>
    ⚠️ CRITICAL: Verification failed and your changes have been AUTOMATICALLY REVERTED.
    The file has been restored to its previous state.
    
    1. Analyze the STDERR output above to understand why your code failed.
    2. You MUST apply a DIFFERENT solution. Do not try the same broken code again.
    3. Fix the logical error or syntax error that caused the failure.
    </DIRECTIVE>"#,
                    verification_result.exit_code,
                    truncate_string_with_graphemes(&verification_result.stdout, 1000),
                    truncate_string_with_graphemes(&verification_result.stderr, 2000)
                )
            } else {
                format!(
                    r#"
    
    <AUTOMATED_VERIFICATION_FAILURE>
    The tool execution succeeded, but an automated check FAILED.
    Exit Code: {:?}
    
    STDOUT:
    {}
    
    STDERR:
    {}
    </AUTOMATED_VERIFICATION_FAILURE>
    
    <DIRECTIVE>
    ⚠️ CRITICAL: Verification failed and automatic revert FAILED.
    The codebase is potentially in a broken state. You MUST fix this immediately.
    
    1. Analyze the STDERR output above.
    2. Fix the error in the current file state.
    </DIRECTIVE>"#,
                    verification_result.exit_code,
                    truncate_string_with_graphemes(&verification_result.stdout, 1000),
                    truncate_string_with_graphemes(&verification_result.stderr, 2000)
                )
            };

            tool_message_content.push_str(&warning);
            if let Some(tx) = ui_tx {
                let status_msg = if reverted {
                    "::status:error:Verification failed. Changes reverted."
                } else {
                    "::status:error:Verification failed. Revert failed."
                };
                let _ = tx.send(status_msg.to_string());
            }
        } else {
            // Warning only (revert suppressed)
            let warning = format!(
                r#"
    
    <AUTOMATED_VERIFICATION_FAILURE>
    The tool execution succeeded, but an automated check FAILED.
    (Auto-revert suppressed to allow fix)
    Exit Code: {:?}
    
    STDOUT:
    {}
    
    STDERR:
    {}
    </AUTOMATED_VERIFICATION_FAILURE>
    
    <DIRECTIVE>
    ⚠️ CRITICAL: Verification failed.
    The codebase is potentially in a broken state. You MUST fix this immediately.
    Do not proceed with other tasks until this is resolved.
    
    1. Analyze the STDERR output above.
    2. Fix the error in the current file state.
    </DIRECTIVE>"#,
                verification_result.exit_code,
                truncate_string_with_graphemes(&verification_result.stdout, 1000),
                truncate_string_with_graphemes(&verification_result.stderr, 2000)
            );
            tool_message_content.push_str(&warning);
            if let Some(tx) = ui_tx {
                let _ = tx.send(
                    "::status:warning:Auto-verification failed. Correction required.".to_string(),
                );
            }
        }
    }
}
