use super::*;
use crate::runtime::RuntimeHandle;
use crate::runtime_db::task_snapshot::{read_preparation::TaskReadMaterial, TaskDomainLoadRequest};
use anyhow::Context;

#[tokio::test]
async fn original_external_task_wraps_the_whole_input_read_under_its_remaining_allowance(
) -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test("external-task-whole-input", |db, scope, state, ledger| {
        let input = InputEvidenceLoadRequest { windows:Vec::new(), mandatory_roots:Default::default(), interrupted_sources:Default::default(), selected_completion_brief_ids:Default::default(), byte_limit:64*1024*1024, record_limit:65536 };
        let work = crate::runtime::resident_work::WorkCaptureRequest::current();
        let tasks = crate::runtime::resident_task::TaskCaptureRequest::current();
        let baseline = ledger.report()?.observed_bytes;
        let run = |limits:(usize,usize), held:bool| -> Result<super::super::task_snapshot::TaskMaintenanceMaterial> {
            let request = TaskDomainLoadRequest { query:Default::default(), capture:tasks.clone(), byte_limit:limits.0, record_limit:limits.1 };
            let mut task = TaskReadMaterial::new(scope, &request, Some(ledger));
            if held { task.charge_final_metadata(1024*1024, 32)?; }
            let allocation = held.then(||vec![0u8;1024*1024]);
            let mut connection = db.connection()?;
            let tx = connection.transaction()?;
            let actual = prepare_input_domains_observation_inner_tx(
                &tx, scope, state, &input, 12, ledger, true, Some(&work), false,
                Some(&tasks), None, Some(&mut task), None, None, None,
            )?;
            assert_eq!(actual.facts.state.as_ref(), Some(state));
            assert!(actual.current_reads.is_some() && actual.work.is_some() && actual.tasks.is_some());
            assert!(actual._input_material_views.is_empty(), "original outer Task owns the whole read's guards");
            drop(actual);
            tx.rollback()?;
            let usage = task.material_estimate();
            assert!(ledger.report()?.observed_bytes > baseline);
            drop((task, allocation));
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            Ok(usage)
        };
        let incoming = run((input.byte_limit,input.record_limit), false)?;
        let full = run((input.byte_limit,input.record_limit), true)?;
        assert!(full.bytes > incoming.bytes && full.records > incoming.records);
        // Current's original selector includes its remaining numeric limits.
        // Measure under the same cap so a shorter encoded limit cannot make a
        // high-cap minus-one probe accidentally larger than the actual cost.
        let mut boundary = full;
        let mut stable = false;
        for _ in 0..8 {
            let actual = run((boundary.bytes,boundary.records), true)?;
            assert!(actual.bytes <= boundary.bytes && actual.records <= boundary.records);
            if (actual.bytes,actual.records) == (boundary.bytes,boundary.records) {
                stable = true;
                break;
            }
            boundary = actual;
        }
        assert!(stable, "same-cap actual material must reach a stable boundary");
        assert!(boundary.bytes > incoming.bytes && boundary.records > incoming.records);
        for limits in [(boundary.bytes-1,boundary.records),(boundary.bytes,boundary.records-1)] {
            assert!(incoming.bytes < limits.0 && incoming.records < limits.1);
            let error = match run(limits, true) {
                Err(error) => error,
                Ok(actual) => anyhow::bail!(
                    "external Task accepted a combined cap: limits={limits:?}, incoming=({}, {}), measured=({}, {}), actual=({}, {})",
                    incoming.bytes, incoming.records, full.bytes, full.records, actual.bytes, actual.records,
                ),
            };
            assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::ResourceExhausted(_)))
                || matches!(error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),Some(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted))
                || matches!(error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),Some(crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(_)))
                || matches!(error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::ResourceExhausted(_))),"{error:#}");
            assert_eq!(ledger.report()?.observed_bytes, baseline);
        }
        let exact = run((boundary.bytes,boundary.records), true)?;
        assert_eq!((exact.bytes,exact.records),(boundary.bytes,boundary.records));
        Ok(())
    }).await
}

#[tokio::test]
async fn external_task_rejects_foreign_phase_and_original_prior_before_forbidden_state(
) -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test(
        "external-task-first-scope",
        |db, scope, state, ledger| {
            let input = InputEvidenceLoadRequest {
                windows: Vec::new(),
                mandatory_roots: Default::default(),
                interrupted_sources: Default::default(),
                selected_completion_brief_ids: Default::default(),
                byte_limit: 64 * 1024 * 1024,
                record_limit: 65536,
            };
            let task_request = TaskDomainLoadRequest {
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                byte_limit: 1024 * 1024,
                record_limit: 65536,
            };
            let baseline = ledger.report()?.observed_bytes;
            db.connection()?.execute_batch(
                "ALTER TABLE agent_states RENAME TO forbidden_external_task_state",
            )?;
            let other = InputEvidenceBudgetLedger::new(Default::default())?;
            let mut task = TaskReadMaterial::new(scope, &task_request, Some(ledger));
            let error = db
                .transaction(|tx| {
                    prepare_input_domains_observation_inner_tx(
                        tx,
                        scope,
                        state,
                        &input,
                        12,
                        &other,
                        true,
                        None,
                        false,
                        None,
                        None,
                        Some(&mut task),
                        None,
                        None,
                        None,
                    )
                    .map(|_| ())
                })
                .err()
                .context("foreign ledger reached the forbidden State query")?;
            assert!(
                format!("{error:#}").contains("original scope or ledger differs"),
                "{error:#}"
            );
            assert_eq!(other.report()?.observed_bytes, 0);
            let held_request = TaskDomainLoadRequest {
                byte_limit: 256 * 1024,
                ..task_request.clone()
            };
            let mut held = TaskReadMaterial::new(scope, &held_request, Some(ledger));
            // A real guarded outer allocation leaves less than the first Input
            // scope guard; the request's larger independent cap cannot hide it.
            held.charge_final_metadata((held_request.byte_limit - 4096) / 4 - 256, 1)?;
            let allocation = vec![0u8; (held_request.byte_limit - 4096) / 4 - 256];
            let error = db
                .transaction(|tx| {
                    prepare_input_domains_observation_inner_tx(
                        tx,
                        scope,
                        state,
                        &input,
                        12,
                        ledger,
                        true,
                        None,
                        false,
                        None,
                        None,
                        Some(&mut held),
                        None,
                        None,
                        None,
                    )
                    .map(|_| ())
                })
                .err()
                .context("outer Task prior was skipped before first Input scope")?;
            assert!(
                matches!(
                    error.downcast_ref::<InputEvidenceSnapshotError>(),
                    Some(InputEvidenceSnapshotError::ResourceExhausted(_))
                ),
                "{error:#}"
            );
            drop((held, allocation, task));
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            Ok(())
        },
    )
    .await
}
