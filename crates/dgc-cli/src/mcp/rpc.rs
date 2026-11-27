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

pub type Message = Result<Option<Request>, Value>;

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
        // Never interpret a partial or oversized frame as a request.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid MCP frame length",
        ));
    };
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid MCP frame length",
        ));
    }
    Ok(Some(parse(bytes)))
}

fn parse(bytes: &[u8]) -> Message {
    let parse_error = || Error::new(PARSE_ERROR, "Parse error").response(Value::Null);
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
    // The server only handles requests. In particular, a notification cannot
    // invoke a tool even if it names the tools/call method.
    let Some(id) = message.remove("id") else {
        return Ok(None);
    };
    if !(id.is_string() || id.is_i64() || id.is_u64()) {
        return Err(invalid_request());
    }
    let params = message.remove("params").unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Err(Error::new(INVALID_PARAMS, "Invalid params").response(id));
    }
    Ok(Some(Request { id, method, params }))
}

pub fn write(output: &mut impl Write, message: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, message)?;
    output.write_all(b"\n")?;
    output.flush()
}
