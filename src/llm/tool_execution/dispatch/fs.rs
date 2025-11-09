use crate::llm::tool_runtime::ToolRuntime;
use crate::tools::list::{FsListMode, FsListOptions};
use crate::tools::read::{FsReadMode, FsReadOptions};
use crate::tools::read_many::FsReadManyOptions;
use anyhow::{Result, anyhow};
use serde_json::json;

pub async fn fs_list(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
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
        Ok(files) => Ok(json!({ "ok": true, "result": files })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn fs_read(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let start_line = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let cursor = args
        .get("cursor")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let page_size = args
        .get("page_size")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let response_budget_chars = args
        .get("response_budget_chars")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let options = FsReadOptions {
        start_line,
        limit,
        cursor,
        page_size,
        response_budget_chars,
        mode: FsReadMode::from_optional_str(args.get("mode").and_then(|v| v.as_str())),
    };

    match runtime.fs.fs_read(path, options) {
        Ok(result) => Ok(json!({ "ok": true, "result": result })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn search_text(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let search_pattern = args
        .get("search_pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let file_glob = args.get("file_glob").and_then(|v| v.as_str());
    match runtime.fs.search_text(search_pattern, file_glob) {
        Ok(rows) => {
            let items: Vec<_> = rows
                .into_iter()
                .map(|(p, ln, text)| {
                    json!({
                        "path": p.display().to_string(),
                        "line": ln,
                        "text": text,
                    })
                })
                .collect();
            Ok(json!({ "ok": true, "results": items }))
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn fs_write(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    match runtime.fs.fs_write(path, content) {
        Ok(()) => Ok(json!({ "ok": true, "path": path, "bytesWritten": content.len() })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn find_file(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let args = serde_json::from_value::<crate::tools::find_file::FindFileArgs>(args.clone())?;
    match runtime.fs.find_file(&args.filename).await {
        Ok(res) => Ok(serde_json::to_value(res)?),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn fs_read_many_files(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
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
        snippet_max_chars: args
            .get("snippet_max_chars")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
    };

    match runtime
        .fs
        .fs_read_many_files(paths, exclude, recursive, options)
    {
        Ok(result) => Ok(json!({ "ok": true, "result": result })),
        Err(e) => Err(anyhow!("{e}")),
    }
}
