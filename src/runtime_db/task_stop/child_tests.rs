use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime_db::{evidence::upsert_agent_identity_tx, RuntimeDb},
    types::AgentProfilePreset,
};

fn fixture() -> Result<(
    tempfile::TempDir,
    RuntimeDb,
    InputEvidenceBudgetLedger,
    TaskStopChildTarget,
    AgentIdentityRecord,
)> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    let target = TaskStopChildTarget {
        scope: InputEvidenceScope {
            agent_id: "original-child".into(),
            identity_incarnation: 1,
        },
        parent_agent_id: "original-parent".into(),
        creation_task_id: "creation-task".into(),
    };
    let identity = AgentIdentityRecord::new(
        &target.scope.agent_id,
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(target.parent_agent_id.clone()),
        Some(target.creation_task_id.clone()),
    );
    db.transaction(|tx| upsert_agent_identity_tx(tx, &identity))?;
    Ok((
        dir,
        db,
        InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?,
        target,
        identity,
    ))
}

#[test]
fn original_child_identity_rechecks_exact_generation_parent_task_and_retired_state() -> Result<()> {
    let (_dir, db, ledger, target, identity) = fixture()?;
    db.transaction(|tx| {
        // This boundary only needs identity, never a successor's State or Task.
        tx.execute(
            "ALTER TABLE agent_states RENAME TO hidden_successor_states",
            [],
        )?;
        let (found, guard) =
            read_original_child_identity_tx(tx, &target, false, MAX_BYTES, &ledger)?;
        assert_eq!(found, identity);
        drop(guard);
        let error = read_original_child_identity_tx(tx, &target, true, MAX_BYTES, &ledger)
            .err()
            .context("Active child accepted as retired")?;
        assert!(error.to_string().contains("not retired before cleanup"));
        for change in [0, 1, 2] {
            let mut changed = identity.clone();
            match change {
                0 => changed.incarnation += 1,
                1 => changed.parent_agent_id = Some("another-parent".into()),
                _ => changed.delegated_from_task_id = Some("another-task".into()),
            };
            upsert_agent_identity_tx(tx, &changed)?;
            let error = read_original_child_identity_tx(tx, &target, false, MAX_BYTES, &ledger)
                .err()
                .context("successor or reassociated child accepted")?;
            assert!(
                error
                    .to_string()
                    .contains("original identity/parent/creation Task"),
                "{error:#}"
            );
        }
        let mut deleted = identity.clone();
        deleted.status = AgentRegistryStatus::Deleted;
        deleted.deleted_at = Some(Utc::now());
        upsert_agent_identity_tx(tx, &deleted)?;
        let (found, guard) =
            read_original_child_identity_tx(tx, &target, true, MAX_BYTES, &ledger)?;
        assert_eq!(found.status, AgentRegistryStatus::Deleted);
        drop(guard);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_child_identity_rejects_normalized_drift_and_budgets_bad_payload_first() -> Result<()> {
    let (_dir, db, ledger, target, _identity) = fixture()?;
    db.transaction(|tx| {
        tx.execute("UPDATE agent_identities SET parent_agent_id='another-parent' WHERE agent_id='original-child'",[])?;
        let error=read_original_child_identity_tx(tx,&target,false,MAX_BYTES,&ledger).err().context("normalized drift accepted")?;
        assert!(error.to_string().contains("normalized identity differs"));
        tx.execute("UPDATE agent_identities SET payload_json=CAST(zeroblob(8192) AS TEXT) WHERE agent_id='original-child'",[])?;
        let error=read_original_child_identity_tx(tx,&target,false,4096,&ledger).err().context("unbounded bad payload decoded")?;
        assert!(error.to_string().contains("ResourceExhausted before decode"),"{error:#}");
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_parent_generation_gate_rejects_successor_and_reused_task_without_state_reads(
) -> Result<()> {
    use crate::{
        runtime::resident_task::{TaskCaptureRequest, TaskKey},
        runtime_db::task_snapshot::*,
        types::AgentState,
    };
    let (_dir, db, ledger, target, _identity) = fixture()?;
    let scope = InputEvidenceScope {
        agent_id: target.parent_agent_id.clone(),
        identity_incarnation: 1,
    };
    let state = AgentState::new(&scope.agent_id);
    let created: DateTime<Utc> = "2026-10-01T00:00:00Z".parse()?;
    let task = TaskRecord {
        id: target.creation_task_id.clone(),
        agent_id: scope.agent_id.clone(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: created,
        updated_at: created,
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail: Some(serde_json::json!({})),
        recovery: None,
    };
    let request = TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: MAX_BYTES,
    };
    let mut parent = AgentIdentityRecord::new(
        &scope.agent_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let original = db.transaction(|tx| {
        upsert_agent_identity_tx(tx, &parent)?;
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &state)?;
        prepare_task_domain_snapshot_tx(tx, &scope, &request)?.require_current()?;
        let keys = std::collections::BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        crate::runtime_db::repositories::upsert_task_tx(tx, &task)?;
        register_task_writes_tx(tx, &proof, &keys, &state, &request)?;
        let snapshot = prepare_task_domain_snapshot_tx(tx, &scope, &request)?.require_current()?;
        let capture = snapshot.resident.capture(&request.capture)?;
        let locator = capture
            .lookup(keys.first().unwrap())?
            .unwrap()
            .locator
            .clone();
        let intent = PreparedTaskStop {
            task: locator,
            kind: task.kind,
            created_at: created,
            rejoin: None,
            owner_epoch: 1,
            signal: TaskStopSignal::NoHandle,
            lease_id: None,
            child: None,
            child_unavailable: None,
            worktree: None,
            worktree_unavailable: None,
            result: None,
        };
        let prepared = PreparedInputMutation::new(
            InputMutationFamily::Task,
            "generation-observation".into(),
            vec![scope.clone()],
            canonical_command_digest(&intent)?,
            created,
        )?;
        Ok(TaskStopRecord {
            effect_seq: 1,
            prepared,
            intent_digest: canonical_command_digest(&intent)?,
            generation_digest: intent.generation_digest()?,
            ack_task: task.clone(),
            ack_task_digest: canonical_command_digest(&task)?,
            revision: 1,
            progress: TaskStopProgress::initial(&intent),
            intent,
            result_nonce: None,
            updated_at: created,
        })
    })?;
    db.transaction(|tx| {
        tx.execute("ALTER TABLE agent_states RENAME TO hidden_successor_states",[])?;
        drop(validate_current_parent_generation_tx(tx,&original,MAX_BYTES,&ledger)?);
        parent.incarnation=2;upsert_agent_identity_tx(tx,&parent)?;
        let error=validate_current_parent_generation_tx(tx,&original,MAX_BYTES,&ledger).err().context("successor parent accepted")?;
        assert!(error.to_string().contains("parent identity generation changed"));
        parent.incarnation=1;upsert_agent_identity_tx(tx,&parent)?;
        let later=crate::runtime_db::migrations::timestamp(created+chrono::Duration::days(1));
        tx.execute("UPDATE tasks SET created_at=?2,payload_json=json_set(payload_json,'$.created_at',?2) WHERE task_id=?1",params![task.id,later])?;
        let error=validate_current_parent_generation_tx(tx,&original,MAX_BYTES,&ledger).err().context("reused Task generation accepted")?;
        assert!(error.to_string().contains("original generation differs"),"{error:#}");
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn child_supervision_and_archive_queue_material_are_bounded_before_owned_decode() -> Result<()> {
    let (_dir, db, ledger, target, identity) = fixture()?;
    db.transaction(|tx| {
        let parent=AgentIdentityRecord::new(&target.parent_agent_id,AgentKind::Named,AgentVisibility::Public,AgentOwnership::SelfOwned,AgentProfilePreset::PublicNamed,None,None);
        upsert_agent_identity_tx(tx,&parent)?;
        tx.execute("INSERT INTO agent_supervisions(supervision_id,supervisor_agent_id,child_agent_id,delegated_from_work_item_id,delegated_from_task_id,state,revision,created_at,updated_at,payload_json) VALUES('original-supervision',?1,?2,CAST(zeroblob(8192) AS TEXT),?3,'active',1,?4,?4,'{}')",
            params![parent.agent_id,identity.agent_id,target.creation_task_id,crate::runtime_db::migrations::timestamp(identity.created_at)])?;
        let error=prepare_child_associated_material_tx(tx,&target,false,4096,&ledger).err().context("large supervision decoded")?;
        assert!(error.to_string().contains("associated ResourceExhausted before decode"));
        tx.execute("INSERT INTO queue_entries(message_id,agent_id,priority,status,created_at,updated_at,payload_json) VALUES('original-queued',?1,'normal','queued',?2,?2,CAST(zeroblob(8192) AS TEXT))",
            params![identity.agent_id,crate::runtime_db::migrations::timestamp(identity.created_at)])?;
        let error=prepare_task_stop_archive_material_tx(tx,&target.scope,4096,&ledger).err().context("large malformed Queue decoded")?;
        assert!(error.to_string().contains("archive ResourceExhausted before decode"));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn archive_wait_budget_includes_the_raw_body_read_by_the_cancellation_writer() -> Result<()> {
    let (_dir, db, ledger, target, identity) = fixture()?;
    db.transaction(|tx| {
        tx.execute("INSERT INTO wait_conditions(wait_condition_id,agent_id,status,kind,waiting_for,created_at,updated_at,payload_json) VALUES('original-wait',?1,'active','message','small',?2,?2,CAST(zeroblob(8192) AS TEXT))",
            params![identity.agent_id,crate::runtime_db::migrations::timestamp(identity.created_at)])?;
        let error=prepare_task_stop_archive_material_tx(tx,&target.scope,4096,&ledger).err().context("large raw Wait was not counted")?;
        assert!(error.to_string().contains("archive ResourceExhausted before decode"),"{error:#}");
        assert_eq!(tx.query_row("SELECT status FROM wait_conditions WHERE wait_condition_id='original-wait'",[],|row|row.get::<_,String>(0))?,"active");
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
