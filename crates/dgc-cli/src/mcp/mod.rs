//! The stdio MCP server attached to one agent by the daemon.

mod rpc;
mod tools;

use std::io;
use std::process::ExitCode;

use dgc_protocol::ids::AgentId;
use dgc_protocol::rpc::{INVALID_PARAMS, METHOD_NOT_FOUND};
use serde_json::{Value, json};

use crate::commands::CliResult;

const SUPPORTED_VERSIONS: [&str; 2] = ["2025-11-25", "2025-06-18"];

pub async fn run(caller: AgentId) -> CliResult<ExitCode> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut line = Vec::new();
    while let Some(message) = rpc::read(&mut input, &mut line)? {
        let response = match message {
            Ok(Some(request)) => {
                let result = dispatch(&caller, &request.method, request.params).await;
                match result {
                    Ok(value) => rpc::success(request.id, value),
                    Err(error) => error.response(request.id),
                }
            }
            Ok(None) => continue,
            Err(response) => response,
        };
        rpc::write(&mut output, &response)?;
    }
    Ok(ExitCode::SUCCESS)
}

async fn dispatch(caller: &AgentId, method: &str, params: Value) -> Result<Value, rpc::Error> {
    match method {
        "initialize" => {
            let requested = params["protocolVersion"]
                .as_str()
                .ok_or(rpc::Error::new(INVALID_PARAMS, "Invalid params"))?;
            let version = SUPPORTED_VERSIONS
                .into_iter()
                .find(|&version| version == requested)
                .unwrap_or(SUPPORTED_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "dgc", "version": env!("CARGO_PKG_VERSION") }
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::list()),
        "tools/call" => Ok(tools::call(caller, &params).await),
        _ => Err(rpc::Error::new(METHOD_NOT_FOUND, "Method not found")),
    }
}
