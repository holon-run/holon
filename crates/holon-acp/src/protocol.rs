//! Typed ACP wire v1 payloads.
//!
//! The shapes follow upstream ACP wire v1. Unknown fields are tolerated so a
//! newer harness does not break the client; unknown `session/update` variants
//! surface as [`SessionUpdate::Unknown`] while the raw notification payload is
//! retained by [`SessionNotification::raw`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// ACP wire version implemented by this client.
pub const ACP_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<FsCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<bool>,
}

impl ClientCapabilities {
    /// Default trimmed client capability set: `fs.*` and `terminal` are omitted.
    pub fn trimmed() -> Self {
        Self::default()
    }

    pub fn advertises_fs(&self) -> bool {
        self.fs.is_some()
    }

    pub fn advertises_terminal(&self) -> bool {
        self.terminal == Some(true)
    }

    pub fn with_terminal(mut self) -> Self {
        self.terminal = Some(true);
        self
    }

    pub fn with_plan_observation(mut self) -> Self {
        self.plan = Some(true);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub client_capabilities: ClientCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_info: Option<ClientInfo>,
}

impl InitializeParams {
    pub fn new(client_capabilities: ClientCapabilities) -> Self {
        Self {
            protocol_version: ACP_PROTOCOL_VERSION,
            client_capabilities,
            client_info: None,
        }
    }

    pub fn with_client_info(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.client_info = Some(ClientInfo {
            name: name.into(),
            version: version.into(),
        });
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub embedded_context: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// `session/load` support, declared as `loadSession` by ACP wire v1.
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    /// Session lifecycle capabilities advertised by the agent.
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

/// Capabilities carried by `agentCapabilities.sessionCapabilities`.
///
/// ACP wire v1 advertises each capability by presence: the field is absent (or
/// `null`) when unsupported, and an object such as `{}` when supported. Keeping
/// the raw value preserves that semantics and tolerates peers that send a
/// boolean instead of an object.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<Value>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

impl SessionCapabilities {
    /// Whether the agent advertises `session/resume`.
    pub fn supports_resume(&self) -> bool {
        declares(&self.resume)
    }

    /// Whether the agent advertises `session/list`.
    pub fn supports_list(&self) -> bool {
        declares(&self.list)
    }
}

/// ACP v1 presence semantics: an object declaration (typically `{}`) or `true`
/// means supported; absent, `null`, or `false` means unsupported.
fn declares(capability: &Option<Value>) -> bool {
    match capability {
        Some(Value::Bool(flag)) => *flag,
        None | Some(Value::Null) => false,
        Some(_) => true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethod {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl AuthMethod {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            description: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    #[serde(default)]
    pub protocol_version: u32,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

impl InitializeResult {
    /// Whether the peer declared `session/resume` support.
    pub fn supports_resume(&self) -> bool {
        self.agent_capabilities
            .session_capabilities
            .supports_resume()
    }

    /// Whether the peer declared `session/load` support.
    pub fn supports_load(&self) -> bool {
        self.agent_capabilities.load_session
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewParams {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<McpServer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
}

impl SessionNewParams {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Self::default()
        }
    }

    pub fn with_additional_directories(mut self, directories: Vec<String>) -> Self {
        self.additional_directories = directories;
        self
    }

    pub fn with_meta(mut self, meta: Map<String, Value>) -> Self {
        self.meta = Some(meta);
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumeParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<McpServer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<String>,
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
}

impl SessionResumeParams {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoadParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<McpServer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<String>,
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
}

impl SessionLoadParams {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        #[serde(default)]
        data: String,
        #[serde(default, rename = "mimeType")]
        mime_type: String,
    },
    ResourceLink {
        #[serde(default)]
        uri: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Variant this client version does not model.
    #[serde(other)]
    Unknown,
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptParams {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

impl PromptParams {
    pub fn text(session_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            prompt: vec![ContentBlock::text(text)],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptResult {
    #[serde(rename = "stopReason")]
    pub stop_reason: StopReason,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<ToolCallStatus>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub locations: Option<Value>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdate {
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<ToolCallStatus>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommand {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
}

/// Cumulative session cost reported on a `usage_update`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    #[serde(default)]
    pub amount: f64,
    #[serde(default)]
    pub currency: String,
}

/// One projected `session/update` variant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    #[serde(rename_all = "camelCase")]
    AgentMessageChunk { content: ContentBlock },
    #[serde(rename_all = "camelCase")]
    AgentThoughtChunk { content: ContentBlock },
    #[serde(rename_all = "camelCase")]
    ToolCall { tool_call: ToolCall },
    #[serde(rename_all = "camelCase")]
    ToolCallUpdate { tool_call_update: ToolCallUpdate },
    #[serde(rename_all = "camelCase")]
    Plan { entries: Vec<PlanEntry> },
    #[serde(rename_all = "camelCase")]
    UsageUpdate {
        #[serde(default)]
        used: u64,
        #[serde(default)]
        size: u64,
        /// Cumulative session cost; absent when the agent does not report it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<UsageCost>,
    },
    #[serde(rename_all = "camelCase")]
    CurrentModeUpdate {
        #[serde(default)]
        current_mode_id: String,
    },
    #[serde(rename_all = "camelCase")]
    AvailableCommandsUpdate {
        #[serde(default)]
        available_commands: Vec<AvailableCommand>,
    },
    /// Variant this client version does not model; raw payload stays on the notification.
    #[serde(other)]
    Unknown,
}

impl SessionUpdate {
    pub fn kind(&self) -> &'static str {
        match self {
            SessionUpdate::AgentMessageChunk { .. } => "agent_message_chunk",
            SessionUpdate::AgentThoughtChunk { .. } => "agent_thought_chunk",
            SessionUpdate::ToolCall { .. } => "tool_call",
            SessionUpdate::ToolCallUpdate { .. } => "tool_call_update",
            SessionUpdate::Plan { .. } => "plan",
            SessionUpdate::UsageUpdate { .. } => "usage_update",
            SessionUpdate::CurrentModeUpdate { .. } => "current_mode_update",
            SessionUpdate::AvailableCommandsUpdate { .. } => "available_commands_update",
            SessionUpdate::Unknown => "unknown",
        }
    }
}

/// One `session/update` notification with its session id and raw payload.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
    pub raw: Value,
}

impl SessionNotification {
    /// Parse notification params; returns `None` when the payload is not a
    /// `session/update` notification.
    pub fn from_params(params: Option<&Value>) -> Result<Option<Self>, crate::error::AcpError> {
        let Some(params) = params else {
            return Ok(None);
        };
        let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Some(update_value) = params.get("update") else {
            return Ok(None);
        };
        let update: SessionUpdate =
            serde_json::from_value(update_value.clone()).map_err(|error| {
                crate::error::AcpError::protocol(format!("invalid session/update payload: {error}"))
            })?;
        Ok(Some(Self {
            session_id: session_id.to_string(),
            update,
            raw: update_value.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trimmed_capabilities_omit_fs_and_terminal() {
        let value = serde_json::to_value(ClientCapabilities::trimmed()).unwrap();
        assert_eq!(value, json!({}));
        let capabilities = ClientCapabilities::trimmed();
        assert!(!capabilities.advertises_fs());
        assert!(!capabilities.advertises_terminal());
    }

    #[test]
    fn explicit_capabilities_are_serialized() {
        let mut capabilities = ClientCapabilities::trimmed().with_terminal();
        capabilities.fs = Some(FsCapabilities {
            read_text_file: true,
            write_text_file: false,
        });
        let value = serde_json::to_value(capabilities).unwrap();
        assert_eq!(
            value,
            json!({
                "terminal": true,
                "fs": {"readTextFile": true, "writeTextFile": false},
            })
        );
    }

    #[test]
    fn initialize_result_reads_session_capabilities_from_agent_capabilities() {
        let result: InitializeResult = serde_json::from_value(json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "sessionCapabilities": {"resume": {}, "list": {}},
            },
            "authMethods": [{"id": "oauth-personal", "name": "Personal OAuth"}],
        }))
        .unwrap();
        assert!(result.supports_resume());
        assert!(result.supports_load());
        assert!(result
            .agent_capabilities
            .session_capabilities
            .supports_list());
        assert_eq!(result.auth_methods[0].id, "oauth-personal");
    }

    #[test]
    fn absent_or_null_session_capabilities_mean_unsupported() {
        let result: InitializeResult = serde_json::from_value(json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": false,
                "sessionCapabilities": {"resume": null},
            },
        }))
        .unwrap();
        assert!(!result.supports_resume());
        assert!(!result.supports_load());
    }

    #[test]
    fn legacy_top_level_session_capabilities_never_grant_support() {
        // A top-level block is not part of ACP wire v1: tolerate it, but never
        // read resume/load support from it.
        let result: InitializeResult = serde_json::from_value(json!({
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": false},
            "sessionCapabilities": {"resume": true, "load": true},
        }))
        .unwrap();
        assert!(!result.supports_resume());
        assert!(!result.supports_load());
        assert_eq!(result.extra["sessionCapabilities"]["resume"], json!(true));
    }

    #[test]
    fn projects_known_and_unknown_session_updates() {
        let params = json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "thinking"}},
        });
        let notification = SessionNotification::from_params(Some(&params))
            .unwrap()
            .unwrap();
        assert_eq!(notification.session_id, "s1");
        assert!(matches!(
            notification.update,
            SessionUpdate::AgentThoughtChunk { .. }
        ));

        let params = json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "future_variant", "payload": {"x": 1}},
        });
        let notification = SessionNotification::from_params(Some(&params))
            .unwrap()
            .unwrap();
        assert_eq!(notification.update, SessionUpdate::Unknown);
        assert_eq!(notification.raw["payload"], json!({"x": 1}));
    }

    #[test]
    fn maps_tool_call_and_usage_updates() {
        let params = json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "tool_call", "toolCall": {"toolCallId": "t1", "status": "in_progress"}},
        });
        let notification = SessionNotification::from_params(Some(&params))
            .unwrap()
            .unwrap();
        match notification.update {
            SessionUpdate::ToolCall { tool_call } => {
                assert_eq!(tool_call.tool_call_id, "t1");
                assert_eq!(tool_call.status, Some(ToolCallStatus::InProgress));
            }
            other => panic!("unexpected update: {other:?}"),
        }

        let params = json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "usage_update", "used": 10, "size": 100},
        });
        let notification = SessionNotification::from_params(Some(&params))
            .unwrap()
            .unwrap();
        assert_eq!(
            notification.update,
            SessionUpdate::UsageUpdate {
                used: 10,
                size: 100,
                cost: None,
            }
        );

        let params = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "usage_update",
                "used": 10,
                "size": 100,
                "cost": {"amount": 0.25, "currency": "USD"},
            },
        });
        let notification = SessionNotification::from_params(Some(&params))
            .unwrap()
            .unwrap();
        assert_eq!(
            notification.update,
            SessionUpdate::UsageUpdate {
                used: 10,
                size: 100,
                cost: Some(UsageCost {
                    amount: 0.25,
                    currency: "USD".to_string(),
                }),
            }
        );
    }

    #[test]
    fn parses_prompt_result_stop_reason() {
        let result: PromptResult =
            serde_json::from_value(json!({"stopReason": "cancelled"})).unwrap();
        assert_eq!(result.stop_reason, StopReason::Cancelled);
        let unknown: PromptResult =
            serde_json::from_value(json!({"stopReason": "future"})).unwrap();
        assert_eq!(unknown.stop_reason, StopReason::Unknown);
    }
}
