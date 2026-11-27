// The operation/availability predicates and paging now live in
// `holon_runtime::provider::model_query` so the Agent tools, HTTP routes and
// CLI share one implementation; their coverage lives with that module. This
// test keeps the tool payload compatibility check.

use holon_domain::model_values::{ModelRef, ResolvedRuntimeModelPolicy};
use holon_domain::{ModelAvailability, ProviderModelEntry};

#[test]
fn old_model_result_deserializes_without_media_projection() {
    let mut value = serde_json::to_value(model("test/image")).unwrap();
    value.as_object_mut().unwrap().remove("media");
    let old: ProviderModelEntry = serde_json::from_value(value).unwrap();
    assert_eq!(old.media, Default::default());
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
