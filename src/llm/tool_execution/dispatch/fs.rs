use crate::llm::tool_runtime::ToolRuntime;
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
    match runtime.fs.fs_list(path, max_depth, pattern) {
        Ok(files) => Ok(json!({ "ok": true, "files": files })),
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
    match runtime.fs.fs_read(path, start_line, limit) {
        Ok(text) => Ok(json!({ "ok": true, "path": path, "content": text })),
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
    match runtime.fs.fs_read_many_files(paths, exclude, recursive) {
        Ok(content) => Ok(json!({ "ok": true, "content": content })),
        Err(e) => Err(anyhow!("{e}")),
    }
}
