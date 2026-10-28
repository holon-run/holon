use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime_db::scheduling_snapshot::tests::{at, ledger, scope, trigger};
use crate::runtime_db::transitions::PreparedExternalTriggerTransition;
use crate::types::CallbackIngressDisposition;

const BODY: &str = r#"{"external_trigger_id":"original-trigger","target_agent_id":"scheduling-agent","scope":"agent","delivery_mode":"wake_hint","token":"original-token","token_hash":"original-hash","status":"active","created_at":"2026-10-01T00:00:00.123456789Z","revoked_at":null,"last_delivered_at":null,"delivery_count":0}"#;

// Independent serialized lengths/slots, never measured constructor usage.
fn cost(bytes: usize, slots: usize) -> (u64, usize) {
    (
        ((bytes as u64 * 4 + slots as u64 * 128 + 512) * 8),
        (slots + 16) * 8,
    )
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn copies(operation: usize) -> Vec<(u64, usize)> {
    let mut costs = Vec::new();
    if operation < 2 {
        costs.push(cost(scope().agent_id.len() + 2, 1));
        costs.push(cost(64 + 2, 1));
    }
    // Eleven keys/colons, ten commas, two braces, eight string values.
    costs.push(cost(BODY.len(), 42));
    costs
}
fn observation(
    phase: &ResourceReadMaterial,
    budget: &InputEvidenceBudgetLedger,
    record: Option<ExternalTriggerRecord>,
) -> Result<ExternalTriggerWriterObservation> {
    let _entry = phase.enter();
    Ok(ExternalTriggerWriterObservation {
        scope: scope(),
        record,
        bytes: 0,
        records: 0,
        ledger: budget.clone(),
        guards: Vec::new(),
        phase: ResourceReadMaterial::current_or_new(budget, &scope())?,
    })
}
fn build(
    operation: usize,
    observed: ExternalTriggerWriterObservation,
) -> Result<PreparedExternalTriggerTransition> {
    match operation {
        0 => PreparedExternalTriggerTransition::ensure(
            observed,
            "new-id".into(),
            "new-token".into(),
            at(),
            None,
        ),
        1 => PreparedExternalTriggerTransition::reset(
            observed,
            "new-id".into(),
            "new-token".into(),
            at(),
            None,
        ),
        2 => PreparedExternalTriggerTransition::revoke(observed, at(), None),
        _ => PreparedExternalTriggerTransition::account(
            observed,
            at(),
            CallbackIngressDisposition::Ignored,
            None,
        ),
    }
}

#[test]
fn four_command_bodies_resume_exact_original_allowance_and_keep_last_holder() -> Result<()> {
    assert_eq!(serde_json::to_string(&trigger())?, BODY);
    for operation in 0..4 {
        let budget = ledger()?;
        let exact = copies(operation).into_iter().fold(header(), add);
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let observed = observation(&phase, &budget, Some(trigger()))?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let command = build(operation, observed)?;
        assert_eq!(phase.finish()?, exact);
        let body = serde_json::to_value(&command)?;
        assert_eq!(
            body["expected"],
            serde_json::from_str::<serde_json::Value>(BODY)?
        );
        if operation < 2 {
            assert_eq!(body["fresh"]["target_agent_id"], scope().agent_id);
            assert_eq!(body["fresh"]["token"], "new-token");
            assert_eq!(
                body["fresh"]["token_hash"],
                crate::callbacks::hash_callback_token("new-token")
            );
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(command);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn every_body_copy_refuses_bytes_and_records_before_allocation() -> Result<()> {
    for operation in 0..4 {
        let mut prefix = header();
        for next in copies(operation) {
            for bytes_bound in [true, false] {
                let budget = ledger()?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &budget,
                    &scope(),
                    if bytes_bound {
                        prefix.0 + next.0 - 1
                    } else {
                        16 * 1024 * 1024
                    },
                    if bytes_bound {
                        65536
                    } else {
                        prefix.1 + next.1 - 1
                    },
                )?;
                let observed = observation(&phase, &budget, Some(trigger()))?;
                let error = build(operation, observed).unwrap_err();
                assert!(
                    format!("{error:#}")
                        .to_lowercase()
                        .contains("resourceexhausted"),
                    "{error:#}"
                );
                assert!(phase.finish().is_err());
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                drop(phase);
                assert_eq!(budget.report()?.observed_bytes, 0);
            }
            prefix = add(prefix, next);
        }
    }
    Ok(())
}

#[test]
fn failed_original_command_phase_cannot_refresh_and_restores_unrelated_tls() -> Result<()> {
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    let first = observation(&phase, &budget, Some(trigger()))?;
    let second = observation(&phase, &budget, Some(trigger()))?;
    let unrelated = ResourceReadMaterial::new(&budget, &scope())?;
    let before = unrelated.finish()?;
    {
        let _entry = unrelated.enter();
        for observed in [first, second] {
            assert!(build(0, observed).is_err());
            assert!(phase.finish().is_err());
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &unrelated.guard()?,
            ));
            assert_eq!(unrelated.finish()?, before);
        }
    }
    drop((phase, unrelated));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn missing_record_skips_clone_and_preserves_original_not_found_errors() -> Result<()> {
    for operation in 0..4 {
        let budget = ledger()?;
        let exact = copies(operation)
            .into_iter()
            .take(if operation < 2 { 2 } else { 0 })
            .fold(header(), add);
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let observed = observation(&phase, &budget, None)?;
        let result = build(operation, observed);
        if operation < 2 {
            let command = result?;
            assert!(serde_json::to_value(&command)?["expected"].is_null());
            drop(command);
        } else {
            assert!(format!("{:#}", result.unwrap_err()).contains("external trigger not found"));
        }
        assert_eq!(phase.finish()?, exact);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
