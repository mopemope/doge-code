//! MCP uses one UTF-8 JSON-RPC message per line, without batches.

use std::io::{self, BufRead, Read, Write};

use dgc_protocol::MAX_MESSAGE_BYTES;
use dgc_protocol::rpc::{INVALID_PARAMS, INVALID_REQUEST, JSONRPC_VERSION, PARSE_ERROR};
use serde_json::{Value, json};

pub struct Request {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

pub struct Error {
    code: i32,
    message: &'static str,
}

impl Error {
    pub const fn new(code: i32, message: &'static str) -> Self {
        Self { code, message }
    }

    pub fn response(self, id: Value) -> Value {
        json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "error": { "code": self.code, "message": self.message }
        })
    }
}

pub fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": JSONRPC_VERSION, "id": id, "result": result})
}

pub enum Inbound {
    Request(Request),
    Cancel(Value),
}

pub type Message = Result<Option<Inbound>, Value>;

pub fn read(input: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<Option<Message>> {
    line.clear();
    // Allow the delimiter and an optional carriage return in addition to the message.
    let n = input
        .take(MAX_MESSAGE_BYTES as u64 + 2)
        .read_until(b'\n', line)?;
    if n == 0 {
        return Ok(None);
    }
    let Some(bytes) = line.strip_suffix(b"\n") else {
        // Drain the remaining bytes without allocating a buffer for them.
        input.skip_until(b'\n')?;
        return Ok(Some(Err(parse_error())));
    };
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Ok(Some(Err(parse_error())));
    }
    Ok(Some(parse(bytes)))
}

fn parse_error() -> Value {
    Error::new(PARSE_ERROR, "Parse error").response(Value::Null)
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.is_i64() || id.is_u64()
}

fn parse(bytes: &[u8]) -> Message {
    if bytes.contains(&b'\r') {
        return Err(parse_error());
    }
    let invalid_request = || Error::new(INVALID_REQUEST, "Invalid request").response(Value::Null);
    let Value::Object(mut message) = serde_json::from_slice(bytes).map_err(|_| parse_error())?
    else {
        return Err(invalid_request());
    };
    if message.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Err(invalid_request());
    }
    let Some(Value::String(method)) = message.remove("method") else {
        return Err(invalid_request());
    };
    let params = message.remove("params").unwrap_or_else(|| json!({}));
    let Some(id) = message.remove("id") else {
        return Ok(
            if method == "notifications/cancelled" && valid_id(&params["requestId"]) {
                Some(Inbound::Cancel(params["requestId"].clone()))
            } else {
                None
            },
        );
    };
    if !valid_id(&id) {
        return Err(invalid_request());
    }
    if !params.is_object() {
        return Err(Error::new(INVALID_PARAMS, "Invalid params").response(id));
    }
    Ok(Some(Inbound::Request(Request { id, method, params })))
}

pub fn write(output: &mut impl Write, message: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, message)?;
    output.write_all(b"\n")?;
    output.flush()
}
