use super::*;
use crate::{
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::register_pruned_input_evidence_tx,
    },
    types::{
        brief_created_event_for, AgentIdentityRecord, AgentKind, AgentOwnership,
        AgentProfilePreset, AgentState, AgentVisibility, AuthorityClass, BriefKind, MessageBody,
        MessageKind, MessageOrigin, Priority, ToolExecutionStatus, TranscriptEntryKind,
    },
};

struct Fixture {
    _dir: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
    at: DateTime<Utc>,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "prepared-evidence".into(),
            identity_incarnation: 1,
        };
        add_scope(&db, &scope)?;
        Ok(Self {
            _dir: dir,
            db,
            scope,
            at: "2026-10-13T00:00:00.123456789Z".parse()?,
        })
    }
    fn transcript(&self) -> TranscriptEntry {
        let mut record = TranscriptEntry::new(
            &self.scope.agent_id,
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::json!({"original":"body"}),
        );
        record.id = "transcript:original".into();
        record.created_at = self.at;
        record
    }
    fn tool(&self) -> ToolExecutionRecord {
        ToolExecutionRecord {
            id: "tool:original".into(),
            agent_id: self.scope.agent_id.clone(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommand".into(),
            created_at: self.at,
            completed_at: Some(self.at),
            duration_ms: 0,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({"cmd":"printf ok"}),
            output: serde_json::json!({"stdout":"ok"}),
            summary: "done".into(),
            invocation_surface: None,
        }
    }
    fn count(&self, table: &str) -> Result<i64> {
        Ok(self
            .db
            .connection()?
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    }
}
fn add_scope(db: &RuntimeDb, scope: &InputEvidenceScope) -> Result<()> {
    db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                &scope.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))
    })
}

#[test]
fn transcript_original_sequence_replays_after_prune_without_recreating_body() -> Result<()> {
    let f = Fixture::new()?;
    let record = f.transcript();
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "transcript-original".into(),
        f.at,
        record.clone(),
    )?;
    let receipt = match f.db.evidence().commit_prepared_input_evidence(&prepared)? {
        InputMutationExecution::Committed { value, receipt } => {
            let PreparedEvidenceValue::Transcript(actual) = value.record else {
                panic!("wrong first ACK")
            };
            assert_eq!(actual.transcript_seq, Some(1));
            receipt
        }
        _ => panic!("first append must commit"),
    };
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM transcript_entries WHERE evidence_id=?1",
            [&record.id],
        )?;
        register_pruned_input_evidence_tx(
            tx,
            &f.scope,
            &[InputEvidenceKey::new(Family::Transcript, &record.id)],
        )?;
        Ok(())
    })?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("pruned original result must replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(f.count("transcript_entries")?, 0);
    assert!(matches!(
        replay.outcome.result,
        InputMutationResult::Evidence {
            actual_seq: Some(1),
            ..
        }
    ));
    Ok(())
}
#[test]
fn idempotent_transcript_new_command_commits_successful_noop_receipt() -> Result<()> {
    let f = Fixture::new()?;
    let record = f.transcript();
    for (nonce, expected) in [("first", true), ("second", false)] {
        let prepared =
            PreparedInputEvidence::transcript(f.scope.clone(), nonce.into(), f.at, record.clone())?;
        let InputMutationExecution::Committed { receipt, .. } =
            f.db.evidence().commit_prepared_input_evidence(&prepared)?
        else {
            panic!("new nonce must commit")
        };
        assert_eq!(receipt.outcome.applied, expected);
    }
    assert_eq!(f.count("input_mutation_receipts")?, 2);
    assert_eq!(f.count("transcript_entries")?, 1);
    Ok(())
}
#[test]
fn tool_receipt_keeps_minimal_result_without_large_canonical_output() -> Result<()> {
    let f = Fixture::new()?;
    let mut tool = f.tool();
    let sentinel = "original-large-tool-output";
    tool.output = serde_json::json!({"stdout":format!("{sentinel}{}","x".repeat(10*1024*1024))});
    let prepared = PreparedInputEvidence::tool(
        f.scope.clone(),
        "tool-original".into(),
        f.at,
        tool,
        Vec::new(),
    )?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("first tool commit missing")
    };
    assert!(matches!(value.record, PreparedEvidenceValue::ToolDone));
    let json = serde_json::to_string(&receipt)?;
    assert!(json.len() < 16 * 1024);
    assert!(!json.contains(sentinel));
    assert!(matches!(
        receipt.outcome.result,
        InputMutationResult::Evidence {
            family: ResultFamily::ToolExecution,
            actual_seq: None,
            ..
        }
    ));
    Ok(())
}
#[test]
fn brief_receipt_has_actual_event_link_and_keeps_original_event_after_audit_prune() -> Result<()> {
    let f = Fixture::new()?;
    let brief = BriefRecord::new(&f.scope.agent_id, BriefKind::Ack, "notice", None, None);
    let event = brief_created_event_for(&brief)?;
    let prepared = PreparedInputEvidence::brief_created(
        f.scope.clone(),
        "brief-original".into(),
        f.at,
        brief,
        event,
        Vec::new(),
    )?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("first Brief ACK missing")
    };
    let PreparedEvidenceValue::BriefCreated(actual) = value.record else {
        panic!("wrong first result")
    };
    assert_eq!(actual.brief.created_event_seq, Some(actual.event.event_seq));
    assert_eq!(
        receipt.outcome.scopes[0].audit_events,
        vec![actual.event.clone()]
    );
    assert!(
        matches!(receipt.outcome.result,InputMutationResult::Evidence{created_event_seq:Some(seq),..} if seq==actual.event.event_seq)
    );
    f.db.connection()?.execute(
        "DELETE FROM audit_events WHERE audit_event_id=?1",
        [&actual.event.id],
    )?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("must replay original")
    };
    assert_eq!(replay, receipt);
    assert_eq!(f.count("audit_events")?, 0);
    Ok(())
}
#[test]
fn standalone_receipt_rejection_rolls_back_brief_event_alias_and_index() -> Result<()> {
    let f = Fixture::new()?;
    f.db.connection()?.execute_batch("CREATE TRIGGER reject_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(IGNORE); END;")?;
    let brief = BriefRecord::new(&f.scope.agent_id, BriefKind::Ack, "notice", None, None);
    let event = brief_created_event_for(&brief)?;
    let prepared = PreparedInputEvidence::brief_created(
        f.scope.clone(),
        "rejected".into(),
        f.at,
        brief,
        event,
        Vec::new(),
    )?;
    assert!(f
        .db
        .evidence()
        .commit_prepared_input_evidence(&prepared)
        .is_err());
    for table in [
        "briefs",
        "audit_events",
        "input_evidence_current",
        "input_mutation_receipts",
        "runtime_index_outbox",
    ] {
        assert_eq!(f.count(table)?, 0, "{table}");
    }
    Ok(())
}
#[test]
fn message_actual_count_and_turn_assignment_are_acknowledged_before_receipts() -> Result<()> {
    let f = Fixture::new()?;
    let message = MessageEnvelope::new(
        &f.scope.agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "input".into(),
        },
    );
    let prepared = PreparedInputEvidence::message(
        f.scope.clone(),
        "message-original".into(),
        f.at,
        message.clone(),
        Vec::new(),
    )?;
    let InputMutationExecution::Committed { value, .. } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("first Message ACK missing")
    };
    assert_eq!(value.agent_state.unwrap().record.total_message_count, 1);
    let mut turn = TurnRecord::new(&f.scope.agent_id, "turn:original", 1);
    turn.input_message_ids.push(message.id.clone());
    let prepared =
        PreparedInputEvidence::turn(f.scope.clone(), "turn-original".into(), f.at, turn)?;
    let InputMutationExecution::Committed { value, .. } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("first Turn ACK missing")
    };
    let PreparedEvidenceValue::Turn(actual) = value.record else {
        panic!("wrong Turn result")
    };
    assert_eq!(actual.input_assignments.len(), 1);
    assert!(actual
        .affected_source_revisions
        .iter()
        .any(|r| r.source_id == message.id));
    assert_eq!(
        value.input_delta.message_turn_keys[&message.id],
        Some("turn:original".into())
    );
    assert_eq!(f.count("input_mutation_receipts")?, 2);
    Ok(())
}
#[test]
fn multi_scope_retention_replays_original_report_after_inventory_becomes_empty() -> Result<()> {
    let f = Fixture::new()?;
    let other = InputEvidenceScope {
        agent_id: "prepared-other".into(),
        identity_incarnation: 1,
    };
    add_scope(&f.db, &other)?;
    for scope in [&f.scope, &other] {
        let mut tool = f.tool();
        tool.id = format!("tool:{}", scope.agent_id);
        tool.agent_id = scope.agent_id.clone();
        tool.created_at = f.at - chrono::Duration::days(400);
        tool.completed_at = Some(tool.created_at);
        f.db.evidence()
            .append_tool_execution_in_scope(scope, &tool, &[])?;
        let mut fresh = f.tool();
        fresh.id = format!("fresh:{}", scope.agent_id);
        fresh.agent_id = scope.agent_id.clone();
        f.db.evidence()
            .append_tool_execution_in_scope(scope, &fresh, &[])?;
    }
    let policy = RuntimeDbRetentionPolicy {
        enabled: true,
        audit_events_days: 1,
        transcript_entries_days: 1,
        tool_executions_days: 1,
        audit_events_min_rows_per_scope: crate::http::MAX_EVENT_STREAM_WINDOW,
        transcript_entries_min_rows: 1,
        tool_executions_min_rows: 1,
        ..Default::default()
    };
    let scopes = f.db.retention_input_scopes(policy.clone(), f.at)?;
    assert_eq!(scopes.len(), 2);
    let prepared = PreparedExternalInputEvidence::Retention(PreparedInputRetention::new(
        scopes,
        "retention-original".into(),
        f.at,
        policy.clone(),
        f.at,
    )?);
    let InputMutationExecution::Committed { value, receipt } =
        f.db.commit_prepared_external_input_evidence(&prepared)?
    else {
        panic!("first retention ACK missing")
    };
    let PreparedExternalInputValue::Retention(actual) = value else {
        panic!("wrong retention result")
    };
    assert_eq!(actual.committed.report.tool_executions.deleted_rows, 2);
    assert_eq!(receipt.outcome.scopes.len(), 2);
    assert_eq!(f.count("tool_executions")?, 2);
    assert!(f.db.retention_input_scopes(policy, f.at)?.is_empty());
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = f.db.commit_prepared_external_input_evidence(&prepared)?
    else {
        panic!("empty later inventory cannot invalidate receipt")
    };
    assert_eq!(replay, receipt);
    assert_eq!(f.count("input_mutation_receipts")?, 1);
    Ok(())
}
#[test]
fn host_typed_boundary_rejects_arbitrary_transcript_as_notice() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "not-notice".into(),
        f.at,
        f.transcript(),
    )?;
    assert!(PreparedExternalInputEvidence::brief_created(prepared).is_err());
    assert_eq!(f.count("input_mutation_receipts")?, 0);
    assert_eq!(f.count("transcript_entries")?, 0);
    Ok(())
}

#[test]
fn standalone_group_keeps_primary_when_sorted_after_secondary() -> Result<()> {
    let f = Fixture::new()?;
    let secondary = InputEvidenceScope {
        agent_id: "aaa-evidence-related".into(),
        identity_incarnation: 1,
    };
    add_scope(&f.db, &secondary)?;
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "evidence-group".into(),
        f.at,
        f.transcript(),
    )?
    .with_scope_group(vec![f.scope.clone(), secondary.clone()])?;
    assert_eq!(prepared.primary_scope(), &f.scope);
    assert_eq!(prepared.descriptor().scopes()[0], secondary);
    let InputMutationExecution::Committed { value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("first group ACK missing")
    };
    let PreparedEvidenceValue::Transcript(record) = value.record else {
        panic!("wrong result")
    };
    assert_eq!(record.agent_id, f.scope.agent_id);
    assert_eq!(receipt.outcome.scopes[0].scope, secondary);
    assert!(!receipt.outcome.scopes[0].notify_scheduler);
    assert!(!receipt.outcome.scopes[0].notify_memory_index);
    assert!(receipt.outcome.scopes[1].notify_scheduler);
    assert!(receipt.outcome.scopes[1].notify_memory_index);
    Ok(())
}


fn native_commit(
    f: &Fixture,
    prepared: &PreparedInputEvidence,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<InputEvidenceCommit<PreparedEvidenceValue>>> {
    let work = super::super::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        byte_limit:64*1024*1024, record_limit:65_536,
    };
    let tasks = super::super::task_snapshot::TaskDomainLoadRequest {
        query:Default::default(),capture:crate::runtime::resident_task::TaskCaptureRequest::current(),
        byte_limit:work.byte_limit,record_limit:work.record_limit,
    };
    f.db.evidence().commit_prepared_input_evidence_with_task_budget(prepared,&work,&tasks,ledger)
}

#[test]
fn original_standalone_evidence_writers_mint_native_births_once_and_preserve_receipt_replay() -> Result<()> {
    let f=Fixture::new()?;
    let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let transcript=PreparedInputEvidence::transcript(f.scope.clone(),"native-transcript".into(),f.at,f.transcript())?;
    let tool=PreparedInputEvidence::tool(f.scope.clone(),"native-tool".into(),f.at,f.tool(),Vec::new())?;
    let mut brief=BriefRecord::new(&f.scope.agent_id,BriefKind::Ack,"actual native Brief",None,None);
    brief.id="native-brief".into();brief.created_at=f.at;
    let brief=PreparedInputEvidence::brief_created(f.scope.clone(),"native-brief-command".into(),f.at,brief.clone(),brief_created_event_for(&brief)?,Vec::new())?;
    for (index,prepared) in [transcript,tool,brief].iter().enumerate() {
        let InputMutationExecution::Committed{value,receipt}=native_commit(&f,prepared,&ledger)? else{panic!("original writer must commit")};
        assert!(!value.input_delta.records.is_empty());
        let expected=u64::try_from(index+1)?;
        assert_eq!(f.db.connection()?.query_row("SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",params![f.scope.agent_id,f.scope.identity_incarnation],|r|r.get::<_,u64>(0))?,expected);
        let (agent,id,family,_)=prepared.command.identity();
        assert_eq!(f.db.connection()?.query_row("SELECT birth_seq FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",params![agent,f.scope.identity_incarnation,family.as_str(),id],|r|r.get::<_,u64>(0))?,expected);
        drop(value);
        let InputMutationExecution::Replayed{receipt:replayed,..}=native_commit(&f,prepared,&ledger)? else{panic!("same original receipt must replay")};
        assert_eq!(replayed,receipt);
        assert_eq!(f.count("input_query_birth_members")?,i64::try_from(index+1)?);
        assert_eq!(ledger.report()?.observed_bytes,0,"before-proof selectors die only after the actual finish consumes them");
        if index==0 {
            f.db.transaction(|tx| {
                tx.execute("DELETE FROM transcript_entries WHERE evidence_id=?1",[id])?;
                register_pruned_input_evidence_tx(tx,&f.scope,&[InputEvidenceKey::new(family,id)])?;
                Ok(())
            })?;
            let InputMutationExecution::Replayed{receipt:after_prune,..}=native_commit(&f,prepared,&ledger)? else{panic!("original receipt must survive actual body prune")};
            assert_eq!(after_prune,receipt);
            assert_eq!(f.count("transcript_entries")?,0);
            assert_eq!(f.db.connection()?.query_row("SELECT birth_seq FROM input_query_birth_members WHERE family='transcript'",[],|r|r.get::<_,u64>(0))?,1);
        }
    }
    assert_eq!(f.count("input_mutation_receipts")?,3);
    Ok(())
}

#[test]
fn existing_accepted_evidence_and_new_nonce_keep_birth_zero_without_raw_adoption() -> Result<()> {
    let f=Fixture::new()?;
    let original=PreparedInputEvidence::transcript(f.scope.clone(),"before-native-source".into(),f.at,f.transcript())?;
    let first=f.db.evidence().commit_prepared_input_evidence(&original)?;
    assert!(matches!(first,InputMutationExecution::Committed{..}));
    let next=PreparedInputEvidence::transcript(f.scope.clone(),"new-native-observation".into(),f.at,f.transcript())?;
    let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(matches!(native_commit(&f,&next,&ledger)?,InputMutationExecution::Committed{..}));
    assert_eq!(f.db.connection()?.query_row("SELECT birth_seq FROM input_query_birth_members",[],|r|r.get::<_,u64>(0))?,0);
    assert_eq!(f.db.connection()?.query_row("SELECT high_water FROM input_query_birth_cursors",[],|r|r.get::<_,u64>(0))?,0);
    let mut raw=f.transcript();raw.id="unqualified-late-transcript".into();
    f.db.transaction(|tx| {append_transcript_entry_tx(tx,&raw)?;Ok(())})?;
    let late=PreparedInputEvidence::transcript(f.scope.clone(),"do-not-adopt-late".into(),f.at,raw)?;
    let error=native_commit(&f,&late,&ledger).err().expect("native proof cannot replace original D3 qualification");
    assert!(error.downcast_ref::<super::super::input_evidence_snapshot::InputEvidenceSnapshotError>().is_some(),"{error:#}");
    assert_eq!(f.count("input_query_birth_members")?,1);
    assert_eq!(f.count("input_mutation_receipts")?,2);
    assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}

#[test]
fn native_evidence_births_and_original_body_rollback_with_the_last_whole_receipt() -> Result<()> {
    let f=Fixture::new()?;
    let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let prepared=PreparedInputEvidence::transcript(f.scope.clone(),"native-before-receipt-fault".into(),f.at,f.transcript())?;
    f.db.connection()?.execute_batch("CREATE TRIGGER fail_native_evidence_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'actual last receipt fault'); END")?;
    let error=native_commit(&f,&prepared,&ledger).err().expect("actual last receipt fault must fail");
    assert!(format!("{error:#}").contains("actual last receipt fault"),"{error:#}");
    for table in ["transcript_entries","input_evidence_current","input_query_birth_members","input_query_birth_cursors","input_mutation_receipts"] {assert_eq!(f.count(table)?,0,"whole original transaction rolled back {table}");}
    assert_eq!(ledger.report()?.observed_bytes,0);
    f.db.connection()?.execute_batch("DROP TRIGGER fail_native_evidence_receipt")?;
    assert!(matches!(native_commit(&f,&prepared,&ledger)?,InputMutationExecution::Committed{..}));
    assert_eq!(f.db.connection()?.query_row("SELECT birth_seq FROM input_query_birth_members",[],|r|r.get::<_,u64>(0))?,1);
    assert_eq!(f.count("input_mutation_receipts")?,1);
    assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}
