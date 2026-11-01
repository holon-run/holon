use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime::{
    input_evidence_budget::InputEvidenceBudgetLedger, resident_input_evidence::InputEvidenceScope,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "scheduling-agent".into(),
        identity_incarnation: 1,
    }
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn at() -> DateTime<Utc> {
    "2026-10-01T00:00:00.123456789Z".parse().unwrap()
}

fn cost(text: &str) -> (u64, usize) {
    // All fixture strings are ASCII without JSON escapes: one string slot.
    (((text.len() as u64 + 2) * 4 + 128 + 512) * 8, 17 * 8)
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn build() -> Result<ExternalTriggerRecord> {
    default_external_trigger_record(
        &scope().agent_id,
        CallbackDeliveryMode::WakeHint,
        at(),
        "borrowed-id",
        "borrowed-token",
    )
}
fn costs() -> [(u64, usize); 4] {
    [
        cost("borrowed-id"),
        cost(&scope().agent_id),
        cost("borrowed-token"),
        cost("0000000000000000000000000000000000000000000000000000000000000000"),
    ]
}

#[test]
fn default_body_owns_each_borrowed_field_once_in_original_transaction() -> Result<()> {
    let budget = ledger()?;
    let body = costs().into_iter().fold((0, 0), add);
    let exact = add(add(header(), header()), body);
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let record = {
        let _entry = transaction.enter();
        build()?
    };
    assert_eq!(record.external_trigger_id, "borrowed-id");
    assert_eq!(record.token.as_deref(), Some("borrowed-token"));
    assert_eq!(record.target_agent_id, scope().agent_id);
    assert_eq!(
        record.token_hash,
        crate::callbacks::hash_callback_token("borrowed-token")
    );
    assert_eq!(phase.finish()?, exact);
    assert_eq!(transaction.finish()?, add(header(), body));
    drop((record, transaction));
    assert_eq!(budget.report()?.observed_bytes, header().0);
    // Releasing the transaction does not refund the original allowance.
    assert!(phase.transaction().is_err());
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn every_default_body_allocation_refuses_bytes_and_records_and_releases_prefix() -> Result<()> {
    let mut prefix = header();
    for next in costs() {
        for byte_bound in [true, false] {
            let budget = ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if byte_bound {
                    prefix.0 + next.0 - 1
                } else {
                    16 * 1024 * 1024
                },
                if byte_bound {
                    65536
                } else {
                    prefix.1 + next.1 - 1
                },
            )?;
            {
                let _entry = phase.enter();
                let error = build().unwrap_err();
                assert!(format!("{error:#}")
                    .to_lowercase()
                    .contains("resourceexhausted"));
                assert!(build().is_err());
            }
            assert!(phase.finish().is_err());
            assert_eq!(budget.report()?.observed_bytes, prefix.0);
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
        prefix = add(prefix, next);
    }
    Ok(())
}
