//! NDJSON JSON-RPC 2.0 frames used by ACP over a single `stdio` stream.

use serde_json::{json, Map, Value};

use crate::error::{AcpError, RpcError};

/// JSON-RPC version emitted by every frame.
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC request identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RpcId {
    Number(u64),
    String(String),
}

impl RpcId {
    pub fn to_value(&self) -> Value {
        match self {
            RpcId::Number(number) => json!(number),
            RpcId::String(value) => Value::String(value.clone()),
        }
    }

    pub fn from_value(value: &Value) -> Result<Self, AcpError> {
        match value {
            Value::Number(number) => number
                .as_u64()
                .map(RpcId::Number)
                .ok_or_else(|| AcpError::protocol("JSON-RPC id must be a non-negative integer")),
            Value::String(value) => Ok(RpcId::String(value.clone())),
            _ => Err(AcpError::protocol(
                "JSON-RPC id must be a number or a string",
            )),
        }
    }
}

/// One decoded protocol frame.
#[derive(Clone, Debug)]
pub enum Frame {
    Request {
        id: RpcId,
        method: String,
        params: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    Response {
        id: RpcId,
        result: Value,
    },
    Error {
        id: RpcId,
        error: RpcError,
    },
}

impl Frame {
    /// Parse one newline-delimited frame. `line` must not include the newline.
    pub fn parse(line: &str) -> Result<Self, AcpError> {
        let value: Value = serde_json::from_str(line)
            .map_err(|error| AcpError::protocol(format!("invalid JSON-RPC frame: {error}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| AcpError::protocol("JSON-RPC frame must be an object"))?;

        if let Some(version) = object.get("jsonrpc") {
            if version.as_str() != Some(JSONRPC_VERSION) {
                return Err(AcpError::protocol("unsupported JSON-RPC version"));
            }
        }

        if let Some(method) = object.get("method") {
            let method = method
                .as_str()
                .ok_or_else(|| AcpError::protocol("JSON-RPC method must be a string"))?
                .to_string();
            let params = object.get("params").cloned();
            return match object.get("id") {
                Some(id) => Ok(Frame::Request {
                    id: RpcId::from_value(id)?,
                    method,
                    params,
                }),
                None => Ok(Frame::Notification { method, params }),
            };
        }

        let id = object
            .get("id")
            .ok_or_else(|| AcpError::protocol("JSON-RPC response is missing id"))?;
        let id = RpcId::from_value(id)?;
        if let Some(error) = object.get("error") {
            let error = serde_json::from_value::<RpcError>(error.clone())
                .map_err(|error| AcpError::protocol(format!("invalid JSON-RPC error: {error}")))?;
            return Ok(Frame::Error { id, error });
        }
        let result = object.get("result").cloned().ok_or_else(|| {
            AcpError::protocol("JSON-RPC response carries neither result nor error")
        })?;
        Ok(Frame::Response { id, result })
    }

    /// Encode one frame as a single line without the trailing newline.
    pub fn to_line(&self) -> Result<String, AcpError> {
        let mut object = Map::new();
        object.insert("jsonrpc".into(), json!(JSONRPC_VERSION));
        match self {
            Frame::Request { id, method, params } => {
                object.insert("id".into(), id.to_value());
                object.insert("method".into(), json!(method));
                if let Some(params) = params {
                    object.insert("params".into(), params.clone());
                }
            }
            Frame::Notification { method, params } => {
                object.insert("method".into(), json!(method));
                if let Some(params) = params {
                    object.insert("params".into(), params.clone());
                }
            }
            Frame::Response { id, result } => {
                object.insert("id".into(), id.to_value());
                object.insert("result".into(), result.clone());
            }
            Frame::Error { id, error } => {
                object.insert("id".into(), id.to_value());
                object.insert(
                    "error".into(),
                    serde_json::to_value(error)
                        .map_err(|error| AcpError::protocol(error.to_string()))?,
                );
            }
        }
        serde_json::to_string(&Value::Object(object))
            .map_err(|error| AcpError::protocol(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_notification_response_and_error() {
        let request =
            Frame::parse(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).unwrap();
        assert!(matches!(request, Frame::Request { .. }));

        let notification =
            Frame::parse(r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#).unwrap();
        assert!(matches!(notification, Frame::Notification { .. }));

        let response = Frame::parse(r#"{"jsonrpc":"2.0","id":"a","result":{"ok":true}}"#).unwrap();
        assert!(matches!(response, Frame::Response { .. }));

        let error =
            Frame::parse(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32002,"message":"missing"}}"#)
                .unwrap();
        match error {
            Frame::Error { error, .. } => assert_eq!(error.code, -32002),
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_version_and_shapes() {
        assert!(Frame::parse(r#"{"jsonrpc":"1.0","id":1,"result":{}}"#).is_err());
        assert!(Frame::parse(r#"{"jsonrpc":"2.0","id":1}"#).is_err());
        assert!(Frame::parse("[]").is_err());
    }

    #[test]
    fn round_trips_frames_through_single_lines() {
        let frames = [
            Frame::Request {
                id: RpcId::Number(7),
                method: "session/new".into(),
                params: Some(json!({"cwd": "/workspace"})),
            },
            Frame::Response {
                id: RpcId::String("abc".into()),
                result: json!({"stopReason": "end_turn"}),
            },
            Frame::Error {
                id: RpcId::Number(9),
                error: RpcError::new(-32000, "Authentication required"),
            },
        ];
        for frame in frames {
            let line = frame.to_line().unwrap();
            assert!(!line.contains('\n'));
            Frame::parse(&line).unwrap();
        }
    }
}
