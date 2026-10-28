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
fn registry_model_catalog_reuses_config_publication_and_preserves_old_holders() -> Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let db = RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
    let registry = RuntimeRegistry::new(config, db)?;
    let old_config = registry.config();
    let models = registry.model_catalog();
    let old_models = Arc::downgrade(&models);
    let scope = InputEvidenceScope {
        agent_id: "registry-model".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let baseline = ResourceReadMaterial::new(&ledger, &scope)?;
    let allowance = baseline.finish()?;
    drop(baseline);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, allowance.0, allowance.1)?;
    phase.read(|| {
        let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
        for _ in 0..3 {
            assert!(Arc::ptr_eq(&models, &registry.model_catalog()));
            assert!(Arc::ptr_eq(&old_config, &registry.config()));
        }
        assert_eq!(denied.attempts(), 0);
        Ok(())
    })?;
    assert_eq!(
        phase.finish()?,
        allowance,
        "catalog read must not rebuild any config material"
    );
    drop(phase);
    let mut next = (*old_config).clone();
    next.default_model = crate::config::ModelRouteRef::parse("openai@default/gpt-6.1")?;
    registry.replace_config(next);
    let published = registry.inner.config.load_full();
    assert_eq!(
        published.config.default_model,
        published.models.default_model
    );
    assert_ne!(published.models.default_model, models.default_model);
    assert!(!Arc::ptr_eq(&published.models, &models));
    assert_eq!(models.default_model, old_config.default_model);
    drop(models);
    assert!(old_models.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
