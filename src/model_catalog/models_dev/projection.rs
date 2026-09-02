//! Projection from `models.dev` DTO to Holon canonical model metadata.
//!
//! The projection maps upstream provider/model fields to Holon
//! `BuiltInModelMetadata`. It uses an explicit provider mapping to connect
//! `models.dev` provider IDs to Holon provider IDs. Capabilities can only
//! narrow, never widen. The DTO preserves tri-state (`Option<bool>`), but
//! the serialized artifact's `ModelCapabilityFlags` uses plain `bool`;
//! omitted upstream capability fields collapse to conservative `false`.
//! Phase 3 narrowing must check the DTO's `Option<bool>` presence before
//! AND-ing, not the artifact's bool value.

use std::collections::BTreeMap;

use crate::config::{ModelRef, ProviderId};
use crate::model_catalog::{BuiltInModelMetadata, ModelCapabilityFlags, ModelMetadataSource};

use super::dto::{ModelsDevModel, ModelsDevSnapshot};

/// Default effective context window percentage projected from models.dev.
const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u8 = 95;

/// Default tool output truncation estimate projected from models.dev.
const DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS: usize = 2_500;

/// Maps a `models.dev` provider ID to a Holon provider ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMapping {
    pub models_dev_id: String,
    pub holon_provider_id: String,
}

impl ProviderMapping {
    /// Returns the default set of provider mappings.
    ///
    /// Includes both direct ID matches (models.dev ID == Holon provider ID)
    /// and name-mismatched mappings (e.g. `moonshotai` → `moonshot`).
    /// Unmapped providers are skipped during projection.
    pub fn default_mappings() -> Vec<Self> {
        // Direct matches: models.dev provider ID == Holon provider ID.
        let direct = [
            "anthropic",
            "openai",
            "deepseek",
            "mistral",
            "nvidia",
            "openrouter",
            "huggingface",
            "xai",
            "minimax",
            "zai",
            "volcengine",
            "stepfun",
            "arcee",
            "chutes",
            "nearai",
            "venice",
            "xiaomi",
        ]
        .into_iter()
        .map(|id| Self {
            models_dev_id: id.to_string(),
            holon_provider_id: id.to_string(),
        })
        .collect::<Vec<_>>();

        // Name-mismatched mappings: the models.dev provider ID differs from
        // the Holon canonical provider ID. Verified against the live
        // `models.dev/api.json` snapshot.
        let mismatched = [
            ("moonshotai", "moonshot"),
            ("togetherai", "together"),
            ("fireworks-ai", "fireworks"),
            ("google", "gemini"),
            ("zhipuai", "bigmodel"),
            ("alibaba", "dashscope"),
        ];
        let mismatched = mismatched
            .into_iter()
            .map(|(md_id, holon_id)| Self {
                models_dev_id: md_id.to_string(),
                holon_provider_id: holon_id.to_string(),
            })
            .collect::<Vec<_>>();

        let mut mappings = direct;
        mappings.extend(mismatched);
        mappings
    }
}

/// A single projected model with its provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedModel {
    pub metadata: BuiltInModelMetadata,
    /// Whether the provider was explicitly mapped.
    pub provider_mapped: bool,
    /// The original `models.dev` provider ID.
    pub models_dev_provider_id: String,
    /// The original `models.dev` model ID.
    pub models_dev_model_id: String,
}

/// Result of projecting a `models.dev` snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionResult {
    /// Successfully projected models (provider was mapped).
    pub projected: Vec<ProjectedModel>,
    /// Models whose provider was not in the mapping (skipped).
    pub unmapped: Vec<UnmappedModel>,
}

/// A model that was skipped because its provider was not in the mapping.
#[derive(Debug, Clone, PartialEq)]
pub struct UnmappedModel {
    pub models_dev_provider_id: String,
    pub models_dev_model_id: String,
    pub model_name: Option<String>,
}

/// Projects `models.dev` DTO data into Holon canonical model metadata.
pub struct Projector {
    mappings: BTreeMap<String, String>,
}

impl Projector {
    /// Creates a projector with the default provider mappings.
    pub fn new() -> Self {
        Self::with_mappings(ProviderMapping::default_mappings())
    }

    /// Creates a projector with custom provider mappings.
    pub fn with_mappings(mappings: Vec<ProviderMapping>) -> Self {
        let map = mappings
            .into_iter()
            .map(|m| (m.models_dev_id, m.holon_provider_id))
            .collect();
        Self { mappings: map }
    }

    /// Projects an entire `models.dev` snapshot into Holon model metadata.
    pub fn project(&self, snapshot: &ModelsDevSnapshot) -> Result<ProjectionResult, String> {
        let mut projected = Vec::new();
        let mut unmapped = Vec::new();

        for (md_provider_id, provider) in &snapshot.providers {
            let holon_provider_id = match self.mappings.get(md_provider_id) {
                Some(id) => id.clone(),
                None => {
                    for (md_model_id, model) in &provider.models {
                        unmapped.push(UnmappedModel {
                            models_dev_provider_id: md_provider_id.clone(),
                            models_dev_model_id: md_model_id.clone(),
                            model_name: model.name.clone(),
                        });
                    }
                    continue;
                }
            };

            let holon_provider = ProviderId::parse(&holon_provider_id).map_err(|e| {
                format!("invalid Holon provider ID from mapping: {holon_provider_id}: {e}")
            })?;

            for (md_model_id, model) in &provider.models {
                let model_ref = ModelRef::new(holon_provider.clone(), md_model_id.clone());
                let metadata = project_model(&model_ref, model);
                projected.push(ProjectedModel {
                    metadata,
                    provider_mapped: true,
                    models_dev_provider_id: md_provider_id.clone(),
                    models_dev_model_id: md_model_id.clone(),
                });
            }
        }

        projected.sort_by(|a, b| {
            a.metadata
                .model_ref
                .as_string()
                .cmp(&b.metadata.model_ref.as_string())
        });

        Ok(ProjectionResult {
            projected,
            unmapped,
        })
    }
}

impl Default for Projector {
    fn default() -> Self {
        Self::new()
    }
}

/// Projects one upstream model into Holon metadata using the shared
/// conservative projection rules. Also used by the supplemental catalog
/// generator, which overrides the provenance source afterwards.
pub(super) fn project_model(model_ref: &ModelRef, model: &ModelsDevModel) -> BuiltInModelMetadata {
    let display_name = model.name.clone().unwrap_or_else(|| model.id.clone());
    let description = model
        .description
        .clone()
        .unwrap_or_else(|| format!("Holon projected metadata for {}.", model.id));

    // Upstream reports `0` limits for some models; treat them as unknown
    // instead of projecting invalid token limits.
    let context_window_tokens = model
        .limit
        .as_ref()
        .and_then(|l| l.context)
        .filter(|v| *v > 0);
    let default_max_output_tokens = model
        .limit
        .as_ref()
        .and_then(|l| l.output)
        .filter(|v| *v > 0);
    let max_output_tokens_upper_limit = default_max_output_tokens;

    let auto_compact_token_limit = context_window_tokens
        .map(|ctx| ctx * DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT as u64 / 100);

    let capabilities = project_capabilities(model);

    let reasoning_effort_options = project_reasoning_effort_options(model);

    BuiltInModelMetadata {
        model_ref: model_ref.clone(),
        display_name,
        description,
        context_window_tokens: context_window_tokens.map(|v| v as usize),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: auto_compact_token_limit.map(|v| v as usize),
        default_max_output_tokens: default_max_output_tokens.map(|v| v as u32),
        max_output_tokens_upper_limit: max_output_tokens_upper_limit.map(|v| v as u32),
        default_verbosity: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities,
        reasoning_effort_options,
        source: ModelMetadataSource::RemoteDiscovered,
        endpoint: None,
    }
}

fn project_capabilities(model: &ModelsDevModel) -> ModelCapabilityFlags {
    let image_input = model
        .modalities
        .as_ref()
        .map(|m| m.input.iter().any(|modality| modality == "image"))
        .unwrap_or(false);

    let image_generation = model
        .modalities
        .as_ref()
        .map(|m| m.output.iter().any(|modality| modality == "image"))
        .unwrap_or(false);

    let supports_reasoning = model.reasoning.unwrap_or(false);

    // These capabilities are not available from models.dev metadata.
    // Conservative defaults: do not assume support.
    let parallel_tool_calls = false;
    let interactive_exec = false;

    ModelCapabilityFlags {
        parallel_tool_calls,
        image_input,
        image_generation,
        supports_reasoning,
        interactive_exec,
    }
}

fn project_reasoning_effort_options(model: &ModelsDevModel) -> Vec<String> {
    model
        .reasoning_options
        .iter()
        .filter(|opt| opt.r#type.as_deref() == Some("effort"))
        .flat_map(|opt| opt.values.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_catalog::ModelMetadataSource;

    fn sample_snapshot() -> ModelsDevSnapshot {
        let json = r#"{
            "openai": {
                "id": "openai",
                "models": {
                    "gpt-5.5": {
                        "id": "gpt-5.5",
                        "name": "GPT-5.5",
                        "description": "Default frontier GPT",
                        "reasoning": true,
                        "reasoning_options": [{"type": "effort", "values": ["none", "low", "high"]}],
                        "modalities": {"input": ["text", "image"], "output": ["text"]},
                        "limit": {"context": 1050000, "output": 128000}
                    },
                    "gpt-4o-mini": {
                        "id": "gpt-4o-mini",
                        "name": "GPT-4o mini",
                        "reasoning": false,
                        "modalities": {"input": ["text", "image"], "output": ["text", "image"]},
                        "limit": {"context": 128000, "output": 16384}
                    }
                }
            },
            "unknown-provider": {
                "id": "unknown-provider",
                "models": {
                    "mystery-model": {
                        "id": "mystery-model",
                        "name": "Mystery Model"
                    }
                }
            }
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn projects_mapped_provider() {
        let snapshot = sample_snapshot();
        let result = Projector::new().project(&snapshot).unwrap();

        assert_eq!(result.projected.len(), 2);
        assert_eq!(result.unmapped.len(), 1);

        let gpt55 = result
            .projected
            .iter()
            .find(|m| m.models_dev_model_id == "gpt-5.5")
            .unwrap();
        assert_eq!(gpt55.metadata.model_ref.as_string(), "openai/gpt-5.5");
        assert_eq!(gpt55.metadata.display_name, "GPT-5.5");
        assert_eq!(gpt55.metadata.context_window_tokens, Some(1_050_000));
        assert_eq!(gpt55.metadata.default_max_output_tokens, Some(128_000));
        assert_eq!(gpt55.metadata.max_output_tokens_upper_limit, Some(128_000));
        assert!(gpt55.metadata.capabilities.supports_reasoning);
        assert!(gpt55.metadata.capabilities.image_input);
        assert!(!gpt55.metadata.capabilities.image_generation);
        assert_eq!(
            gpt55.metadata.reasoning_effort_options,
            vec!["none", "low", "high"]
        );
        assert_eq!(gpt55.metadata.source, ModelMetadataSource::RemoteDiscovered);
        assert!(gpt55.metadata.endpoint.is_none());
    }

    #[test]
    fn projects_image_generation() {
        let snapshot = sample_snapshot();
        let result = Projector::new().project(&snapshot).unwrap();

        let gpt4o = result
            .projected
            .iter()
            .find(|m| m.models_dev_model_id == "gpt-4o-mini")
            .unwrap();
        assert!(gpt4o.metadata.capabilities.image_generation);
        assert!(!gpt4o.metadata.capabilities.supports_reasoning);
    }

    #[test]
    fn skips_unmapped_provider() {
        let snapshot = sample_snapshot();
        let result = Projector::new().project(&snapshot).unwrap();

        assert_eq!(result.unmapped.len(), 1);
        assert_eq!(
            result.unmapped[0].models_dev_provider_id,
            "unknown-provider"
        );
        assert_eq!(result.unmapped[0].models_dev_model_id, "mystery-model");
    }

    #[test]
    fn auto_compact_token_limit_derived() {
        let snapshot = sample_snapshot();
        let result = Projector::new().project(&snapshot).unwrap();

        let gpt55 = result
            .projected
            .iter()
            .find(|m| m.models_dev_model_id == "gpt-5.5")
            .unwrap();
        // 1050000 * 95 / 100 = 997500
        assert_eq!(gpt55.metadata.auto_compact_token_limit, Some(997_500));
    }

    #[test]
    fn custom_mapping_overrides_default() {
        let snapshot = sample_snapshot();
        let projector = Projector::with_mappings(vec![ProviderMapping {
            models_dev_id: "openai".to_string(),
            holon_provider_id: "openrouter".to_string(),
        }]);
        let result = projector.project(&snapshot).unwrap();

        assert_eq!(result.projected.len(), 2);
        assert!(result.projected[0]
            .metadata
            .model_ref
            .provider
            .as_str()
            .contains("openrouter"));
    }
}
