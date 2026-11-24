use super::*;
use holon_system::resource_material::{ResourceObserver, ResourceObserverScope};
use std::{cell::Cell, io};

thread_local! {
    static MATERIAL: Cell<(u64, usize)> = const { Cell::new((0, 0)) };
}

fn charge_metadata(bytes: u64, records: usize) -> io::Result<()> {
    MATERIAL.with(|material| {
        let (used_bytes, used_records) = material.get();
        material.set((used_bytes + bytes, used_records + records));
    });
    Ok(())
}

#[test]
fn metadata_selection_borrows_only_winners_before_any_owning_material() -> anyhow::Result<()> {
    let catalog = BuiltInModelCatalog::new();
    let model_ref = ModelRef::new(ProviderId::openai(), "gpt-5.4");
    let mut discovered = catalog.get(&model_ref).unwrap().clone();
    discovered.description = "selected remote description".into();
    let discovered_models = discovered_by_route([(model_ref.clone(), discovered)]);
    let overrides = HashMap::from([(
        model_ref.clone(),
        ModelRuntimeOverride {
            display_name: Some("selected override".into()),
            ..Default::default()
        },
    )]);
    let unused = ModelRuntimeOverride {
        display_name: Some("unused-汉🦀".repeat(64 * 1024)),
        description: Some("unused".repeat(64 * 1024)),
        ..Default::default()
    };
    let context = ContextConfig::default();
    let expected = catalog.resolve_policy(
        &model_ref,
        &overrides,
        &discovered_models,
        Some(&unused),
        &context,
        8192,
    );
    let _observer = ResourceObserverScope::enter(Some(ResourceObserver {
        payload: |bytes| charge_metadata(bytes, 0),
        buffer: |bytes| charge_metadata(bytes, 0),
        metadata: charge_metadata,
    }));
    let selected = catalog.resolve_metadata(
        &model_ref,
        None,
        catalog.get(&model_ref),
        &overrides,
        &discovered_models,
        Some(&unused),
        &context,
        8192,
        None,
    );
    assert!(std::ptr::eq(
        selected.display_name.unwrap(),
        overrides
            .get(&model_ref)
            .unwrap()
            .display_name
            .as_ref()
            .unwrap(),
    ));
    assert!(std::ptr::eq(
        selected.description,
        discovered_models
            .get(&ModelRouteRef::from_legacy_model_ref(&model_ref))
            .unwrap()
            .description
            .as_str(),
    ));
    assert_eq!(
        MATERIAL.get(),
        (0, 0),
        "selection must not own candidate or evidence copies"
    );
    assert_eq!(selected.try_into_policy()?, expected);
    let (bytes, records) = MATERIAL.get();
    assert!(bytes > 0 && records > 0);
    Ok(())
}
