use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::ResourceReadMaterial;

const BODY: &str = r#"{"external_trigger_id":"original-trigger","target_agent_id":"scheduling-agent","scope":"agent","delivery_mode":"wake_hint","token":"original-token","token_hash":"original-hash","status":"active","created_at":"2026-10-01T00:00:00.123456789Z","revoked_at":null,"last_delivered_at":null,"delivery_count":0}"#;
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "scheduling-agent".into(),
        identity_incarnation: 1,
    }
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn trigger() -> ExternalTriggerRecord {
    serde_json::from_str(BODY).unwrap()
}
fn cost() -> (u64, usize) {
    // Eleven keys/colons, ten commas, two braces, eight string values.
    ((BODY.len() as u64 * 4 + 42 * 128 + 512) * 8, (42 + 16) * 8)
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

#[test]
fn commit_record_and_effect_copies_share_exact_transaction_allowance() -> Result<()> {
    assert_eq!(serde_json::to_string(&trigger())?, BODY);
    let budget = ledger()?;
    let exact = add(add(header(), header()), add(cost(), cost()));
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let mut effects = PostCommitEffects::default();
    {
        let _entry = transaction.enter();
        let record = clone_trigger_record(&trigger())?;
        push_trigger_effect(&mut effects, &record)?;
        assert_eq!(effects.external_triggers, vec![record]);
    }
    assert_eq!(phase.finish()?, exact);
    effects
        .scheduling_material_budget_views
        .push(transaction.guard()?);
    drop((transaction, phase));
    assert_eq!(
        budget.report()?.observed_bytes,
        add(header(), add(cost(), cost())).0
    );
    drop(effects);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn every_commit_record_or_effect_copy_refuses_before_mutating_effects() -> Result<()> {
    for copy in 0..2 {
        let prefix = if copy == 0 {
            header()
        } else {
            add(header(), cost())
        };
        for byte_bound in [true, false] {
            let budget = ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if byte_bound {
                    prefix.0 + cost().0 - 1
                } else {
                    16 * 1024 * 1024
                },
                if byte_bound {
                    65536
                } else {
                    prefix.1 + cost().1 - 1
                },
            )?;
            let mut effects = PostCommitEffects::default();
            {
                let _entry = phase.enter();
                let error = if copy == 0 {
                    clone_trigger_record(&trigger()).unwrap_err()
                } else {
                    let record = clone_trigger_record(&trigger())?;
                    push_trigger_effect(&mut effects, &record).unwrap_err()
                };
                assert!(format!("{error:#}")
                    .to_lowercase()
                    .contains("resourceexhausted"));
                assert!(push_trigger_effect(&mut effects, &trigger()).is_err());
            }
            assert!(effects.external_triggers.is_empty());
            assert!(phase.finish().is_err());
            assert_eq!(budget.report()?.observed_bytes, prefix.0);
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn callback_hash_verification_preserves_exact_lowercase_semantics_without_material() -> Result<()> {
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    for token in ["", "abc", "quoted-\\\"-中文", "a\0b"] {
        let expected = crate::callbacks::hash_callback_token(token);
        let _entry = phase.enter();
        assert!(crate::callbacks::callback_token_hash_matches(
            token, &expected
        ));
        for invalid in [
            expected.to_uppercase(),
            expected[..63].into(),
            format!("{expected}0"),
            "é".repeat(32),
            "0".repeat(64),
        ] {
            assert!(!crate::callbacks::callback_token_hash_matches(
                token, &invalid
            ));
        }
        assert_eq!(phase.finish()?, header());
    }
    assert_eq!(
        crate::callbacks::hash_callback_token("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
