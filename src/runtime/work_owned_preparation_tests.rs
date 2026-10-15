use super::*;
use crate::runtime::tests::support::{context_config, tempdir};
use crate::runtime_db::transitions::{
    PreparedInputTransition, WorkItemMutation, WorkItemTransitionCommand,
};
use crate::types::{WorkItemRecord, WorkItemState};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, WorkItemRecord)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "owned-work-source",
        home.path().into(),
        context_config(),
    )?;
    runtime.stop_context_resource_observer().await?;
    let record = WorkItemRecord::new(
        runtime.instance_agent_id(),
        "actual accepted Work source",
        WorkItemState::Open,
    );
    let mut owner = runtime.begin_mutation().await?;
    let prepared = PreparedInputTransition::work(
        owner.input_evidence_scope(),
        "accepted-owned-source".into(),
        record.updated_at,
        WorkItemTransitionCommand {
            agent_id: record.agent_id.clone(),
            mutation: WorkItemMutation::Insert {
                record: record.clone(),
            },
            agent_state: None,
            brief_evidence: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: true,
            fault: None,
        },
        Default::default(),
        false,
    )?;
    let ack = owner.commit_prepared_input_transition(prepared).await?;
    drop(owner);
    runtime.apply_transition_commit(ack).await;
    Ok((home, runtime, record))
}
fn scope(runtime: &RuntimeHandle) -> super::super::resident_input_evidence::InputEvidenceScope {
    super::super::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.instance_agent_id().into(),
        identity_incarnation: runtime.instance_incarnation(),
    }
}
fn request(records: usize, bytes: usize) -> WorkDomainLoadRequest {
    let mut capture = WorkCaptureRequest::current();
    capture.include_live = false;
    capture.nonopen_limit = 0;
    WorkDomainLoadRequest {
        capture,
        record_limit: records,
        byte_limit: bytes,
    }
}
#[tokio::test]
async fn actual_work_ack_owned_source_is_pure_under_held_owner_and_pins_until_last_drop(
) -> Result<()> {
    let (_home, runtime, record) = fixture().await?;
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    // Keep the real owner: the synchronous source must neither reserve again
    // nor silently enter a raw lookup when its original body is selected.
    let owner = runtime.begin_mutation().await?;
    runtime.inner.runtime_db.connection()?.execute_batch(
        "DROP TABLE work_items; DROP TABLE agent_states; DROP TABLE agent_identities;",
    )?;
    let (actual, material) = runtime.validate_owned_work_item(&record.agent_id, &record.id)?;
    assert_eq!(actual, record);
    assert!(material._selected.is_some());
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > before);
    let weak = Arc::downgrade(&material);
    let holder = material.clone();
    drop(material);
    assert!(weak.upgrade().is_some());
    drop((holder, actual, owner));
    assert!(weak.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    Ok(())
}
#[tokio::test]
async fn named_work_source_counts_nodes_and_normalized_bytes_before_decode_without_late_adoption(
) -> Result<()> {
    let (_home, runtime, record) = fixture().await?;
    let scope = scope(&runtime);
    let ledger = &runtime.inner.input_evidence_budget;
    let before = ledger.report()?.observed_bytes;
    let saved: String = runtime.inner.runtime_db.connection()?.query_row(
        "SELECT payload_json FROM work_items WHERE work_item_id=?1",
        [&record.id],
        |r| r.get(0),
    )?;
    let nodes = serde_json::to_string(&vec![0u8; 20_000])?;
    runtime.inner.runtime_db.connection()?.execute("UPDATE work_items SET payload_json=json_set(payload_json,'$.todo_list',json(?2)) WHERE work_item_id=?1",rusqlite::params![record.id,nodes])?;
    let error = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            work_snapshot::read_owned_work_item_source_tx(
                tx,
                &scope,
                &record.id,
                &request(4096, 1024 * 1024),
                ledger,
            )
        })
        .err()
        .context("native nodes must reject before invalid Todo decode")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    runtime.inner.runtime_db.connection()?.execute(
        "UPDATE work_items SET payload_json=?2,revision=?3 WHERE work_item_id=?1",
        rusqlite::params![record.id, saved, "r".repeat(256 * 1024)],
    )?;
    let error = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            work_snapshot::read_owned_work_item_source_tx(
                tx,
                &scope,
                &record.id,
                &request(65_536, 128 * 1024),
                ledger,
            )
        })
        .err()
        .context("normalized TEXT revision must reject before projection")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    runtime.inner.runtime_db.connection()?.execute(
        "UPDATE work_items SET revision=?2 WHERE work_item_id=?1",
        rusqlite::params![record.id, record.revision],
    )?;
    runtime.inner.runtime_db.connection()?.execute("INSERT INTO work_items(work_item_id,agent_id,state,objective,revision,created_at,updated_at,payload_json) SELECT 'own-raw-late',agent_id,state,objective,revision,created_at,updated_at,'{invalid' FROM work_items WHERE work_item_id=?1",[&record.id])?;
    let error = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            work_snapshot::read_owned_work_item_source_tx(
                tx,
                &scope,
                "own-raw-late",
                &request(65_536, 64 * 1024 * 1024),
                ledger,
            )
        })
        .err()
        .context("raw late must lack qualification before decoding")?;
    assert!(
        matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::NeedsPreparation(key)) if key.id=="own-raw-late"),
        "{error:#}"
    );
    let aliases: usize = runtime.inner.runtime_db.connection()?.query_row(
        "SELECT COUNT(*) FROM work_domain_records WHERE record_id='own-raw-late'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(aliases, 0);
    assert_eq!(ledger.report()?.observed_bytes, before);
    Ok(())
}
#[tokio::test]
async fn named_work_source_rejects_foreign_pruned_and_retired_before_poisoned_bodies() -> Result<()>
{
    let (_home, runtime, record) = fixture().await?;
    let scope = scope(&runtime);
    let ledger = &runtime.inner.input_evidence_budget;
    let before = ledger.report()?.observed_bytes;
    runtime.inner.runtime_db.connection()?.execute("INSERT INTO work_items(work_item_id,agent_id,state,objective,revision,created_at,updated_at,payload_json) SELECT 'foreign-poison','other-owner',state,objective,revision,created_at,updated_at,?2 FROM work_items WHERE work_item_id=?1",rusqlite::params![record.id,"{bad".repeat(256*1024)])?;
    let foreign = runtime.inner.runtime_db.transaction(|tx| {
        work_snapshot::read_owned_work_item_source_tx(
            tx,
            &scope,
            "foreign-poison",
            &request(16, 8192),
            ledger,
        )
    })?;
    assert!(matches!(foreign, WorkItemSourceRead::Foreign));
    runtime.inner.runtime_db.connection()?.execute("UPDATE work_domain_records SET availability='pruned',content_digest=NULL,fingerprint_json=NULL WHERE family='work' AND record_id=?1",[&record.id])?;
    runtime.inner.runtime_db.connection()?.execute(
        "UPDATE work_items SET payload_json='{poison' WHERE work_item_id=?1",
        [&record.id],
    )?;
    let error = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            work_snapshot::read_owned_work_item_source_tx(
                tx,
                &scope,
                &record.id,
                &request(65_536, 64 * 1024 * 1024),
                ledger,
            )
        })
        .err()
        .context("Pruned must not hydrate poison")?;
    assert!(
        matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::NeedsPreparation(key)) if key.id==record.id),
        "{error:#}"
    );
    let saved_state: String = runtime.inner.runtime_db.connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
        |r| r.get(0),
    )?;
    runtime.inner.runtime_db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
    )?;
    let error = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            work_snapshot::read_owned_work_item_source_tx(
                tx,
                &scope,
                &record.id,
                &request(65_536, 64 * 1024 * 1024),
                ledger,
            )
        })
        .err()
        .context("NoState cannot hydrate a mutation source")?;
    assert!(
        matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::NeedsPreparation(key)) if key.id==record.id),
        "{error:#}"
    );
    let original_state: crate::types::AgentState = serde_json::from_str(&saved_state)?;
    runtime.inner.runtime_db.transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &original_state)
    })?;
    runtime.inner.runtime_db.connection()?.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',?2) WHERE agent_id=?1",rusqlite::params![scope.agent_id,scope.identity_incarnation+1])?;
    runtime.inner.runtime_db.connection()?.execute(
        "UPDATE agent_states SET payload_json='{successor-state-poison' WHERE agent_id=?1",
        [&scope.agent_id],
    )?;
    let error = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            work_snapshot::read_owned_work_item_source_tx(
                tx,
                &scope,
                &record.id,
                &request(65_536, 64 * 1024 * 1024),
                ledger,
            )
        })
        .err()
        .context("retired original cannot read successor State")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::RetiredScope)
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    Ok(())
}
