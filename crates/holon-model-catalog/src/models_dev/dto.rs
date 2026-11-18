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
    /// Null entries appear upstream (e.g. sarvam models) and are dropped.
    #[serde(default, deserialize_with = "deserialize_reasoning_values")]
    pub values: Vec<String>,
}

/// Deserializes `Vec<String>` while dropping upstream `null` entries.
fn deserialize_reasoning_values<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Vec::<Option<String>>::deserialize(deserializer)?;
    Ok(raw.into_iter().flatten().collect())
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
///
/// The live `models.dev` API encodes this either as a boolean flag or as an
/// object carrying the interleaved field name; both shapes deserialize into
/// this struct (`enabled` records the boolean form).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelsDevInterleaved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl<'de> Deserialize<'de> for ModelsDevInterleaved {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Boolean(bool),
            Object { field: Option<String> },
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Boolean(enabled) => Self {
                enabled: Some(enabled),
                field: None,
            },
            Raw::Object { field } => Self {
                enabled: None,
                field,
            },
        })
    }
}

#[cfg(test)]
mod tests;
