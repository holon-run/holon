//! Operation-scoped media discovery. Model support is not execution authority.

use crate::{
    config::{ResolvedModelCapabilities, ResolvedProviderEndpointConfig},
    model_catalog::{
        ModelMetadataField, ModelMetadataOrigin, ModelModality, ResolvedRuntimeModelPolicy,
    },
};

use super::image_generation::resolve_image_generation_contract;

pub use holon_domain::{
    MediaModelCapabilities, MediaOperation, MediaOperationAvailability, MediaProviderCapabilities,
    MediaSupport, MediaToolDefaults,
};

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

#[cfg(holon_lib_test_shard = "c")]
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
