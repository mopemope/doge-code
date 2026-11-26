//! JSON-RPC style envelope types and error codes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC_VERSION: &str = "2.0";

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL: i32 = -32603;
pub const NOT_FOUND: i32 = -32004;
pub const INVALID_STATE: i32 = -32009;
pub const VERSION_MISMATCH: i32 = -32010;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: RpcError) -> Self {
        Self {
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Server-to-client message with no id and no reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            method: method.into(),
            params,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_roundtrip() {
        let req = Request::new(7, "agent.list", json!({"workspace_id": "ws_x"}));
        let text = serde_json::to_string(&req).expect("serialize");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            json!({"jsonrpc": "2.0", "id": 7, "method": "agent.list", "params": {"workspace_id": "ws_x"}})
        );
        let back: Request = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, req);
    }

    #[test]
    fn request_without_params_defaults_to_null() {
        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"daemon.status"}"#).unwrap();
        assert_eq!(req.params, serde_json::Value::Null);
    }

    #[test]
    fn response_omits_absent_result_or_error() {
        let ok = serde_json::to_value(Response::ok(1, json!({"a": 1}))).unwrap();
        assert_eq!(ok, json!({"id": 1, "result": {"a": 1}}));
        let err =
            serde_json::to_value(Response::err(2, RpcError::new(NOT_FOUND, "missing"))).unwrap();
        assert_eq!(
            err,
            json!({"id": 2, "error": {"code": -32004, "message": "missing"}})
        );
    }

    #[test]
    fn notification_roundtrip() {
        let n = Notification::new("lagged", json!({"next_seq": 3}));
        let back: Notification = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn error_codes_match_spec() {
        assert_eq!(
            [
                PARSE_ERROR,
                INVALID_REQUEST,
                METHOD_NOT_FOUND,
                INVALID_PARAMS,
                INTERNAL
            ],
            [-32700, -32600, -32601, -32602, -32603]
        );
        assert_eq!(
            [NOT_FOUND, INVALID_STATE, VERSION_MISMATCH],
            [-32004, -32009, -32010]
        );
    }
}
