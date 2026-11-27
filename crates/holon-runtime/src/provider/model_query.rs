//! Shared Provider/Model query predicates and page handling.
//!
//! The Agent tools (`ListModelProviders` / `ListProviderModels`), the HTTP
//! routes (`GET /api/models/providers[...]`) and the CLI (`holon config
//! model-providers list`, `holon config models list`) all query the same
//! resolved projection. Availability, media capability and paging decisions
//! must therefore live here instead of being re-derived per surface.

use crate::types::{
    ModelAvailability, ModelProviderAvailability, ModelProviderEntry, ProviderModelEntry,
};
use holon_domain::MediaOperation;

/// Page size used when a caller does not pass `limit`.
pub const DEFAULT_PAGE_LIMIT: usize = 100;

/// Largest page a caller may request.
pub const MAX_PAGE_LIMIT: usize = 500;

/// The cursor did not match a model in the filtered set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCursor;

/// Resolve the effective page size, applying the shared default and bounds.
pub fn clamp_page_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

/// Provider group id shared by every Provider projection.
///
/// The default endpoint is the plain provider family; any other endpoint is
/// qualified as `family:endpoint`.
pub fn provider_group_id(provider_family: &str, endpoint: &str) -> String {
    if endpoint == "default" {
        provider_family.to_string()
    } else {
        format!("{provider_family}:{endpoint}")
    }
}

/// Whether a resolved model belongs to the given provider selector.
///
/// Accepts the group id exposed by the Provider list (which may contain `:`)
/// and the legacy `route_provider` id, matching the Agent tool contract.
pub fn model_matches_provider(model: &ProviderModelEntry, provider: &str) -> bool {
    model.route_provider == provider
        || provider_group_id(&model.provider_family, &model.endpoint) == provider
}

/// Whether a resolved provider belongs in a filtered provider list.
///
/// `include_unavailable` is the diagnostic view: it skips every filter, so a
/// requested `operation` is ignored rather than applied on top of it.
pub fn provider_matches(
    provider: &ModelProviderEntry,
    operation: Option<MediaOperation>,
    include_unavailable: bool,
) -> bool {
    include_unavailable
        || match operation {
            Some(operation) => provider.media.selectable_models(operation) > 0,
            None => provider.availability != ModelProviderAvailability::Unavailable,
        }
}

/// Whether a resolved model belongs in a filtered model list.
///
/// `include_unavailable` is the diagnostic view: it skips every filter, so a
/// requested `operation` is ignored rather than applied on top of it.
pub fn model_matches(
    model: &ProviderModelEntry,
    operation: Option<MediaOperation>,
    include_unavailable: bool,
) -> bool {
    include_unavailable
        || match operation {
            Some(operation) => model.media.operation(operation).call_selectable,
            None => model.availability != ModelAvailability::Unavailable,
        }
}

/// Slice one page out of an already-filtered model list.
///
/// The cursor is the `model_ref` of the last entry returned by the previous
/// page, so pages concatenate back into the full filtered sequence. A cursor
/// that no longer matches is rejected rather than silently restarting at the
/// first page, which would make paginated callers repeat entries.
pub fn page_models(
    models: Vec<ProviderModelEntry>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<(Vec<ProviderModelEntry>, Option<String>), InvalidCursor> {
    let limit = limit.clamp(1, MAX_PAGE_LIMIT);
    let start = match cursor {
        None => 0,
        Some(cursor) => {
            models
                .iter()
                .position(|model| model.model_ref == cursor)
                .ok_or(InvalidCursor)?
                + 1
        }
    };
    let mut page = models
        .into_iter()
        .skip(start)
        .take(limit + 1)
        .collect::<Vec<_>>();
    if page.len() <= limit {
        return Ok((page, None));
    }

    page.truncate(limit);
    let next_cursor = page.last().map(|model| model.model_ref.clone());
    Ok((page, next_cursor))
}

#[cfg(all(test, holon_lib_test_shard = "c"))]
mod tests {
    use super::*;
    use crate::types::{MediaModelCapabilities, MediaProviderCapabilities};
    use holon_domain::model_values::{ModelRef, ResolvedRuntimeModelPolicy};

    #[test]
    fn operation_filter_is_independent_of_text_turn_availability() {
        let mut image_only = model("test/image");
        image_only.availability = ModelAvailability::Unavailable;
        image_only.selectable = false;
        image_only.media.generate_image.call_selectable = true;
        assert!(model_matches(
            &image_only,
            Some(MediaOperation::GenerateImage),
            false
        ));
        assert!(!model_matches(&image_only, None, false));
        assert!(!model_matches(
            &image_only,
            Some(MediaOperation::ViewImage),
            false
        ));
        image_only.media.deny_tool_access();
        assert!(!model_matches(
            &image_only,
            Some(MediaOperation::GenerateImage),
            false
        ));
        assert!(model_matches(
            &image_only,
            Some(MediaOperation::GenerateImage),
            true
        ));
        assert!(!image_only.media.generate_image.call_selectable);
    }

    #[test]
    fn diagnostic_view_skips_operation_and_availability_filters() {
        let mut capable = model("test/capable");
        capable.media.generate_image.call_selectable = true;
        let unavailable_capable = {
            let mut entry = capable.clone();
            entry.availability = ModelAvailability::Unavailable;
            entry.selectable = false;
            entry
        };
        let mut unavailable_plain = model("test/plain");
        unavailable_plain.availability = ModelAvailability::Unavailable;
        unavailable_plain.selectable = false;
        let mut available_plain = model("test/text");
        available_plain.media.generate_image.call_selectable = false;

        let all = vec![
            capable,
            unavailable_capable,
            unavailable_plain,
            available_plain,
        ];
        let selected = |operation, include_unavailable| {
            all.iter()
                .filter(|entry| model_matches(entry, operation, include_unavailable))
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            selected(Some(MediaOperation::GenerateImage), true),
            vec!["capable", "capable", "plain", "text"]
        );
        assert_eq!(
            selected(None, true),
            vec!["capable", "capable", "plain", "text"]
        );
        // Capability and availability are independent: the unavailable clone
        // still advertises generate_image, so it stays in the capability view.
        assert_eq!(
            selected(Some(MediaOperation::GenerateImage), false),
            vec!["capable", "capable"]
        );
        assert_eq!(selected(None, false), vec!["capable", "text"]);
    }

    #[test]
    fn pagination_cursor_resumes_after_last_returned_model() {
        let models = vec![
            model("test/a"),
            model("test/b"),
            model("test/c"),
            model("test/d"),
        ];

        let (first_page, next_cursor) = page_models(models.clone(), None, 2).expect("first page");
        assert_eq!(
            refs(&first_page),
            vec!["test/a".to_string(), "test/b".to_string()]
        );
        assert_eq!(next_cursor.as_deref(), Some("test/b"));

        let (second_page, next_cursor) =
            page_models(models, next_cursor.as_deref(), 2).expect("second page");
        assert_eq!(
            refs(&second_page),
            vec!["test/c".to_string(), "test/d".to_string()]
        );
        assert_eq!(next_cursor, None);
    }

    #[test]
    fn pagination_pages_reassemble_the_full_sequence() {
        let models = (0..7)
            .map(|index| model(&format!("test/m{index}")))
            .collect::<Vec<_>>();
        let expected = refs(&models);

        for limit in [1, 2, 3] {
            let mut collected = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let (page, next_cursor) =
                    page_models(models.clone(), cursor.as_deref(), limit).expect("page");
                assert!(page.len() <= limit);
                collected.extend(refs(&page));
                match next_cursor {
                    Some(next) => {
                        assert_ne!(Some(next.clone()), cursor, "cursor must advance");
                        cursor = Some(next);
                    }
                    None => break,
                }
            }
            assert_eq!(collected, expected, "limit={limit}");
        }
    }

    #[test]
    fn pagination_rejects_unknown_cursor() {
        let models = vec![model("test/a"), model("test/b")];
        assert_eq!(
            page_models(models, Some("test/missing"), 1).unwrap_err(),
            InvalidCursor
        );
    }

    #[test]
    fn provider_filter_defaults_to_reachable_providers() {
        let available = provider("test-available", ModelProviderAvailability::Available, 2);
        let degraded = provider("test-degraded", ModelProviderAvailability::Degraded, 1);
        let unavailable = provider(
            "test-unavailable",
            ModelProviderAvailability::Unavailable,
            0,
        );
        let providers = [available, degraded, unavailable];

        let default_view = providers
            .iter()
            .filter(|entry| provider_matches(entry, None, false))
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(default_view, vec!["test-available", "test-degraded"]);

        let diagnostic_view = providers
            .iter()
            .filter(|entry| provider_matches(entry, None, true))
            .count();
        assert_eq!(diagnostic_view, providers.len());
    }

    #[test]
    fn provider_operation_filter_requires_selectable_media() {
        let mut with_image = provider("test-image", ModelProviderAvailability::Degraded, 1);
        with_image.media.generate_image_selectable_models = 1;
        let without_image = provider("test-text", ModelProviderAvailability::Available, 3);

        let matching = [&with_image, &without_image]
            .into_iter()
            .filter(|entry| provider_matches(entry, Some(MediaOperation::GenerateImage), false))
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(matching, vec!["test-image"]);
    }

    fn refs(models: &[ProviderModelEntry]) -> Vec<String> {
        models.iter().map(|entry| entry.model_ref.clone()).collect()
    }

    fn model(model_ref: &str) -> ProviderModelEntry {
        let policy = ResolvedRuntimeModelPolicy {
            model_ref: ModelRef::parse(model_ref).unwrap(),
            ..ResolvedRuntimeModelPolicy::default()
        };
        ProviderModelEntry {
            provider: "test".to_string(),
            provider_family: "test".to_string(),
            endpoint: "default".to_string(),
            route_provider: "test".to_string(),
            id: model_ref.trim_start_matches("test/").to_string(),
            model_ref: model_ref.to_string(),
            display_name: model_ref.to_string(),
            availability: ModelAvailability::Available,
            selectable: true,
            unavailable_reason: None,
            metadata_source: "test".to_string(),
            policy,
            parameter_contracts: Vec::new(),
            supported_parameters: Vec::new(),
            media: MediaModelCapabilities::default(),
            policy_notes: Vec::new(),
        }
    }

    fn provider(
        id: &str,
        availability: ModelProviderAvailability,
        selectable_models: usize,
    ) -> ModelProviderEntry {
        let mut media = MediaProviderCapabilities::default();
        media.view_image_selectable_models = selectable_models;
        ModelProviderEntry {
            id: id.to_string(),
            provider_family: id.to_string(),
            endpoint: "default".to_string(),
            route_provider: id.to_string(),
            display_name: Some(id.to_string()),
            availability,
            provider_configured: true,
            provider_source: None,
            transport: None,
            credential_source: None,
            credential_kind: None,
            credential_configured: true,
            default_model: None,
            model_count: selectable_models,
            discovered_model_count: 0,
            media,
            policy_notes: Vec::new(),
        }
    }
}
