use crate::planning::create_task_plan;
use crate::tui::view::TuiApp;

use tracing::info;

use crate::tui::commands::core::TuiExecutor;

impl TuiExecutor {
    /// Handle /plan command for task analysis and planning
    pub fn handle_plan_command(&mut self, task_description: &str, ui: &mut TuiApp) {
        if task_description.is_empty() {
            ui.push_log("Usage: /plan <task description>");
            return;
        }

        ui.push_log(format!("> /plan {}", task_description));
        ui.push_log("[INFO] Analyzing task and generating plan...");
        ui.status = crate::tui::state::Status::Processing;
        ui.dirty = true;

        // Use tokio runtime for asynchronous processing
        let task_analyzer = &self.task_analyzer;
        let task_desc = task_description.to_string();

        // Analysis is performed synchronously
        match task_analyzer.analyze(&task_desc) {
            Ok(classification) => {
                ui.push_log("[ANALYSIS] Task analysis completed.");
                ui.push_log(format!(
                    "[PLANNING] Task classification: {:?}",
                    classification.task_type
                ));
                ui.push_log(format!(
                    "[TARGET] Complexity: {:.1}/1.0",
                    classification.complexity_score
                ));
                ui.push_log(format!(
                    "[STATS] Estimated steps: {}",
                    classification.estimated_steps
                ));
                ui.push_log(format!(
                    "[WARNING] Risk level: {:?}",
                    classification.risk_level
                ));
                ui.push_log(format!(
                    "[TOOLS] Required tools: {}",
                    classification.required_tools.join(", ")
                ));
                ui.push_log(format!(
                    "[CONFIRMED] Confidence: {:.1}%",
                    classification.confidence * 100.0
                ));
                ui.push_log("[INFO] Decomposing task into steps...");

                // Decomposition is performed asynchronously in a separate thread
                let rt = tokio::runtime::Handle::current();
                let task_analyzer_clone = task_analyzer.clone();
                let ui_tx = self.ui_tx.clone();
                let plan_manager = self.plan_manager.clone();

                rt.spawn(async move {
                    match task_analyzer_clone
                        .decompose(&classification, &task_desc)
                        .await
                    {
                        Ok(steps) => {
                            if let Some(tx) = ui_tx {
                                let _ = tx.send("[INFO] Plan decomposition completed.".to_string());

                                for (i, step) in steps.iter().enumerate() {
                                    let step_icon = match step.step_type {
                                        crate::planning::StepType::Analysis => "[ANALYSIS]",
                                        crate::planning::StepType::Planning => "[PLANNING]",
                                        crate::planning::StepType::Implementation => {
                                            "[IMPLEMENTATION]"
                                        }
                                        crate::planning::StepType::Validation => "[VALIDATION]",
                                        crate::planning::StepType::Cleanup => "[CLEANUP]",
                                    };

                                    let _ = tx.send(format!(
                                        "  {}. {} {} ({}s)",
                                        i + 1,
                                        step_icon,
                                        step.description,
                                        step.estimated_duration
                                    ));

                                    if !step.dependencies.is_empty() {
                                        let _ = tx.send(format!(
                                            "     Dependencies: {}",
                                            step.dependencies.join(", ")
                                        ));
                                    }

                                    if !step.required_tools.is_empty() {
                                        let _ = tx.send(format!(
                                            "     Tools: {}",
                                            step.required_tools.join(", ")
                                        ));
                                    }
                                }

                                let plan = create_task_plan(task_desc, classification, steps);

                                // Register the plan
                                if let Ok(plan_manager) = plan_manager.lock() {
                                    match plan_manager.register_plan(plan.clone()) {
                                        Ok(plan_id) => {
                                            let _ = tx.send(format!(
                                                "[TIMER] Total estimated time: {}s",
                                                plan.total_estimated_duration
                                            ));
                                            let _ = tx.send(format!("[ID] Plan ID: {}", plan_id));
                                            let _ = tx.send(
                                                "[INFO] Plan generation completed.".to_string(),
                                            );
                                            let _ = tx.send("[INFO] How to execute:".to_string());
                                            let _ = tx.send(
                                                "   /execute        - Execute the latest plan"
                                                    .to_string(),
                                            );
                                            let _ = tx.send(format!(
                                                "   /execute {}  - Execute this plan",
                                                plan_id
                                            ));
                                            let _ = tx.send(
                                                "   Or give instructions like 'execute this plan'"
                                                    .to_string(),
                                            );

                                            info!(
                                                "Generated and registered plan with ID: {}",
                                                plan_id
                                            );
                                        }
                                        Err(e) => {
                                            let _ = tx.send(format!(
                                                "[ERROR] Failed to register plan: {}",
                                                e
                                            ));
                                        }
                                    }
                                }
                                let _ = tx.send("::status:idle".to_string());
                            }
                        }
                        Err(e) => {
                            if let Some(tx) = ui_tx {
                                let _ =
                                    tx.send(format!("[ERROR] Failed to decompose steps: {}", e));
                                let _ = tx.send("::status:idle".to_string());
                            }
                        }
                    }
                });
            }
            Err(e) => {
                ui.push_log(format!("[ERROR] Failed to analyze task: {}", e));
                ui.status = crate::tui::state::Status::Idle;
                ui.dirty = true;
            }
        }
    }
}
