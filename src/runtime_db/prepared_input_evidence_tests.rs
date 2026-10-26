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
use anyhow::Context as _;

#[path = "current_read_writes_tests.rs"]
mod current_read_writes_tests;
#[path = "prepared_input_query_tests.rs"]
mod query_tests;
#[path = "prepared_turn_barrier_tests.rs"]
mod turn_barrier_tests;

#[test]
fn idle_batch_validation_borrows_all_unchanged_fields_and_preserves_fences() -> Result<()> {
    use crate::{
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut prior = TurnRecord::new(&f.scope.agent_id, "borrowed-idle-turn", 1);
    prior.tool_execution_ids = vec!["existing-tool".into(); 32];
    let mut record = prior.clone();
    record.input_message_ids.push("anchor".into());
    record.idle_input_batch = Some(crate::types::IdleInputBatchRecord {
        anchor_message_id: "anchor".into(),
        activation_id: None,
        sender_agent_id: "sender".into(),
        cutoff: f.at,
        max_items: 8,
        max_bytes: 1024,
        rendered_bytes: 0,
        items: Vec::new(),
    });
    let header_bytes = u64::try_from(f.scope.agent_id.len() + 256)? * 8;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &f.scope, header_bytes, 64)?;
    let header = phase.finish()?;
    phase.read(|| {
        assert!(idle_batch_invariants(&prior) == idle_batch_invariants(&record));
        Ok(())
    })?;
    assert_eq!(phase.finish()?, header);
    PreparedInputEvidence::turn_idle_batch(
        f.scope.clone(),
        "borrowed-idle-prepare".into(),
        f.at,
        &prior,
        record.clone(),
        &ledger,
    )?;
    for (index, field) in [
        "run_id",
        "tool_execution_ids",
        "external_side_effect_possible",
    ]
    .into_iter()
    .enumerate()
    {
        let mut changed = record.clone();
        match field {
            "run_id" => changed.run_id = Some("different-run".into()),
            "tool_execution_ids" => changed.tool_execution_ids.clear(),
            _ => changed.external_side_effect_possible = true,
        }
        let error = PreparedInputEvidence::turn_idle_batch(
            f.scope.clone(),
            format!("borrowed-idle-reject-{index}"),
            f.at,
            &prior,
            changed,
            &ledger,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "idle batch changed unrelated Turn fields"
        );
    }
    Ok(())
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
    at: DateTime<Utc>,
}

fn native_query_request(
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    after: Option<u64>,
) -> super::super::input_evidence_snapshot::InputEvidenceLoadRequest {
    super::super::input_evidence_snapshot::InputEvidenceLoadRequest {
        query: Some(crate::runtime::resident_input_evidence::InputQueryRead {
            selection: crate::runtime::resident_input_evidence::InputQuerySelection {
                required_keys: Default::default(),
                birth_after: after,
            },
            ledger: ledger.clone(),
        }),
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    }
}

#[test]
fn native_query_prepares_complete_interval_and_capture_survives_writer_invalidation() -> Result<()>
{
    use super::super::input_evidence_snapshot::{
        prepare_input_evidence_snapshot_tx, InputEvidenceBootstrapSnapshot,
    };
    use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
    let f = Fixture::new()?;
    for index in 0..3 {
        let mut record = f.transcript();
        record.id = format!("native-query-{index}");
        let prepared = PreparedInputEvidence::transcript(
            f.scope.clone(),
            format!("query-writer-{index}"),
            f.at,
            record,
        )?;
        f.db.evidence().commit_prepared_input_evidence(&prepared)?;
    }
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let request = native_query_request(&ledger, Some(0));
    let InputEvidenceBootstrapSnapshot::Current(snapshot) =
        f.db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &f.scope, &request))?
    else {
        panic!("actual current scope");
    };
    let resident = snapshot.resident.with_budget(ledger.clone())?;
    let capture = resident.capture_for_request(&request)?;
    let query = capture.query.as_ref().context("complete birth query")?;
    assert_eq!(query.birth_cursor, 3);
    assert_eq!(
        query.born.keys().copied().collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(capture.records.len(), 3);
    for index in 0..3 {
        assert!(capture
            .lookup(&InputEvidenceKey::new(
                Family::Transcript,
                format!("native-query-{index}"),
            ))?
            .is_some());
    }
    let foreign = native_query_request(
        &InputEvidenceBudgetLedger::new(Default::default())?,
        Some(0),
    );
    assert!(resident.capture_for_request(&foreign).is_err());
    let suffix_request = native_query_request(&ledger, Some(2));
    let suffix = resident.capture_for_request(&suffix_request)?;
    assert_eq!(
        suffix
            .query
            .as_ref()
            .unwrap()
            .born
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert_eq!(suffix.records.len(), 1);
    let mut exact_request = native_query_request(&ledger, None);
    exact_request
        .query
        .as_mut()
        .unwrap()
        .selection
        .required_keys
        .insert(InputEvidenceKey::new(Family::Transcript, "native-query-0"));
    let exact = resident.capture_for_request(&exact_request)?;
    assert!(exact.query.as_ref().unwrap().born.is_empty());
    assert_eq!(exact.query.as_ref().unwrap().birth_cursor, 3);
    assert_eq!(exact.records.len(), 1);
    let adjacent = InputEvidenceKey::new(Family::Transcript, "native-query-1");
    assert!(matches!(
        exact.lookup(&adjacent),
        Err(crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(key))
            if key == adjacent
    ));
    drop((suffix, exact, suffix_request, exact_request));
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "query-writer-next".into(),
        f.at,
        f.transcript(),
    )?;
    let InputMutationExecution::Committed { value, .. } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("actual writer cut");
    };
    let next = resident
        .with_committed(&value.input_delta, Default::default())?
        .context("body domain remains available")?;
    assert!(next.capture_for_request(&request).is_err());
    assert_eq!(query.birth_cursor, 3);
    assert!(ledger.report()?.observed_bytes > 0);
    // The committed delta is also a real holder of the newly observed body.
    drop((next, resident, request, value));
    assert!(
        ledger.report()?.observed_bytes > 0,
        "last capture owns its material"
    );
    drop(f);
    for record in &capture.records {
        assert!(capture.lookup(&record.locator.key)?.is_some());
    }
    drop(capture);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn native_query_rejects_gaps_future_frontiers_and_unqualified_body_addresses() -> Result<()> {
    use super::super::input_evidence_snapshot::prepare_input_evidence_snapshot_tx;
    use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let record = f.transcript();
    f.db.transaction(|tx| append_transcript_entry_tx(tx, &record).map(|_| ()))?;
    let mut request = native_query_request(&ledger, None);
    request
        .query
        .as_mut()
        .unwrap()
        .selection
        .required_keys
        .insert(InputEvidenceKey::new(Family::Transcript, &record.id));
    let error =
        f.db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &f.scope, &request))
            .expect_err("a selected raw address is not body qualification");
    assert!(matches!(
        error.downcast_ref::<super::super::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(
            super::super::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(_)
        )
    ));
    let request = native_query_request(&ledger, Some(1));
    assert!(f
        .db
        .transaction(|tx| { prepare_input_evidence_snapshot_tx(tx, &f.scope, &request) })
        .is_err());
    for index in 0..2 {
        let mut body = f.transcript();
        body.id = format!("native-gap-{index}");
        let prepared = PreparedInputEvidence::transcript(
            f.scope.clone(),
            format!("gap-writer-{index}"),
            f.at,
            body,
        )?;
        f.db.evidence().commit_prepared_input_evidence(&prepared)?;
    }
    f.db.connection()?.execute(
        "DELETE FROM input_query_birth_members WHERE birth_seq=1",
        [],
    )?;
    let request = native_query_request(&ledger, Some(0));
    let error =
        f.db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &f.scope, &request))
            .expect_err("count the complete interval, never LIMIT it");
    assert!(matches!(
        error.downcast_ref::<crate::runtime::resident_input_evidence::InputQueryError>(),
        Some(crate::runtime::resident_input_evidence::InputQueryError::Incomplete)
    ));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn native_input_birth_uses_actual_ack_and_does_not_repeat_on_replay_or_noop() -> Result<()> {
    let f = Fixture::new()?;
    let record = f.transcript();
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "native-first".into(),
        f.at,
        record.clone(),
    )?;
    let InputMutationExecution::Committed { mut value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("first Native writer must commit");
    };
    let advance = value
        .input_delta
        .query_advance
        .as_ref()
        .expect("original birth facts")
        .clone();
    advance.validate_delta(&f.scope, &value.input_delta.records)?;
    // Birth facts retain locator scalars, not another owning body/locator copy.
    let locator = &mut std::sync::Arc::get_mut(&mut value.input_delta.records[0])
        .expect("birth advance must not pin the original body")
        .locator;
    let revision = locator.revision;
    locator.revision += 1;
    assert!(advance
        .validate_delta(&f.scope, &value.input_delta.records)
        .is_err());
    std::sync::Arc::get_mut(&mut value.input_delta.records[0])
        .unwrap()
        .locator
        .revision = revision;
    let digest = std::mem::replace(
        &mut std::sync::Arc::get_mut(&mut value.input_delta.records[0])
            .unwrap()
            .locator
            .content_digest,
        "0".repeat(64),
    );
    assert!(advance
        .validate_delta(&f.scope, &value.input_delta.records)
        .is_err());
    std::sync::Arc::get_mut(&mut value.input_delta.records[0])
        .unwrap()
        .locator
        .content_digest = digest;
    advance.validate_delta(&f.scope, &value.input_delta.records)?;
    let body = std::sync::Arc::downgrade(&value.input_delta.records[0]);
    assert!(advance.validate_delta(&f.scope, &[]).is_err());
    let mut foreign = f.scope.clone();
    foreign.identity_incarnation += 1;
    assert!(advance
        .validate_delta(&foreign, &value.input_delta.records)
        .is_err());
    let weak = advance.weak_guard_for_fixture();
    let high_water = || -> Result<u64> {
        Ok(f.db.connection()?.query_row(
            "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
            params![f.scope.agent_id, f.scope.identity_incarnation], |row| row.get(0),
        )?)
    };
    assert_eq!(high_water()?, 1);
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("exact Native receipt must replay");
    };
    assert_eq!(replay, receipt);
    let next =
        PreparedInputEvidence::transcript(f.scope.clone(), "native-noop".into(), f.at, record)?;
    let InputMutationExecution::Committed {
        value: noop,
        receipt,
    } = f.db.evidence().commit_prepared_input_evidence(&next)?
    else {
        panic!("new descriptor must preserve original successful no-op");
    };
    assert!(!receipt.outcome.applied);
    assert_eq!(high_water()?, 1);
    noop.input_delta
        .query_advance
        .as_ref()
        .expect("no-op actual cut")
        .validate_delta(&f.scope, &noop.input_delta.records)?;
    assert_eq!(f.count("input_query_birth_members")?, 1);
    drop(value);
    assert!(
        body.upgrade().is_none(),
        "birth facts must not retain body ownership"
    );
    assert!(
        weak.upgrade().is_some(),
        "last publication holder retains original phase"
    );
    drop(advance);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[test]
fn native_input_birth_does_not_adopt_an_existing_unqualified_body() -> Result<()> {
    let f = Fixture::new()?;
    let record = f.transcript();
    f.db.transaction(|tx| append_transcript_entry_tx(tx, &record).map(|_| ()))?;
    let before =
        f.db.transaction(|tx| committed_transcript_tx(tx, &f.scope.agent_id, &record.id))?;
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "native-old-body".into(),
        f.at,
        record.clone(),
    )?;
    let error =
        f.db.evidence()
            .commit_prepared_input_evidence(&prepared)
            .expect_err("Native birth cannot qualify an existing unqualified body");
    assert!(matches!(
        error.downcast_ref::<super::super::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(super::super::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(id))
            if id == &record.id
    ));
    let after =
        f.db.transaction(|tx| committed_transcript_tx(tx, &f.scope.agent_id, &record.id))?;
    assert_eq!(serde_json::to_value(after)?, serde_json::to_value(before)?);
    assert_eq!(f.count("transcript_entries")?, 1);
    for table in [
        "input_evidence_current",
        "input_query_birth_members",
        "input_query_birth_cursors",
        "input_mutation_receipts",
    ] {
        assert_eq!(f.count(table)?, 0, "{table}");
    }
    Ok(())
}

#[test]
fn native_input_allowance_rejects_before_business_and_receipt_failure_rolls_back_birth(
) -> Result<()> {
    use crate::{
        config::StateMemoryLimits, runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };
    let f = Fixture::new()?;
    let prepared = PreparedInputEvidence::transcript(
        f.scope.clone(),
        "native-budget".into(),
        f.at,
        f.transcript(),
    )?;
    for limits in [
        StateMemoryLimits {
            history_per_agent_bytes: 1,
            pending_per_agent_bytes: 1,
            ..Default::default()
        },
        StateMemoryLimits {
            record_limit: 1,
            ..Default::default()
        },
    ] {
        let ledger = InputEvidenceBudgetLedger::new(limits)?;
        let error =
            f.db.evidence()
                .commit_prepared_input_evidence_with_task_budget(
                    &prepared,
                    &super::super::work_snapshot::WorkDomainLoadRequest {
                        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                        record_limit: 65_536,
                        byte_limit: 64 * 1024 * 1024,
                    },
                    &super::super::task_snapshot::TaskDomainLoadRequest {
                        recovery_at: None,
                        query: Default::default(),
                        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                        record_limit: 65_536,
                        byte_limit: 64 * 1024 * 1024,
                    },
                    &ledger,
                    None,
                    None,
                )
                .expect_err("configured Input allowance cannot admit original Native material");
        assert!(matches!(
            error.downcast_ref::<super::super::input_evidence_snapshot::InputEvidenceSnapshotError>(),
            Some(
                super::super::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_)
            )
        ), "{error:#}");
        for table in [
            "transcript_entries",
            "input_query_birth_members",
            "input_query_birth_cursors",
            "input_mutation_receipts",
        ] {
            assert_eq!(f.count(table)?, 0, "{table}");
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    f.db.connection()?.execute_batch(
        "CREATE TRIGGER reject_native_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'original receipt fault'); END;",
    )?;
    let error =
        f.db.evidence()
            .commit_prepared_input_evidence(&prepared)
            .expect_err("original receipt fault");
    assert!(
        format!("{error:#}").contains("original receipt fault"),
        "{error:#}"
    );
    for table in [
        "transcript_entries",
        "input_evidence_current",
        "input_query_birth_members",
        "input_query_birth_cursors",
        "input_mutation_receipts",
    ] {
        assert_eq!(f.count(table)?, 0, "{table}");
    }
    Ok(())
}

#[test]
fn native_input_phase_rejects_foreign_tx_and_ledger_without_losing_original_guards() -> Result<()> {
    use super::super::input_evidence_snapshot::native_birth::NativeInputWriteProof;
    use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
    let f = Fixture::new()?;
    let other = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let foreign_ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    f.db.transaction(|tx| {
        let mut proof = NativeInputWriteProof::prepare_tx(
            tx,
            &f.scope,
            &ledger,
            (64 * 1024 * 1024, 65_536),
            None,
            |visit| visit(Family::Transcript, "native-bound"),
        )?
        .expect("installed Native source");
        other.db.transaction(|foreign_tx| {
            assert!(proof
                .with_input_phase(foreign_tx, &ledger, None, |_| Ok(()))
                .is_err());
            Ok(())
        })?;
        assert!(proof
            .with_input_phase(tx, &foreign_ledger, None, |_| Ok(()))
            .is_err());
        proof
            .with_input_phase(tx, &ledger, None, |_| -> Result<()> {
                anyhow::bail!("original material action failed");
            })
            .expect_err("action error preserves phase");
        proof.with_input_phase(tx, &ledger, None, |_| Ok(()))?;
        proof.with_input_phase(tx, &ledger, None, |material| {
            use crate::runtime::input_evidence_budget::BudgetViewGuard;
            let (bytes, records) = material.remaining_limits();
            let guard_metadata = std::mem::size_of::<BudgetViewGuard>()
                + 2 * std::mem::size_of::<std::sync::Arc<BudgetViewGuard>>();
            material.observe_native_footprint(bytes - guard_metadata, records - 1)?;
            assert_eq!(material.remaining_limits(), (0, 0));
            Ok(())
        })?;
        let mut delta = Default::default();
        proof.finish_delta_tx(tx, &mut delta, &ledger, None)?;
        delta
            .query_advance
            .as_ref()
            .expect("empty actual cut")
            .validate_delta(&f.scope, &[])?;
        Ok(())
    })
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
    let current = value
        .current_read_writes
        .as_ref()
        .context("Brief omitted original Current ACK")?;
    assert_eq!(current.scope, f.scope);
    assert_eq!(current.revision, current.base_revision + 1);
    assert!(current.is_complete());
    assert_eq!(current.facts().len(), 1);
    assert!(current.qualifies(&(
        crate::runtime_db::current_read_writes::CurrentFamily::Event,
        actual.event.id.clone(),
    )));
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
