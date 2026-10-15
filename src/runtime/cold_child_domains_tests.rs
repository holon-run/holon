use super::*;
use serde_json::json;
use crate::runtime::input_evidence_budget::{
    InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};
use crate::runtime::tests::support::{context_config, persist_task_transition, tempdir};
use crate::runtime_db::cold_child_domains::{prepare_cold_child_domains_tx, ColdChildReadError};
use crate::runtime_db::{
    input_evidence_snapshot::InputEvidenceSnapshotError,
    scheduling_snapshot::SchedulingSnapshotError,
};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, WorkItemRecord, TaskRecord)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "cold-child-source",
        home.path().into(),
        context_config(),
    )?;
    let work = runtime
        .create_work_item(
            "actual cold child work".into(),
            Some(crate::types::WorkItemPlanStatus::Ready),
            Some("original plan".into()),
            Vec::new(),
        )
        .await?;
    let now = runtime.now();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: runtime.instance_agent_id().into(),
        kind: TaskKind::AgentMessageWait,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: Some(work.id.clone()),
        summary: Some("original cold waiting task".into()),
        detail: Some(json!({"target_agent_id":"actual-target", "business_completion":false})),
        recovery: Some(crate::types::TaskRecoverySpec::AgentMessageWait {
            summary: "original cold waiting task".into(),
            message: "actual accepted request".into(),
            target_agent_id: "actual-target".into(),
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_id: None,
            after_delivery_rowid: None,
        }),
    };
    persist_task_transition(&runtime, &task, "cold_child_actual_task").await?;
    runtime.refresh_context_resources().await?;
    runtime.quiesce_child_summary_observer_for_test().await?;
    Ok((home, runtime, work, task))
}
fn exhausted(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ColdChildReadError>(),
        Some(ColdChildReadError::ResourceExhausted)
    ) || matches!(
        error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
        Some(crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<crate::runtime_db::work_snapshot::WorkSnapshotError>(),
        Some(crate::runtime_db::work_snapshot::WorkSnapshotError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),
        Some(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted)
    )
}
#[tokio::test]
async fn cold_child_actual_qualified_domains_share_state_view_and_keep_original_guards_without_ready_install(
) -> Result<()> {
    let (_home, runtime, work, task) = fixture().await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let scope = root.work.as_ref().unwrap().scope();
    let ledger = &runtime.inner.input_evidence_budget;
    let strong = Arc::strong_count(&root);
    let baseline = ledger.report()?.observed_bytes;
    let source = runtime
        .runtime_db()
        .transaction(|tx| prepare_cold_child_domains_tx(tx, scope, ledger, LIMITS, (0, 0)))?;
    assert_eq!(source.facts().state.as_ref(), Some(root.state.as_ref()));
    assert_eq!(
        source.facts().identity.as_ref(),
        Some(root.active_identity()?.as_ref())
    );
    assert!(source.work().coverage().live_work_ids.contains(&work.id));
    assert!(source.tasks().coverage().active_task_ids.contains(&task.id));
    assert!(source.tasks().coverage().unsettled_complete);
    assert_eq!(source.scheduling().scope(), scope);
    assert!(source.inputs().belongs_to(scope));
    assert_eq!(source.current().scope(), scope);
    assert!(source.material_estimate()?.0 > 0);
    assert_eq!(Arc::strong_count(&root), strong);
    assert!(Arc::ptr_eq(
        &root,
        &owner.capture_ready_snapshot()?.unwrap()
    ));
    let foreign = InputEvidenceBudgetLedger::new(ledger.limits())?;
    assert!(matches!(
        source
            .validate_accounting(scope, &foreign)
            .unwrap_err()
            .downcast_ref::<ColdChildReadError>(),
        Some(ColdChildReadError::ForeignMaterial)
    ));
    let guard = source.first_guard_for_test().clone();
    let weak = Arc::downgrade(&guard);
    drop(source);
    assert!(weak.upgrade().is_some());
    drop(guard);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    // A raw repository append is not an original qualified Work ACK. Its large
    // malformed body must stay outside this selected cold namespace.
    let raw = WorkItemRecord::new(
        runtime.instance_agent_id(),
        "x".repeat(9 * 1024 * 1024),
        crate::types::WorkItemState::Open,
    );
    assert!(runtime.runtime_db().work_items().insert_new(&raw)?);
    runtime.runtime_db().connection()?.execute(
        "UPDATE work_items SET payload_json='{' WHERE work_item_id=?1",
        [&raw.id],
    )?;
    let source = runtime
        .runtime_db()
        .transaction(|tx| prepare_cold_child_domains_tx(tx, scope, ledger, LIMITS, (0, 0)))?;
    assert!(!source.work().coverage().live_work_ids.contains(&raw.id));
    assert!(source.material_estimate()?.0 < u64::try_from(raw.objective.len())?);
    let members:usize=runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM work_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='work' AND record_id=?3",
        rusqlite::params![scope.agent_id,scope.identity_incarnation,raw.id],|row|row.get(0))?;
    assert_eq!(members, 0);
    assert!(Arc::ptr_eq(
        &root,
        &owner.capture_ready_snapshot()?.unwrap()
    ));
    drop(source);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn cold_child_held_actual_prior_is_joint_before_every_domain_decoder_and_at_measured_peak(
) -> Result<()> {
    let (_home, runtime, _work, _task) = fixture().await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let scope = root.work.as_ref().unwrap().scope();
    let ledger = &runtime.inner.input_evidence_budget;
    // Warm only the named original qualification sidecars; no source is installed.
    drop(
        runtime
            .runtime_db()
            .transaction(|tx| prepare_cold_child_domains_tx(tx, scope, ledger, LIMITS, (0, 0)))?,
    );
    let baseline = ledger.report()?.observed_bytes;
    let prior = (128 * 1024u64, 64usize);
    let held = vec![0u8; usize::try_from(prior.0)?];
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let held_baseline = ledger.report()?.observed_bytes;
    let fits = |limits| -> Result<bool> {
        let result = runtime
            .runtime_db()
            .transaction(|tx| prepare_cold_child_domains_tx(tx, scope, ledger, limits, prior));
        let fits = match result {
            Ok(source) => {
                drop(source);
                true
            }
            Err(error) => {
                assert!(exhausted(&error), "{error:#}");
                false
            }
        };
        assert_eq!(ledger.report()?.observed_bytes, held_baseline);
        assert!(Arc::ptr_eq(
            &root,
            &owner.capture_ready_snapshot()?.unwrap()
        ));
        Ok(fits)
    };
    assert!(fits(LIMITS)?);
    let mut nodes = (1usize, LIMITS.1);
    while nodes.0 < nodes.1 {
        let middle = nodes.0 + (nodes.1 - nodes.0) / 2;
        if fits((LIMITS.0, middle))? {
            nodes.1 = middle
        } else {
            nodes.0 = middle + 1
        }
    }
    let mut bytes = (1usize, LIMITS.0);
    while bytes.0 < bytes.1 {
        let middle = bytes.0 + (bytes.1 - bytes.0) / 2;
        if fits((middle, nodes.0))? {
            bytes.1 = middle
        } else {
            bytes.0 = middle + 1
        }
    }
    assert!(!fits((bytes.0 - 1, nodes.0))?);
    assert!(!fits((bytes.0, nodes.0 - 1))?);
    assert!(fits((bytes.0, nodes.0))?);
    let alone = runtime
        .runtime_db()
        .transaction(|tx| prepare_cold_child_domains_tx(tx, scope, ledger, LIMITS, (0, 0)))?;
    assert!(alone.material_estimate()?.0 < u64::try_from(LIMITS.0)?);
    drop(alone);
    assert_eq!(held.len(), usize::try_from(prior.0)?);
    drop((held, guard));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn cold_child_foreign_incarnation_rejects_before_successor_malformed_state_and_current_scope_raw_gate_precedes_json(
) -> Result<()> {
    let (_home, runtime, _work, _task) = fixture().await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let scope = root.work.as_ref().unwrap().scope();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let original: String = runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    runtime.runtime_db().connection()?.execute(
        "UPDATE agent_states SET payload_json=?2 WHERE agent_id=?1",
        rusqlite::params![
            scope.agent_id,
            "{".to_owned() + &"x".repeat(9 * 1024 * 1024)
        ],
    )?;
    let mut foreign = scope.clone();
    foreign.identity_incarnation += 1;
    let error = runtime
        .runtime_db()
        .transaction(|tx| {
            prepare_cold_child_domains_tx(tx, &foreign, ledger, (256 * 1024, 65536), (0, 0))
        })
        .err()
        .context("foreign cold scope was accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::RetiredScope)
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let error = runtime
        .runtime_db()
        .transaction(|tx| {
            prepare_cold_child_domains_tx(tx, scope, ledger, (256 * 1024, 65536), (0, 0))
        })
        .err()
        .context("current huge State decoded before raw gate")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    runtime.runtime_db().connection()?.execute(
        "UPDATE agent_states SET payload_json=?2 WHERE agent_id=?1",
        rusqlite::params![scope.agent_id, original],
    )?;
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert!(Arc::ptr_eq(
        &root,
        &owner.capture_ready_snapshot()?.unwrap()
    ));
    Ok(())
}
