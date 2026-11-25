//! Typed ACP client errors and JSON-RPC error classification.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC code returned when the harness requires authentication.
pub const AUTHENTICATION_REQUIRED: i64 = -32000;
/// JSON-RPC code returned when a session no longer exists on the harness.
pub const SESSION_NOT_FOUND: i64 = -32002;
/// JSON-RPC code returned by a peer that does not implement a method.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC code used when the client cancels an in-flight request.
pub const REQUEST_CANCELLED: i64 = -32800;

/// A JSON-RPC 2.0 error object as carried on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Authentication method identifiers advertised inside error `data`.
    pub fn auth_methods(&self) -> Vec<String> {
        collect_auth_methods(self.data.as_ref())
    }
}

/// Error surface of the ACP client.
#[derive(Clone, Debug, thiserror::Error)]
pub enum AcpError {
    #[error("ACP transport error: {0}")]
    Transport(String),
    #[error("ACP protocol violation: {0}")]
    Protocol(String),
    #[error("peer does not support {operation}")]
    UnsupportedByPeer { operation: &'static str },
    #[error("client does not implement capability `{capability}`")]
    UnsupportedCapability { capability: String },
    #[error("ACP authentication required: {message}")]
    AuthenticationRequired {
        message: String,
        auth_methods: Vec<String>,
    },
    #[error("ACP session `{session_id}` not found")]
    SessionNotFound { session_id: String },
    #[error("ACP RPC error {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("ACP turn was cancelled")]
    Cancelled,
    #[error("ACP connection closed")]
    Closed,
    #[error("unknown ACP harness profile `{name}`")]
    UnknownHarness { name: String },
}

impl AcpError {
    /// Classify a wire error into a typed client error.
    ///
    /// `session_id` is the session the caller was addressing, if any, so a
    /// `-32002` response can be reported with the affected session.
    pub fn from_rpc(error: RpcError, session_id: Option<&str>) -> Self {
        match error.code {
            AUTHENTICATION_REQUIRED => {
                let auth_methods = error.auth_methods();
                AcpError::AuthenticationRequired {
                    message: error.message,
                    auth_methods,
                }
            }
            SESSION_NOT_FOUND => AcpError::SessionNotFound {
                session_id: session_id.unwrap_or_default().to_string(),
            },
            code => AcpError::Rpc {
                code,
                message: error.message,
                data: error.data,
            },
        }
    }

    pub fn is_authentication_required(&self) -> bool {
        matches!(self, AcpError::AuthenticationRequired { .. })
    }

    pub fn is_session_not_found(&self) -> bool {
        matches!(self, AcpError::SessionNotFound { .. })
    }

    /// Operator-facing recovery hint for recoverable failures.
    pub fn recovery_hint(&self) -> Option<&'static str> {
        match self {
            AcpError::AuthenticationRequired { .. } => Some(
                "run `agy acp --login`, or configure GEMINI_API_KEY before retrying; \
                 do not retry in a loop and do not fall back to a different model",
            ),
            AcpError::SessionNotFound { .. } => Some(
                "rebuild the session with session/new and replay only task-level handoff content",
            ),
            _ => None,
        }
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        AcpError::Protocol(message.into())
    }

    pub(crate) fn transport(message: impl Into<String>) -> Self {
        AcpError::Transport(message.into())
    }
}

fn collect_auth_methods(data: Option<&Value>) -> Vec<String> {
    let Some(value) = data else {
        return Vec::new();
    };
    let Some(items) = value.get("authMethods").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::String(id) => Some(id.clone()),
            Value::Object(map) => map
                .get("id")
                .or_else(|| map.get("method"))
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_authentication_error_with_methods() {
        let error = AcpError::from_rpc(
            RpcError {
                code: AUTHENTICATION_REQUIRED,
                message: "Authentication required".into(),
                data: Some(json!({ "authMethods": ["oauth-personal", "gemini-api-key"] })),
            },
            None,
        );
        match error {
            AcpError::AuthenticationRequired { auth_methods, .. } => {
                assert_eq!(auth_methods, vec!["oauth-personal", "gemini-api-key"]);
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }

    #[test]
    fn classifies_session_not_found() {
        let error = AcpError::from_rpc(
            RpcError::new(SESSION_NOT_FOUND, "unknown session"),
            Some("session-1"),
        );
        assert!(error.is_session_not_found());
        assert!(error.to_string().contains("session-1"));
    }

    #[test]
    fn passes_through_other_rpc_codes() {
        let error = AcpError::from_rpc(RpcError::new(-32603, "boom"), None);
        match error {
            AcpError::Rpc { code, .. } => assert_eq!(code, -32603),
            other => panic!("unexpected classification: {other:?}"),
        }
    }
}
