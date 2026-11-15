use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool specification describing name, description, and input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform_grammar: Option<ToolFreeformGrammar>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolFreeformGrammar {
    pub syntax: String,
    pub definition: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptStability {
    Stable,
    AgentScoped,
    TurnScoped,
}
