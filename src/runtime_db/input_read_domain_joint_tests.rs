use super::*;
use crate::runtime::RuntimeHandle;
use crate::runtime_db::task_snapshot::TaskSnapshotError;
use crate::runtime_db::work_snapshot::WorkSnapshotError;

fn policy(bytes: usize, records: usize) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: bytes,
        record_limit: records,
    }
}

#[tokio::test]
async fn actual_task_sql_and_held_input_allocation_use_one_phase_and_keep_new_guards() -> Result<()>
{
    RuntimeHandle::with_original_input_current_fixture_for_test("joint-task-input", |db, scope, state, ledger| {
        let baseline = ledger.report()?.observed_bytes;
        let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
        let allocation = 1024 * 1024;
        let read = |input: &mut InputReadMaterial<'_>| {
            input.with_task_material(scope, ledger, |task| {
                db.transaction(|tx| crate::runtime_db::task_recovery::prepare_original_task_domains_using_material_tx(
                    tx, scope, state, &crate::runtime::resident_task::TaskCaptureRequest::current(),
                    &[], full.record_limit, full.byte_limit, ledger, None, task,
                ))
            })
        };
        // Measure the actual SQL path, not a mirrored cost calculation.
        let mut alone = InputReadMaterial::new(scope, &full, ledger, (0, 0))?;
        alone.observe_projected(u64::try_from(allocation)?, 1)?;
        let measured_held = vec![0u8; allocation];
        let prior = alone.usage();
        let actual = read(&mut alone)?;
        let added = (alone.usage().0 - prior.0, alone.usage().1 - prior.1);
        assert!(added.0 > 0 && added.1 > 0);
        drop((actual, alone, measured_held));
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        for by_records in [false, true] {
            let limited = policy(
                if by_records { full.byte_limit } else { prior.0 + added.0 - 1 },
                if by_records { prior.1 + added.1 - 1 } else { full.record_limit },
            );
            let mut input = InputReadMaterial::new(scope, &limited, ledger, (0, 0))?;
            input.observe_projected(u64::try_from(allocation)?, 1)?;
            let held = vec![0u8; allocation];
            let error = read(&mut input).err().context("Task received another full phase allowance")?;
            assert!(matches!(error.downcast_ref::<TaskSnapshotError>(), Some(TaskSnapshotError::ResourceExhausted(_))), "{error:#}");
            assert_eq!(held.len(), allocation);
            drop((held, input));
            assert_eq!(ledger.report()?.observed_bytes, baseline);
        }
        let mut input = InputReadMaterial::new(scope, &full, ledger, (0, 0))?;
        input.observe_projected(u64::try_from(allocation)?, 1)?;
        let held = vec![0u8; allocation];
        let before = input.usage();
        let actual = read(&mut input)?;
        assert!(input.usage().0 > before.0 && input.usage().1 > before.1);
        let guard = input.guards.last().context("Task did not return its actual guard")?.clone();
        let weak = Arc::downgrade(&guard);
        drop((actual, input, held));
        assert!(weak.upgrade().is_some());
        drop(guard);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        Ok(())
    }).await
}

#[tokio::test]
async fn actual_work_decoder_uses_input_remaining_before_its_selector_and_body_allocations(
) -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test(
        "joint-work-input",
        |db, scope, state, ledger| {
            let baseline = ledger.report()?.observed_bytes;
            let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
            let read = |input: &mut InputReadMaterial<'_>| {
                db.transaction(|tx| {
                    input.prepare_original_work_tx(
                        tx,
                        scope,
                        state,
                        &crate::runtime::resident_work::WorkCaptureRequest::current(),
                        None,
                        &[],
                        false,
                        ledger,
                    )
                })
            };
            let mut alone = InputReadMaterial::new(scope, &full, ledger, (0, 0))?;
            let allocation = 1024 * 1024;
            alone.observe_projected(u64::try_from(allocation)?, 1)?;
            let measured_held = vec![0u8; allocation];
            let prior = alone.usage();
            let (actual, selected) = read(&mut alone)?;
            let added = (alone.usage().0 - prior.0, alone.usage().1 - prior.1);
            assert!(added.0 > 0 && added.1 > 0);
            drop((actual, selected, alone, measured_held));
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            for by_records in [false, true] {
                let limited = policy(
                    if by_records {
                        full.byte_limit
                    } else {
                        prior.0 + added.0 - 1
                    },
                    if by_records {
                        prior.1 + added.1 - 1
                    } else {
                        full.record_limit
                    },
                );
                let mut input = InputReadMaterial::new(scope, &limited, ledger, (0, 0))?;
                input.observe_projected(u64::try_from(allocation)?, 1)?;
                let held = vec![0u8; allocation];
                let error = read(&mut input)
                    .err()
                    .context("Work decoder ignored held Input material")?;
                assert!(
                    matches!(
                        error.downcast_ref::<WorkSnapshotError>(),
                        Some(WorkSnapshotError::ResourceExhausted(_))
                    ),
                    "{error:#}"
                );
                assert_eq!(held.len(), allocation);
                drop((held, input));
                assert_eq!(ledger.report()?.observed_bytes, baseline);
            }
            let mut input = InputReadMaterial::new(scope, &full, ledger, (0, 0))?;
            input.observe_projected(u64::try_from(allocation)?, 1)?;
            let held = vec![0u8; allocation];
            let (actual, selected) = read(&mut input)?;
            let guard = actual
                .material_views
                .last()
                .context("Work actual guard absent")?
                .clone();
            let weak = Arc::downgrade(&guard);
            drop((actual, selected, input, held));
            assert!(weak.upgrade().is_some());
            drop(guard);
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn foreign_joint_task_and_work_ledger_are_rejected_before_any_source_sql() -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test(
        "joint-domains-foreign",
        |db, scope, state, ledger| {
            let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
            let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
            let mut input = InputReadMaterial::new(scope, &full, ledger, (0, 0))?;
            db.connection()?.execute_batch(
                "ALTER TABLE agent_states RENAME TO joint_task_work_state_forbidden",
            )?;
            let before = input.usage();
            let error = input
                .with_task_material(scope, &foreign, |_| -> Result<()> {
                    anyhow::bail!("foreign ledger reached Task reader")
                })
                .unwrap_err();
            assert!(
                error.to_string().contains("another original ledger"),
                "{error:#}"
            );
            let error = db
                .transaction(|tx| {
                    input.prepare_original_work_tx(
                        tx,
                        scope,
                        state,
                        &crate::runtime::resident_work::WorkCaptureRequest::current(),
                        None,
                        &[],
                        false,
                        &foreign,
                    )
                })
                .err()
                .context("foreign Work phase accepted")?;
            assert!(
                error.to_string().contains("another original ledger"),
                "{error:#}"
            );
            assert_eq!(input.usage(), before);
            assert_eq!(foreign.report()?.observed_bytes, 0);
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn original_task_joint_rejects_large_damaged_state_before_first_native_or_owned_decode(
) -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test(
        "joint-task-first-state",
        |db, scope, state, ledger| {
            let limits = policy(256 * 1024, MAX_RECORDS);
            let baseline = ledger.report()?.observed_bytes;
            let original: String = db.connection()?.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                [&scope.agent_id],
                |row| row.get(0),
            )?;
            db.connection()?.execute(
                "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
                rusqlite::params![
                    "[".to_owned() + &"x".repeat(9 * 1024 * 1024),
                    scope.agent_id
                ],
            )?;
            let mut input = InputReadMaterial::new(scope, &limits, ledger, (0, 0))?;
            let error = input
                .with_task_material(scope, ledger, |task| {
                    db.transaction(|tx|
            crate::runtime_db::task_recovery::prepare_original_task_domains_using_material_tx(
                tx,scope,state,&crate::runtime::resident_task::TaskCaptureRequest::current(),&[],
                limits.record_limit,limits.byte_limit,ledger,None,task,
            )
        )
                })
                .err()
                .context("Task first scope decoder bypassed the joint gate")?;
            assert!(
                matches!(
                    error.downcast_ref::<TaskSnapshotError>(),
                    Some(TaskSnapshotError::ResourceExhausted(_))
                ),
                "{error:#}"
            );
            drop(input);
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            db.connection()?.execute(
                "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
                rusqlite::params![original, scope.agent_id],
            )?;
            Ok(())
        },
    )
    .await
}
