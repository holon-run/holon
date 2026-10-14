use super::tests::{at, legacy, request, scope, task};
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        evidence,
        input_evidence_snapshot::{prepare_input_evidence_snapshot_tx, InputEvidenceLoadRequest},
    },
    types::{
        AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus, TaskKind,
        TaskStatus,
    },
};
const TASK: &str = "fixed-original-running-task";
const SOURCE: &str = "fixed-original-running-source";
fn original(tx: &Transaction<'_>) -> Result<()> {
    let mut value = task(TASK, TaskStatus::Queued);
    value.kind = TaskKind::ActorInvocation;
    value.detail = Some(serde_json::json!({"rejoin_obligation_id":TASK,"rejoin_generation":1}));
    repositories::upsert_task_tx(tx, &value)?;
    let mut source = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TaskStatus,
        MessageOrigin::Task {
            task_id: TASK.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original pending Running".into(),
        },
    );
    source.id = SOURCE.into();
    source.turn_id = Some("original-running-preallocated-turn".into());
    source.created_at = at() + chrono::Duration::seconds(1);
    source.task_id = Some(TASK.into());
    source.admission_context = Some(AdmissionContext::RuntimeOwned);
    source.delivery_surface = Some(MessageDeliverySurface::TaskRejoin);
    source.metadata = Some(
        serde_json::json!({"task_id":TASK,"task_kind":"actor_invocation","task_status":"running","task_detail":value.detail}),
    );
    source.normalize_admission_fields();
    let (source, _) = evidence::append_message_tx(tx, &source)?;
    repositories::upsert_queue_entry_tx(
        tx,
        &QueueEntryRecord {
            message_id: source.id,
            agent_id: scope().agent_id,
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: source.created_at,
            updated_at: source.created_at,
        },
    )?;
    let mut state = AgentState::new(&scope().agent_id);
    state.total_message_count = 1;
    evidence::upsert_agent_state_tx(tx, &state)?;
    Ok(())
}
fn capture_source(
    tx: &Transaction<'_>,
) -> Result<crate::runtime::resident_input_evidence::CapturedInputEvidence> {
    let request = InputEvidenceLoadRequest {
        windows: vec![],
        mandatory_roots: BTreeSet::from([InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            SOURCE,
        )]),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    };
    let crate::runtime_db::input_evidence_snapshot::InputEvidenceBootstrapSnapshot::Current(
        current,
    ) = prepare_input_evidence_snapshot_tx(tx, &scope(), &request)?
    else {
        anyhow::bail!("original Input scope retired");
    };
    current.resident.capture()
}
#[test]
fn v81_pending_running_freezes_full_precision_generation_without_current_source_alias() -> Result<()>
{
    let (_home, db) = legacy(true, original)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (resident, inputs) = db.transaction(|tx| {
        let current =
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
                .require_current()?;
        let association = &current.resident.coverage().running.sources[SOURCE];
        assert_eq!(association.created_at, at());
        assert!(association.created_at.to_rfc3339().contains("123456789"));
        let current_alias: u64 = tx.query_row(
            "SELECT COUNT(*) FROM input_evidence_current WHERE family='message' AND record_id=?1",
            [SOURCE],
            |row| row.get(0),
        )?;
        assert_eq!(
            current_alias, 0,
            "association metadata grants no current Input alias"
        );
        Ok((current.resident, capture_source(tx)?))
    })?;
    let capture = resident.capture(&request().capture)?;
    let qualified = capture
        .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
        .unwrap();
    let source = inputs
        .lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message, SOURCE))?
        .unwrap();
    assert!(capture.fixed_running_generation_matches(qualified, source)?);
    drop(resident);
    drop(inputs);
    drop(capture);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn fixed_running_generation_does_not_follow_later_actual_task_birth_ack() -> Result<()> {
    let (_home, db) = legacy(true, original)?;
    db.transaction(|tx| {
        let before =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        let before_capture = before.resident.capture(&request().capture)?;
        let old = before_capture
            .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
            .unwrap();
        let CanonicalTaskFact::Task(original) = &old.fact else {
            unreachable!()
        };
        let mut changed = original.clone();
        changed.created_at += chrono::Duration::seconds(1);
        changed.updated_at += chrono::Duration::seconds(2);
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, TASK)]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &changed)?;
        let facts = register_task_writes_tx(
            tx,
            &proof,
            &keys,
            before.facts.state.as_ref().unwrap(),
            &request(),
        )?;
        let next = before.resident.with_committed(&facts.delta)?;
        let capture = next.capture(&request().capture)?;
        let source = capture_source(tx)?;
        assert!(!capture.fixed_running_generation_matches(
            capture
                .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
                .unwrap(),
            source
                .lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message, SOURCE))?
                .unwrap()
        )?);
        assert_eq!(next.coverage().running.sources[SOURCE].created_at, at());
        Ok(())
    })
}
#[test]
fn fixed_running_late_raw_member_and_foreign_task_never_gain_association() -> Result<()> {
    let (_home, db) = legacy(true, original)?;
    db.transaction(|tx| {
        let raw: String = tx.query_row(
            "SELECT payload_json FROM messages WHERE evidence_id=?1",
            [SOURCE],
            |row| row.get(0),
        )?;
        let mut source = repositories::decode_message_payload(&raw)?;
        source.id = "post-seal-running".into();
        source.created_at += chrono::Duration::seconds(3);
        evidence::append_message_tx(tx, &source)?;
        repositories::upsert_queue_entry_tx(
            tx,
            &QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: scope().agent_id,
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: source.created_at,
                updated_at: source.created_at,
            },
        )?;
        let current =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert!(current
            .resident
            .coverage()
            .running
            .sources
            .contains_key(SOURCE));
        assert!(!current
            .resident
            .coverage()
            .running
            .sources
            .contains_key(&source.id));
        assert!(tx
            .execute(
                "UPDATE task_query_fixed_messages SET running_created_at=?1 WHERE evidence_id=?2",
                params![source.created_at.to_rfc3339(), SOURCE]
            )
            .is_err());
        Ok(())
    })
}
#[test]
fn running_association_selector_material_is_admitted_before_owned_sql_rows() -> Result<()> {
    let (_home, db) = legacy(true, original)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let seed = binding(tx, &scope())?.unwrap();
        let fixed = qualification::manifests(tx, Some(&seed))?;
        let mut charged = false;
        let error =
            running_sources::observe_tx(tx, &scope(), Some(&seed), &fixed, |bytes, count| {
                charged = true;
                assert!(bytes > 0 && count >= 8);
                anyhow::bail!("injected original allowance exhausted")
            })
            .unwrap_err();
        assert!(charged && error.to_string().contains("allowance exhausted"));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn first_import_copies_original_running_expected_birth_without_rebinding() -> Result<()> {
    let (_home, db) = legacy(false, original)?;
    let mut received = task(TASK, TaskStatus::Queued);
    received.kind = TaskKind::ActorInvocation;
    received.detail = Some(serde_json::json!({"rejoin_obligation_id":TASK,"rejoin_generation":1}));
    received.created_at += chrono::Duration::seconds(1);
    received.updated_at += chrono::Duration::seconds(2);
    db.tasks().import_legacy(vec![received])?;
    db.context_episodes().import_legacy(Vec::new())?;
    db.transaction(|tx| {
        let seed=binding(tx,&scope())?.unwrap();
        let old=super::super::episode_snapshot::task_checkpoint::load(tx,&seed,"upgrade")?.query_inventory.unwrap();
        let new=super::super::episode_snapshot::task_checkpoint::load(tx,&seed,"first_import")?.query_inventory.unwrap();
        let expected=|inventory:&str|->Result<(String,String)> {
            Ok(tx.query_row("SELECT running_created_at,running_generation_digest FROM task_query_fixed_messages WHERE inventory_id=?1 AND evidence_id=?2",params![inventory,SOURCE],|row|Ok((row.get(0)?,row.get(1)?)))?)
        };
        assert_eq!(expected(&old.id)?,expected(&new.id)?);
        let current=prepare_task_domain_snapshot_tx(tx,&scope(),&request())?.require_current()?;
        let captured=current.resident.capture(&request().capture)?;
        let inputs=capture_source(tx)?;
        assert!(!captured.fixed_running_generation_matches(captured.lookup(&TaskKey::new(TaskFamily::Task,TASK))?.unwrap(),inputs.lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message,SOURCE))?.unwrap())?);
        Ok(())
    })
}

#[test]
fn fixed_running_actual_ledger_rejects_oversize_metadata_before_datetime_decode() -> Result<()> {
    let (_home, db) = legacy(true, original)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let seed = binding(tx, &scope())?.unwrap();
        let fixed = qualification::manifests(tx, Some(&seed))?;
        // Explicit corruption injection; restore the original immutable trigger.
        tx.execute_batch("DROP TRIGGER task_query_fixed_message_update")?;
        tx.execute(
            "UPDATE task_query_fixed_messages SET running_created_at=?1 WHERE evidence_id=?2",
            params!["bad-date".repeat(8192), SOURCE],
        )?;
        super::super::episode_snapshot::task_checkpoint::queries::ensure_schema(tx)?;
        let mut small = request();
        small.byte_limit = 4096;
        let original_scope = scope();
        let mut material =
            read_preparation::TaskReadMaterial::new(&original_scope, &small, Some(&ledger));
        let error = running_sources::observe_tx(
            tx,
            &original_scope,
            Some(&seed),
            &fixed,
            |bytes, count| material.charge_ids(bytes, count),
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ));
        drop(material);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn fixed_running_generation_distinguishes_actual_absent_parent_from_explicit_null_ack() -> Result<()>
{
    let (_home, db) = legacy(true, original)?;
    db.transaction(|tx| {
        let before =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        let old_capture = before.resident.capture(&request().capture)?;
        let CanonicalTaskFact::Task(original) = &old_capture
            .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
            .unwrap()
            .fact
        else {
            unreachable!()
        };
        let mut changed = original.clone();
        changed.updated_at += chrono::Duration::seconds(2);
        changed.detail.as_mut().unwrap()["parent_turn_id"] = serde_json::Value::Null;
        assert_ne!(
            task_running_generation_digest(original)?,
            task_running_generation_digest(&changed)?
        );
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, TASK)]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &changed)?;
        let facts = register_task_writes_tx(
            tx,
            &proof,
            &keys,
            before.facts.state.as_ref().unwrap(),
            &request(),
        )?;
        let next = before.resident.with_committed(&facts.delta)?;
        let capture = next.capture(&request().capture)?;
        let inputs = capture_source(tx)?;
        assert!(!capture.fixed_running_generation_matches(
            capture
                .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
                .unwrap(),
            inputs
                .lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message, SOURCE))?
                .unwrap()
        )?);
        assert_eq!(next.coverage().running.sources[SOURCE].created_at, at());
        Ok(())
    })
}

#[test]
fn fixed_running_source_fingerprint_matches_original_input_seed_with_or_without_canonical_hash(
) -> Result<()> {
    for with_hash in [true, false] {
        let (_home, db) = legacy(true, |tx| {
            original(tx)?;
            if !with_hash {
                // A valid old row without a canonical hash is frozen at the
                // same original cut, never introduced after qualification.
                tx.execute(
                    "UPDATE messages SET content_hash=NULL WHERE evidence_id=?1",
                    [SOURCE],
                )?;
            }
            Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            let seed = binding(tx, &scope())?.unwrap();
            let fixed: String = tx.query_row("SELECT fingerprint_json FROM task_query_fixed_messages WHERE evidence_id=?1", [SOURCE], |row| row.get(0))?;
            let original: String = tx.query_row("SELECT fingerprint_json FROM input_evidence_seed_membership WHERE seed_id=?1 AND family='message' AND record_id=?2 LIMIT 1", params![seed, SOURCE], |row| row.get(0))?;
            assert_eq!(fixed, original, "canonical hash presence: {with_hash}");
            let fingerprint: serde_json::Value = serde_json::from_str(&fixed)?;
            assert_eq!(fingerprint.get("frozen_payload_digest").is_some(), !with_hash);
            let current = prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?.require_current()?;
            assert_eq!(current.resident.coverage().running.sources.len(), 1);
            let selected = current.resident.capture(&request().capture)?;
            let inputs = capture_source(tx)?;
            let source = inputs.lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message, SOURCE))?.unwrap();
            let task = selected.lookup(&TaskKey::new(TaskFamily::Task, TASK))?.unwrap();
            assert!(selected.fixed_running_generation_matches(task, source)?);
            let frozen_digest: String = tx.query_row("SELECT content_digest FROM task_query_fixed_messages WHERE evidence_id=?1", [SOURCE], |row| row.get(0))?;
            assert_eq!(source.locator.content_digest, frozen_digest);
            drop(inputs);
            drop(selected);
            drop(current);
            // Keeping a fixed member ID never accepts a changed raw body.
            tx.execute("UPDATE messages SET payload_json=replace(payload_json,'original pending Running','changed raw pending Running') WHERE evidence_id=?1", [SOURCE])?;
            let error = capture_source(tx).err().context("fixed Source accepted changed raw body")?;
            assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(), Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ProjectionChanged(_))));
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
