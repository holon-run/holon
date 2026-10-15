use super::*;
use crate::runtime::input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory};

fn full_request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: MAX_METADATA_RECORDS,
        byte_limit: MAX_METADATA_BYTES,
    }
}

#[test]
fn work_decoder_retires_json_scratch_but_keeps_the_original_typed_results_guarded() -> Result<()> {
    let (_home, db) = fresh()?;
    let row = work("typed-decoder-lifetime", WorkItemState::Open);
    db.transaction(|tx| insert_qualified_work(tx, &row).map(|_| ()))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    let (stored, qualified, coverage, alias, guards) = db.transaction(|tx| {
        let actual_scope = scope();
        let mut phase = crate::runtime_db::work_snapshot::read_preparation::WorkReadMaterial::new(
            &actual_scope,
            &full_request(),
            &ledger,
            None,
        )?;
        let before = phase.usage();
        let key = WorkKey::new(WorkFamily::Work, &row.id);
        let stored = phase
            .record_tx(tx, &key)?
            .context("actual typed Work missing")?;
        let original_decoder_nodes: usize = tx.query_row(
            "SELECT COUNT(*)+7 FROM work_items p,json_tree(p.payload_json) WHERE p.work_item_id=?1",
            [&row.id],
            |r| r.get(0),
        )?;
        assert!(
            phase.usage().1 - before.1 < original_decoder_nodes * 8,
            "dead decoder copies must not survive as typed body accounting"
        );
        let qualified = stored.qualified(&actual_scope, 1)?;
        assert_eq!(qualified.record, stored.record);
        let coverage = phase.coverage_tx(tx)?.context("actual coverage missing")?;
        let alias = phase.alias_tx(tx, &key)?.context("actual alias missing")?;
        assert_eq!(
            ledger.report()?.observed_bytes,
            u64::try_from(phase.usage().0)?
        );
        Ok((stored, qualified, coverage, alias, phase.into_guards()))
    })?;
    assert_eq!(Some(&coverage), original.as_ref());
    assert_eq!(
        alias.digest.as_deref(),
        Some(qualified.content_digest.as_str())
    );
    let weak = Arc::downgrade(guards.last().context("typed result guard missing")?);
    let held = guards.clone();
    drop(guards);
    assert!(weak.upgrade().is_some() && ledger.report()?.observed_bytes > 0);
    drop((stored, qualified, coverage, alias));
    drop(held);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        original
    );
    Ok(())
}

#[test]
fn work_decoder_first_decode_peak_still_counts_original_live_prior_material() -> Result<()> {
    let (_home, db) = fresh()?;
    let mut row = work("first-decode-prior", WorkItemState::Open);
    row.objective = "x".repeat(8 * 1024);
    db.transaction(|tx| insert_qualified_work(tx, &row).map(|_| ()))?;
    let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let result: Result<()> = db.transaction(|tx| {
        let actual_scope = scope();
        let mut policy = full_request();
        policy.byte_limit = 128 * 1024;
        let mut phase = crate::runtime_db::work_snapshot::read_preparation::WorkReadMaterial::new(
            &actual_scope,
            &policy,
            &ledger,
            None,
        )?;
        phase.charge(0, 96 * 1024, 1)?;
        phase.record_tx(tx, &WorkKey::new(WorkFamily::Work, &row.id))?;
        Ok(())
    });
    let error = result
        .err()
        .context("first decode must not borrow a future scratch release")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        original
    );
    Ok(())
}

#[test]
fn work_material_budget_precedes_large_raw_alias_and_normalized_wait_decoders() -> Result<()> {
    for normalized in [false, true] {
        let (_home, db) = fresh()?;
        let mut wait = task_wait("guarded-wait", "original-task", "original-trigger")?;
        wait.status = WaitConditionStatus::Active;
        wait.resolved_at = None;
        wait.trigger_message_id = None;
        db.transaction(|tx| write_qualified_wait(tx, &wait, &request()).map(|_| ()))?;
        let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        db.transaction(|tx| {
            let large="[".to_owned()+&"x".repeat(256*1024);
            if normalized {
                tx.execute("UPDATE wait_conditions SET continuation_json=?1 WHERE wait_condition_id=?2",params![large,wait.id])?;
            } else {
                tx.execute("UPDATE work_domain_records SET fingerprint_json=?1 WHERE agent_id=?2 AND family='wait' AND record_id=?3",params![large,scope().agent_id,wait.id])?;
            }
            Ok(())
        })?;
        let mut policy = full_request();
        policy.byte_limit = 128 * 1024;
        let error = db
            .transaction(|tx| {
                prepare_work_domain_snapshot_with_budget_tx(
                    tx,
                    &scope(),
                    &policy,
                    false,
                    &ledger,
                    None,
                )
            })
            .err()
            .context("large raw metadata must reject before malformed JSON decode")?;
        assert!(
            matches!(
                error.downcast_ref::<WorkSnapshotError>(),
                Some(WorkSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(
            db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
            original
        );
    }
    Ok(())
}

#[test]
fn work_material_counts_native_nodes_before_wait_decode_and_releases_actual_guards() -> Result<()> {
    let (_home, db) = fresh()?;
    let mut wait = task_wait("native-wait", "original-task", "original-trigger")?;
    wait.status = WaitConditionStatus::Active;
    wait.resolved_at = None;
    wait.trigger_message_id = None;
    wait.continuation = Some(serde_json::json!({"items":vec![0;2048]}));
    db.transaction(|tx| write_qualified_wait(tx, &wait, &full_request()).map(|_| ()))?;
    let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut small = full_request();
    small.record_limit = 8192;
    let raw:usize=db.connection()?.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(continuation_json AS BLOB)) FROM wait_conditions WHERE wait_condition_id=?1",[&wait.id],|r|r.get(0))?;
    assert!(raw * 8 < small.byte_limit);
    let error = db
        .transaction(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(tx, &scope(), &small, false, &ledger, None)
        })
        .err()
        .context("raw fits but owned JSON node count must backpressure")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        original
    );
    let loaded = db
        .transaction(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(
                tx,
                &scope(),
                &full_request(),
                false,
                &ledger,
                None,
            )
        })?
        .require_current()?;
    assert!(!loaded.material_views.is_empty());
    let weak_guard = Arc::downgrade(&loaded.material_views[0]);
    assert!(ledger.report()?.observed_bytes > 0);
    let domain = loaded.resident.with_budget(ledger.clone())?;
    let capture = domain.capture(&WorkCaptureRequest::current())?;
    let record = capture
        .records
        .iter()
        .find(|r| r.key.id == wait.id)
        .context("selected original Wait missing")?
        .clone();
    let weak_record = Arc::downgrade(&record);
    assert!(
        record.resident_bytes
            > usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
                &record.record
            )?)?
    );
    let physical = ledger.report()?.physical_records.bytes;
    let candidate = ledger.register_work_view(
        &scope(),
        InputMemoryCategory::Candidate,
        std::iter::once(&record),
    )?;
    assert_eq!(
        ledger.report()?.physical_records.bytes,
        physical,
        "same Arc must not be charged as another body"
    );
    drop(loaded.material_views);
    assert!(weak_guard.upgrade().is_none());
    drop(domain);
    drop(candidate);
    drop(capture);
    assert!(weak_record.upgrade().is_some());
    drop(record);
    assert!(weak_record.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn work_material_retires_before_successor_state_decode_and_preserves_nostate_semantics(
) -> Result<()> {
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id=?1",
            [scope().agent_id],
        )?;
        Ok(())
    })?;
    let no_state = db
        .transaction(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(
                tx,
                &scope(),
                &full_request(),
                true,
                &ledger,
                None,
            )
        })?
        .require_current()?;
    assert!(no_state.facts.state.is_none());
    drop(no_state);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        let mut successor = identity();
        successor.incarnation = 2;
        upsert_agent_identity_tx(tx, &successor)?;
        upsert_agent_state_tx(tx, &AgentState::new("agent-a"))?;
        tx.execute(
            "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
            params!["[".to_owned() + &"x".repeat(256 * 1024), scope().agent_id],
        )?;
        Ok(())
    })?;
    let retired = db.transaction(|tx| {
        prepare_work_domain_snapshot_with_budget_tx(
            tx,
            &scope(),
            &full_request(),
            true,
            &ledger,
            None,
        )
    })?;
    assert!(matches!(retired,WorkBootstrapSnapshot::Retired{scope:original} if original==scope()));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn work_material_keeps_legacy_absent_incarnation_and_rejects_foreign_body_before_json() -> Result<()>
{
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        tx.execute("UPDATE agent_identities SET payload_json=json_remove(payload_json,'$.incarnation') WHERE agent_id=?1",[scope().agent_id])?;
        Ok(())
    })?;
    let old = db
        .transaction(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(
                tx,
                &scope(),
                &full_request(),
                false,
                &ledger,
                None,
            )
        })?
        .require_current()?;
    assert_eq!(
        old.facts
            .identity
            .as_ref()
            .context("legacy identity missing")?
            .incarnation,
        1
    );
    drop(old);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    db.transaction(|tx| {
        let mut foreign = work("foreign-work", WorkItemState::Open);
        foreign.agent_id = "foreign-owner".into();
        raw_legacy_work(tx, &foreign)?;
        tx.execute(
            "UPDATE work_items SET payload_json=?1 WHERE work_item_id=?2",
            params!["[".to_owned() + &"x".repeat(1024 * 1024), foreign.id],
        )?;
        Ok(())
    })?;
    let mut explicit = full_request();
    explicit
        .capture
        .mandatory_roots
        .insert(WorkKey::new(WorkFamily::Work, "foreign-work"));
    let error = db
        .transaction(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(
                tx,
                &scope(),
                &explicit,
                false,
                &ledger,
                None,
            )
        })
        .err()
        .context("foreign raw body must not acquire qualification")?;
    assert!(
        format!("{error:#}").contains("Work canonical ID belongs to another agent"),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        original
    );
    let adopted:bool=db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM work_domain_records WHERE agent_id=?1 AND record_id='foreign-work')",[scope().agent_id],|r|r.get(0))?;
    assert!(!adopted);
    Ok(())
}

#[test]
fn original_writer_preflight_backpressures_native_wait_pool_before_any_business_write() -> Result<()>
{
    use crate::types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        QueueEntryRecord, QueueEntryStatus,
    };
    for malformed_raw in [false, true] {
        let (_home, db) = fresh()?;
        let mut wait = task_wait("writer-pool-original", "original-task", "original-trigger")?;
        wait.status = WaitConditionStatus::Active;
        wait.resolved_at = None;
        wait.trigger_message_id = None;
        wait.continuation = Some(serde_json::json!({"items":vec![0;2048]}));
        db.transaction(|tx| write_qualified_wait(tx, &wait, &full_request()).map(|_| ()))?;
        let before = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
        if malformed_raw {
            db.transaction(|tx| {
                tx.execute(
                    "UPDATE wait_conditions SET continuation_json=?1 WHERE wait_condition_id=?2",
                    params!["[".to_owned() + &"x".repeat(256 * 1024), wait.id],
                )?;
                Ok(())
            })?;
        }
        let message = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "actual original admission".into(),
            },
        );
        let queue = QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: message.agent_id.clone(),
            priority: message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: message.created_at,
            updated_at: message.created_at,
        };
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let mut policy = full_request();
        if malformed_raw {
            policy.byte_limit = 128 * 1024;
        } else {
            policy.record_limit = 8192;
        }
        let error = db
            .transaction(|tx| {
                crate::runtime_db::transitions::predicted_trigger_wait_tx(
                    tx,
                    &scope(),
                    &message,
                    &queue,
                    &policy,
                    &ledger,
                    None,
                )
            })
            .err()
            .context("original complete pool must reject before decoder/reducer")?;
        assert!(
            matches!(
                error.downcast_ref::<WorkSnapshotError>(),
                Some(WorkSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(
            db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
            before
        );
        assert!(db.messages().by_id(Some("agent-a"), &message.id)?.is_none());
        assert!(db.queue_entries().latest(&message.id)?.is_none());
    }
    Ok(())
}

#[test]
fn guarded_writer_delta_clones_hold_original_material_until_last_drop() -> Result<()> {
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let record = work("actual-guarded-writer", WorkItemState::Open);
    let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &record.id)]);
    let facts = db.transaction(|tx| {
        let proof = prepare_work_write_with_budget_tx(
            tx,
            &scope(),
            &keys,
            &full_request(),
            false,
            &ledger,
            None,
        )?;
        repositories::insert_new_work_item_tx(tx, &record)?;
        let mut material =
            writes::WorkWriteMaterial::for_readback(&proof, &full_request(), None)?.unwrap();
        material.reserve_actual_keys(&proof)?;
        let state = material.final_state_tx(tx, &proof)?;
        material
            .register_tx(
                tx,
                &proof,
                &keys,
                &Default::default(),
                &state,
                &full_request(),
                false,
            )
            .map(|(facts, _)| facts)
    })?;
    let guards = facts
        .delta
        ._material_views
        .as_ref()
        .context("actual writer omitted material")?;
    let weak = Arc::downgrade(&guards[0]);
    let clone = facts.delta.clone();
    let held = ledger.report()?.observed_bytes;
    assert!(held > 0);
    drop(facts);
    assert!(weak.upgrade().is_some());
    assert_eq!(
        ledger.report()?.observed_bytes,
        held,
        "cloning a delta shares its original guards"
    );
    drop(clone);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(db.work_items().latest(&record.id)?.is_some());
    Ok(())
}

#[test]
fn work_completion_material_counts_actual_brief_revision_before_d3_string_read() -> Result<()> {
    let (_home, db) = fresh()?;
    let record = work("normalized-brief-revision-work", WorkItemState::Completed);
    let result = brief("normalized-brief-revision-result", &record.id);
    db.transaction(|tx| {
        insert_qualified_work(tx, &record)?;
        write_brief(tx, &result)?;
        register_work_result_writes_tx(
            tx,
            &scope(),
            &BTreeSet::from([result.id.clone()]),
            &request(),
        )?;
        Ok(())
    })?;
    let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    db.transaction(|tx| {
        tx.execute("UPDATE briefs SET created_event_seq=?1 WHERE evidence_id=?2",params!["x".repeat(256*1024),result.id])?;
        let (text,bytes):(bool,usize)=tx.query_row(
            "SELECT typeof(created_event_seq)='text',length(CAST(created_event_seq AS BLOB)) FROM briefs WHERE evidence_id=?1",[&result.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert!(text && bytes==256*1024,"real INTEGER-affinity schema permits this corruption");
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut policy = full_request();
    policy.byte_limit = 128 * 1024;
    policy
        .capture
        .latest_result_work_ids
        .insert(record.id.clone());
    let error = db
        .transaction(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(tx, &scope(), &policy, false, &ledger, None)
        })
        .err()
        .context("actual normalized revision must reject before fingerprint String acquisition")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        original
    );
    Ok(())
}

#[test]
fn actual_prepared_work_writer_rejects_incoming_native_nodes_without_partial_ack() -> Result<()> {
    use crate::runtime_db::transitions::{
        PreparedInputTransition, WorkItemMutation, WorkItemTransitionCommand,
    };
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut record = work("native-writer-no-partial-ack", WorkItemState::Open);
    record.todo_list = (0..2048)
        .map(|_| crate::types::TodoItem {
            text: String::new(),
            state: crate::types::TodoItemState::Pending,
        })
        .collect();
    let nonce = "native-writer-original-nonce".to_owned();
    let prepared = PreparedInputTransition::work(
        scope(),
        nonce.clone(),
        time(),
        WorkItemTransitionCommand {
            agent_id: scope().agent_id,
            mutation: WorkItemMutation::Insert {
                record: record.clone(),
            },
            agent_state: None,
            brief_evidence: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: false,
            fault: None,
        },
        Default::default(),
        false,
    )?;
    let mut policy = full_request();
    policy.record_limit = 8192;
    assert!(
        usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
            &record
        )?)? * 8
            < policy.byte_limit
    );
    let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    let error = db
        .transitions_in_scope(&scope())
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &prepared,
            &policy,
            &ledger,
            &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit: policy.record_limit,
                byte_limit: policy.byte_limit,
            },
        )
        .err()
        .context("actual writer must reject native nodes before its first Work SQL")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(db.work_items().latest(&record.id)?.is_none());
    let (aliases,receipts):(i64,i64)=db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM work_domain_records WHERE record_id=?1),(SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?2)",params![record.id,nonce],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!((aliases, receipts), (0, 0));
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        original
    );
    Ok(())
}

#[test]
fn actual_work_revision_text_is_budgeted_before_loader_or_writer_projection() -> Result<()> {
    let (_home, db) = fresh()?;
    let record = work("large-normalized-work-revision", WorkItemState::Open);
    db.transaction(|tx| insert_qualified_work(tx, &record).map(|_| ()))?;
    let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    let before_aliases: usize =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM work_domain_records", [], |row| {
                row.get(0)
            })?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE work_items SET revision=?1 WHERE work_item_id=?2",
            params!["r".repeat(256 * 1024), record.id],
        )?;
        Ok(())
    })?;
    let (payload_bytes, revision_bytes, revision_type): (usize, usize, String) =
        db.connection()?.query_row(
            "SELECT length(CAST(payload_json AS BLOB)),length(CAST(revision AS BLOB)),typeof(revision) FROM work_items WHERE work_item_id=?1",
            [&record.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    assert_eq!(revision_type, "text");
    let mut policy = full_request();
    policy.byte_limit = 128 * 1024;
    assert!(payload_bytes * 8 < policy.byte_limit);
    assert!(revision_bytes > policy.byte_limit);
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &record.id)]);
    for writer in [false, true] {
        let error = db
            .transaction(|tx| -> Result<()> {
                if writer {
                    prepare_work_write_with_budget_tx(
                        tx,
                        &scope(),
                        &keys,
                        &policy,
                        false,
                        &ledger,
                        None,
                    )?;
                } else {
                    prepare_work_domain_snapshot_with_budget_tx(
                        tx,
                        &scope(),
                        &policy,
                        false,
                        &ledger,
                        None,
                    )?;
                }
                anyhow::bail!("large normalized Work revision escaped material admission")
            })
            .err()
            .context("projection must be rejected before String acquisition")?;
        assert!(
            matches!(
                error.downcast_ref::<WorkSnapshotError>(),
                Some(WorkSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(
            db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
            original
        );
        assert_eq!(
            db.connection()?
                .query_row("SELECT COUNT(*) FROM work_domain_records", [], |row| row
                    .get::<_, usize>(
                    0
                ))?,
            before_aliases
        );
    }
    Ok(())
}

#[test]
fn selected_work_material_uses_original_remaining_phase_and_retains_actual_guard() -> Result<()> {
    let (_home, db) = fresh()?;
    let mut wait = task_wait("selected-real-sql", "actual-task", "actual-trigger")?;
    wait.status = WaitConditionStatus::Active;
    wait.resolved_at = None;
    wait.trigger_message_id = None;
    db.transaction(|tx| write_qualified_wait(tx, &wait, &request()).map(|_| ()))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let loaded = db
        .transaction(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(
                tx,
                &scope(),
                &full_request(),
                false,
                &ledger,
                None,
            )
        })?
        .require_current()?;
    let original = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    let actual_scope = scope();
    let mut narrow = full_request();
    narrow.byte_limit = 64 * 1024;
    let baseline = ledger.report()?.observed_bytes;
    let mut phase = crate::runtime_db::work_snapshot::read_preparation::WorkReadMaterial::new(
        &actual_scope,
        &narrow,
        &ledger,
        None,
    )?;
    phase.charge(0, 56 * 1024, 1)?;
    let error = phase
        .capture_resident(&loaded.resident, &narrow.capture)
        .err()
        .context("selected material must debit original remaining")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        original
    );
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let mut phase = crate::runtime_db::work_snapshot::read_preparation::WorkReadMaterial::new(
        &actual_scope,
        &full_request(),
        &ledger,
        None,
    )?;
    let selected = phase.capture_resident(&loaded.resident, &full_request().capture)?;
    let weak = Arc::downgrade(&selected.budget_views[1]);
    let body = Arc::downgrade(&selected.records[0]);
    drop(phase);
    drop(loaded);
    assert!(weak.upgrade().is_some() && body.upgrade().is_some());
    drop(selected);
    assert!(weak.upgrade().is_none() && body.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn recovery_derived_work_selectors_are_admitted_before_request_clone_and_carried_to_return(
) -> Result<()> {
    use crate::types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    };
    let (_home, db) = fresh()?;
    let wait = task_wait("recovery-selectors", "actual-task", "actual-trigger")?;
    db.transaction(|tx| write_qualified_wait(tx, &wait, &request()).map(|_| ()))?;
    let state = db
        .agent_states()
        .latest(&scope().agent_id)?
        .context("actual State")?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let before = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    let mut pending = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "borrowed original pending selector".into(),
        },
    );
    pending.id = "large-derived-id".repeat(8192);
    let error = db
        .transaction(|tx| {
            super::input_recovery::prepare_original_work_for_input_recovery_tx(
                tx,
                &scope(),
                &state,
                &full_request().capture,
                None,
                std::slice::from_ref(&pending),
                true,
                (65_536, 32 * 1024),
                &ledger,
                None,
            )
            .map(|_| ())
        })
        .err()
        .context("large derived selector must fail before owned request")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        before
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (current, selected) = db.transaction(|tx| {
        super::input_recovery::prepare_original_work_for_input_recovery_tx(
            tx,
            &scope(),
            &state,
            &full_request().capture,
            None,
            &[],
            true,
            (full_request().record_limit, full_request().byte_limit),
            &ledger,
            None,
        )
    })?;
    assert!(selected.is_empty());
    let material = Arc::downgrade(
        current
            .material_views
            .last()
            .context("original material retained")?,
    );
    assert!(material.upgrade().is_some());
    assert_eq!(current.resident.scope(), &scope());
    drop(current);
    assert!(material.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_work_preproof_and_preflight_share_one_actual_phase_allowance() -> Result<()> {
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut record = work("shared-original-work-phase", WorkItemState::Open);
    record.objective = "original qualified body ".repeat(128);
    db.transaction(|tx| insert_qualified_work(tx, &record).map(|_| ()))?;
    let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &record.id)]);
    let total = db.transaction(|tx| {
        let proof = prepare_work_write_with_budget_tx(
            tx,
            &scope(),
            &keys,
            &full_request(),
            false,
            &ledger,
            None,
        )?;
        let first = proof.original_phase_usage();
        let held = ledger.report()?.observed_bytes;
        let total =
            preflight_work_write_snapshot_with_shared_phase_tx(tx, &proof, &full_request(), None)?;
        assert!(total.0 > first.0 && total.1 > first.1);
        assert_eq!(proof.original_phase_usage(), first);
        assert_eq!(
            ledger.report()?.observed_bytes,
            held,
            "temporary preflight drops while the original proof remains held"
        );
        Ok(total)
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut limited = full_request();
    limited.byte_limit = total.0 - 1;
    let before = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    // The independent loader itself fits; only the original still-held proof
    // plus its new preflight material must reject this same policy.
    drop(
        db.transaction(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(
                tx,
                &scope(),
                &limited,
                false,
                &ledger,
                None,
            )
        })?
        .require_current()?,
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let error = db
        .transaction(|tx| {
            let proof = prepare_work_write_with_budget_tx(
                tx,
                &scope(),
                &keys,
                &limited,
                false,
                &ledger,
                None,
            )?;
            preflight_work_write_snapshot_with_shared_phase_tx(tx, &proof, &limited, None)?;
            Ok(())
        })
        .err()
        .context("coexisting original Work materials must reject")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        before
    );
    assert_eq!(db.work_items().latest(&record.id)?, Some(record));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn work_phase_resume_preserves_incremental_remaining_without_reobserving_prior() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let original_scope = scope();
    let mut policy = full_request();
    policy.byte_limit = 64 * 1024;
    let mut first =
        read_preparation::WorkReadMaterial::new(&original_scope, &policy, &ledger, None)?;
    first.charge(24 * 1024, 0, 1)?;
    let prior = first.usage();
    let held = ledger.report()?.observed_bytes;
    let limits = first.limits();
    let original_guards = first.into_guards();
    let weak = Arc::downgrade(&original_guards[0]);
    // Model the existing Memory request: it has already deducted the old cost.
    // The resumed local Work phase must not deduct that cost for a second time.
    policy.byte_limit = 16 * 1024;
    let mut resumed = read_preparation::WorkReadMaterial::resume(
        &original_scope,
        &policy,
        &ledger,
        None,
        prior,
        limits,
    )?;
    let base = resumed.usage().0 - prior.0;
    assert_eq!(ledger.report()?.observed_bytes, held + u64::try_from(base)?);
    resumed.charge(8 * 1024, 0, 1)?;
    assert!(resumed.usage().0 < limits.0);
    let error = resumed
        .charge(16 * 1024, 0, 1)
        .err()
        .context("new stage must retain its actual incremental limit")?;
    assert!(matches!(
        error.downcast_ref::<WorkSnapshotError>(),
        Some(WorkSnapshotError::ResourceExhausted(_))
    ));
    drop(resumed);
    assert_eq!(ledger.report()?.observed_bytes, held);
    assert!(weak.upgrade().is_some());
    drop(original_guards);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
