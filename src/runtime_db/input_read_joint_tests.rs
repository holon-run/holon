use super::*;
use crate::runtime::RuntimeHandle;
use crate::runtime_db::current_read_snapshot::{
    prepare_current_read_snapshot_tx, prepare_current_read_using_input_material_tx,
};

#[tokio::test]
async fn actual_current_decoder_and_original_input_allocation_share_one_remaining_phase(
) -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test("joint-current-input", |db, scope, state, ledger| {
    let baseline = ledger.report()?.observed_bytes;
    let allocation = 1024 * 1024;
    for positive in [false, true] {
        let policy = InputEvidenceLoadRequest {
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            windows: Vec::new(),
            byte_limit: if positive {
                2 * 1024 * 1024
            } else {
                allocation + 16 * 1024
            },
            record_limit: MAX_RECORDS,
        };
        let request = CurrentReadRequest::current(policy.byte_limit, policy.record_limit);
        // The real Current SQL decoder fits this allowance on its own.
        let standalone = db.transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, scope, state, &request, ledger)
        })?;
        drop(standalone);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        let revision: u64 = db.connection()?.query_row("SELECT revision FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=?2", rusqlite::params![scope.agent_id,scope.identity_incarnation], |r|r.get(0))?;
        let result = db.transaction(|tx| {
            let mut input = InputReadMaterial::new(scope, &policy, ledger, (0, 0))?;
            input.observe_projected(u64::try_from(allocation)?, 1)?;
            // This is an actual original Input allocation, admitted before its
            // first Vec and kept through the Current handoff, not a numeric prior.
            let held_input = vec![0u8; allocation];
            let before = input.usage();
            let mut remaining = request.clone();
            input.narrow_current_request(&mut remaining)?;
            let current = prepare_current_read_using_input_material_tx(
                tx,
                scope,
                state,
                &remaining,
                ledger,
                &mut input,
                false,
            )?;
            assert!(input.usage().0 > before.0 && input.usage().1 > before.1);
            let last = Arc::downgrade(
                current
                    ._material
                    .last()
                    .context("original Current guard missing")?,
            );
            let guards = current._material.clone();
            drop((current, input, held_input));
            assert!(last.upgrade().is_some());
            drop(guards);
            assert!(last.upgrade().is_none());
            Ok(())
        });
        if positive {
            result?;
        } else {
            let error = result
                .err()
                .context("joint phase ignored original held Input allocation")?;
            assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::ResourceExhausted)
                ),
                "{error:#}"
            );
            assert_eq!(db.connection()?.query_row("SELECT revision FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=?2",rusqlite::params![scope.agent_id,scope.identity_incarnation],|r|r.get::<_,u64>(0))?,revision);
        }
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    Ok(())
    }).await
}

#[tokio::test]
async fn foreign_input_phase_is_rejected_before_actual_current_source_sql() -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test(
        "joint-current-foreign",
        |db, scope, state, ledger| {
            let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
            let policy = InputEvidenceLoadRequest {
                mandatory_roots: Default::default(),
                interrupted_sources: Default::default(),
                selected_completion_brief_ids: Default::default(),
                windows: Vec::new(),
                byte_limit: MAX_METADATA_BYTES,
                record_limit: MAX_RECORDS,
            };
            let mut input = InputReadMaterial::new(scope, &policy, ledger, (0, 0))?;
            let mut request = CurrentReadRequest::current(policy.byte_limit, policy.record_limit);
            input.narrow_current_request(&mut request)?;
            db.connection()?.execute_batch(
                "ALTER TABLE agent_states RENAME TO joint_current_state_forbidden",
            )?;
            let before = input.usage();
            let error = db
                .transaction(|tx| {
                    prepare_current_read_using_input_material_tx(
                        tx, scope, state, &request, &foreign, &mut input, false,
                    )
                })
                .err()
                .context("foreign phase accepted")?;
            assert!(
                error.to_string().contains("another Input ledger"),
                "{error:#}"
            );
            assert_eq!(input.usage(), before);
            assert_eq!(foreign.report()?.observed_bytes, 0);
            Ok(())
        },
    )
    .await
}
