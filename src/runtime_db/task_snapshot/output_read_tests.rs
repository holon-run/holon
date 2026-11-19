use super::tests::{at, legacy, legacy_fixture, scope, task};
use super::*;
use crate::{
    config::StateMemoryLimits,
    domain::execution_protocol::{WorkItemExecutionRecord, WorkItemExecutionState},
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime::resident_input_evidence::{
        InputEvidenceReadError, PreparedInputEvidence, ResidentInputEvidence,
    },
    runtime_db::{
        episode_snapshot::task_checkpoint::capture_legacy_query_inventory_for_test,
        evidence,
        input_evidence_snapshot::{
            register_pruned_input_evidence_tx, InputEvidenceLoadRequest, InputEvidenceSnapshotError,
        },
        migrations::{apply_migration, MIGRATIONS},
    },
    types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        TaskStatus, TranscriptEntry, TranscriptEntryKind, TurnRecord, TurnTerminalKind,
        TurnTerminalSummary,
    },
};
const TASK: &str = "focused-output";
const SOURCE: &str = "focused-output-source";
const TURN: &str = "focused-output-turn-001";
const TRANSCRIPT: &str = "focused-output-transcript";
struct OutputFixtureRequest {
    load: TaskDomainLoadRequest,
    _role: crate::runtime::task_queries::TaskQueryLease,
}
impl std::ops::Deref for OutputFixtureRequest {
    type Target = TaskDomainLoadRequest;
    fn deref(&self) -> &Self::Target {
        &self.load
    }
}
impl std::ops::DerefMut for OutputFixtureRequest {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.load
    }
}
fn request(ledger: &InputEvidenceBudgetLedger) -> Result<OutputFixtureRequest> {
    let role = crate::runtime::tests::support::task_query_lease_for_fixture(
        &scope(),
        1,
        crate::runtime::task_queries::TaskQuerySpec {
            output_task_ids: &[TASK],
            ..Default::default()
        },
        ledger,
        4096,
        4 * 1024 * 1024,
    )?;
    assert!(
        role.selection()
            .required_keys
            .contains(&TaskKey::new(TaskFamily::Task, TASK)),
        "the actual Output role includes its normalized Task selector"
    );
    let load = TaskDomainLoadRequest {
        recovery_at: None,
        query: role.selection().clone(),
        capture: TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            recovery_source_ids: BTreeSet::new(),
            required_keys: BTreeSet::from([TaskKey::new(TaskFamily::Task, TASK)]),
        },
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    };
    Ok(OutputFixtureRequest { load, _role: role })
}

fn inputs() -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            SOURCE,
        )]),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn historical_source(tx: &Transaction<'_>, work_item_id: Option<&str>) -> Result<()> {
    repositories::upsert_task_tx(tx, &task(TASK, TaskStatus::Completed))?;
    let mut source = MessageEnvelope::new(
        "task-agent",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: TASK.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original historical output".into(),
        },
    );
    source.id = SOURCE.into();
    source.work_item_id = work_item_id.map(str::to_owned);
    source.created_at = at();
    source.metadata = Some(serde_json::json!({"task_id":TASK,"task_status":"completed"}));
    evidence::append_message_tx(tx, &source)?;
    Ok(())
}

fn historical(tx: &Transaction<'_>) -> Result<()> {
    historical_source(tx, None)?;
    for index in 1..=514 {
        let mut turn = TurnRecord::new(
            "task-agent",
            format!("focused-output-turn-{index:03}"),
            index,
        );
        turn.created_at = at() + chrono::Duration::seconds(i64::try_from(index)?);
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: turn.created_at + chrono::Duration::seconds(1),
            duration_ms: 1000,
        });
        if index == 1 {
            turn.input_message_ids.push(SOURCE.into());
        }
        repositories::upsert_turn_record_tx(tx, &turn)?;
    }
    let mut transcript = TranscriptEntry::new(
        "task-agent",
        TranscriptEntryKind::AssistantRound,
        Some(0),
        Some(SOURCE.into()),
        serde_json::json!({"turn_id":TURN,"text":"accepted transcript"}),
    );
    transcript.id = TRANSCRIPT.into();
    transcript.created_at = at();
    evidence::append_transcript_entry_tx(tx, &transcript)?;
    for index in 0..513 {
        let mut entry = TranscriptEntry::new(
            "task-agent",
            TranscriptEntryKind::AssistantRound,
            Some(0),
            None,
            serde_json::json!({"text":"unrelated transcript"}),
        );
        entry.id = format!("focused-transcript-noise-{index:03}");
        entry.created_at = at();
        evidence::append_transcript_entry_tx(tx, &entry)?;
    }
    append_newer_messages(tx)?;
    let mut state = AgentState::new("task-agent");
    state.turn_index = 514;
    evidence::upsert_agent_state_tx(tx, &state)?;
    Ok(())
}

fn append_newer_messages(tx: &Transaction<'_>) -> Result<()> {
    for index in 0..=crate::runtime::resident_task::TASK_OUTPUT_RECENT_MESSAGES {
        let mut noise = MessageEnvelope::new(
            "task-agent",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "output inventory noise".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: index.to_string(),
            },
        );
        noise.id = format!("focused-noise-{index:03}");
        noise.created_at = at();
        evidence::append_message_tx(tx, &noise)?;
    }
    Ok(())
}
type TaskAliasObservation = (u64, String, Option<String>, Option<String>);
fn task_alias(tx: &Transaction<'_>) -> Result<Option<TaskAliasObservation>> {
    tx.query_row("SELECT revision,availability,content_digest,projection_digest FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3",
        params![scope().agent_id,scope().identity_incarnation,TASK],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional().map_err(Into::into)
}
fn recall(
    tx: &Transaction<'_>,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(
    ResidentTaskDomain,
    crate::runtime_db::input_evidence_snapshot::TaskOutputInputRecall,
)> {
    let current =
        prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(&ledger)?.load, ledger)?
            .require_current()?;
    let capture = current.resident.capture_query(&request(&ledger)?.query)?;
    let fact = capture
        .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
        .unwrap();
    let source = capture.output_source(TASK)?.unwrap();
    assert_eq!(source.place, TaskOutputSourcePlace::HistoricalResult);
    let original_alias = task_alias(tx)?;
    let recall = prepare_task_output_input_recall_tx(
        tx,
        current.facts.state.as_ref().unwrap(),
        current.resident.coverage().revision,
        fact,
        Some(source),
        &request(&ledger)?.load,
        ledger,
    )?;
    assert_eq!(
        task_alias(tx)?,
        original_alias,
        "read-only Input acquisition preserves the actual named Task alias exactly"
    );
    Ok((current.resident, recall))
}

#[test]
fn output_recall_keeps_v113_work_execution_proofs_and_rejects_changed_bytes() -> Result<()> {
    const WORK: &str = "output-legacy-work";
    const BODY: &str = r#"{"source_revision":7,"state":{"state":"needs_repair","generation":3,"repair_id":"repair"}}"#;
    let key = InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, WORK);
    for proof_kind in ["current", "fixed", "overlay"] {
        let (_home, db) = legacy_fixture(
            true,
            |tx| {
                historical_source(tx, Some(WORK))?;
                append_newer_messages(tx)
            },
            |conn| {
                for m in MIGRATIONS.iter().filter(|m| (90..=94).contains(&m.version)) {
                    apply_migration(conn, m)?;
                }
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT INTO execution_protocol_work_items
                     VALUES('task-agent',?1,7,3,'needs_repair',?2)",
                    params![WORK, BODY],
                )?;
                // Model the historical writer's focused inventory before its
                // Task checkpoint is registered, using the real inventory seal.
                let inventory = capture_legacy_query_inventory_for_test(&tx, "task-agent")?;
                let seed: String = tx.query_row(
                    "SELECT seed_id FROM agent_episode_continuity
                     WHERE agent_id='task-agent' AND identity_incarnation=1",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(
                    tx.execute(
                        "UPDATE episode_continuity_seeds
                         SET roots_json=json_set(roots_json,'$.task_domain_checkpoint.query_inventory',json(?2))
                         WHERE seed_id=?1",
                        params![seed, serde_json::to_string(&inventory)?],
                    )?,
                    1
                );
                tx.commit()?;
                for m in MIGRATIONS
                    .iter()
                    .filter(|m| (95..=112).contains(&m.version))
                {
                    apply_migration(conn, m)?;
                }
                let tx = conn.transaction()?;
                let (proof, marker, digest): (String, String, String) = tx.query_row(
                    "SELECT fingerprint_json,json_extract(fingerprint_json,'$.revision_marker'),
                            json_extract(fingerprint_json,'$.frozen_payload_digest')
                     FROM task_query_fixed_input_members
                     WHERE inventory_id=?1 AND family='work_execution' AND record_id=?2",
                    params![inventory.id, WORK],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                assert_eq!(marker, "7:3:needs_repair");
                // Isolate the proof family; the focused fixed proof remains as a
                // non-fallback for a Current alias or an authoritative overlay.
                tx.execute(
                    "DELETE FROM input_evidence_eligibility WHERE family='work_execution' AND record_id=?1",
                    [WORK],
                )?;
                if proof_kind == "current" {
                    tx.execute(
                        "INSERT INTO input_evidence_current
                         (agent_id,identity_incarnation,family,record_id,revision,content_digest,
                          fingerprint_json,availability,qualification)
                         VALUES('task-agent',1,'work_execution',?1,3,?2,
                                json_remove(?3,'$.frozen_payload_digest'),'present','committed_writer')",
                        params![WORK, digest, proof],
                    )?;
                } else if proof_kind == "overlay" {
                    tx.execute(
                        "INSERT INTO input_evidence_eligibility
                         VALUES (?1,'upgrade','work_execution',?2,'{}',1)",
                        params![seed, WORK],
                    )?;
                    tx.execute(
                        "INSERT INTO input_evidence_seed_recovery_current
                         (seed_id,family,record_id,revision,fingerprint_json,origin_kind)
                         VALUES (?1,'work_execution',?2,1,?3,'fixed_member')",
                        params![seed, WORK, proof],
                    )?;
                }
                tx.commit()
                    .context("commit WorkExecution proof fixture before v113")
            },
        )?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            let (tasks, recalled) = recall(tx, &ledger)
                .with_context(|| format!("recall migrated {proof_kind} WorkExecution"))?;
            let domain =
                ResidentInputEvidence::with_task_output_recall(None, recalled, ledger.clone())?;
            let captured = tasks.capture_query(&request(&ledger)?.query)?;
            let task = captured.lookup(&TaskKey::new(TaskFamily::Task, TASK))?.unwrap();
            let selected = domain.capture_task_output_for_request(&task.locator, &inputs())?;
            let work = selected.lookup(&key)?.expect("accepted Message owns WorkExecution");
            assert_eq!(
                work.record,
                PreparedInputEvidence::WorkExecution {
                    work_item_id: WORK.into(),
                    record: WorkItemExecutionRecord {
                        source_revision: 7,
                        state: WorkItemExecutionState::Paused {
                            generation: 3,
                            reason: "repair".into(),
                        },
                    },
                },
                "{proof_kind}"
            );
            assert_eq!(work.locator.revision, if proof_kind == "current" { 3 } else { 1 });
            drop(selected);
            drop(domain);

            // Same-sized, still decodable old body: it must fail the original digest check.
            tx.execute(
                "UPDATE execution_protocol_work_items SET payload_json=?1 WHERE agent_id='task-agent' AND work_item_id=?2",
                params![BODY.replace("\"repair\"", "\"edited\""), WORK],
            )?;
            let err = recall(tx, &ledger).err().context("changed WorkExecution body accepted")?;
            assert!(
                matches!(err.downcast_ref::<InputEvidenceSnapshotError>(),
                    Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == WORK),
                "{proof_kind}: {err:#}"
            );
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn original_fixed_full_output_uses_strict_selected_read_only_owning_closure_and_repeats_without_body_sql(
) -> Result<()> {
    let (_home, db) = legacy(true, historical)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (tasks, domain) = db.transaction(|tx| {
        let (tasks, recall) = recall(tx, &ledger)?;
        let generic: u64 = tx.query_row(
            "SELECT COUNT(*) FROM input_evidence_seed_membership WHERE record_id IN (?1,?2,?3)",
            params![SOURCE, TURN, TRANSCRIPT],
            |row| row.get(0),
        )?;
        assert_eq!(
            generic, 0,
            "all selected historical owning members are beyond D3's original fixed windows"
        );
        let aliases: u64 = tx.query_row(
            "SELECT COUNT(*) FROM input_evidence_current WHERE agent_id='task-agent'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            aliases, 0,
            "read-only historical recall must not mint mutation/body aliases"
        );
        let domain = ResidentInputEvidence::with_task_output_recall(None, recall, ledger.clone())?;
        Ok((tasks, domain))
    })?;
    let report = ledger.report()?;
    assert!(
        report
            .categories
            .get(&crate::runtime::input_evidence_budget::InputMemoryCategory::History)
            .is_some_and(|usage| usage.bytes > 0),
        "readonly selected bodies retain real History observation after material guards drop"
    );
    assert_eq!(report.retained_unclassified.bytes, 0);
    let capture = tasks.capture_query(&request(&ledger)?.query)?;
    let locator = &capture
        .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
        .unwrap()
        .locator;
    let mut later_task = locator.clone();
    later_task.revision += 1;
    assert!(
        domain
            .capture_task_output_for_request(&later_task, &inputs())
            .is_err(),
        "old read-only bytes cannot impersonate a later Task generation/version"
    );
    let selected = domain.capture_task_output_for_request(locator, &inputs())?;
    assert_eq!(
        selected.records.len(),
        3,
        "only Source, its actual Turn and complete Transcript closure are pinned"
    );
    assert!(selected
        .lookup(&InputEvidenceKey::new(InputEvidenceFamily::Turn, TURN))?
        .is_some());
    let transcript = selected
        .lookup(&InputEvidenceKey::new(
            InputEvidenceFamily::Transcript,
            TRANSCRIPT,
        ))?
        .unwrap();
    let weak = Arc::downgrade(transcript);
    db.transaction(|tx| {
        for table in [
            "messages",
            "turn_records",
            "transcript_entries",
            "tasks",
            "agent_states",
            "agent_identities",
        ] {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} RENAME TO forbidden_output_{table}"
            ))?;
        }
        Ok(())
    })?;
    let repeated = domain.capture_task_output_for_request(locator, &inputs())?;
    assert!(Arc::ptr_eq(
        selected
            .lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message, SOURCE))?
            .unwrap(),
        repeated
            .lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message, SOURCE))?
            .unwrap()
    ));
    drop((tasks, capture, domain));
    assert!(
        weak.upgrade().is_some(),
        "selected capture owns its historical allocations without the domain"
    );
    drop((selected, repeated));
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn output_read_cap_rejects_foreign_or_late_actual_turn_and_changed_task_source_revision(
) -> Result<()> {
    for mode in [
        "foreign",
        "late",
        "task_revision",
        "task_alias_pruned",
        "task_alias_version",
        "task_body",
        "source_version",
        "transcript_relation",
    ] {
        let (_home, db) = legacy(true, historical)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            let current=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&request(&ledger)?.load,&ledger)?.require_current()?;
            let capture=current.resident.capture_query(&request(&ledger)?.query)?;
            let fact=capture.lookup(&TaskKey::new(TaskFamily::Task,TASK))?.unwrap();
            let source=capture.output_source(TASK)?;
            match mode {
                "foreign"|"late" => {
                    repositories::upsert_turn_record_tx(tx,&TurnRecord::new(if mode=="foreign" {"foreign-agent"} else {"task-agent"},"unaccepted-output-turn",515))?;
                    tx.execute("UPDATE conversation_input_assignments SET turn_id='unaccepted-output-turn' WHERE agent_id='task-agent' AND message_id=?1",[SOURCE])?;
                }
                "task_revision" => { tx.execute("UPDATE task_domain_current SET revision=revision+1 WHERE agent_id='task-agent' AND identity_incarnation=1",[])?; }
                "task_alias_pruned" | "task_alias_version" => {
                    upsert_alias(tx, &scope(), &fact.locator.key, &Alias {
                        revision: fact.locator.revision + 1,
                        availability: if mode == "task_alias_pruned" { TaskAvailability::Pruned } else { TaskAvailability::Present },
                        digest: (mode == "task_alias_version").then(|| fact.locator.content_digest.clone()),
                        projection_digest: (mode == "task_alias_version").then(|| fact.locator.projection_digest.clone()),
                    })?;
                }
                "task_body" => { tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.summary','unacknowledged task drift') WHERE task_id=?1",[TASK])?; }
                "source_version" => { tx.execute("UPDATE messages SET payload_json=json_set(payload_json,'$.body.text','unacknowledged output drift') WHERE evidence_id=?1",[SOURCE])?; }
                "transcript_relation" => { tx.execute("UPDATE transcript_entries SET turn_id='unaccepted-output-turn' WHERE evidence_id=?1",[TRANSCRIPT])?; }
                _=>unreachable!(),
            }
            assert!(prepare_task_output_input_recall_tx(tx,current.facts.state.as_ref().unwrap(),current.resident.coverage().revision,fact,source,&request(&ledger)?.load,&ledger).is_err(),"{mode}");
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE agent_id='task-agent'",[],|row|row.get::<_,u64>(0))?,0);
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
#[test]
fn selected_output_raw_material_is_budgeted_before_decode_and_failure_releases_actual_guards(
) -> Result<()> {
    let (_home, db) = legacy(true, historical)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let current = prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &scope(),
            &request(&ledger)?.load,
            &ledger,
        )?
        .require_current()?;
        let capture = current.resident.capture_query(&request(&ledger)?.query)?;
        let fact = capture
            .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
            .unwrap();
        // Corrupt storage after indexing so the output budget is exercised.
        tx.execute_batch("DROP INDEX idx_messages_agent_trusted_operator;")?;
        tx.execute(
            "UPDATE messages SET payload_json=CAST(zeroblob(9437184) AS TEXT) WHERE evidence_id=?1",
            [SOURCE],
        )?;
        let error = prepare_task_output_input_recall_tx(
            tx,
            current.facts.state.as_ref().unwrap(),
            current.resident.coverage().revision,
            fact,
            capture.output_source(TASK)?,
            &request(&ledger)?.load,
            &ledger,
        )
        .err()
        .context("oversized output decoded before budget")?;
        assert!(
            !matches!(
                error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn pruned_owning_transcript_invalidates_recall_without_reentering_old_pinned_bytes() -> Result<()> {
    let (_home, db) = legacy(true, historical)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (tasks, domain) = db.transaction(|tx| {
        let (tasks, recall) = recall(tx, &ledger)?;
        Ok((
            tasks,
            ResidentInputEvidence::with_task_output_recall(None, recall, ledger.clone())?,
        ))
    })?;
    let capture = tasks.capture_query(&request(&ledger)?.query)?;
    let fact = capture
        .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
        .unwrap();
    let old = domain.capture_task_output_for_request(&fact.locator, &inputs())?;
    let delta=db.transaction(|tx| {
        tx.execute("DELETE FROM transcript_entries WHERE evidence_id=?1",[TRANSCRIPT])?;
        let delta=register_pruned_input_evidence_tx(tx,&scope(),&[InputEvidenceKey::new(InputEvidenceFamily::Transcript,TRANSCRIPT)])?;
        assert!(delta.output_recall_invalidation.is_some());
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_output_fixed_input_prunes WHERE family='transcript' AND record_id=?1",[TRANSCRIPT],|row|row.get::<_,u64>(0))?,1);
        let state:AgentState=serde_json::from_str(&tx.query_row("SELECT payload_json FROM agent_states WHERE agent_id='task-agent'",[],|row|row.get::<_,String>(0))?)?;
        assert!(prepare_task_output_input_recall_tx(tx,&state,tasks.coverage().revision,fact,capture.output_source(TASK)?,&request(&ledger)?.load,&ledger).is_err(),"a selected pruned child cannot disappear from the owning frontier");
        Ok(delta)
    })?;
    let next = domain.with_committed(&delta, BTreeSet::new())?.unwrap();
    assert!(matches!(
        next.capture_task_output_for_request(&fact.locator, &inputs())
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>(),
        Some(InputEvidenceReadError::NeedsPreparation(_))
    ));
    assert!(
        old.lookup(&InputEvidenceKey::new(
            InputEvidenceFamily::Transcript,
            TRANSCRIPT
        ))?
        .is_some(),
        "old pins retain bytes without restoring qualification"
    );
    db.transaction(|tx| {
        assert!(!tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM transcript_entries WHERE evidence_id=?1)",
            [TRANSCRIPT],
            |row| row.get::<_, bool>(0)
        )?);
        Ok(())
    })?;
    drop((next, domain, tasks, capture, old));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn output_read_retired_scope_and_no_state_never_hydrate_successor_or_ready() -> Result<()> {
    for mode in ["no_state", "successor"] {
        let (_home, db) = legacy(true, historical)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            let current = prepare_task_domain_snapshot_with_budget_tx(
                tx,
                &scope(),
                &request(&ledger)?.load,
                &ledger,
            )?
            .require_current()?;
            let capture = current.resident.capture_query(&request(&ledger)?.query)?;
            let fact = capture
                .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
                .unwrap();
            if mode == "no_state" {
                tx.execute("DELETE FROM agent_states WHERE agent_id='task-agent'", [])?;
            } else {
                let mut successor = super::tests::identity();
                successor.incarnation = 2;
                evidence::upsert_agent_identity_tx(tx, &successor)?;
                tx.execute(
                    "UPDATE agent_states SET payload_json='{' WHERE agent_id='task-agent'",
                    [],
                )?;
            }
            let error = prepare_task_output_input_recall_tx(
                tx,
                current.facts.state.as_ref().unwrap(),
                current.resident.coverage().revision,
                fact,
                capture.output_source(TASK)?,
                &request(&ledger)?.load,
                &ledger,
            )
            .err()
            .context("retired/NoState output acquired bodies")?;
            assert!(
                matches!(
                    error.downcast_ref::<TaskSnapshotError>(),
                    Some(TaskSnapshotError::RetiredScope)
                ),
                "{mode}: {error:#}"
            );
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
#[test]
fn fixed_source_actual_prune_keeps_accepted_head_unavailable_after_raw_restore() -> Result<()> {
    let (_home, db) = legacy(true, historical)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let current=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&request(&ledger)?.load,&ledger)?.require_current()?;
        let capture=current.resident.capture_query(&request(&ledger)?.query)?;
        let fact=capture.lookup(&TaskKey::new(TaskFamily::Task,TASK))?.unwrap();
        tx.execute_batch("CREATE TEMP TABLE original_readonly_source AS SELECT * FROM messages WHERE evidence_id='focused-output-source'")?;
        tx.execute("DELETE FROM messages WHERE evidence_id=?1",[SOURCE])?;
        let delta=register_pruned_input_evidence_tx(tx,&scope(),&[InputEvidenceKey::new(InputEvidenceFamily::Message,SOURCE)])?;
        assert!(delta.output_recall_invalidation.is_some());
        assert!(delta.removals.is_empty(),"readonly invalidation must not invent a D3 alias/removal revision");
        let (task_delta,_material)=register_task_source_metadata_tx(tx,&scope(),&delta,&request(&ledger)?.load,&ledger,None)?.context("actual focused prune did not refresh the same Task domain")?;
        assert_eq!(task_delta.base_revision,current.resident.coverage().revision);
        assert_eq!(task_delta.revision,task_delta.base_revision+1);
        assert_eq!(task_delta.coverage.output.heads[TASK].source.as_ref().unwrap().availability,TaskOutputSourceAvailability::Pruned);
        let installed=current.resident.with_committed(&task_delta)?;
        assert_eq!(installed.coverage().revision,task_delta.revision);
        let head=query_sources::observe_output_heads_tx(tx,&scope(),&[&fact.locator],|_,_|Ok(()))?;
        assert_eq!(head.heads[TASK].source.as_ref().unwrap().availability,TaskOutputSourceAvailability::Pruned);
        tx.execute("INSERT INTO messages SELECT * FROM original_readonly_source",[])?;
        let head=query_sources::observe_output_heads_tx(tx,&scope(),&[&fact.locator],|_,_|Ok(()))?;
        assert_eq!(head.heads[TASK].source.as_ref().unwrap().availability,TaskOutputSourceAvailability::Pruned,"unacknowledged identical raw restore cannot reenter the old accepted Source");
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE agent_id='task-agent'",[],|row|row.get::<_,u64>(0))?,0);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn readonly_output_recall_above_legacy_history_hint_keeps_exact_guards() -> Result<()> {
    let (_home, db) = legacy(true, historical)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits {
        history_per_agent_bytes: 1024,
        history_host_bytes: 1024,
        ..Default::default()
    })?;
    let domain = db.transaction(|tx| {
        let (_tasks, recall) = recall(tx, &ledger)?;
        ResidentInputEvidence::with_task_output_recall(None, recall, ledger.clone())
    })?;
    assert!(ledger.report()?.observed_bytes > 1024);
    drop(domain);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM input_evidence_current WHERE agent_id='task-agent'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn output_sql_material_and_recall_copy_ignore_cumulative_request_hints() -> Result<()> {
    for mode in ["bytes", "records"] {
        let (_home, db) = legacy(true, historical)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let (tasks, domain) = db.transaction(|tx| {
            let (tasks, recall) = recall(tx, &ledger)?;
            Ok((
                tasks,
                ResidentInputEvidence::with_task_output_recall(None, recall, ledger.clone())?,
            ))
        })?;
        let capture = tasks.capture_query(&request(&ledger)?.query)?;
        let fact = capture
            .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
            .unwrap();
        let original = domain.capture_task_output_for_request(&fact.locator, &inputs())?;
        let baseline = ledger.report()?.observed_bytes;
        db.transaction(|tx| {
            let state: AgentState = serde_json::from_str(&tx.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id='task-agent'",
                [],
                |row| row.get::<_, String>(0),
            )?)?;
            let probe = prepare_task_output_input_recall_tx(
                tx,
                &state,
                tasks.coverage().revision,
                fact,
                capture.output_source(TASK)?,
                &request(&ledger)?.load,
                &ledger,
            )?;
            let (sql_bytes, sql_records) = probe.admitted_material()?;
            drop(probe);
            let (copy_bytes, copy_records) = domain.task_output_recall_copy_material(3)?;
            assert!(copy_bytes > 0 && copy_records > 0);
            let mut limited = request(&ledger)?;
            if mode == "bytes" {
                limited.byte_limit =
                    sql_bytes.checked_add(usize::try_from(copy_bytes)?).unwrap() - 1;
                assert!(
                    sql_bytes < limited.byte_limit
                        && usize::try_from(copy_bytes)? < limited.byte_limit
                );
            } else {
                limited.record_limit = sql_records.checked_add(copy_records).unwrap() - 1;
                assert!(sql_records < limited.record_limit && copy_records < limited.record_limit);
            }
            let selected = prepare_task_output_input_recall_tx(
                tx,
                &state,
                tasks.coverage().revision,
                fact,
                capture.output_source(TASK)?,
                &limited,
                &ledger,
            )?;
            let merged = ResidentInputEvidence::with_task_output_recall(
                Some(&domain),
                selected,
                ledger.clone(),
            )?;
            assert!(ledger.report()?.observed_bytes > baseline);
            drop(merged);
            drop(limited);
            assert_eq!(
                ledger.report()?.observed_bytes,
                baseline,
                "failed merge released only its new guards"
            );
            assert_eq!(
                tx.query_row(
                    "SELECT COUNT(*) FROM input_evidence_current WHERE agent_id='task-agent'",
                    [],
                    |row| row.get::<_, u64>(0)
                )?,
                0
            );
            Ok(())
        })?;
        let repeated = domain.capture_task_output_for_request(&fact.locator, &inputs())?;
        assert!(Arc::ptr_eq(
            original
                .lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message, SOURCE))?
                .unwrap(),
            repeated
                .lookup(&InputEvidenceKey::new(InputEvidenceFamily::Message, SOURCE))?
                .unwrap()
        ));
        drop((original, repeated, capture, tasks, domain));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn original_read_only_fixed_task_preflight_authorizes_output_without_current_task_alias(
) -> Result<()> {
    let (_home, db) = legacy(true, historical)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let empty = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: TaskCaptureRequest {
                include_active: false,
                include_unsettled: false,
                terminal_limit: 0,
                recovery_source_ids: BTreeSet::new(),
                required_keys: BTreeSet::new(),
            },
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        };
        prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &empty, &ledger)?
            .require_current()?;
        assert!(task_alias(tx)?.is_none());
        let load = request(&ledger)?;
        let preflight =
            loader::load_original_with_budget(tx, &scope(), &load.load, false, false, &ledger)?
                .require_current()?;
        let captured = preflight.resident.capture_query(&load.query)?;
        let fact = captured
            .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
            .unwrap();
        assert_eq!(fact.locator.revision, 1);
        assert!(
            task_alias(tx)?.is_none(),
            "original preflight creates no alias"
        );
        let recall = prepare_task_output_input_recall_tx(
            tx,
            preflight.facts.state.as_ref().unwrap(),
            preflight.resident.coverage().revision,
            fact,
            captured.output_source(TASK)?,
            &load.load,
            &ledger,
        )?;
        assert!(
            task_alias(tx)?.is_none(),
            "private readcap grants no Task mutation alias"
        );
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM input_evidence_current WHERE agent_id='task-agent'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
        let domain = ResidentInputEvidence::with_task_output_recall(None, recall, ledger.clone())?;
        let selected = domain.capture_task_output_for_request(&fact.locator, &inputs())?;
        assert_eq!(selected.records.len(), 3);
        assert!(selected
            .lookup(&InputEvidenceKey::new(
                InputEvidenceFamily::Transcript,
                TRANSCRIPT
            ))?
            .is_some());
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
