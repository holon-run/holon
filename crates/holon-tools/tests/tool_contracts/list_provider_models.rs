use holon_domain::model_values::{ModelRef, ResolvedRuntimeModelPolicy};
use holon_domain::{MediaOperation, ModelAvailability, ProviderModelEntry};
use holon_tools::builtins::list_provider_models::{model_matches_operation, page_provider_models};

#[test]
fn operation_filter_is_independent_of_text_turn_availability() {
    let mut image_only = model("test/image");
    image_only.availability = ModelAvailability::Unavailable;
    image_only.selectable = false;
    image_only.media.generate_image.call_selectable = true;
    assert!(model_matches_operation(
        &image_only,
        Some(MediaOperation::GenerateImage),
        false
    ));
    assert!(!model_matches_operation(&image_only, None, false));
    assert!(!model_matches_operation(
        &image_only,
        Some(MediaOperation::ViewImage),
        false
    ));
    image_only.media.deny_tool_access();
    assert!(!model_matches_operation(
        &image_only,
        Some(MediaOperation::GenerateImage),
        false
    ));
    assert!(model_matches_operation(
        &image_only,
        Some(MediaOperation::GenerateImage),
        true
    ));
    assert!(!image_only.media.generate_image.call_selectable);
}

#[test]
fn pagination_cursor_resumes_after_last_returned_model() {
    let models = vec![
        model("test/a"),
        model("test/b"),
        model("test/c"),
        model("test/d"),
    ];

    let (first_page, next_cursor) = page_provider_models(models.clone(), None, 2);
    assert_eq!(
        first_page
            .iter()
            .map(|entry| entry.model_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["test/a", "test/b"]
    );
    assert_eq!(next_cursor.as_deref(), Some("test/b"));

    let (second_page, next_cursor) = page_provider_models(models, next_cursor.as_deref(), 2);
    assert_eq!(
        second_page
            .iter()
            .map(|entry| entry.model_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["test/c", "test/d"]
    );
    assert_eq!(next_cursor, None);
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
        media: Default::default(),
        policy_notes: Vec::new(),
    }
}

#[test]
fn old_model_result_deserializes_without_media_projection() {
    let mut value = serde_json::to_value(model("test/image")).unwrap();
    value.as_object_mut().unwrap().remove("media");
    let old: ProviderModelEntry = serde_json::from_value(value).unwrap();
    assert_eq!(old.media, Default::default());
}
