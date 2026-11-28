//! The stdio MCP server attached to one agent by the daemon.

mod rpc;
mod tools;

use std::collections::{HashMap, hash_map::Entry};
use std::io;
use std::process::ExitCode;

use dgc_protocol::ids::AgentId;
use dgc_protocol::rpc::{INTERNAL, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::commands::CliResult;

const SUPPORTED_VERSIONS: [&str; 2] = ["2025-11-25", "2025-06-18"];

pub async fn run(caller: AgentId) -> CliResult<ExitCode> {
    // This subcommand owns the process. Keep panic payloads out of stderr.
    std::panic::set_hook(Box::new(|_| {}));
    let (send, input) = mpsc::channel(8);
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

    serve(caller, input, io::stdout().lock()).await
}

async fn serve(
    caller: AgentId,
    mut input: mpsc::Receiver<io::Result<rpc::Message>>,
    mut output: impl io::Write,
) -> CliResult<ExitCode> {
    let mut calls = JoinSet::new();
    let mut pending: HashMap<Value, AbortHandle> = HashMap::new();
    loop {
        let response = tokio::select! {
            message = input.recv() => {
                let Some(message) = message else { break };
                match message? {
                    Ok(Some(rpc::Inbound::Request(request))) if request.method == "tools/call" => {
                        match pending.entry(request.id.clone()) {
                            Entry::Occupied(_) => rpc::Error::new(
                                INVALID_REQUEST,
                                "Request ID in use",
                            ).response(request.id),
                            Entry::Vacant(entry) => {
                                let caller = caller.clone();
                                let task = calls.spawn(async move {
                                    #[cfg(test)]
                                    if request.params["name"] == "panic-test" {
                                        panic!("private test panic payload");
                                    }
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
                        if let Some(task) = pending.remove(&id) {
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
                        if !pending.get(&id).is_some_and(|call| call.id() == task) {
                            continue;
                        }
                        pending.remove(&id);
                        respond(id, result)
                    }
                    Some(Err(error)) => {
                        // A cancelled request is already gone, even if its ID was reused.
                        let request = pending.extract_if(|_, call| call.id() == error.id()).next();
                        match request {
                            Some((id, _)) if error.is_panic() => {
                                rpc::Error::new(INTERNAL, "Internal error").response(id)
                            }
                            _ => continue,
                        }
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

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::time::timeout;

    use super::*;

    #[tokio::test]
    async fn panicked_tool_task_replies_with_internal_error() {
        let (writer, reader) = UnixStream::pair().expect("response socket pair");
        reader.set_nonblocking(true).expect("nonblocking reader");
        let reader = tokio::net::UnixStream::from_std(reader).expect("async reader");
        let mut responses = BufReader::new(reader).lines();
        let (send, input) = mpsc::channel(1);
        let id = json!("request-\"panic\"");
        let client = async {
            let replies = timeout(Duration::from_secs(15), async {
                let mut replies = Vec::new();
                // Reuse exactly the same ID once the panic reply has been sent.
                for tool in ["panic-test", "unknown-test-tool"] {
                    send.send(Ok(Ok(Some(rpc::Inbound::Request(rpc::Request {
                        id: id.clone(),
                        method: "tools/call".to_owned(),
                        params: json!({"name": tool}),
                    })))))
                    .await
                    .expect("server accepting requests");
                    let line = responses
                        .next_line()
                        .await
                        .expect("response read")
                        .expect("response");
                    replies.push(serde_json::from_str::<Value>(&line).expect("JSON response"));
                }
                replies
            })
            .await;
            drop(send);
            replies
        };
        let (server, replies) = tokio::join!(serve(AgentId::generate(), input, writer), client);
        assert_eq!(server.expect("server result"), ExitCode::SUCCESS);
        let replies = replies.expect("tool did not answer its request ID");
        assert_eq!(
            replies[0],
            json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": INTERNAL, "message": "Internal error"}
            })
        );
        assert_eq!(replies[1]["id"], id);
        assert_eq!(replies[1]["error"]["code"], INVALID_PARAMS);
    }
}
