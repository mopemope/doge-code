use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::Path;

const DESCRIPTION: &str = r#"
Use this tool to create and manage a structured task list for your current coding session. This helps you track progress, organize complex tasks, and demonstrate thoroughness to the user.
It also helps the user understand the progress of the task and overall progress of their requests.
## When to Use This Tool
Use this tool proactively in these scenarios:
1. Complex multi-step tasks - When a task requires 3 or more distinct steps or actions
2. Non-trivial and complex tasks - Tasks that require careful planning or multiple operations
3. User explicitly requests todo list - When the user directly asks you to use the todo list
4. User provides multiple tasks - When users provide a list of things to be done (numbered or comma-separated)
5. After receiving new instructions - Immediately capture user requirements as todos
6. When you start working on a task - Mark it as in_progress BEFORE beginning work. Ideally you should only have one todo as in_progress at a time
7. After completing a task - Mark it as completed and add any new follow-up tasks discovered during implementation
## When NOT to Use This Tool
Skip using this tool when:
1. There is only a single, straightforward task
2. The task is trivial and tracking it provides no organizational benefit
3. The task can be completed in less than 3 trivial steps
4. The task is purely conversational or informational
NOTE that you should not use this tool if there is only one trivial task to do. In this case you are better off just doing the task directly.
## Examples of When to Use the Todo List
<example>
User: I want to add a dark mode toggle to the application settings. Make sure you run the tests and build when you're done!
Assistant: I'll help add a dark mode toggle to your application settings. Let me create a todo list to track this implementation.
*Creates todo list with the following items:*
1. Create dark mode toggle component in Settings page
2. Add dark mode state management (context/store)
3. Implement CSS-in-JS styles for dark theme
4. Update existing components to support theme switching
5. Run tests and build process, addressing any failures or errors that occur
*Begins working on the first task*
<reasoning>
The assistant used the todo list because:
1. Adding dark mode is a multi-step feature requiring UI, state management, and styling changes
2. The user explicitly requested tests and build be run afterward
3. The assistant inferred that tests and build need to pass by adding "Ensure tests and build succeed" as the final task
</reasoning>
</example>
<example>
User: Help me rename the function getCwd to getCurrentWorkingDirectory across my project
Assistant: Let me first search through your codebase to find all occurrences of 'getCwd'.
*Uses grep or search tools to locate all instances of getCwd in the codebase*
Assistant: I've found 15 instances of 'getCwd' across 8 different files. Let me create a todo list to track these changes.
*Creates todo list with specific items for each file that needs updating*
<reasoning>
The assistant used the todo list because:
1. First, the assistant searched to understand the scope of the task
2. Upon finding multiple occurrences across different files, it determined this was a complex task with multiple steps
3. The todo list helps ensure every instance is tracked and updated systematically
4. This approach prevents missing any occurrences and maintains code consistency
</reasoning>
</example>
<example>
User: I need to implement these features for my e-commerce site: user registration, product catalog, shopping cart, and checkout flow.
Assistant: I'll help implement these features. First, let's add all the features to the todo list.
*Creates a todo list breaking down each feature into specific tasks based on the project architecture*
Assistant: Let's start with user registration. This will involve creating database models, API endpoints, and frontend forms that integrate with your existing authentication system.
<reasoning>
The assistant used the todo list because:
1. The user provided multiple complex features to implement in a comma separated list
2. The todo list helps organize these large features into manageable tasks
3. This approach allows for tracking progress across the entire implementation
</reasoning>
</example>
<example>
User: Can you help optimize my React application? It's rendering slowly and has performance issues.</user>
Assistant: I'll help optimize your React application. First, let me examine your codebase to identify potential performance bottlenecks.
*Reviews component structure, render patterns, state management, and data fetching*
Assistant: After analyzing your codebase, I've identified several performance issues. Let me create a todo list to track our optimization efforts.
*Creates todo list with items like: 1) Implement memoization for expensive calculations in ProductList, 2) Add virtualization for long lists in Dashboard, 3) Optimize image loading in Gallery component, 4) Fix state update loops in ShoppingCart, 5) Review bundle size and implement code splitting*
Let's start by implementing memoization for the expensive calculations in your ProductList component.</assistant>
<reasoning>
The assistant used the todo list because:
1. First, the assistant examined the codebase to identify specific performance issues
2. Based on this analysis, it identified multiple optimization opportunities
3. Performance optimization is a non-trivial task requiring multiple steps
4. The todo list helps methodically track improvements across different components
5. This systematic approach ensures all performance bottlenecks are addressed
</reasoning>
</example>
## Examples of When NOT to Use the Todo List
<example>
User: How do I print 'Hello World' in Python?
Assistant: In Python, you can print "Hello World" with this simple code:
python
print("Hello World")
This will output the text "Hello World" to the console when executed.</assistant>
<reasoning>
The assistant did not use the todo list because this is a single, trivial task that can be completed in one step. There's no need to track multiple tasks or steps for such a straightforward request.
</reasoning>
</example>
<example>
User: What does the git status command do?
Assistant: The git status command shows the current state of your working directory and staging area. It displays which changes have been staged, which haven't, and which files aren't being tracked by Git. This is a fundamental command for understanding the current state of your repository before making commits.
<reasoning>
The assistant did not use the todo list because this is an informational request with no actual coding task to complete. The user is simply asking for an explanation, not for the assistant to perform multiple steps or tasks.
</reasoning>
</example>
<example>
User: Can you add a comment to the calculateTotal function to explain what it does?
Assistant: Sure, let me add a comment to the calculateTotal function to explain what it does.
* Uses the Edit tool to add a comment to the calculateTotal function *
<reasoning>
The assistant did not use the todo list because this is a single, straightforward task confined to one location in the code. Adding a comment doesn't require tracking multiple steps or systematic organization.
</reasoning>
</example>
<example>
User: Run npm install for me and tell me what happens.
Assistant: I'll run the npm install command for you.
*Executes: npm install*
The command completed successfully. Here's the output:
[Output of npm install command]
All dependencies have been installed according to your package.json file.
<reasoning>
The assistant did not use the todo list because this is a single command execution with immediate results. There are no multiple steps to track or organize, making the todo list unnecessary for this straightforward task.
</reasoning>
</example>
## Task States and Management
1. **Task States**: Use these states to track progress:
   - pending: Task not yet started
   - in_progress: Currently working on (limit to ONE task at a time)
   - completed: Task finished successfully
2. **Task Management**:
   - Update task status in real-time as you work
   - Mark tasks complete IMMEDIATELY after finishing (don't batch completions)
   - Only have ONE task in_progress at any time
   - Complete current tasks before starting new ones
   - Remove tasks that are no longer relevant from the list entirely
3. **Task Completion Requirements**:
   - ONLY mark a task as completed when you have FULLY accomplished it
   - If you encounter errors, blockers, or cannot finish, keep the task as in_progress
   - When blocked, create a new task describing what needs to be resolved
   - Never mark a task as completed if:
     - Tests are failing
     - Implementation is partial
     - You encountered unresolved errors
     - You couldn't find necessary files or dependencies
4. **Task Breakdown**:
   - Create specific, actionable items
   - Break complex tasks into smaller, manageable steps
   - Use clear, descriptive task names
When in doubt, use this tool. Being proactive with task management demonstrates attentiveness and ensures you complete all requirements successfully.
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String, // pending, in_progress, completed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoList {
    // session_id is optional to remain compatible with older files that
    // contained only the `todos` field.
    session_id: Option<String>,
    todos: Vec<TodoItem>,
}

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "todo_write".to_string(),
            description: DESCRIPTION.to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                  "type": "string",
                                  "minLength": 1,
                                },
                                "id": {"type": "string"},
                                "content": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                            },
                            "required": ["id", "content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        },
    }
}

/// Write or update the todo list file for the given session.
///
/// This function will create `.doge/todos/<session_id>.json` if it doesn't
/// exist. If it does exist the function will update existing todo items by
/// matching on `id`. Items that do not exist will be appended.
pub fn todo_write(todos: Vec<TodoItem>, session_id: &str) -> Result<String> {
    todo_write_from_base_path(todos, session_id, ".")
}

/// Helper that allows tests to specify a base path.
pub fn todo_write_from_base_path(
    todos: Vec<TodoItem>,
    session_id: &str,
    base_path: &str,
) -> Result<String> {
    // Define the todo file path
    let todo_dir = Path::new(base_path).join(".doge").join("todos");
    let todo_file_path = todo_dir.join(format!("{}.json", session_id));

    // Create the todo directory if it doesn't exist
    fs::create_dir_all(&todo_dir)
        .with_context(|| format!("Failed to create todo directory: {}", todo_dir.display()))?;

    // Load existing todos if the file exists
    let mut existing = if todo_file_path.exists() {
        let content = fs::read_to_string(&todo_file_path)
            .with_context(|| format!("Failed to read todo file: {}", todo_file_path.display()))?;

        // Try to parse the existing file. If parsing fails we'll fallback to an
        // empty list so we don't block the user's request.
        match serde_json::from_str::<TodoList>(&content) {
            Ok(mut tl) => {
                // Ensure session_id is set
                if tl.session_id.is_none() {
                    tl.session_id = Some(session_id.to_string());
                }
                tl
            }
            Err(_) => {
                // Try to salvage if the file contains an object with `todos` field
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(todos_val) = v.get("todos") {
                        if let Ok(parsed_todos) =
                            serde_json::from_value::<Vec<TodoItem>>(todos_val.clone())
                        {
                            TodoList {
                                session_id: Some(session_id.to_string()),
                                todos: parsed_todos,
                            }
                        } else {
                            TodoList {
                                session_id: Some(session_id.to_string()),
                                todos: vec![],
                            }
                        }
                    } else {
                        TodoList {
                            session_id: Some(session_id.to_string()),
                            todos: vec![],
                        }
                    }
                } else {
                    TodoList {
                        session_id: Some(session_id.to_string()),
                        todos: vec![],
                    }
                }
            }
        }
    } else {
        TodoList {
            session_id: Some(session_id.to_string()),
            todos: vec![],
        }
    };

    // Merge incoming todos: update items with matching id, otherwise append.
    for new_item in todos.into_iter() {
        if let Some(pos) = existing.todos.iter().position(|t| t.id == new_item.id) {
            existing.todos[pos] = new_item;
        } else {
            existing.todos.push(new_item);
        }
    }

    // Ensure session_id is set to the current session
    existing.session_id = Some(session_id.to_string());

    // Serialize the todo list to JSON
    let json_content = serde_json::to_string_pretty(&existing)
        .with_context(|| "Failed to serialize todo list to JSON")?;

    // Write the JSON content to the file
    fs::write(&todo_file_path, &json_content).with_context(|| {
        format!(
            "Failed to write todo list to file: {}",
            todo_file_path.display()
        )
    })?;

    Ok(json_content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_todo_write_create_and_update() {
        // Create a temporary directory for testing
        let temp_dir = tempdir().unwrap();
        let base = temp_dir.path().to_str().unwrap();
        let session_id = "test-session";

        // Initial todos
        let initial = vec![
            TodoItem {
                id: "1".to_string(),
                content: "First".to_string(),
                status: "pending".to_string(),
            },
            TodoItem {
                id: "2".to_string(),
                content: "Second".to_string(),
                status: "pending".to_string(),
            },
        ];

        // Create file
        todo_write_from_base_path(initial.clone(), session_id, base).unwrap();

        let todo_file_path = std::path::Path::new(base)
            .join(".doge")
            .join("todos")
            .join(format!("{}.json", session_id));
        assert!(todo_file_path.exists());

        let content = fs::read_to_string(&todo_file_path).unwrap();
        let read: TodoList = serde_json::from_str(&content).unwrap();
        assert_eq!(read.todos.len(), 2);
        assert_eq!(read.session_id.as_deref(), Some(session_id));
        assert_eq!(read.todos[0].id, "1");
        assert_eq!(read.todos[0].content, "First");
        assert_eq!(read.todos[0].status, "pending");

        // Update: modify id 1 and append id 3
        let update = vec![
            TodoItem {
                id: "1".to_string(),
                content: "First updated".to_string(),
                status: "completed".to_string(),
            },
            TodoItem {
                id: "3".to_string(),
                content: "Third".to_string(),
                status: "pending".to_string(),
            },
        ];

        todo_write_from_base_path(update.clone(), session_id, base).unwrap();

        let content2 = fs::read_to_string(&todo_file_path).unwrap();
        let read2: TodoList = serde_json::from_str(&content2).unwrap();
        assert_eq!(read2.todos.len(), 3);

        let t1 = read2.todos.iter().find(|t| t.id == "1").unwrap();
        assert_eq!(t1.content, "First updated");
        assert_eq!(t1.status, "completed");

        let t2 = read2.todos.iter().find(|t| t.id == "2").unwrap();
        assert_eq!(t2.content, "Second");
        assert_eq!(t2.status, "pending");

        let t3 = read2.todos.iter().find(|t| t.id == "3").unwrap();
        assert_eq!(t3.content, "Third");
        assert_eq!(t3.status, "pending");
    }
}
