use super::*;
use crate::{runtime::tests::support::LifecycleHarness, types::TaskKind};

const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
fn task(id: &str, detail: Option<serde_json::Value>) -> TaskRecord {
    let at = Utc::now();
    TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Queued,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail,
        recovery: None,
    }
}
fn only(id: &str) -> TaskCaptureRequest {
    TaskCaptureRequest {
        include_active: false,
        include_unsettled: false,
        terminal_limit: 0,
        required_keys: BTreeSet::from([TaskKey::new(TaskFamily::Task, id)]),
    }
}

#[tokio::test]
async fn actual_regular_task_capture_is_pure_and_only_same_arc_body_is_loaned() -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime
        .persist_task_transition(
            &task(
                "large-unselected-task",
                // The actual W1 payload caps each string at2048 and each
                // array at64. Keep a genuinely large canonical body after W1.
                Some(serde_json::json!({
                    "large":vec!["x".repeat(2048);64],
                    "nodes":vec![vec![0;64];8],
                })),
            ),
            "task_queued",
        )
        .await?;
    runtime
        .persist_task_transition(&task("small-selected-task", None), "task_queued")
        .await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Task root missing")?;
    let domain = root.tasks.as_ref().context("actual Task domain missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE tasks RENAME TO regular_task_forbidden_tasks;
         ALTER TABLE agent_states RENAME TO regular_task_forbidden_states;
         ALTER TABLE agent_identities RENAME TO regular_task_forbidden_identities;",
    )?;
    let request = only("small-selected-task");
    let (original, hold) = domain.capture_with_material(&request, ledger, LIMITS, (0, 0), None)?;
    assert_eq!(original.records.len(), 1);
    let key = TaskKey::new(TaskFamily::Task, "small-selected-task");
    assert!(Arc::ptr_eq(
        &original.records[0],
        domain.records.get(&key).unwrap()
    ));
    let large = domain
        .records
        .get(&TaskKey::new(TaskFamily::Task, "large-unselected-task"))
        .unwrap();
    let CanonicalTaskFact::Task(actual_large) = &large.fact else {
        anyhow::bail!("actual unselected Task has another family")
    };
    let detail = actual_large
        .detail
        .as_ref()
        .context("canonical large detail absent")?;
    let strings = detail["large"]
        .as_array()
        .context("canonical string array absent")?;
    assert_eq!(strings.len(), 64);
    assert!(strings
        .iter()
        .all(|value| value.as_str().is_some_and(|text| text.len() == 2048)));
    let rows = detail["nodes"]
        .as_array()
        .context("canonical native rows absent")?;
    assert_eq!(rows.len(), 8);
    assert!(rows
        .iter()
        .all(|row| row.as_array().is_some_and(|nodes| nodes.len() == 64)));
    let large_cost = task_body_material(large)?;
    assert!(
        large_cost.0 > 256 * 1024 && large_cost.1 > 512,
        "actual W1 did not preserve the intended large canonical fixture: {large_cost:?}"
    );
    assert!(
        hold.phase_peak().0 < large_cost.0,
        "unselected body entered Task allowance: selected peak={:?}, actual unselected={large_cost:?}",
        hold.phase_peak()
    );
    assert!(
        hold.phase_peak().1 < large_cost.1,
        "unselected native body entered Task allowance"
    );
    hold.validate_accounting(domain.scope(), ledger)?;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(hold.validate_accounting(domain.scope(), &foreign).is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    let prior = hold.material_estimate()?;
    let (shared, shared_hold) =
        domain.capture_with_material(&request, ledger, LIMITS, prior, Some(&original))?;
    let body = task_body_material(&shared.records[0])?;
    let full = shared_hold.material_estimate()?;
    let added = shared_hold.additional_material_estimate()?;
    assert_eq!(full.0 - added.0, body.0);
    assert_eq!(full.1 - added.1, body.1);
    assert!(
        added.0 > 0 && added.1 > 0,
        "new selector/coverage/pin metadata was waived"
    );
    drop((shared, shared_hold));
    assert!(domain
        .capture_with_material(
            &request,
            ledger,
            LIMITS,
            (body.0 - 1, body.1),
            Some(&original)
        )
        .is_err());
    // Same qualified locator/content in another physical allocation grants no
    // discount. The private adversarial copy has an actual same-ledger view.
    let other_metadata = retained_metadata(&original)?;
    let other_guard = ledger.register_owned_bytes(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: other_metadata.0,
        },
    )?;
    let new_body_guard = ledger.register_owned_bytes(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: body.0,
            metadata_estimate_bytes: 256,
        },
    )?;
    let replacement = Arc::new((*original.records[0]).clone());
    let replacement_guard = ledger.register_task_view(
        domain.scope(),
        InputMemoryCategory::Pins,
        std::iter::once(&replacement),
    )?;
    drop(new_body_guard);
    let other = CapturedTasks {
        scope: original.scope.clone(),
        revision: original.revision,
        coverage: original.coverage.clone(),
        records: vec![replacement],
        stop_records: Vec::new(),
        _budget_views: vec![other_guard, replacement_guard],
    };
    let actual_prior = (
        prior.0.checked_mul(2).unwrap(),
        prior.1.checked_mul(2).unwrap(),
    );
    let (different, different_hold) =
        domain.capture_with_material(&request, ledger, LIMITS, actual_prior, Some(&other))?;
    assert_eq!(
        different_hold.material_estimate()?,
        different_hold.additional_material_estimate()?
    );
    assert!(Arc::ptr_eq(&different.records[0], &original.records[0]));
    assert!(!Arc::ptr_eq(&different.records[0], &other.records[0]));
    drop((different, different_hold, other));
    assert_eq!(
        Arc::strong_count(&root),
        strong,
        "capture retained its publication"
    );
    let weak = Arc::downgrade(hold._guards.last().unwrap());
    drop(original);
    assert!(weak.upgrade().is_some());
    drop(hold);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn actual_regular_task_caller_joint_limits_admit_before_clones() -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime
        .persist_task_transition(&task("joint-selected-task", None), "task_queued")
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let domain = root.tasks.as_ref().unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let request = only("joint-selected-task");
    let (captured, hold) = domain.capture_with_material(&request, ledger, LIMITS, (0, 0), None)?;
    let peak = hold.phase_peak();
    drop((captured, hold));
    let prior_bytes = 128 * 1024usize;
    let bytes = vec![7u8; prior_bytes];
    let prior = (u64::try_from(bytes.len())?, 16usize);
    let guard = ledger.register_owned_bytes(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let held = ledger.report()?.observed_bytes;
    let exact = (usize::try_from(prior.0 + peak.0)?, prior.1 + peak.1);
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let error = domain
            .capture_with_material(&request, ledger, limits, prior, None)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted("Task capture material"))
        ));
        assert_eq!(ledger.report()?.observed_bytes, held);
        assert!(Arc::ptr_eq(
            &root,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));
    }
    let (captured, hold) = domain.capture_with_material(&request, ledger, exact, prior, None)?;
    assert_eq!(hold.phase_peak(), peak);
    assert!(
        hold.material_estimate()?.0 < prior.0,
        "caller prior was returned as owned capture"
    );
    drop((captured, hold));
    assert_eq!(ledger.report()?.observed_bytes, held);
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(domain
        .capture_with_material(&request, &foreign, LIMITS, prior, None)
        .is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    drop((guard, bytes));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn regular_task_retained_handoff_rejects_live_scratch_holder_and_foreign_phase() -> Result<()>
{
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime
        .persist_task_transition(&task("handoff-task", None), "task_queued")
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let domain = root.tasks.as_ref().unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let mut phase = TaskCapturePhase::new(domain, ledger, LIMITS, (0, 0), None)?;
    let captured = domain.capture_observed(&only("handoff-task"), false, &mut phase)?;
    let scratch = phase.owned.as_ref().unwrap().guard();
    assert!(phase
        .finish(captured)
        .unwrap_err()
        .to_string()
        .contains("another live holder"));
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(scratch);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut phase = TaskCapturePhase::new(domain, ledger, LIMITS, (0, 0), None)?;
    let captured = domain.capture_observed(&only("handoff-task"), false, &mut phase)?;
    phase.owned = Some(foreign.reserve_owned_completion(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: phase.owned_bytes,
        },
    )?);
    assert!(phase
        .finish(captured)
        .unwrap_err()
        .to_string()
        .contains("another phase"));
    assert_eq!(foreign.report()?.observed_bytes, 0);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[test]
fn task_observation_stream_preserves_original_canonical_bytes_and_roundtrip() -> Result<()> {
    for entries in [0usize, 1, 5] {
        let map: BTreeMap<_, _> = (0..entries)
            .map(|n| {
                (
                    TaskKey::new(
                        if n % 2 == 0 {
                            TaskFamily::Task
                        } else {
                            TaskFamily::ResultSettlement
                        },
                        format!("key-\"\\\n雪-{n}"),
                    ),
                    if n % 3 == 0 {
                        TaskAvailability::Present
                    } else {
                        TaskAvailability::Pruned
                    },
                )
            })
            .collect();
        let original = serde_json::to_vec(&map.iter().collect::<Vec<_>>())?;
        let streamed = serde_json::to_vec(&observations::Borrowed(&map))?;
        assert_eq!(streamed, original);
        let mut coverage: TaskCoverage = serde_json::from_value(serde_json::json!({
            "revision":1,"active_complete":true,"unsettled_complete":true,
            "active_task_ids":[],"unsettled_result_ids":[],"task_metadata":{},
            "result_metadata":{},"result_dependencies":{},"task_result_ids":{},
            "activation_result_ids":{},"reply_result_ids":{},"required_keys":[],
            "observations":[],"terminal":{"limit":128,"keys":[],"is_exhaustive":true}
        }))?;
        coverage.observations = map;
        let encoded = serde_json::to_vec(&coverage)?;
        assert_eq!(serde_json::from_slice::<TaskCoverage>(&encoded)?, coverage);
        assert_eq!(
            crate::runtime::input_evidence_budget::serialized_bytes(&observations::Borrowed(
                &coverage.observations
            ))?,
            u64::try_from(original.len())?
        );
    }
    Ok(())
}

#[test]
fn borrowed_task_key_observer_matches_actual_key_object_bytes_and_native_nodes() -> Result<()> {
    for family in [TaskFamily::Task, TaskFamily::ResultSettlement] {
        for id in ["", "actual-key", "quote\"\\\n雪"] {
            let key = TaskKey::new(family, id);
            let borrowed = BorrowedTaskKey { family, id };
            assert_eq!(serde_json::to_vec(&key)?, serde_json::to_vec(&borrowed)?);
            assert_eq!(json_cost(&key, 1)?, json_cost(&borrowed, 1)?);
            assert_eq!(json_cost(&key, 3)?, json_cost(&borrowed, 3)?);
        }
    }
    Ok(())
}
