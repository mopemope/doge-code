use crate::llm::tool_execution::dispatch::ToolOutput;
use crate::llm::tool_runtime::ToolRuntime;
use crate::tools::list::{FsListMode, FsListOptions};
use crate::tools::read::{FsReadMode, FsReadOptions};
use crate::tools::read_many::FsReadManyOptions;
use anyhow::{Result, anyhow};
use serde_json::json;

pub async fn fs_list(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let max_depth = args
        .get("max_depth")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let pattern = args.get("pattern").and_then(|v| v.as_str());
    let options = FsListOptions {
        mode: FsListMode::from_optional_str(args.get("mode").and_then(|v| v.as_str())),
        cursor: args
            .get("cursor")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
        page_size: args
            .get("page_size")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
        max_entries: args
            .get("max_entries")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
        response_budget_chars: args
            .get("response_budget_chars")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
    };

    match runtime.fs.fs_list(path, max_depth, pattern, options) {
        Ok(files) => {
            let value = json!({ "ok": true, "result": files });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: serde_json::to_string(&files).unwrap_or_default(),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn fs_read(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let start_line = read_usize(args, "start_line", false)?;
    let limit = read_usize(args, "limit", false)?;
    let cursor = read_usize(args, "cursor", false)?;
    let page_size = read_usize(args, "page_size", false)?;
    let response_budget_chars = read_usize(args, "response_budget_chars", false)?;
    let options = FsReadOptions {
        start_line,
        limit,
        cursor,
        page_size,
        response_budget_chars,
        mode: FsReadMode::from_optional_str(args.get("mode").and_then(|v| v.as_str())),
    };

    match runtime
        .fs
        .fs_read_async(
            path.to_owned(),
            options,
            runtime.cancel_token.clone().unwrap_or_default(),
        )
        .await
    {
        Ok(result) => {
            let value = json!({ "ok": true, "result": result });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                // Truncate content for summary if too long, or just use length
                result_summary: format!("Read {} bytes from {}", result.content.len(), path),
            })
        }
        Err(e) => Err(
            if matches!(
                e.downcast_ref::<crate::llm::LlmErrorKind>(),
                Some(crate::llm::LlmErrorKind::Cancelled)
            ) {
                e
            } else {
                crate::tools::budget::bounded_read_error(e)
            },
        ),
    }
}

pub async fn search_text(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let search_pattern = args
        .get("search_pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let file_glob = args.get("file_glob").and_then(|v| v.as_str());
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let options = crate::tools::search_text::SearchTextOptions {
        max_results,
        offset,
        response_budget_chars: args
            .get("response_budget_chars")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
    };
    match runtime
        .fs
        .search_text_with_options_async(
            search_pattern.to_owned(),
            file_glob.map(str::to_owned),
            options,
            runtime.cancel_token.clone().unwrap_or_default(),
        )
        .await
    {
        Ok(result) => {
            let truncated = result.truncated;
            let next_offset = result.next_offset;
            let effective_offset = result.offset;
            let effective_max_results = result.max_results;
            let warnings = result.warnings;
            let items: Vec<_> = result
                .rows
                .into_iter()
                .map(|(p, ln, text)| {
                    json!({
                        "path": p.display().to_string(),
                        "line": ln,
                        "text": text,
                    })
                })
                .collect();
            let value = json!({
                "ok": true,
                "results": items,
                "meta": {
                    "offset": effective_offset,
                    "max_results": effective_max_results,
                    "returned": items.len(),
                    "truncated": truncated,
                    "next_offset": next_offset
                },
                "warnings": warnings
            });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: if truncated {
                    format!(
                        "Found matches for '{}': returned {} (truncated, next_offset={:?})",
                        search_pattern,
                        items.len(),
                        next_offset
                    )
                } else {
                    format!("Found {} matches for '{}'", items.len(), search_pattern)
                },
            })
        }
        Err(e) => Err(e),
    }
}

pub async fn fs_write(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let params: crate::tools::write::FsWriteArgs = serde_json::from_value(args.clone())?;
    let path = params.path.as_str();
    let content = params.content.as_str();
    match runtime
        .fs
        .fs_write_with_attribution(path, content, &runtime.attribution)
        .await
    {
        Ok(res) => {
            let value = json!({
                "ok": true,
                "success": res.success,
                "changed": res.changed,
                "path": path,
                "bytesWritten": res.bytes_written,
                "message": res.message,
                "warnings": res.warnings,
            });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: res.success,
                result_summary: if res.changed {
                    format!("Wrote {} bytes to {}", res.bytes_written, path)
                } else {
                    format!("No change needed for {}", path)
                },
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn find_file(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let options = serde_json::from_value::<crate::tools::find_file::FindFileOptions>(args.clone())?;
    let args = serde_json::from_value::<crate::tools::find_file::FindFileArgs>(args.clone())?;
    match runtime
        .fs
        .find_file_with_options(&args.filename, options)
        .await
    {
        Ok(res) => {
            let value = serde_json::to_value(&res)?;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!(
                    "Found {} of {} files{}",
                    res.returned,
                    res.total_matches,
                    if res.truncated {
                        " (more available)"
                    } else {
                        ""
                    }
                ),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn fs_read_many_files(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let paths = args
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let exclude = args.get("exclude").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>()
    });
    let recursive = args.get("recursive").and_then(|v| v.as_bool());
    let options = FsReadManyOptions {
        mode: FsReadMode::from_optional_str(args.get("mode").and_then(|v| v.as_str())),
        cursor: read_usize(args, "cursor", true)?,
        page_size: read_usize(args, "page_size", false)?,
        max_entries: read_usize(args, "max_entries", false)?,
        response_budget_chars: read_usize(args, "response_budget_chars", false)?,
        snippet_max_chars: read_usize(args, "snippet_max_chars", false)?,
    };

    match runtime
        .fs
        .fs_read_many_files_async(
            paths,
            exclude,
            recursive,
            options,
            runtime.cancel_token.clone().unwrap_or_default(),
        )
        .await
    {
        Ok(result) => {
            let value = json!({ "ok": true, "result": result });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Read {} files", result.files.len()),
            })
        }
        Err(e) => Err(
            if matches!(
                e.downcast_ref::<crate::llm::LlmErrorKind>(),
                Some(crate::llm::LlmErrorKind::Cancelled)
            ) {
                e
            } else {
                crate::tools::budget::bounded_read_error(e)
            },
        ),
    }
}

fn read_usize(args: &serde_json::Value, key: &str, allow_zero: bool) -> Result<Option<usize>> {
    let Some(value) = args.get(key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let value = value
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            anyhow!("invalid argument: {key} must be a nonnegative integer fitting usize")
        })?;
    anyhow::ensure!(
        allow_zero || value > 0,
        "invalid argument: {key} must be greater than zero"
    );
    Ok(Some(value))
}
