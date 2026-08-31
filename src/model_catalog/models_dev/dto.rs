//! Upstream DTO for the `models.dev` API.
//!
//! All fields are `Option<T>` to preserve tri-state semantics: a missing field
//! is `unknown`, not `false`. The DTO does not share types with runtime
//! catalog structures to avoid accidental coupling.

use std::collections::BTreeMap;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// The top-level `models.dev` API response: a map of provider ID to provider.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelsDevSnapshot {
    pub providers: BTreeMap<String, ModelsDevProvider>,
}

impl<'de> Deserialize<'de> for ModelsDevSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = BTreeMap::<String, Value>::deserialize(deserializer)?;
        let mut providers = BTreeMap::new();

        for (key, value) in raw {
            let Some(object) = value.as_object() else {
                // Keep the adapter forward-compatible with future scalar
                // metadata added alongside provider entries.
                continue;
            };
            if !object.contains_key("id") {
                continue;
            }

            let provider = serde_json::from_value(value).map_err(D::Error::custom)?;
            providers.insert(key, provider);
        }

        Ok(Self { providers })
    }
}

/// A single provider entry in the `models.dev` API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelsDevProvider {
    pub id: String,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelsDevModel>,
}

/// A single model entry within a provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelsDevModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    /// Whether the model accepts file/image attachments.
    #[serde(default)]
    pub attachment: Option<bool>,
    /// Whether the model supports reasoning.
    #[serde(default)]
    pub reasoning: Option<bool>,
    /// Reasoning configuration options.
    #[serde(default)]
    pub reasoning_options: Vec<ModelsDevReasoningOption>,
    /// Whether the model supports tool calls.
    #[serde(default)]
    pub tool_call: Option<bool>,
    /// Whether the model supports structured output.
    #[serde(default)]
    pub structured_output: Option<bool>,
    /// Whether the model supports temperature parameter.
    #[serde(default)]
    pub temperature: Option<bool>,
    /// Knowledge cutoff date (e.g. "2025-05").
    #[serde(default)]
    pub knowledge: Option<String>,
    /// Release date of the model.
    #[serde(default)]
    pub release_date: Option<String>,
    /// Last updated date.
    #[serde(default)]
    pub last_updated: Option<String>,
    /// Input and output modalities.
    #[serde(default)]
    pub modalities: Option<ModelsDevModalities>,
    /// Whether model weights are open.
    #[serde(default)]
    pub open_weights: Option<bool>,
    /// Token limits.
    #[serde(default)]
    pub limit: Option<ModelsDevLimit>,
    /// Cost information (provenance only, not used in runtime decisions).
    #[serde(default)]
    pub cost: Option<ModelsDevCost>,
    /// Interleaved reasoning content configuration.
    #[serde(default)]
    pub interleaved: Option<ModelsDevInterleaved>,
}

/// Reasoning option type (e.g. effort levels or toggle).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelsDevReasoningOption {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub values: Vec<String>,
}

/// Input and output modalities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelsDevModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

/// Token limits for a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelsDevLimit {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
    #[serde(default)]
    pub input: Option<u64>,
}

/// Cost information (provenance metadata only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelsDevCost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

/// Interleaved reasoning content configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelsDevInterleaved {
    #[serde(default)]
    pub field: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_model() {
        let json = r#"{
            "test-provider": {
                "id": "test-provider",
                "models": {
                    "test-model": {
                        "id": "test-model"
                    }
                }
            }
        }"#;
        let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
        let provider = &snapshot.providers["test-provider"];
        assert_eq!(provider.id, "test-provider");
        assert!(provider.env.is_empty());
        assert!(provider.api.is_none());
        let model = &provider.models["test-model"];
        assert_eq!(model.id, "test-model");
        assert!(model.name.is_none());
        assert!(model.reasoning.is_none());
    }

    #[test]
    fn parse_full_model() {
        let json = r#"{
            "openai": {
                "id": "openai",
                "env": ["OPENAI_API_KEY"],
                "npm": "@ai-sdk/openai",
                "api": "https://api.openai.com/v1",
                "name": "OpenAI",
                "doc": "https://platform.openai.com/docs",
                "models": {
                    "gpt-5.5": {
                        "id": "gpt-5.5",
                        "name": "GPT-5.5",
                        "description": "Default frontier GPT",
                        "family": "gpt",
                        "attachment": true,
                        "reasoning": true,
                        "reasoning_options": [{"type": "effort", "values": ["none", "low", "high"]}],
                        "tool_call": true,
                        "structured_output": true,
                        "temperature": true,
                        "knowledge": "2025-12-01",
                        "release_date": "2026-04-23",
                        "last_updated": "2026-04-23",
                        "modalities": {"input": ["text", "image"], "output": ["text"]},
                        "open_weights": false,
                        "limit": {"context": 1050000, "output": 128000},
                        "cost": {"input": 5, "output": 30, "cache_read": 0.5}
                    }
                }
            }
        }"#;
        let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
        let model = &snapshot.providers["openai"].models["gpt-5.5"];
        assert_eq!(model.name.as_deref(), Some("GPT-5.5"));
        assert_eq!(model.reasoning, Some(true));
        assert_eq!(model.reasoning_options.len(), 1);
        assert_eq!(model.reasoning_options[0].r#type.as_deref(), Some("effort"));
        assert_eq!(
            model.reasoning_options[0].values,
            vec!["none", "low", "high"]
        );
        assert_eq!(model.limit.as_ref().unwrap().context, Some(1050000));
        assert_eq!(
            model.modalities.as_ref().unwrap().input,
            vec!["text", "image"]
        );
    }

    #[test]
    fn ignores_unknown_fields() {
        let json = r#"{
            "test": {
                "id": "test",
                "future_field": "ignored",
                "models": {
                    "m": {"id": "m", "another_future_field": 42}
                }
            }
        }"#;
        let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snapshot.providers["test"].models["m"].id, "m");
    }

    #[test]
    fn ignores_unknown_top_level_scalar_fields() {
        let json = r#"{
            "schema_version": 2,
            "generated_at": "2026-08-31T00:00:00Z",
            "test": {
                "id": "test",
                "models": {}
            }
        }"#;
        let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snapshot.providers.len(), 1);
        assert_eq!(snapshot.providers["test"].id, "test");
    }
}
