use serde::Deserialize;
use serde_json::Value;

/// Only Completed authorizes projecting calls into executable local tools.
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum InferenceEvent {
    #[serde(rename = "response.completed")]
    Completed { response: Value },
    #[serde(rename = "response.failed")]
    Failed { response: Value },
    #[serde(rename = "response.incomplete")]
    Incomplete,
    #[serde(rename = "error")]
    Error {
        #[serde(flatten)]
        body: serde_json::Map<String, Value>,
    },
    #[serde(rename = "response.output_text.delta")]
    TextDelta,
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionArgumentsDelta,
    #[serde(
        rename = "response.output_item.added",
        alias = "response.output_item.done"
    )]
    OutputItem,
    #[serde(other)]
    Other,
}

use anyhow::{Result, bail};

const MAX_FRAME: usize = 4 * 1024 * 1024;
/// Byte-oriented SSE framing; UTF-8 is decoded only after a complete frame.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    data: Vec<String>,
    frame_bytes: usize,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<serde_json::Value>> {
        let mut events = Vec::new();
        for byte in bytes {
            self.pending.push(*byte);
            self.frame_bytes += 1;
            if self.frame_bytes > MAX_FRAME {
                bail!("Responses SSE frame exceeds size limit");
            }
            if *byte != b'\n' {
                continue;
            }
            self.pending.pop();
            if self.pending.last() == Some(&b'\r') {
                self.pending.pop();
            }
            let line = std::str::from_utf8(&self.pending)
                .map_err(|_| anyhow::anyhow!("invalid SSE UTF-8"))?;
            if line.is_empty() {
                if !self.data.is_empty() {
                    let data = self.data.join("\n");
                    if data != "[DONE]" {
                        events.push(
                            serde_json::from_str(&data)
                                .map_err(|_| anyhow::anyhow!("invalid Responses SSE event"))?,
                        );
                    }
                    self.data.clear();
                }
                self.frame_bytes = 0;
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            }
            self.pending.clear();
        }
        Ok(events)
    }
}
