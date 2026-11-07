use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

#[test]
fn metadata_selection_borrows_only_winners_before_any_owning_material() -> anyhow::Result<()> {
    let catalog = BuiltInModelCatalog::new();
    let model_ref = ModelRef::new(ProviderId::openai(), "gpt-5.4");
    let mut discovered = catalog.get(&model_ref).unwrap().clone();
    discovered.description = "selected remote description".into();
    let discovered_models = HashMap::from([(model_ref.clone(), discovered)]);
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
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: "metadata-selection".into(),
        identity_incarnation: 1,
    };
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    let initial = phase.finish()?;
    let expected = catalog.resolve_policy(
        &model_ref,
        &overrides,
        &discovered_models,
        Some(&unused),
        &context,
        8192,
    );
    let selected = phase.read(|| {
        Ok(catalog.resolve_metadata(
            &model_ref,
            None,
            catalog.get(&model_ref),
            &overrides,
            &discovered_models,
            Some(&unused),
            &context,
            8192,
            None,
        ))
    })?;
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
            .get(&model_ref)
            .unwrap()
            .description
            .as_str(),
    ));
    assert_eq!(
        phase.finish()?,
        initial,
        "selection must not own candidate or evidence copies"
    );
    assert_eq!(phase.read(|| selected.try_into_policy())?, expected);
    let used = phase.finish()?;
    assert!(used.0 > initial.0 && used.1 > initial.1);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
