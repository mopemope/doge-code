use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String, // pending, in_progress, completed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoList {
    pub todos: Vec<TodoItem>,
}

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "todo_read".to_string(),
            description: "Read the todo list for the current session.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
    }
}

pub fn todo_read(session_id: &str) -> Result<TodoList> {
    todo_read_from_base_path(session_id, ".")
}

pub fn todo_read_from_base_path(session_id: &str, base_path: &str) -> Result<TodoList> {
    // Define the todo file path
    let todo_dir = Path::new(base_path).join(".doge").join("todos");
    let todo_file_path = todo_dir.join(format!("{}.json", session_id));

    // Check if the file exists
    if !todo_file_path.exists() {
        // Return an empty todo list if the file doesn't exist
        return Ok(TodoList { todos: vec![] });
    }

    // Read the file content
    let json_content = fs::read_to_string(&todo_file_path)
        .with_context(|| format!("Failed to read todo file: {}", todo_file_path.display()))?;

    // Parse the JSON content
    let todo_list: TodoList = serde_json::from_str(&json_content)
        .with_context(|| format!("Failed to parse todo file: {}", todo_file_path.display()))?;

    Ok(todo_list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_todo_read_success() {
        // Create a temporary directory for testing
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.path();

        // Set up a mock session ID
        let session_id = "test-session-id";

        // Create a sample todo list
        let todos = vec![
            TodoItem {
                id: "task-1".to_string(),
                content: "Implement feature A".to_string(),
                status: "pending".to_string(),
            },
            TodoItem {
                id: "task-2".to_string(),
                content: "Fix bug B".to_string(),
                status: "in_progress".to_string(),
            },
        ];
        let todo_list = TodoList { todos };

        // Serialize the todo list to JSON
        let json_content = serde_json::to_string_pretty(&todo_list).unwrap();

        // Write the JSON content to a file
        let todo_dir = temp_path.join(".doge").join("todos");
        fs::create_dir_all(&todo_dir).unwrap();
        let todo_file_path = todo_dir.join(format!("{}.json", session_id));
        fs::write(&todo_file_path, json_content).unwrap();

        // Call todo_read function
        let result = todo_read_from_base_path(session_id, temp_path.to_str().unwrap());

        // Check the result
        assert!(result.is_ok());
        let read_todo_list = result.unwrap();
        assert_eq!(read_todo_list.todos.len(), 2);
        assert_eq!(read_todo_list.todos[0].id, "task-1");
        assert_eq!(read_todo_list.todos[0].content, "Implement feature A");
        assert_eq!(read_todo_list.todos[0].status, "pending");
        assert_eq!(read_todo_list.todos[1].id, "task-2");
        assert_eq!(read_todo_list.todos[1].content, "Fix bug B");
        assert_eq!(read_todo_list.todos[1].status, "in_progress");
    }

    #[test]
    fn test_todo_read_file_not_found() {
        // Create a temporary directory for testing
        let temp_dir = tempdir().unwrap();
        let _temp_path = temp_dir.path(); // Prefix with underscore to indicate it's intentionally unused

        // Set up a mock session ID
        let session_id = "non-existent-session-id";

        // Call todo_read function
        let result = todo_read_from_base_path(session_id, temp_dir.path().to_str().unwrap());

        // Check the result
        assert!(result.is_ok());
        let read_todo_list = result.unwrap();
        assert_eq!(read_todo_list.todos.len(), 0);
    }
}
