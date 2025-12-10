//! Tool Execution Module
//! This module handles the execution of tools called by the LLM agent.

use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ToolCall};
use crate::tools::FsTools;
use anyhow::{Context, Result};
use std::sync::mpsc;
use tracing::{debug, error, info};

/// Processes tool calls from the LLM and executes them.
/// This function handles the tool execution workflow and updates the message history.
pub async fn process_tool_calls(
    msg: &ChatMessage,
    fs: &FsTools,
    messages: &mut Vec<ChatMessage>,
    ui_tx: &Option<mpsc::Sender<String>>,
    file_was_written: &mut bool,
) -> Result<()> {
    debug!(tool_calls = ?msg.tool_calls, "Processing tool calls");
    
    // Add assistant message with tool calls to history
    messages.push(ChatMessage {
        role: "assistant".into(),
        content: msg.content.clone(),
        tool_calls: msg.tool_calls.clone(),
        tool_call_id: None,
    });

    // Send intermediate content to UI if available
    if let Some(content) = &msg.content {
        if !content.is_empty() {
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::status:thinking:{}", content));
            }
        }
    }

    // Execute each tool call
    let runtime = ToolRuntime::build(fs).await?;
    
    for tool_call in &msg.tool_calls {
        debug!(tool_call = ?tool_call, "Executing tool call");
        
        // Send tool execution status to UI
        if let Some(tx) = ui_tx {
            let _ = tx.send(format!("::status:tool:{}", tool_call.function.name));
        }

        // Execute the tool
        match runtime.execute_tool(tool_call).await {
            Ok(tool_response) => {
                info!(tool_response = ?tool_response, "Tool executed successfully");
                
                // Add tool response to message history
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(tool_response),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tool_call.id.clone()),
                });

                // Check if file was written
                if tool_call.function.name == "fs_write" {
                    *file_was_written = true;
                }
            }
            Err(e) => {
                error!(error = %e, "Tool execution failed");
                
                // Add error response to message history
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(format!("Error executing tool: {}", e)),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tool_call.id.clone()),
                });

                return Err(e).context("Tool execution failed");
            }
        }
    }

    Ok(())
}

/// Completes the agent loop by handling the final response and diff review.
pub async fn complete_agent_loop(
    mut messages: Vec<ChatMessage>,
    msg: ChatMessage,
    cfg: &crate::config::AppConfig,
    ui_tx: &Option<mpsc::Sender<String>>,
    file_was_written: bool,
) -> Result<(Vec<ChatMessage>, ChatMessage)> {
    // Send final assistant content to UI if present
    if let Some(content) = &msg.content {
        if !content.is_empty() {
            if let Some(tx) = ui_tx {
                debug!(response_content = ?content, "Sending LLM response content (final)");
                let _ = tx.send(format!("::status:done:{}", content));
            }
        }
    }

    // Add final assistant message to history
    messages.push(ChatMessage {
        role: "assistant".into(),
        content: msg.content.clone(),
        tool_calls: msg.tool_calls.clone(),
        tool_call_id: None,
    });

    Ok((messages, msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_process_tool_calls() {
        // Test tool call processing
        // This would need mocking and proper setup
    }

    #[test]
    fn test_complete_agent_loop() {
        // Test agent loop completion
        // This would need mocking and proper setup
    }
}