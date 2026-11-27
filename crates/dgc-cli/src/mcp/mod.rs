//! The stdio MCP server attached to one agent by the daemon.

mod rpc;
mod tools;

use std::collections::{HashMap, hash_map::Entry};
use std::io;
use std::process::ExitCode;

use dgc_protocol::ids::AgentId;
use dgc_protocol::rpc::{INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::commands::CliResult;

const SUPPORTED_VERSIONS: [&str; 2] = ["2025-11-25", "2025-06-18"];

pub async fn run(caller: AgentId) -> CliResult<ExitCode> {
    let (send, mut input) = mpsc::channel(8);
    // A native reader thread can stay blocked in a pipe read while the async
    // server shuts down. Tokio's runtime does not have to join that thread.
    std::thread::Builder::new()
        .name("dgc-mcp-stdin".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut line = Vec::new();
            loop {
                let message = match rpc::read(&mut stdin, &mut line) {
                    Ok(Some(message)) => Ok(message),
                    Ok(None) => return,
                    Err(error) => {
                        let _ = send.blocking_send(Err(error));
                        return;
                    }
                };
                if send.blocking_send(message).is_err() {
                    return;
                }
            }
        })?;

    let mut output = io::stdout().lock();
    let mut calls = JoinSet::new();
    let mut pending: HashMap<String, AbortHandle> = HashMap::new();
    loop {
        let response = tokio::select! {
            message = input.recv() => {
                let Some(message) = message else { break };
                match message? {
                    Ok(Some(rpc::Inbound::Request(request))) if request.method == "tools/call" => {
                        let key = request.id.to_string();
                        match pending.entry(key) {
                            Entry::Occupied(_) => rpc::Error::new(
                                INVALID_REQUEST,
                                "Request ID in use",
                            ).response(request.id),
                            Entry::Vacant(entry) => {
                                let caller = caller.clone();
                                let task = calls.spawn(async move {
                                    let result = tools::call(&caller, &request.params).await;
                                    (request.id, result)
                                });
                                entry.insert(task);
                                continue;
                            }
                        }
                    }
                    Ok(Some(rpc::Inbound::Request(request))) => {
                        respond(request.id, dispatch(&request.method, request.params))
                    }
                    Ok(Some(rpc::Inbound::Cancel(id))) => {
                        if let Some(task) = pending.remove(&id.to_string()) {
                            task.abort();
                        }
                        continue;
                    }
                    Ok(None) => continue,
                    Err(response) => response,
                }
            }
            completed = calls.join_next_with_id(), if !calls.is_empty() => {
                match completed {
                    Some(Ok((task, (id, result)))) => {
                        let key = id.to_string();
                        if !pending.get(&key).is_some_and(|call| call.id() == task) {
                            continue;
                        }
                        pending.remove(&key);
                        respond(id, result)
                    }
                    Some(Err(error)) => {
                        pending.retain(|_, call| call.id() != error.id());
                        continue;
                    }
                    None => continue,
                }
            }
        };
        rpc::write(&mut output, &response)?;
    }
    calls.shutdown().await;
    Ok(ExitCode::SUCCESS)
}

fn respond(id: Value, result: Result<Value, rpc::Error>) -> Value {
    match result {
        Ok(value) => rpc::success(id, value),
        Err(error) => error.response(id),
    }
}

fn dispatch(method: &str, params: Value) -> Result<Value, rpc::Error> {
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
        _ => Err(rpc::Error::new(METHOD_NOT_FOUND, "Method not found")),
    }
}
