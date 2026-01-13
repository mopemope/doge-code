use crate::tui::commands::core::TuiExecutor;

use crate::tui::view::TuiApp;

/// Handle /stack command to manage ephemeral task queue
pub fn handle_stack(_executor: &mut TuiExecutor, ui: &mut TuiApp, args: &str) {
    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    let subcommand = parts.first().copied().unwrap_or("list");
    let content = parts.get(1).copied().unwrap_or("").trim();

    match subcommand {
        "add" => {
            if content.is_empty() {
                ui.push_log("[ERROR] Please specify a task description.");
                return;
            }
            ui.task_queue.push_back(content.to_string());
            ui.push_log(format!("[Stack] Added task: {}", content));
        }
        "next" => {
            if let Some(task) = ui.task_queue.pop_front() {
                ui.textarea.insert_str(&task);
                ui.push_log(format!("[Stack] Popped next task: {}", task));
            } else {
                ui.push_log("[Stack] No tasks in stack.");
            }
        }
        "list" => {
            if ui.task_queue.is_empty() {
                ui.push_log("[Stack] No tasks in stack.");
            } else {
                let tasks: Vec<String> = ui
                    .task_queue
                    .iter()
                    .enumerate()
                    .map(|(i, task)| format!("{}. {}", i + 1, task))
                    .collect();

                ui.push_markdown_response("## Ephemeral Task Stack");
                for task_str in tasks {
                    ui.push_log(task_str);
                }
            }
        }
        "clear" => {
            if ui.task_queue.is_empty() {
                ui.push_log("[Stack] Stack is already empty.");
            } else {
                let count = ui.task_queue.len();
                ui.task_queue.clear();
                ui.push_log(format!("[Stack] Cleared {} tasks.", count));
            }
        }
        _ => {
            ui.push_log(format!("[Stack] Unknown subcommand: {}", subcommand));
            ui.push_log("Usage: /stack [add <task>|next|list]");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::tui::commands::core::TuiExecutor;
    use crate::tui::view::TuiApp; // Changed from crate::tui::state::TuiApp to crate::tui::view::TuiApp

    fn create_test_app() -> TuiApp {
        // TuiApp::new(title, model, theme_name)
        TuiApp::new("Test App", None, "default").expect("Failed to create TuiApp for testing")
    }

    #[tokio::test]
    async fn test_handle_stack_add() {
        // TuiApp::new が重い依存（例えばターミナル操作）を持っていなければこれで動く。
        let mut app = create_test_app();
        // TuiExecutor::new(cfg, semantic_service)
        let mut executor =
            TuiExecutor::new(AppConfig::default()).expect("Failed to create executor");

        handle_stack(&mut executor, &mut app, "add Task1");
        assert_eq!(app.task_queue.len(), 1);
        assert_eq!(app.task_queue[0], "Task1");

        handle_stack(&mut executor, &mut app, "add Task2");
        assert_eq!(app.task_queue.len(), 2);
        assert_eq!(app.task_queue[1], "Task2");
    }

    #[tokio::test]
    async fn test_handle_stack_next() {
        let mut app = create_test_app();
        let mut executor =
            TuiExecutor::new(AppConfig::default()).expect("Failed to create executor");

        app.task_queue.push_back("Task1".to_string());
        app.task_queue.push_back("Task2".to_string());

        handle_stack(&mut executor, &mut app, "next");
        assert_eq!(app.task_queue.len(), 1);
        // textarea に "Task1" が入っているはずだが、textareaの状態検証は難しいかも。
        // しかし task_queue からは消えているはず。
        assert_eq!(app.task_queue[0], "Task2");

        // check logs if possible
        // assert!(app.shell_output_buffer.contains("Popped next task: Task1"));
    }
}
