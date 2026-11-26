//! Host callbacks for ACP reverse requests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AcpError;

/// One option offered by a `session/request_permission` request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    #[serde(default)]
    pub option_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
}

/// A harness request to approve a tool call it wants to execute itself.
///
/// The request is evidence, never authority: option names and tool-call titles
/// originate from the harness and must be validated against the bound execution
/// scope before any approval.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PermissionRequest {
    pub session_id: String,
    pub tool_call: Value,
    pub options: Vec<PermissionOption>,
}

impl PermissionRequest {
    pub fn from_params(params: Option<&Value>) -> Result<Self, AcpError> {
        let params = params
            .and_then(Value::as_object)
            .ok_or_else(|| AcpError::protocol("permission request is missing params"))?;
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let tool_call = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let options = params
            .get("options")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| AcpError::protocol(format!("invalid permission options: {error}")))?
            .unwrap_or_default();
        Ok(Self {
            session_id,
            tool_call,
            options,
        })
    }

    pub fn option(&self, option_id: &str) -> Option<&PermissionOption> {
        self.options
            .iter()
            .find(|option| option.option_id == option_id)
    }
}

/// The client's answer to a permission request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcome {
    #[serde(rename_all = "camelCase")]
    Selected {
        option_id: String,
    },
    Cancelled,
}

/// Wire shape of the `session/request_permission` response.
///
/// ACP wire v1 nests the outcome inside a response object, so a selected option
/// travels as `{"outcome": {"outcome": "selected", "optionId": "allow_once"}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub outcome: PermissionOutcome,
}

impl PermissionResponse {
    pub fn new(outcome: PermissionOutcome) -> Self {
        Self { outcome }
    }
}

/// A harness request for structured user input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ElicitationRequest {
    pub session_id: String,
    /// Elicitation mode advertised by the peer: `form`, `url`, or another
    /// reserved value. Holon only answers `form`.
    pub mode: String,
    pub message: String,
    pub requested_schema: Option<Value>,
    pub tool_call_id: Option<String>,
}

impl ElicitationRequest {
    /// Whether this is a form elicitation the client can render and answer.
    pub fn is_form(&self) -> bool {
        self.mode.is_empty() || self.mode == "form"
    }

    pub fn from_params(params: Option<&Value>) -> Result<Self, AcpError> {
        let params = params
            .and_then(Value::as_object)
            .ok_or_else(|| AcpError::protocol("elicitation request is missing params"))?;
        Ok(Self {
            session_id: params
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            mode: params
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            message: params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            requested_schema: params
                .get("requestedSchema")
                .or_else(|| params.get("requested_schema"))
                .cloned(),
            tool_call_id: params
                .get("toolCallId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        })
    }
}

/// The client's answer to an `elicitation/create` request.
///
/// The wire shape is `{ "action": "accept", "content": { ... } }`,
/// `{ "action": "decline" }`, or `{ "action": "cancel" }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ElicitationResponse {
    #[serde(rename_all = "camelCase")]
    Accept {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    Decline,
    Cancel,
}

impl ElicitationResponse {
    pub fn accept(content: Value) -> Self {
        Self::Accept {
            content: Some(content),
        }
    }

    pub fn to_value(&self) -> Result<Value, AcpError> {
        serde_json::to_value(self)
            .map_err(|error| AcpError::protocol(format!("invalid elicitation response: {error}")))
    }
}

/// Host policy surface for ACP reverse requests.
#[async_trait]
pub trait AcpClientHandler: Send + Sync {
    /// Decide one permission request. The default denies by cancelling, which
    /// is the safe default when no execution policy is wired in.
    async fn request_permission(&self, _request: PermissionRequest) -> PermissionOutcome {
        PermissionOutcome::Cancelled
    }

    /// Answer an elicitation request. The default rejects it because the
    /// trimmed client capability set does not advertise elicitation.
    async fn create_elicitation(
        &self,
        _request: ElicitationRequest,
    ) -> Result<ElicitationResponse, AcpError> {
        Err(AcpError::UnsupportedCapability {
            capability: "elicitation".to_string(),
        })
    }
}

/// Handler that cancels every permission request and rejects elicitation.
#[derive(Debug, Default)]
pub struct DenyAllHandler;

#[async_trait]
impl AcpClientHandler for DenyAllHandler {}
