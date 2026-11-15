//! JSON encoding for the OpenAI Chat Completions protocol.

use serde_json::{json, Map, Value};

use crate::Role;

/// Protocol options chosen by the caller for this request.
pub struct RequestBody<'a> {
    pub model: &'a str,
    pub messages: Vec<Value>,
    pub output_limit: Option<(&'a str, u32)>,
    pub stream: bool,
    pub tools: Vec<Value>,
    pub tool_choice: Option<&'a str>,
    pub prompt_cache_key: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
}

impl RequestBody<'_> {
    pub fn encode(self) -> Map<String, Value> {
        let mut body = Map::new();
        body.insert("model".into(), Value::String(self.model.into()));
        body.insert("messages".into(), Value::Array(self.messages));
        body.insert("stream".into(), Value::Bool(self.stream));
        if let Some((field, limit)) = self.output_limit {
            body.insert(field.into(), Value::from(limit));
        }
        if !self.tools.is_empty() {
            body.insert("tools".into(), Value::Array(self.tools));
        }
        for (name, value) in [
            ("tool_choice", self.tool_choice),
            ("prompt_cache_key", self.prompt_cache_key),
            ("reasoning_effort", self.reasoning_effort),
        ] {
            if let Some(value) = value {
                body.insert(name.into(), Value::String(value.into()));
            }
        }
        body
    }
}

pub fn message(
    role: Role,
    content: Value,
    tool_calls: Vec<Value>,
    tool_call_id: Option<&str>,
) -> Value {
    let mut message = Map::new();
    message.insert("role".into(), Value::String(role_name(role).into()));
    message.insert("content".into(), content);
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    if let Some(id) = tool_call_id {
        message.insert("tool_call_id".into(), Value::String(id.into()));
    }
    Value::Object(message)
}

pub fn function_tool(
    name: &str,
    description: Option<&str>,
    parameters: &Value,
    strict: bool,
) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
            "strict": strict,
        }
    })
}

pub fn function_call(id: &str, name: &str, arguments: &Value) -> serde_json::Result<Value> {
    Ok(json!({
        "id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": serde_json::to_string(arguments)?,
        }
    }))
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}
