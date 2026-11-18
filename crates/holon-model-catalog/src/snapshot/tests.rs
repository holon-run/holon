use super::*;

#[test]
#[ignore = "writes the checked-in built-in registry snapshot"]
fn regenerate_builtin_snapshot() {
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    let snapshot = RegistrySnapshot::from_catalog("builtin-2026-08-28", &catalog);
    let json = serde_json::to_string_pretty(&snapshot).expect("snapshot must serialize");
    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../holon-domain/src/builtin_snapshot_v1.json"
        ),
        format!("{json}\n"),
    )
    .expect("snapshot must be writable");
}

#[test]
fn built_in_snapshot_matches_legacy_catalog() {
    let snapshot = legacy_catalog().expect("built-in snapshot must be valid");
    let legacy = BuiltInModelCatalog::from_legacy_definitions();
    assert_eq!(snapshot, legacy);
}

#[test]
fn built_in_catalog_includes_checked_in_supplement() {
    let catalog = built_in_catalog().expect("built-in snapshot must be valid");
    let supplement =
        crate::models_dev::supplement::ModelsDevSupplement::parse(MODELS_DEV_SUPPLEMENT)
            .expect("checked-in supplement must parse");
    for model in &supplement.models {
        let entry = catalog.get(&model.model_ref).unwrap_or_else(|| {
            panic!(
                "supplement model {} must merge",
                model.model_ref.as_string()
            )
        });
        assert_eq!(
            entry.source,
            crate::ModelMetadataSource::ModelsDevSupplement
        );
        let route = ModelRouteRef::new(
            model.model_ref.provider.clone(),
            holon_domain::model_values::ProviderEndpointId::default_endpoint(),
            model.model_ref.model.clone(),
        );
        assert!(
            catalog.get_route(&route).is_some(),
            "supplement model {} must have a default route",
            model.model_ref.as_string()
        );
    }
}

#[test]
fn rejects_unknown_schema_version() {
    let raw = BUILT_IN_SNAPSHOT.replacen("\"schema_version\": 1", "\"schema_version\": 999", 1);
    let error = parse_and_validate(&raw).expect_err("unknown version must fail");
    assert!(error.contains("unsupported schema version"));
}

#[test]
fn rejects_unknown_fields() {
    let raw =
        BUILT_IN_SNAPSHOT.replacen("\"revision\":", "\"unexpected\": true,\n  \"revision\":", 1);
    let error = parse_and_validate(&raw).expect_err("unknown fields must fail");
    assert!(error.contains("unknown field"));
}

#[test]
fn rejects_duplicate_models() {
    let mut snapshot =
        serde_json::from_str::<RegistrySnapshot>(BUILT_IN_SNAPSHOT).expect("valid fixture");
    snapshot.models.push(snapshot.models[0].clone());
    let raw = serde_json::to_string(&snapshot).expect("snapshot must serialize");
    let error = parse_and_validate(&raw).expect_err("duplicate model must fail");
    assert!(error.contains("duplicate model"));
}

#[test]
fn rejects_route_capability_expansion() {
    let mut snapshot =
        serde_json::from_str::<RegistrySnapshot>(BUILT_IN_SNAPSHOT).expect("valid fixture");
    let route = snapshot
        .routes
        .iter_mut()
        .find(|route| {
            let model = snapshot
                .models
                .iter()
                .find(|model| model.model_ref == route.route_ref.model_ref())
                .expect("route model");
            !model.capabilities.image_generation
        })
        .expect("route with unsupported image generation");
    route.policy.capabilities.image_generation = Some(true);
    let raw = serde_json::to_string(&snapshot).expect("snapshot must serialize");
    let error = parse_and_validate(&raw).expect_err("capability expansion must fail");
    assert!(error.contains("cannot enable intrinsic capability image_generation"));
}
