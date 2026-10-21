//! Operation-scoped media discovery. Model support is not execution authority.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::{ResolvedModelCapabilities, ResolvedProviderEndpointConfig},
    model_catalog::{
        ModelMetadataField, ModelMetadataOrigin, ModelModality, ResolvedRuntimeModelPolicy,
    },
};

use super::image_generation::{resolve_image_generation_contract, ImageGenerationContract};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaOperation {
    ViewImage,
    GenerateImage,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaToolDefaults {
    pub view_image: Option<String>,
    pub generate_image: Option<String>,
    pub output_format: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaSupport {
    Supported,
    Unsupported,
    #[default]
    Unknown,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaOperationAvailability {
    pub support: MediaSupport,
    pub call_selectable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaModelCapabilities {
    pub image_input: MediaSupport,
    /// Image generation, not a claim that an ordinary text turn returns images.
    pub image_output: MediaSupport,
    pub view_image: MediaOperationAvailability,
    pub generate_image: MediaOperationAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_generation_contract: Option<ImageGenerationContract>,
}

impl MediaModelCapabilities {
    pub fn operation(&self, operation: MediaOperation) -> &MediaOperationAvailability {
        match operation {
            MediaOperation::ViewImage => &self.view_image,
            MediaOperation::GenerateImage => &self.generate_image,
        }
    }

    pub(crate) fn deny_tool_access(&mut self) {
        for operation in [&mut self.view_image, &mut self.generate_image] {
            operation.call_selectable = false;
            operation.unavailable_reason = Some("agent_capability_not_allowed".into());
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaProviderCapabilities {
    pub image_input: MediaSupport,
    pub image_output: MediaSupport,
    pub view_image_selectable_models: usize,
    pub generate_image_selectable_models: usize,
    pub observed_routes: usize,
    /// Catalog/discovery may not enumerate every model the service offers.
    pub inventory_complete: bool,
}

impl MediaProviderCapabilities {
    pub fn selectable_models(&self, operation: MediaOperation) -> usize {
        match operation {
            MediaOperation::ViewImage => self.view_image_selectable_models,
            MediaOperation::GenerateImage => self.generate_image_selectable_models,
        }
    }

    pub(crate) fn aggregate<'a>(
        models: impl IntoIterator<Item = &'a MediaModelCapabilities>,
    ) -> Self {
        let models = models.into_iter().collect::<Vec<_>>();
        Self {
            image_input: aggregate_support(models.iter().map(|model| model.image_input)),
            image_output: aggregate_support(models.iter().map(|model| model.image_output)),
            view_image_selectable_models: models
                .iter()
                .filter(|model| model.view_image.call_selectable)
                .count(),
            generate_image_selectable_models: models
                .iter()
                .filter(|model| model.generate_image.call_selectable)
                .count(),
            observed_routes: models.len(),
            inventory_complete: false,
        }
    }
}

fn aggregate_support(values: impl Iterator<Item = MediaSupport>) -> MediaSupport {
    if values
        .into_iter()
        .any(|value| value == MediaSupport::Supported)
    {
        MediaSupport::Supported
    } else {
        // Absence in a partial inventory is not evidence of service-wide absence.
        MediaSupport::Unknown
    }
}

fn model_support(
    value: bool,
    field: ModelMetadataField,
    policy: &ResolvedRuntimeModelPolicy,
) -> MediaSupport {
    if value {
        MediaSupport::Supported
    } else {
        match policy.evidence.source_for(field) {
            None
            | Some(ModelMetadataOrigin::UnknownFallback | ModelMetadataOrigin::RuntimeDefault) => {
                MediaSupport::Unknown
            }
            Some(_) => MediaSupport::Unsupported,
        }
    }
}

fn operation_availability(
    model: MediaSupport,
    transport_supports: bool,
    available: bool,
    unavailable_reason: Option<&str>,
) -> MediaOperationAvailability {
    let support = if !transport_supports {
        MediaSupport::Unsupported
    } else {
        model
    };
    let reason = if !transport_supports {
        Some("media_transport_unsupported")
    } else if model == MediaSupport::Unsupported {
        Some("media_capability_mismatch")
    } else if model == MediaSupport::Unknown {
        Some("media_capability_unknown")
    } else if !available {
        Some(unavailable_reason.unwrap_or("model_route_unavailable"))
    } else {
        None
    };
    MediaOperationAvailability {
        support,
        call_selectable: support == MediaSupport::Supported && available,
        unavailable_reason: reason.map(ToString::to_string),
    }
}

pub(crate) fn resolved_media_capabilities(
    endpoint: Option<&ResolvedProviderEndpointConfig>,
    policy: &ResolvedRuntimeModelPolicy,
    capabilities: Option<&ResolvedModelCapabilities>,
    available: bool,
    unavailable_reason: Option<&str>,
) -> MediaModelCapabilities {
    // The intrinsic projection precedes transport constraints; a missing lowering
    // must not turn "model accepts images" into "model does not accept images".
    let image_input = model_support(
        capabilities
            .map(|caps| {
                caps.intrinsic
                    .input_modalities
                    .contains(&ModelModality::Image)
            })
            .unwrap_or(policy.capabilities.image_input),
        ModelMetadataField::ImageInput,
        policy,
    );
    let image_output = model_support(
        capabilities
            .map(|caps| {
                caps.intrinsic
                    .output_modalities
                    .contains(&ModelModality::Image)
            })
            .unwrap_or(policy.capabilities.image_generation),
        ModelMetadataField::ImageGeneration,
        policy,
    );
    let transport = endpoint.map(|endpoint| endpoint.runtime_config.transport);
    MediaModelCapabilities {
        image_input,
        image_output,
        view_image: operation_availability(
            image_input,
            transport.is_some_and(|kind| kind.supports_view_image_observation_generation()),
            available,
            unavailable_reason,
        ),
        generate_image: operation_availability(
            image_output,
            transport.is_some_and(|kind| kind.supports_image_generation()),
            available,
            unavailable_reason,
        ),
        image_generation_contract: endpoint
            .filter(|_| image_output == MediaSupport::Supported)
            .map(|endpoint| {
                let route = crate::config::ModelRouteRef::new(
                    endpoint.provider.clone(),
                    endpoint.endpoint.clone(),
                    policy.model_ref.model.clone(),
                );
                resolve_image_generation_contract(
                    &route,
                    &endpoint.runtime_config,
                    capabilities
                        .map(|caps| caps.image_output)
                        .unwrap_or(policy.capabilities.image_generation),
                )
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_and_unknown_are_not_conflated() {
        let unavailable = operation_availability(
            MediaSupport::Supported,
            true,
            false,
            Some("credential_missing"),
        );
        assert_eq!(unavailable.support, MediaSupport::Supported);
        assert!(!unavailable.call_selectable);
        assert_eq!(
            unavailable.unavailable_reason.as_deref(),
            Some("credential_missing")
        );
        let unknown = operation_availability(MediaSupport::Unknown, true, true, None);
        assert_eq!(unknown.support, MediaSupport::Unknown);
        assert!(!unknown.call_selectable);
        let transport = operation_availability(MediaSupport::Supported, false, true, None);
        assert_eq!(transport.support, MediaSupport::Unsupported);
    }

    #[test]
    fn provider_support_means_at_least_one_observed_route() {
        let mut supported = MediaModelCapabilities::default();
        supported.image_input = MediaSupport::Supported;
        supported.view_image = operation_availability(MediaSupport::Supported, true, true, None);
        let unknown = MediaModelCapabilities::default();
        let summary = MediaProviderCapabilities::aggregate([&supported, &unknown]);
        assert_eq!(summary.image_input, MediaSupport::Supported);
        assert_eq!(summary.image_output, MediaSupport::Unknown);
        assert_eq!(summary.view_image_selectable_models, 1);
        assert_eq!(summary.observed_routes, 2);
        assert!(!summary.inventory_complete);
        assert_eq!(
            MediaProviderCapabilities::aggregate([]).image_input,
            MediaSupport::Unknown
        );
    }

    #[test]
    fn denied_tools_preserve_model_support_but_are_not_candidates() {
        let mut model = MediaModelCapabilities::default();
        model.image_input = MediaSupport::Supported;
        model.view_image = operation_availability(MediaSupport::Supported, true, true, None);
        model.deny_tool_access();
        assert_eq!(model.image_input, MediaSupport::Supported);
        assert!(!model.operation(MediaOperation::ViewImage).call_selectable);
        assert_eq!(
            model.view_image.unavailable_reason.as_deref(),
            Some("agent_capability_not_allowed")
        );
    }
}
