use crate::llm::LlmErrorKind;
use crate::llm::stream_tools::{ToolDeltaBuffer, execute_tool_call};
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ChoiceMessage};
use crate::session::SessionManager;
use crate::tools::FsTools;
use anyhow::{Result, anyhow};
use futures::StreamExt;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::warn;

pub async fn run_agent_streaming_once(
    client: &crate::llm::client_core::OpenAIClient,
    model: &str,
    fs: &FsTools,
    mut messages: Vec<ChatMessage>,
    cancel: Option<CancellationToken>,
    session_manager: Option<Arc<Mutex<SessionManager>>>,
) -> Result<(Vec<ChatMessage>, Option<ChoiceMessage>)> {
    let cancel_token = cancel.unwrap_or_default();

    // Start stream
    let mut stream = client
        .chat_stream(model, messages.clone(), Some(cancel_token.clone()))
        .await?;
    let runtime = ToolRuntime::build(fs).await?;
    let mut buf = ToolDeltaBuffer::new();
    let mut acc_text = String::new();

    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                warn!("run_agent_streaming_once cancelled");
                return Err(anyhow!(LlmErrorKind::Cancelled));
            }
            chunk = stream.next() => chunk,
        };

        match chunk {
            Some(Ok(delta)) => {
                if delta.is_empty() {
                    break;
                }
                if let Some(rest) = delta.strip_prefix("__TOOL_CALLS_DELTA__:") {
                    // Parse synthetic tool_calls marker and feed into buffer.
                    if let Ok(deltas) =
                        serde_json::from_str::<Vec<crate::llm::stream::ToolCallDelta>>(rest)
                    {
                        for d in deltas {
                            let idx = d.index.unwrap_or(0);
                            let (name_delta, args_delta) = if let Some(f) = d.function {
                                (Some(f.name), Some(f.arguments))
                            } else {
                                (None, None)
                            };
                            buf.push_delta(idx, name_delta.as_deref(), args_delta.as_deref(), None);
                            // Try finalize and execute immediately when JSON becomes valid
                            if buf.finalize_sync_call(idx).is_ok() {
                                // Update session with tool call count if session manager is available
                                if let Some(ref sm) = session_manager {
                                    let mut session_mgr = sm.lock().unwrap();
                                    if let Err(e) =
                                        session_mgr.update_current_session_with_tool_call_count()
                                    {
                                        tracing::error!(
                                            ?e,
                                            "Failed to update session with tool call count"
                                        );
                                    }
                                }

                                let exec = execute_tool_call(&runtime, idx, &buf).await;
                                if let Ok(val) = exec {
                                    messages.push(ChatMessage {
                                        role: "tool".into(),
                                        content: Some(
                                            serde_json::to_string(&val)
                                                .unwrap_or_else(|_| "{\"ok\":false}".to_string()),
                                        ),
                                        tool_calls: vec![],
                                        tool_call_id: None,
                                    });
                                    // One tool exec per streaming round (minimal policy)
                                    return Ok((messages, None));
                                }
                            }
                        }
                    }
                } else {
                    // Accumulate plain text
                    acc_text.push_str(&delta);
                }
            }
            Some(Err(e)) => {
                if let Some(kind) = e.downcast_ref::<LlmErrorKind>()
                    && kind == &LlmErrorKind::Cancelled
                {
                    warn!("run_agent_streaming_once stream cancelled");
                    return Err(anyhow!(LlmErrorKind::Cancelled));
                }
                return Err(e);
            }
            None => break, // End of stream
        }
    }

    // If we have accumulated content and no tool call executed, return it as assistant message
    if !acc_text.is_empty() {
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some(acc_text.clone()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        return Ok((
            messages,
            Some(ChoiceMessage {
                role: "assistant".into(),
                content: acc_text,
            }),
        ));
    }

    Ok((messages, None))
}
