use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "image-trace".into(),
        identity_incarnation: 1,
    }
}

#[test]
fn image_trace_buffers_keep_original_material_through_last_clone() -> anyhow::Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source = ResourceReadMaterial::new(&ledger, &scope())?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let home = tempfile::tempdir()?;
    let trace = ProviderHttpTrace::failure_only_for_tests(home.path());
    let request = trace
        .begin_request_with_material(
            Some("image-trace"),
            "test",
            Some("test"),
            "http://localhost",
            "images",
            &[("authorization", "secret".into())],
            &json!({"image_url":"data:image/png;base64,YWJj", "api_key":"secret"}),
            &material,
        )?
        .expect("trace");
    request.write_response_body("unpersisted response");
    source.finish()?;
    {
        let state = request.inner.lock().unwrap();
        assert_eq!(state.events.len(), 2);
        assert_eq!(state.events[0]["body"]["api_key"], "[REDACTED]");
        let wire = serde_json::to_string(&state.events)?;
        assert!(!wire.contains("secret"));
        assert!(!wire.contains("material"));
        assert_eq!(state.events[1]["body"], "unpersisted response");
    }
    let weak = Arc::downgrade(&source.guard()?);
    let last = request.clone();
    drop((request, material, source));
    assert!(weak.upgrade().is_some());
    assert!(home.path().read_dir()?.next().is_none());
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn image_trace_refuses_before_capturing_body_or_creating_files() -> anyhow::Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let initial_bytes = (scope.agent_id.len() as u64 + 256) * 8;
    let source = ResourceReadMaterial::with_test_limits(&ledger, &scope, initial_bytes, 64)?;
    let material = ProviderRequestMaterial::scoped(source.clone());
    let home = tempfile::tempdir()?;
    let trace = ProviderHttpTrace::failure_only_for_tests(home.path());
    assert!(trace
        .begin_request_with_material(
            Some("image-trace"),
            "test",
            None,
            "http://localhost",
            "images",
            &[],
            &json!({"body": "must not be captured"}),
            &material,
        )
        .is_err());
    assert!(source.finish().is_err());
    assert!(home.path().read_dir()?.next().is_none());
    drop((source, material));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
