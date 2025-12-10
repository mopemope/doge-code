use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use std::collections::VecDeque;
use std::path::PathBuf;

const UNDO_STACK_CAPACITY: usize = 20;

#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct UndoStack {
    stack: VecDeque<BackupEntry>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self {
            stack: VecDeque::with_capacity(UNDO_STACK_CAPACITY),
        }
    }

    pub fn push(&mut self, path: PathBuf, content: String) {
        if self.stack.len() >= UNDO_STACK_CAPACITY {
            self.stack.pop_front();
        }
        self.stack.push_back(BackupEntry { path, content });
    }

    pub fn pop(&mut self) -> Option<BackupEntry> {
        self.stack.pop_back()
    }
}

pub fn undo_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "undo".to_string(),
            description: "Revert the last file modification (edit or write). Use this immediately if a change broke the code or was incorrect. It restores the file to the state before the LAST tool call.".to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
    }
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct UndoResult {
    pub path: String,
    pub message: String,
}

pub async fn undo(
    fs_tools: &crate::tools::FsTools, // Circular dep avoidance: Pass FsTools ref or decouple? FsTools holds UndoStack.
) -> Result<UndoResult> {
    let mut stack = fs_tools.undo_stack.write().await;
    match stack.pop() {
        Some(entry) => {
            // Restore file content
            tokio::fs::write(&entry.path, &entry.content).await?;
            Ok(UndoResult {
                path: entry.path.to_string_lossy().to_string(),
                message: format!(
                    "Successfully restored {} to previous version.",
                    entry.path.display()
                ),
            })
        }
        None => {
            // No backup found
            Ok(UndoResult {
                path: "".to_string(),
                message: "Undo stack is empty. Cannot revert any more changes.".to_string(),
            })
        }
    }
}
