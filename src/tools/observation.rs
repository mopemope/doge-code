//! Read-only `observation_read` builtin: retrieve an offloaded tool result
//! from the conversation-owned Observation Store without re-running the
//! original tool.

use crate::llm::observation::{
    MAX_OBSERVATION_READ_CHARS, OBSERVATION_READ_TOOL_NAME, ObservationStore,
};
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: OBSERVATION_READ_TOOL_NAME.to_string(),
            strict: None,
            description: "Retrieve a tool result that history says was offloaded. Use this instead of rerunning the original operation merely to recover its old output.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Observation id, e.g. obs-000001."},
                    "offset": {"type": "integer", "description": "Byte offset into the stored result (default 0)."},
                    "limit": {"type": "integer", "description": "Maximum bytes to return (default 6000, max 6000); JSON escaping and metadata may reduce the page."}
                },
                "required": ["id"]
            }),
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct ObservationReadArgs {
    pub id: String,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Exact targeted retrieval only: one id per call, bounded paging.
/// Never dispatches `read_file`/`search`/`execute`/MCP behind the scenes.
pub fn observation_read(
    store: &ObservationStore,
    args: ObservationReadArgs,
) -> Result<ObservationReadResponse> {
    let id = args.id.trim();
    if id.is_empty() {
        anyhow::bail!("id is required");
    }
    let offset = args.offset.unwrap_or(0);
    let limit = args.limit.unwrap_or(MAX_OBSERVATION_READ_CHARS);
    if limit > MAX_OBSERVATION_READ_CHARS {
        anyhow::bail!("limit exceeds maximum {} bytes", MAX_OBSERVATION_READ_CHARS);
    }
    let page = store
        .read_paged(id, offset, limit)
        .map_err(|e| anyhow!(e.to_string()))?;
    let mut response = ObservationReadResponse::from_page(&page);
    if super::budget::serialized_tool_output_fits(&response, OBSERVATION_READ_TOOL_NAME)? {
        return Ok(response);
    }
    response
        .warnings
        .push("serialized JSON limit applied; request next_cursor for unread bytes".into());
    let content = std::mem::take(&mut response.content);
    let boundaries = std::iter::once(0)
        .chain(
            content
                .char_indices()
                .map(|(index, ch)| index + ch.len_utf8()),
        )
        .collect::<Vec<_>>();
    let mut low = 0;
    let mut high = boundaries.len() - 1;
    // Prefix size is monotone, except at EOF where next_cursor disappears.
    // The full EOF page was already tested; every shorter page keeps a cursor.
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        set_page_prefix(&mut response, &content, boundaries[mid]);
        if super::budget::serialized_tool_output_fits(&response, OBSERVATION_READ_TOOL_NAME)? {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    set_page_prefix(&mut response, &content, boundaries[low]);
    anyhow::ensure!(
        low > 0
            && super::budget::serialized_tool_output_fits(&response, OBSERVATION_READ_TOOL_NAME)?,
        "observation metadata and one character cannot fit the serialized JSON limit; no bytes consumed"
    );
    Ok(response)
}

fn set_page_prefix(response: &mut ObservationReadResponse, content: &str, end: usize) {
    response.content = content[..end].to_owned();
    response.end_byte = response.start_byte + end;
    response.next_cursor = (response.end_byte < response.total_bytes).then_some(response.end_byte);
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ObservationReadResponse {
    pub ok: bool,
    pub id: String,
    pub tool: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub total_bytes: usize,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl ObservationReadResponse {
    pub fn from_page(page: &crate::llm::observation::PagedObservation) -> Self {
        Self {
            ok: true,
            id: page.id.clone(),
            tool: page.tool_name.clone(),
            start_byte: page.start_byte,
            end_byte: page.end_byte,
            total_bytes: page.total_bytes,
            content: page.page.clone(),
            next_cursor: page.next_cursor,
            warnings: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_store() -> (ObservationStore, String) {
        let mut store = ObservationStore::new();
        let content = "αβγ日本語🎉".repeat(200);
        let id = store
            .insert("call-1".into(), "fs_read".into(), content, 100)
            .unwrap();
        (store, id)
    }

    #[test]
    fn test_observation_read_exact_recovery() {
        let (store, id) = seeded_store();
        let original = store.get(&id).unwrap().content.clone();
        // Reassemble through paging.
        let mut out = String::new();
        let mut offset = 0usize;
        loop {
            let res = observation_read(
                &store,
                ObservationReadArgs {
                    id: id.clone(),
                    offset: Some(offset),
                    limit: Some(500),
                },
            )
            .unwrap();
            out.push_str(&res.content);
            match res.next_cursor {
                Some(n) => offset = n,
                None => break,
            }
        }
        assert_eq!(out, original);
    }

    #[test]
    fn observation_read_pages_survive_model_output_budget_losslessly() {
        use crate::llm::truncate_tool_output;
        let content = json!({"ok": true, "result": {"content": "\\".repeat(8_000)}}).to_string();
        let mut store = ObservationStore::new();
        let id = store
            .insert(
                "call".into(),
                "fs_read".into(),
                content.clone(),
                content.len(),
            )
            .unwrap();
        let mut recovered = String::new();
        let mut offset = 0;
        let mut pages = 0;
        loop {
            let page = observation_read(
                &store,
                ObservationReadArgs {
                    id: id.clone(),
                    offset: Some(offset),
                    limit: None,
                },
            )
            .unwrap();
            let serialized = serde_json::to_string(&page).unwrap();
            eprintln!(
                "page={pages}, content_bytes={}, serialized_chars={}, cursor={:?}",
                page.content.len(),
                serialized.chars().count(),
                page.next_cursor
            );
            let visible = truncate_tool_output(serialized.clone(), OBSERVATION_READ_TOOL_NAME);
            assert_eq!(
                visible, serialized,
                "global fallback changed a recoverable page"
            );
            assert_eq!(page.start_byte, offset);
            assert_eq!(page.end_byte, offset + page.content.len());
            recovered.push_str(&page.content);
            pages += 1;
            if let Some(next) = page.next_cursor {
                assert!(next > offset);
                offset = next;
            } else {
                break;
            }
            assert!(pages < 20);
        }
        eprintln!(
            "original_bytes={}, recovered_bytes={}, pages={pages}",
            content.len(),
            recovered.len()
        );
        assert_eq!(recovered, content);
    }

    #[test]
    fn serialized_paging_edge_observation_unicode_tiny_limits_and_eof() {
        for (content, limits) in [
            ("😀\\\u{1}".repeat(2_000), vec![None]),
            (
                "😀\\\u{1}日\"終".to_owned(),
                vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(500)],
            ),
        ] {
            for limit in limits {
                let mut store = ObservationStore::new();
                let id = store
                    .insert(
                        "call".into(),
                        "fs_read".into(),
                        content.clone(),
                        content.len(),
                    )
                    .unwrap();
                let mut out = String::new();
                let mut offset = 0;
                loop {
                    let page = observation_read(
                        &store,
                        ObservationReadArgs {
                            id: id.clone(),
                            offset: Some(offset),
                            limit,
                        },
                    )
                    .unwrap();
                    assert!(
                        super::super::budget::serialized_tool_output_fits(
                            &page,
                            OBSERVATION_READ_TOOL_NAME
                        )
                        .unwrap()
                    );
                    assert_eq!(page.end_byte, page.start_byte + page.content.len());
                    assert!(content.is_char_boundary(page.end_byte));
                    assert_eq!(page.content, content[page.start_byte..page.end_byte]);
                    out.push_str(&page.content);
                    if let Some(next) = page.next_cursor {
                        assert!(next > offset);
                        offset = next;
                    } else {
                        assert_eq!(page.end_byte, content.len());
                        break;
                    }
                }
                assert_eq!(out, content);
                let eof = observation_read(
                    &store,
                    ObservationReadArgs {
                        id: id.clone(),
                        offset: Some(content.len()),
                        limit,
                    },
                )
                .unwrap();
                assert!(eof.content.is_empty() && eof.next_cursor.is_none());
            }
        }
    }

    #[test]
    fn serialized_paging_edge_observation_oversized_metadata_has_no_progress() {
        let mut store = ObservationStore::new();
        let id = store
            .insert("call".into(), "\\".repeat(9_000), "😀".into(), 4)
            .unwrap();
        let error = observation_read(
            &store,
            ObservationReadArgs {
                id: id.clone(),
                offset: None,
                limit: None,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("no bytes consumed"));
        assert_eq!(store.get(&id).unwrap().content, "😀");
    }

    #[test]
    fn test_observation_read_unknown_id_errors_without_fallback() {
        let store = ObservationStore::new();
        let err = observation_read(
            &store,
            ObservationReadArgs {
                id: "obs-999999".into(),
                offset: None,
                limit: None,
            },
        )
        .expect_err("unknown id must error");
        assert!(err.to_string().contains("unknown observation"));
    }

    #[test]
    fn test_observation_read_rejects_unbounded() {
        let (store, id) = seeded_store();
        let err = observation_read(
            &store,
            ObservationReadArgs {
                id,
                offset: None,
                limit: Some(1_000_000),
            },
        )
        .expect_err("over-limit must error");
        assert!(err.to_string().contains("exceeds maximum"));
    }
}
