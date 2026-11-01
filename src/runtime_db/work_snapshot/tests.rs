use super::*;
use crate::{
    runtime_db::{
        evidence::{
            append_transcript_entry_tx, insert_brief_evidence_tx, upsert_agent_identity_tx,
            upsert_agent_state_tx,
        },
        input_evidence_snapshot::{register_committed_input_evidence_tx, CanonicalInputEvidence},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        BriefContentSource, BriefContentSourceRelation, WaitConditionRecord, WaitConditionStatus,
        WorkItemRecord, WorkItemState,
    },
};
use chrono::{DateTime, Utc};
use rusqlite::Connection;

fn time() -> DateTime<Utc> {
    "2026-10-12T00:00:00.123456789Z".parse().unwrap()
}
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    }
}
fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        "agent-a",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}
fn request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 1024,
        byte_limit: 1024 * 1024,
    }
}
#[test]
fn work_loader_consumes_original_scope_state_without_redecoding() -> Result<()> {
    let (_home, db) = legacy(|_| Ok(()))?;
    db.transaction(|tx| {
        let scope = scope();
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        let original = read.facts().state.as_deref().unwrap().id.as_ptr();
        let snapshot =
            loader::load_with_scope_read(tx, &scope, &request(), true, false, false, Some(read))?
                .require_current()?;
        assert_eq!(
            snapshot.facts.state.as_deref().unwrap().id.as_ptr(),
            original
        );
        Ok(())
    })
}

#[test]
fn named_work_preflight_shares_scope_state_without_writes_and_rejects_drift() -> Result<()> {
    let (_home, db) = legacy(|_| Ok(()))?;
    db.transaction(|tx| {
        let scope = scope();
        prepare_work_domain_snapshot_tx(tx, &scope, &request())?.require_current()?;
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        let original = read.facts().state.as_ref().unwrap();
        let before: i64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        for _ in 0..2 {
            let snapshot = preflight_named_work_snapshot_tx(tx, &scope, &request(), &read)?
                .require_current()?;
            assert!(std::sync::Arc::ptr_eq(
                original,
                snapshot.facts.state.as_ref().unwrap()
            ));
        }
        assert_eq!(
            tx.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?,
            before
        );
        tx.execute(
            "UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending',17) WHERE agent_id=?1",
            [&scope.agent_id],
        )?;
        let after_drift: i64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        assert_eq!(
            preflight_named_work_snapshot_tx(tx, &scope, &request(), &read)
                .unwrap_err().to_string(),
            "context scope changed after its validated read"
        );
        assert_eq!(
            tx.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?,
            after_drift
        );
        Ok(())
    })
}

#[test]
fn work_loader_scope_transfer_rejects_drift_before_sidecar_writes() -> Result<()> {
    let (_home, db) = legacy(|_| Ok(()))?;
    db.transaction(|tx| {
        let scope = scope();
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        tx.execute(
            "UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending',17) WHERE agent_id=?1",
            [&scope.agent_id],
        )?;
        assert_eq!(
            loader::load_with_scope_read(
                tx, &scope, &request(), true, false, false, Some(read),
            ).unwrap_err().to_string(),
            "context scope changed after its validated read"
        );
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM work_domain_current", [], |row| row.get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
}

fn ensure_schema(tx: &Transaction<'_>) -> Result<()> {
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_domain_current')",[],|row|row.get(0))?;
    if !exists {
        tx.execute_batch(WORK_DOMAIN_SCHEMA)?;
    }
    capture_work_domain_upgrade_tx(tx)
}
pub(super) fn fresh() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        ensure_schema(tx)?;
        upsert_agent_identity_tx(tx, &identity())?;
        upsert_agent_state_tx(tx, &AgentState::new("agent-a"))?;
        prepare_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        Ok(())
    })?;
    Ok((dir, db))
}
fn work(id: &str, state: WorkItemState) -> WorkItemRecord {
    let mut work = WorkItemRecord::new("agent-a", id, state);
    work.id = id.into();
    work.created_at = time();
    work.updated_at = time();
    work
}
fn insert_qualified_work(
    tx: &Transaction<'_>,
    work: &WorkItemRecord,
) -> Result<CommittedWorkFacts> {
    let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &work.id)]);
    let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
    repositories::insert_new_work_item_tx(tx, work)?;
    let state = ContextDurableRead::new(tx, "agent-a")
        .scope_facts()?
        .into_owned_state()?
        .unwrap();
    register_work_writes_tx(tx, &proof, &keys, &BTreeSet::new(), &state, &request())
}
fn raw_legacy_work(tx: &Transaction<'_>, work: &WorkItemRecord) -> Result<()> {
    tx.execute("INSERT INTO work_items(work_item_id,agent_id,state,objective,revision,created_at,updated_at,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![work.id,work.agent_id,serde_json::to_value(&work.state)?.as_str().unwrap(),work.objective,work.revision,work.created_at.to_rfc3339(),work.updated_at.to_rfc3339(),serde_json::to_string(work)?])?;
    Ok(())
}
fn legacy(
    prepare: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<(tempfile::TempDir, RuntimeDb)> {
    legacy_with_import_status(true, prepare)
}
fn legacy_with_import_status(
    imported: bool,
    prepare: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let mut conn = Connection::open(&path)?;
    ensure_migration_table(&conn)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut conn, migration)?;
    }
    {
        let tx = conn.transaction()?;
        upsert_agent_identity_tx(&tx, &identity())?;
        upsert_agent_state_tx(&tx, &AgentState::new("agent-a"))?;
        if imported {
            tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[time().to_rfc3339()])?;
        }
        prepare(&tx)?;
        tx.commit()?;
    }
    drop(conn);
    let db = RuntimeDb::open_and_migrate(&path, dir.path().join("runtime.lock"))?;
    db.transaction(ensure_schema)?;
    Ok((dir, db))
}
fn brief(id: &str, work: &str) -> BriefRecord {
    let mut brief = BriefRecord::new("agent-a", BriefKind::Result, "result", None, None);
    brief.id = id.into();
    brief.work_item_id = Some(work.into());
    brief.created_at = time();
    brief
}
fn write_brief(tx: &Transaction<'_>, brief: &BriefRecord) -> Result<()> {
    insert_brief_evidence_tx(tx, brief)?;
    register_committed_input_evidence_tx(
        tx,
        &scope(),
        &[CanonicalInputEvidence::Brief(brief.clone())],
    )?;
    Ok(())
}

#[test]
fn actual_work_ack_and_noop_preserve_exact_final_coverage() -> Result<()> {
    let (_dir, db) = fresh()?;
    let before = db
        .transaction(|tx| {
            prepare_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    let value = work("actual-work", WorkItemState::Open);
    let facts = db.transaction(|tx| insert_qualified_work(tx, &value))?;
    let resident = before.with_committed(&facts.delta)?;
    let captured = resident.capture(&WorkCaptureRequest::current())?;
    assert_eq!(captured.records.len(), 1);
    assert_eq!(
        captured.records[0].record,
        CanonicalWorkRecord::Work(value.clone())
    );
    assert!(resident.coverage().live_work_ids.contains(&value.id));
    assert!(!resident
        .coverage()
        .mandatory_roots
        .contains(&WorkKey::new(WorkFamily::Work, &value.id)));
    let same = db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]);
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &AgentState::new("agent-a"),
            &request(),
        )
    })?;
    assert_eq!(same.delta.base_revision, same.delta.revision);
    assert!(same.delta.records.is_empty());
    assert_eq!(
        resident.with_committed(&same.delta)?.coverage(),
        resident.coverage()
    );
    Ok(())
}

#[test]
fn fixed_manifest_limits_history_and_never_adopts_a_raw_late_work_or_result() -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("live-fixed", WorkItemState::Open))?;
        for index in 0..130 {
            raw_legacy_work(
                tx,
                &work(&format!("history-{index:03}"), WorkItemState::Completed),
            )?;
        }
        Ok(())
    })?;
    db.transaction(|tx| {
        raw_legacy_work(tx, &work("late-unqualified", WorkItemState::Completed))?;
        insert_brief_evidence_tx(tx, &brief("late-result", "live-fixed"))?;
        let loaded =
            prepare_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert_eq!(loaded.resident.coverage().nonopen.keys.len(), 128);
        assert!(loaded.resident.coverage().nonopen.is_exhaustive);
        assert_eq!(
            loaded.resident.coverage().latest_results["live-fixed"],
            None
        );
        assert!(!loaded
            .resident
            .coverage()
            .observations
            .contains_key(&WorkKey::new(WorkFamily::Work, "late-unqualified")));
        let mut explicit = request();
        explicit
            .capture
            .mandatory_roots
            .insert(WorkKey::new(WorkFamily::Work, "late-unqualified"));
        assert!(prepare_work_domain_snapshot_tx(tx, &scope(), &explicit).is_err());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn fresh_reincarnation_does_not_inherit_old_work_and_pruned_ids_never_resurrect() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = work("old-work", WorkItemState::Completed);
    db.transaction(|tx| insert_qualified_work(tx, &value).map(|_| ()))?;
    db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]);
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        tx.execute("DELETE FROM work_items WHERE work_item_id=?1", [&value.id])?;
        let facts =
            register_work_pruned_tx(tx, &proof, &keys, &AgentState::new("agent-a"), &request())?;
        assert_eq!(
            facts.delta.removals[&WorkKey::new(WorkFamily::Work, &value.id)],
            WorkAvailability::Pruned
        );
        assert!(prepare_work_write_tx(tx, &scope(), &keys).is_err());
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut next = identity();
        next.incarnation = 2;
        upsert_agent_identity_tx(tx, &next)?;
        let next_scope = InputEvidenceScope {
            identity_incarnation: 2,
            ..scope()
        };
        raw_legacy_work(tx, &value)?;
        assert!(prepare_work_write_tx(
            tx,
            &next_scope,
            &BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)])
        )
        .is_err());
        let loaded =
            prepare_work_domain_snapshot_tx(tx, &next_scope, &request())?.require_current()?;
        assert!(loaded.resident.coverage().nonopen.keys.is_empty());
        assert!(matches!(
            prepare_work_domain_snapshot_tx(tx, &scope(), &request())?,
            WorkBootstrapSnapshot::Retired { .. }
        ));
        Ok(())
    })?;
    Ok(())
}

#[test]
fn implicit_wait_cancellation_returns_actual_millis_rows_and_complete_turn_frontier() -> Result<()>
{
    let (_dir, db) = fresh()?;
    let old: WaitConditionRecord = serde_json::from_value(
        serde_json::json!({"id":"old-wait","agent_id":"agent-a","status":"active","kind":"operator","waiting_for":"operator","created_at":time(),"updated_at":time(),"turn_id":"wait-turn"}),
    )?;
    db.transaction(|tx| {
        let keys = work_wait_write_keys_tx(tx, std::slice::from_ref(&old))?;
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        let written = repositories::upsert_wait_condition_with_writes_tx(tx, &old)?;
        let actual = written
            .wait_ids
            .into_iter()
            .map(|id| WorkKey::new(WorkFamily::Wait, id))
            .collect();
        register_work_writes_tx(
            tx,
            &proof,
            &actual,
            &BTreeSet::new(),
            &AgentState::new("agent-a"),
            &request(),
        )?;
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut newer = old.clone();
        newer.id = "new-wait".into();
        newer.updated_at += chrono::Duration::seconds(1);
        let keys = work_wait_write_keys_tx(tx, std::slice::from_ref(&newer))?;
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        let written = repositories::upsert_wait_condition_with_writes_tx(tx, &newer)?;
        assert_eq!(
            written.wait_ids,
            BTreeSet::from([old.id.clone(), newer.id.clone()])
        );
        let actual = written
            .wait_ids
            .into_iter()
            .map(|id| WorkKey::new(WorkFamily::Wait, id))
            .collect();
        let mut policy = request();
        policy.capture.required_turn_ids.insert("wait-turn".into());
        let facts = register_work_writes_tx(
            tx,
            &proof,
            &actual,
            &BTreeSet::new(),
            &AgentState::new("agent-a"),
            &policy,
        )?;
        assert_eq!(
            facts.delta.coverage.turn_wait_keys["wait-turn"],
            BTreeSet::from([old.id.clone(), newer.id.clone()])
        );
        let cancelled = facts
            .delta
            .records
            .iter()
            .find(|record| record.key.id == old.id)
            .unwrap();
        let CanonicalWorkRecord::Wait(cancelled) = &cancelled.record else {
            panic!("Wait expected")
        };
        assert_eq!(cancelled.status, WaitConditionStatus::Cancelled);
        assert_eq!(cancelled.updated_at.timestamp_subsec_nanos() % 1_000_000, 0);
        assert!(facts.delta.coverage.unresolved_wait_ids.contains(&newer.id));
        assert!(!facts.delta.coverage.unresolved_wait_ids.contains(&old.id));
        Ok(())
    })?;
    Ok(())
}

#[test]
fn qualification_failure_rolls_back_the_original_work_sql() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {tx.execute_batch("CREATE TRIGGER fail_work_alias BEFORE INSERT ON work_domain_records BEGIN SELECT RAISE(ABORT,'fixture qualification failure'); END")?;Ok(())})?;
    let error = db
        .transaction(|tx| insert_qualified_work(tx, &work("rolled-back", WorkItemState::Open)))
        .unwrap_err();
    assert!(format!("{error:#}").contains("fixture qualification failure"));
    let count: i64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM work_items WHERE work_item_id='rolled-back'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn qualified_result_head_uses_original_sort_and_optional_sources_are_edge_bound() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("result-work", WorkItemState::Completed))?;
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut a = brief("result-a", "result-work");
        a.content_source = BriefContentSource::TranscriptEntry {
            entry_id: "missing-transcript".into(),
            relation: BriefContentSourceRelation::DerivedFrom,
        };
        let mut z = a.clone();
        z.id = "result-z".into();
        write_brief(tx, &a)?;
        write_brief(tx, &z)?;
        let facts = register_work_result_writes_tx(
            tx,
            &scope(),
            &BTreeSet::from([a.id.clone(), z.id.clone()]),
            &request(),
        )?;
        assert_eq!(
            facts.delta.coverage.latest_results["result-work"]
                .as_ref()
                .unwrap()
                .brief_id,
            z.id
        );
        let selected = BTreeSet::from([z.id.clone()]);
        let proofs = completion_source_observations_tx(
            tx,
            &scope(),
            &selected,
            &InputEvidenceKey::new(InputEvidenceFamily::Brief, &z.id),
        )?;
        let read = ContextDurableRead::new(tx, "agent-a").scope_read()?;
        assert_eq!(
            completion_source_observations_with_scope_tx(
                tx,
                &scope(),
                &selected,
                &InputEvidenceKey::new(InputEvidenceFamily::Brief, &z.id),
                Some(&read),
            )?,
            proofs
        );
        assert_eq!(proofs.len(), 1);
        assert_eq!(proofs[0].declared_target.id, "missing-transcript");
        assert_eq!(proofs[0].head.key.id, z.id);
        assert_eq!(proofs[0].source, proofs[0].head);
        assert!(completion_source_observations_tx(
            tx,
            &scope(),
            &BTreeSet::from([a.id.clone()]),
            &InputEvidenceKey::new(InputEvidenceFamily::Brief, &a.id)
        )
        .is_err());
        let mut unqualified = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::json!({"text":"late"}),
        );
        unqualified.id = "missing-transcript".into();
        let (unqualified, _) = append_transcript_entry_tx(tx, &unqualified)?;
        assert!(completion_source_observations_tx(
            tx,
            &scope(),
            &selected,
            &InputEvidenceKey::new(InputEvidenceFamily::Brief, &z.id)
        )
        .is_err());
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Transcript(unqualified.clone())],
        )?;
        let mut policy = request();
        policy
            .capture
            .latest_result_work_ids
            .insert("result-work".into());
        let refreshed =
            prepare_work_domain_snapshot_tx(tx, &scope(), &policy)?.require_current()?;
        assert_eq!(
            refreshed.resident.coverage().latest_results["result-work"]
                .as_ref()
                .unwrap()
                .transcript_entry_id
                .as_deref(),
            Some("missing-transcript")
        );
        assert!(completion_source_observations_tx(
            tx,
            &scope(),
            &selected,
            &InputEvidenceKey::new(InputEvidenceFamily::Brief, &z.id)
        )?
        .is_empty());
        tx.execute(
            "DELETE FROM transcript_entries WHERE evidence_id=?1",
            [&unqualified.id],
        )?;
        crate::runtime_db::input_evidence_snapshot::register_pruned_input_evidence_tx(
            tx,
            &scope(),
            &[InputEvidenceKey::new(
                InputEvidenceFamily::Transcript,
                &unqualified.id,
            )],
        )?;
        assert!(completion_source_observations_tx(
            tx,
            &scope(),
            &selected,
            &InputEvidenceKey::new(InputEvidenceFamily::Brief, &z.id)
        )
        .is_err());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn deleting_cleanup_requires_original_qualified_work_and_keeps_scope_revision_strict() -> Result<()>
{
    let (_dir, db) = fresh()?;
    let value = work("deleting-work", WorkItemState::Open);
    db.transaction(|tx| {
        insert_qualified_work(tx, &value)?;
        let mut deleting = identity();
        deleting.status = AgentRegistryStatus::Deleting;
        upsert_agent_identity_tx(tx, &deleting)?;
        Ok(())
    })?;
    db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]);
        assert!(prepare_work_write_tx(tx, &scope(), &keys).is_err());
        let proof = prepare_original_work_write_tx(tx, &scope(), &keys)?;
        proof.validate_admission_tx(tx, &value.id)?;
        let revision = loader::current_coverage(tx, &scope())?.unwrap().revision;
        proof.validate_domain_observation_tx(tx, revision)?;
        assert!(proof
            .validate_domain_observation_tx(tx, revision + 1)
            .unwrap_err()
            .downcast_ref::<WorkSnapshotError>()
            .is_some());
        assert_eq!(
            proof.admitted_work_ids(),
            BTreeSet::from([value.id.clone()])
        );
        let mut changed = value.clone();
        changed.objective = "cleanup update".into();
        changed.revision += 1;
        changed.updated_at += chrono::Duration::seconds(1);
        repositories::update_expected_work_item_tx(tx, &changed, value.revision)?;
        let facts = register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &AgentState::new("agent-a"),
            &request(),
        )?;
        assert!(facts
            .delta
            .records
            .iter()
            .any(|record| record.record == CanonicalWorkRecord::Work(changed.clone())));
        assert!(prepare_original_work_write_tx(
            tx,
            &scope(),
            &BTreeSet::from([WorkKey::new(WorkFamily::Work, "new-without-admission")])
        )
        .is_err());
        Ok(())
    })?;
    let revision =
        db.transaction(|tx| loader::current_coverage(tx, &scope()).map(|x| x.unwrap().revision))?;
    // Normal observations cannot turn a retired-cleanup scope into new activation.
    assert!(db
        .transaction(|tx| validate_work_domain_observation_tx(tx, &scope(), revision))
        .is_err());
    db.transaction(|tx| {
        let proof = prepare_original_work_write_tx(
            tx,
            &scope(),
            &BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]),
        )?;
        let mut successor = identity();
        successor.incarnation += 1;
        upsert_agent_identity_tx(tx, &successor)?;
        assert!(proof.validate_domain_observation_tx(tx, revision).is_err());
        assert!(prepare_original_work_write_tx(
            tx,
            &scope(),
            &BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)])
        )
        .is_err());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn revision_change_is_typed_and_oversized_required_body_has_no_partial_prepare() -> Result<()> {
    let (_dir, db) = fresh()?;
    let before =
        db.transaction(|tx| loader::current_coverage(tx, &scope()).map(|x| x.unwrap().revision))?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("versioned", WorkItemState::Open))?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| validate_work_domain_observation_tx(tx, &scope(), before))
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::RevisionMismatch{expected,..}) if *expected==before)
    );
    let (_legacy_dir, legacy_db) =
        legacy(|tx| raw_legacy_work(tx, &work("bounded", WorkItemState::Completed)))?;
    let mut small = request();
    small.byte_limit = 1;
    let error = legacy_db
        .transaction(|tx| prepare_work_domain_snapshot_tx(tx, &scope(), &small))
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkSnapshotError>(),
        Some(WorkSnapshotError::ResourceExhausted("selected body bytes"))
    ));
    let aliases: i64 = legacy_db.connection()?.query_row(
        "SELECT COUNT(*) FROM work_domain_records",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(aliases, 0);
    Ok(())
}

#[test]
fn actual_result_outside_history_window_survives_trim_and_by_id_prepare() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("old-outside-window", WorkItemState::Completed))?;
        for i in 0..129 {
            let mut value = work(&format!("new-{i:03}"), WorkItemState::Completed);
            value.updated_at += chrono::Duration::seconds(1);
            insert_qualified_work(tx, &value)?;
        }
        let before =
            prepare_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert!(!before
            .resident
            .coverage()
            .observations
            .contains_key(&WorkKey::new(WorkFamily::Work, "old-outside-window")));
        let result = brief("outside-result", "old-outside-window");
        write_brief(tx, &result)?;
        register_work_result_writes_tx(
            tx,
            &scope(),
            &BTreeSet::from([result.id.clone()]),
            &request(),
        )?;
        assert!(loader::current_coverage(tx, &scope())?
            .unwrap()
            .latest_results
            .get("old-outside-window")
            .is_none());
        let mut policy = request();
        policy
            .capture
            .latest_result_work_ids
            .insert("old-outside-window".into());
        let again = prepare_work_domain_snapshot_tx(tx, &scope(), &policy)?.require_current()?;
        assert_eq!(
            again.resident.coverage().latest_results["old-outside-window"]
                .as_ref()
                .unwrap()
                .brief_id,
            result.id
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn new_work_and_qualified_result_share_one_finalizer_revision() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        let value = work("new-completed", WorkItemState::Completed);
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]);
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        repositories::insert_new_work_item_tx(tx, &value)?;
        let result = brief("same-tx-result", &value.id);
        write_brief(tx, &result)?;
        let facts = register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::from([result.id.clone()]),
            &AgentState::new("agent-a"),
            &request(),
        )?;
        assert_eq!(facts.delta.revision, facts.delta.base_revision + 1);
        assert_eq!(
            facts.delta.coverage.latest_results[&value.id]
                .as_ref()
                .unwrap()
                .brief_id,
            result.id
        );
        assert_eq!(
            loader::current_coverage(tx, &scope())?.unwrap().revision,
            facts.delta.revision
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn preflight_checks_fixed_bodies_without_writing_sidecars_or_advancing_proof() -> Result<()> {
    let (_dir, db) =
        legacy(|tx| raw_legacy_work(tx, &work("fixed-preflight", WorkItemState::Completed)))?;
    db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, "fixed-preflight")]);
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        let checked =
            preflight_work_write_snapshot_tx(tx, &proof, &request())?.require_current()?;
        assert!(checked
            .resident
            .coverage()
            .observations
            .contains_key(&WorkKey::new(WorkFamily::Work, "fixed-preflight")));
        assert!(loader::current_coverage(tx, &scope())?.is_none());
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM work_domain_records", [], |row| {
            row.get(0)
        })?;
        assert_eq!(count, 0);
        let heads: i64 =
            tx.query_row("SELECT COUNT(*) FROM work_domain_result_heads", [], |row| {
                row.get(0)
            })?;
        assert_eq!(heads, 0);
        let mut changed = work("fixed-preflight", WorkItemState::Completed);
        changed.objective = "prepared change".into();
        changed.revision = 2;
        changed.updated_at += chrono::Duration::seconds(1);
        repositories::update_expected_work_item_tx(tx, &changed, 1)?;
        let facts = register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &AgentState::new("agent-a"),
            &request(),
        )?;
        assert_eq!(facts.delta.base_revision, 1);
        assert_eq!(facts.delta.revision, 2);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn bound_result_head_is_one_completion_edge_while_intent_source_stays_strict() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        let mut value = work("bound-completion", WorkItemState::Completed);
        value.result_brief_id = Some("bound-result".into());
        value.completion_intent = Some(crate::types::WorkItemCompletionIntent {
            work_item_id: value.id.clone(),
            source_activation_id: None,
            source_message_id: Some("independent-source".into()),
            source_turn_id: Some("independent-turn".into()),
            expected_work_revision: value.revision,
            report_requirement: crate::types::CompletionReportRequirement::Required,
            report_state: crate::types::CompletionReportState::Bound,
            result_brief_id: value.result_brief_id.clone(),
            created_at: time(),
            updated_at: time(),
        });
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]);
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        repositories::insert_new_work_item_tx(tx, &value)?;
        let head = brief("bound-result", &value.id);
        write_brief(tx, &head)?;
        let state = ContextDurableRead::new(tx, "agent-a")
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        let facts = register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::from([head.id.clone()]),
            &state,
            &request(),
        )?;
        let head_key = InputEvidenceKey::new(InputEvidenceFamily::Brief, &head.id);
        assert!(facts.input_roots().contains(&head_key));
        assert!(!facts.owning_input_roots.contains(&head_key));
        assert!(facts.owning_input_roots.contains(&InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            "independent-source"
        )));
        assert!(facts.owning_input_roots.contains(&InputEvidenceKey::new(
            InputEvidenceFamily::Turn,
            "independent-turn"
        )));
        // A genuinely different previous result remains a strict owning edge.
        let mut later = brief("later-result", &value.id);
        later.created_at += chrono::Duration::seconds(1);
        write_brief(tx, &later)?;
        let facts = register_work_result_writes_tx(
            tx,
            &scope(),
            &BTreeSet::from([later.id.clone()]),
            &request(),
        )?;
        assert!(facts.owning_input_roots.contains(&head_key));
        Ok(())
    })?;
    Ok(())
}

#[path = "input_completion_tests.rs"]
mod input_completion_tests;

fn raw_legacy_wait(tx: &Transaction<'_>, wait: &WaitConditionRecord) -> Result<()> {
    let stamp = |at: DateTime<Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    tx.execute("INSERT INTO wait_conditions(wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,waiting_for,created_at,updated_at,expires_at,resolved_at,cancelled_at,last_turn_id,trigger_message_id,triggered_at,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",params![wait.id,wait.agent_id,wait.work_item_id,serde_json::to_value(&wait.status)?.as_str().unwrap(),serde_json::to_value(&wait.kind)?.as_str().unwrap(),wait.source,wait.subject_ref,wait.waiting_for,stamp(wait.created_at),stamp(wait.updated_at),wait.expires_at.map(stamp),wait.resolved_at.map(stamp),wait.cancelled_at.map(stamp),wait.turn_id,wait.trigger_message_id,wait.triggered_at.map(stamp),serde_json::to_string(wait)?])?;
    Ok(())
}
fn task_wait(id: &str, task: &str, trigger: &str) -> Result<WaitConditionRecord> {
    Ok(serde_json::from_value(
        serde_json::json!({"id":id,"agent_id":"agent-a","status":"resolved","kind":"task","waiting_for":"task","wake_sources":[{"kind":"task_result","task_id":task}],"trigger_message_id":trigger,"created_at":time(),"updated_at":time(),"resolved_at":time()}),
    )?)
}
fn write_qualified_wait(
    tx: &Transaction<'_>,
    wait: &WaitConditionRecord,
    policy: &WorkDomainLoadRequest,
) -> Result<CommittedWorkFacts> {
    let keys = work_wait_write_keys_tx(tx, std::slice::from_ref(wait))?;
    let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
    let actual = repositories::upsert_wait_condition_with_writes_tx(tx, wait)?
        .wait_ids
        .into_iter()
        .map(|id| WorkKey::new(WorkFamily::Wait, id))
        .collect();
    let state = ContextDurableRead::new(tx, "agent-a")
        .scope_facts()?
        .into_owned_state()?
        .unwrap();
    register_work_writes_tx(tx, &proof, &actual, &BTreeSet::new(), &state, policy)
}

#[test]
fn fixed_accepted_task_result_keeps_all_status_waits_and_owner_outside_optional_window(
) -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        let mut owner = work("old-wait-owner", WorkItemState::Completed);
        owner.created_at -= chrono::Duration::days(1);
        owner.updated_at = owner.created_at;
        raw_legacy_work(tx, &owner)?;
        for index in 0..129 {
            raw_legacy_work(
                tx,
                &work(&format!("newer-{index:03}"), WorkItemState::Completed),
            )?;
        }
        let mut message = crate::types::MessageEnvelope::new(
            "agent-a",
            crate::types::MessageKind::TaskResult,
            crate::types::MessageOrigin::Task {
                task_id: "accepted-task".into(),
            },
            crate::types::AuthorityClass::RuntimeInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "result".into(),
            },
        );
        message.id = "accepted-result".into();
        message.task_id = Some("accepted-task".into());
        crate::runtime_db::evidence::upsert_message_tx(tx, &message)?;
        tx.execute("INSERT INTO queue_entries(message_id,agent_id,priority,status,created_at,updated_at,payload_json) VALUES(?1,'agent-a','normal','queued',?2,?2,'{}')",params![message.id,time().to_rfc3339()])?;
        let mut state = ContextDurableRead::new(tx, "agent-a")
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        state.pending = 1;
        upsert_agent_state_tx(tx, &state)?;
        let mut previous_result = message.clone();
        previous_result.id = "accepted-earlier-result".into();
        previous_result.created_at -= chrono::Duration::seconds(1);
        crate::runtime_db::evidence::upsert_message_tx(tx, &previous_result)?;
        for (id, status, trigger) in [
            ("a-resolved", "resolved", "accepted-result"),
            ("z-cancelled", "cancelled", "accepted-earlier-result"),
        ] {
            let mut wait = task_wait(id, "accepted-task", trigger)?;
            wait.work_item_id = Some(owner.id.clone());
            if status == "cancelled" {
                wait.status = WaitConditionStatus::Cancelled;
                wait.resolved_at = None;
                wait.cancelled_at = Some(time());
            }
            raw_legacy_wait(tx, &wait)?;
        }
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut policy=request();policy.capture.nonopen_limit=0;
        policy.capture.required_task_ids.insert("accepted-task".into());
        policy.capture.required_trigger_message_ids.insert("accepted-result".into());
        let resident=prepare_work_domain_snapshot_tx(tx,&scope(),&policy)?.require_current()?.resident;
        let captured=resident.capture(&policy.capture)?;
        assert_eq!(captured.waits_for_task("accepted-task")?.iter().map(|wait|wait.id.as_str()).collect::<Vec<_>>(),vec!["a-resolved","z-cancelled"]);
        assert_eq!(captured.waits_triggered_by("accepted-result")?.iter().map(|wait|wait.id.as_str()).collect::<Vec<_>>(),vec!["a-resolved"]);
        assert!(captured.records.iter().any(|record|record.key==WorkKey::new(WorkFamily::Work,"old-wait-owner")));
        assert!(resident.coverage().nonopen.keys.is_empty());
        let edges:i64=tx.query_row("SELECT COUNT(*) FROM work_domain_wait_edges WHERE agent_id='agent-a' AND identity_incarnation=1 AND availability='present'",[],|row|row.get(0))?;
        assert_eq!(edges,4);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn raw_late_matching_wait_is_not_an_observed_empty_or_new_qualification() -> Result<()> {
    let (_dir, db) = legacy(|_| Ok(()))?;
    db.transaction(|tx| {
        let wait = task_wait("raw-late-wait", "late-task", "late-trigger")?;
        repositories::upsert_wait_condition_with_writes_tx(tx, &wait)?;
        Ok(())
    })?;
    let mut policy = request();
    policy.capture.required_task_ids.insert("late-task".into());
    let error = db
        .transaction(|tx| prepare_work_domain_snapshot_tx(tx, &scope(), &policy))
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::NeedsPreparation(key)) if key==&WorkKey::new(WorkFamily::Wait,"raw-late-wait"))
    );
    db.transaction(|tx| {
        assert!(alias(
            tx,
            &scope(),
            &WorkKey::new(WorkFamily::Wait, "raw-late-wait")
        )?
        .is_none());
        let count: i64 =
            tx.query_row("SELECT COUNT(*) FROM work_domain_wait_edges", [], |row| {
                row.get(0)
            })?;
        assert_eq!(count, 0);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn qualified_wait_relations_replace_superseded_edges_and_survive_trim_then_prune() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut wait = task_wait(
        "qualified-relation-wait",
        "original-task",
        "original-trigger",
    )?;
    wait.status = WaitConditionStatus::Active;
    wait.resolved_at = None;
    db.transaction(|tx| write_qualified_wait(tx, &wait, &request()).map(|_| ()))?;
    db.transaction(|tx| {
        let mut changed=wait.clone();changed.status=WaitConditionStatus::Resolved;changed.resolved_at=Some(time()+chrono::Duration::seconds(1));changed.wake_sources=vec![crate::types::WakeSource::TaskResult{task_id:"new-task".into()}];changed.trigger_message_id=Some("new-trigger".into());changed.updated_at+=chrono::Duration::seconds(1);
        let mut policy=request();policy.capture.required_task_ids.extend(["original-task".into(),"new-task".into()]);policy.capture.required_trigger_message_ids.extend(["original-trigger".into(),"new-trigger".into()]);
        let delta=write_qualified_wait(tx,&changed,&policy)?.delta;
        assert!(delta.coverage.task_wait_keys["original-task"].is_empty());
        assert_eq!(delta.coverage.task_wait_keys["new-task"],BTreeSet::from([wait.id.clone()]));
        assert!(delta.coverage.trigger_wait_keys["original-trigger"].is_empty());
        let old:i64=tx.query_row("SELECT COUNT(*) FROM work_domain_wait_edges WHERE source_id IN ('original-task','original-trigger')",[],|row|row.get(0))?;assert_eq!(old,0);
        Ok(())
    })?;
    db.transaction(|tx| {
        let loaded =
            prepare_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert!(loaded.resident.coverage().task_wait_keys.is_empty());
        Ok(())
    })?;
    db.transaction(|tx| {
        let keys=BTreeSet::from([WorkKey::new(WorkFamily::Wait,&wait.id)]);
        let proof=prepare_work_write_tx(tx,&scope(),&keys)?;
        tx.execute("DELETE FROM wait_conditions WHERE wait_condition_id=?1",[&wait.id])?;
        let state=ContextDurableRead::new(tx,"agent-a").scope_facts()?.into_owned_state()?.unwrap();
        register_work_pruned_tx(tx,&proof,&keys,&state,&request())?;
        let pruned:i64=tx.query_row("SELECT COUNT(*) FROM work_domain_wait_edges WHERE wait_id=?1 AND availability='pruned'",[&wait.id],|row|row.get(0))?;assert_eq!(pruned,2);
        Ok(())
    })?;
    let mut task_policy = request();
    task_policy
        .capture
        .required_task_ids
        .insert("new-task".into());
    let mut trigger_policy = request();
    trigger_policy
        .capture
        .required_trigger_message_ids
        .insert("new-trigger".into());
    for policy in [task_policy, trigger_policy] {
        let error = db
            .transaction(|tx| prepare_work_domain_snapshot_tx(tx, &scope(), &policy))
            .unwrap_err();
        assert!(
            matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::NeedsPreparation(key)) if key.id==wait.id)
        );
    }
    db.transaction(|tx| {
        let mut another=scope();another.identity_incarnation=2;
        let edges:i64=tx.query_row("SELECT COUNT(*) FROM work_domain_wait_edges WHERE agent_id=?1 AND identity_incarnation=?2",params![another.agent_id,another.identity_incarnation],|row|row.get(0))?;assert_eq!(edges,0);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn terminal_wait_ack_keeps_exact_present_observation_without_a_turn_or_owner() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut wait = task_wait("no-turn-terminal", "original-task", "original-trigger")?;
    wait.status = WaitConditionStatus::Active;
    wait.resolved_at = None;
    wait.turn_id = None;
    wait.work_item_id = None;
    wait.trigger_message_id = None;
    wait.triggered_at = None;
    db.transaction(|tx| write_qualified_wait(tx, &wait, &request()).map(|_| ()))?;
    let before = db
        .transaction(|tx| {
            load_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    assert!(!before
        .coverage()
        .mandatory_roots
        .contains(&WorkKey::new(WorkFamily::Wait, &wait.id)));
    let mut cancelled = wait.clone();
    cancelled.status = WaitConditionStatus::Cancelled;
    cancelled.updated_at += chrono::Duration::seconds(1);
    cancelled.cancelled_at = Some(cancelled.updated_at);
    let delta = db.transaction(|tx| Ok(write_qualified_wait(tx, &cancelled, &request())?.delta))?;
    let key = WorkKey::new(WorkFamily::Wait, &wait.id);
    assert_eq!(
        delta.coverage.observations.get(&key),
        Some(&WorkAvailability::Present)
    );
    assert!(!delta.coverage.unresolved_wait_ids.contains(&wait.id));
    assert!(delta.coverage.turn_wait_keys.is_empty());
    let next = before.with_committed(&delta)?;
    let captured = next.capture(&WorkCaptureRequest {
        mandatory_roots: BTreeSet::from([key.clone()]),
        ..WorkCaptureRequest::current()
    })?;
    let Some(record) = captured.records.iter().find(|record| record.key == key) else {
        anyhow::bail!("terminal ACK disappeared from its committed resident");
    };
    let CanonicalWorkRecord::Wait(actual) = &record.record else {
        unreachable!()
    };
    assert_eq!(actual.status, WaitConditionStatus::Cancelled);
    assert_eq!(
        actual.cancelled_at.unwrap().timestamp_millis(),
        cancelled.updated_at.timestamp_millis()
    );
    assert_eq!(
        before
            .capture(&WorkCaptureRequest::current())?
            .records
            .iter()
            .find_map(|record| match &record.record {
                CanonicalWorkRecord::Wait(wait) => Some(wait.status.clone()),
                _ => None,
            })
            .unwrap(),
        WaitConditionStatus::Active
    );
    Ok(())
}

#[test]
fn wait_cleanup_preserves_current_history_without_loading_optional_body_debt() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut history = work("retained-history", WorkItemState::Completed);
    history.objective = "h".repeat(16 * 1024);
    db.transaction(|tx| insert_qualified_work(tx, &history).map(|_| ()))?;
    let mut wait = task_wait("cleanup-current-wait", "cleanup-task", "cleanup-trigger")?;
    wait.status = WaitConditionStatus::Active;
    wait.resolved_at = None;
    wait.turn_id = None;
    wait.trigger_message_id = None;
    wait.triggered_at = None;
    db.transaction(|tx| write_qualified_wait(tx, &wait, &request()).map(|_| ()))?;
    let prior = db
        .transaction(|tx| {
            load_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    let mut policy = request();
    policy.capture.nonopen_limit = 0;
    policy.byte_limit = 4096;
    let delta = db.transaction(|tx| {
        let proof = prepare_work_wait_cleanup_tx(
            tx,
            &scope(),
            &BTreeSet::from([wait.id.clone()]),
            &policy,
        )?;
        let mut changed = wait.clone();
        changed.status = WaitConditionStatus::Cancelled;
        changed.updated_at += chrono::Duration::seconds(1);
        changed.cancelled_at = Some(changed.updated_at);
        let writes = repositories::upsert_wait_condition_with_writes_tx(tx, &changed)?;
        let state = ContextDurableRead::new(tx, "agent-a")
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        Ok(register_work_wait_cleanup_tx(tx, &proof, &writes, &state, &policy)?.delta)
    })?;
    assert_eq!(delta.coverage.nonopen, prior.coverage().nonopen);
    assert_eq!(
        delta.coverage.latest_results,
        prior.coverage().latest_results
    );
    assert_eq!(delta.records.len(), 1);
    assert_eq!(
        delta.records[0].key,
        WorkKey::new(WorkFamily::Wait, &wait.id)
    );
    let next = prior.with_committed(&delta)?;
    let history_key = WorkKey::new(WorkFamily::Work, &history.id);
    let old = prior.capture(&WorkCaptureRequest::current())?;
    let new = next.capture(&WorkCaptureRequest::current())?;
    let old = old
        .records
        .iter()
        .find(|record| record.key == history_key)
        .unwrap();
    let new = new
        .records
        .iter()
        .find(|record| record.key == history_key)
        .unwrap();
    assert!(Arc::ptr_eq(old, new));
    assert!(!next.coverage().unresolved_wait_ids.contains(&wait.id));
    assert!(prior.coverage().unresolved_wait_ids.contains(&wait.id));
    Ok(())
}

#[path = "completion_writes_tests.rs"]
mod completion_writes_tests;

#[path = "startup_wait_tests.rs"]
mod startup_wait_tests;

#[test]
fn finalizer_view_has_exact_final_revision_and_selected_bodies_even_for_noop() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = work("final-memory-view", WorkItemState::Open);
    db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]);
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        repositories::insert_new_work_item_tx(tx, &value)?;
        let (facts, view) = register_work_writes_with_view_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &AgentState::new("agent-a"),
            &request(),
        )?;
        assert_eq!(facts.delta.revision, facts.delta.base_revision + 1);
        assert_eq!(view.revision, facts.delta.revision);
        assert!(view.live_complete);
        assert_eq!(view.records.len(), 1);
        assert_eq!(
            view.records[0].record,
            CanonicalWorkRecord::Work(value.clone())
        );
        Ok(())
    })?;
    db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]);
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        let (facts, view) = register_work_writes_with_view_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &AgentState::new("agent-a"),
            &request(),
        )?;
        assert_eq!(facts.delta.base_revision, facts.delta.revision);
        assert!(facts.delta.records.is_empty());
        assert_eq!(view.revision, facts.delta.revision);
        assert!(view.live_work_ids.contains(&value.id));
        assert_eq!(view.records.len(), 1);
        assert_eq!(
            view.records[0].record,
            CanonicalWorkRecord::Work(value.clone())
        );
        assert_eq!(
            loader::current_coverage(tx, &scope())?.unwrap(),
            facts.delta.coverage
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn finalizer_view_selection_does_not_depend_on_working_memory_overlay() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = work("stable-work-selection", WorkItemState::Open);
    db.transaction(|tx| insert_qualified_work(tx, &value).map(|_| ()))?;
    db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &value.id)]);
        let proof = prepare_work_write_tx(tx, &scope(), &keys)?;
        let mut state = AgentState::new("agent-a");
        state.working_memory.current_working_memory.objective = Some("only-memory-overlay".into());
        upsert_agent_state_tx(tx, &state)?;
        let (facts, view) = register_work_writes_with_view_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &state,
            &request(),
        )?;
        assert_eq!(facts.delta.base_revision, facts.delta.revision);
        assert_eq!(view.revision, facts.delta.revision);
        assert_eq!(
            view.records[0].record,
            CanonicalWorkRecord::Work(value.clone())
        );
        assert_eq!(view.live_work_ids, facts.delta.coverage.live_work_ids);
        Ok(())
    })?;
    Ok(())
}
