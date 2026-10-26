use super::*;
use crate::{
    runtime_db::{
        evidence::{
            append_message_tx, append_transcript_entry_tx, insert_tool_evidence_tx,
            upsert_agent_identity_tx, upsert_agent_state_tx,
        },
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        repositories::upsert_turn_record_tx,
        RuntimeDb,
    },
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentState, AgentVisibility, AuthorityClass,
        MessageBody, MessageKind, MessageOrigin, Priority, ToolExecutionStatus,
        TranscriptEntryKind, WorkItemRecord, WorkItemState,
    },
};
use rusqlite::Connection;

fn time() -> DateTime<Utc> {
    "2026-10-12T00:00:00Z".parse().unwrap()
}
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    }
}

#[test]
fn request_root_union_borrows_duplicates_and_preserves_failed_material() -> Result<()> {
    use crate::{
        config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
        runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        (scope.agent_id.len() as u64 + 256) * 8,
        64,
    )?;
    let mut request = InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([Key::new(Family::Message, "pending")]),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: 1024,
        byte_limit: 4 * 1024 * 1024,
    };
    let baseline = phase.finish()?;
    phase.read(|| {
        for _ in 0..100 {
            request.insert_admitted_root(Family::Message, "pending")?;
        }
        Ok(())
    })?;
    assert_eq!(phase.finish()?, baseline);
    assert_eq!(request.mandatory_roots.len(), 1);
    // The same ID in another family is a new key and still needs admission.
    assert!(phase
        .read(|| request.insert_admitted_root(Family::Turn, "pending"))
        .unwrap_err()
        .to_string()
        .contains("ResourceExhausted"));
    assert!(phase
        .read(|| request.insert_admitted_root(Family::Message, "pending"))
        .is_err());
    assert_eq!(request.mandatory_roots.len(), 1);
    Ok(())
}

#[test]
fn request_root_batch_admits_the_union_once_and_preserves_atomic_failure() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::{cloned_value_material, ResourceReadMaterial},
        runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let ids = (0..100)
        .map(|index| format!("pending-{index}"))
        .collect::<Vec<_>>();
    let roots = || ids.iter().map(|id| (Family::Message, id.as_str()));
    // The phase can admit one union, not one aggregate envelope per key.
    let per_key_records = ids
        .iter()
        .map(|id| {
            cloned_value_material(&(Family::Message, id.as_str())).map(|(_, count)| count * 8)
        })
        .collect::<Result<Vec<_>>>()?
        .iter()
        .sum::<usize>();
    assert!(per_key_records > 8192);
    for short in [false, true] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            1024 * 1024,
            if short { 64 } else { 8192 },
        )?;
        let mut request = InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: BTreeSet::new(),
            interrupted_sources: BTreeSet::new(),
            selected_completion_brief_ids: BTreeSet::new(),
            record_limit: 1024,
            byte_limit: 4 * 1024 * 1024,
        };
        if short {
            assert!(phase.read(|| request.extend_admitted_roots(roots)).is_err());
            assert!(request.mandatory_roots.is_empty());
            assert!(phase
                .read(|| request.extend_admitted_roots(std::iter::empty))
                .is_err());
            continue;
        }
        phase.read(|| request.extend_admitted_roots(roots))?;
        assert_eq!(request.mandatory_roots.len(), ids.len());
        assert!(ids
            .iter()
            .all(|id| request.contains_root(Family::Message, id)));
        let baseline = phase.finish()?;
        phase.read(|| {
            for _ in 0..5 {
                request.extend_admitted_roots(roots)?;
            }
            Ok(())
        })?;
        assert_eq!(phase.finish()?, baseline);
        phase.read(|| {
            request.extend_admitted_roots(|| std::iter::once((Family::Turn, ids[0].as_str())))
        })?;
        assert!(request.contains_root(Family::Turn, &ids[0]));
        assert_eq!(request.mandatory_roots.len(), ids.len() + 1);
    }
    Ok(())
}

#[test]
fn preparation_view_borrows_frozen_selectors_and_moves_admitted_roots() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::{cloned_value_material, ResourceReadMaterial},
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::{InputQueryRead, InputQuerySelection},
        },
    };
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let request = InputEvidenceLoadRequest {
        query: Some(InputQueryRead {
            selection: InputQuerySelection {
                required_keys: (0..128)
                    .map(|index| Key::new(Family::Message, format!("selected-{index}")))
                    .collect(),
                birth_after: Some(0),
            },
            ledger: ledger.clone(),
        }),
        windows: vec![InputWindowRequest {
            selector: InputWindowSelector::RecentBriefs,
            limit: 64,
        }],
        mandatory_roots: BTreeSet::from([Key::new(Family::Message, "pending")]),
        interrupted_sources: BTreeSet::from(["interrupted".into()]),
        selected_completion_brief_ids: BTreeSet::from(["head".into()]),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    };
    // The old whole-request clone cannot fit, but root expansion can.
    assert!(cloned_value_material(&request)?.1 * 8 > 4096);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 4096)?;
    let mut view = request.borrowed();
    let baseline = phase.finish()?;
    phase.read(|| {
        for _ in 0..100 {
            view.insert_admitted_root(Family::Message, "pending")?;
            view.insert_admitted_completion_brief("head")?;
        }
        Ok(())
    })?;
    assert_eq!(phase.finish()?, baseline);
    phase.read(|| {
        view.insert_admitted_root(Family::Turn, "pending")?;
        view.insert_admitted_completion_brief("new-head")
    })?;
    assert!(std::ptr::eq(
        view.query.unwrap(),
        request.query.as_ref().unwrap()
    ));
    assert!(std::ptr::eq(view.windows, request.windows.as_slice()));
    assert!(std::ptr::eq(
        view.interrupted_sources,
        &request.interrupted_sources
    ));
    assert!(matches!(view.mandatory_roots, Cow::Owned(_)));
    assert!(matches!(view.selected_completion_brief_ids, Cow::Owned(_)));
    let expanded = phase.finish()?;
    phase.read(|| {
        for _ in 0..100 {
            view.insert_admitted_root(Family::Turn, "pending")?;
            view.insert_admitted_completion_brief("new-head")?;
        }
        Ok(())
    })?;
    assert_eq!(phase.finish()?, expanded);
    let root = view.mandatory_roots.first().unwrap() as *const Key;
    let roots = view.mandatory_roots.into_owned();
    assert_eq!(roots.first().unwrap() as *const Key, root);
    assert_eq!(request.mandatory_roots.len(), 1);
    assert_eq!(request.selected_completion_brief_ids.len(), 1);
    Ok(())
}

#[test]
fn preparation_view_preserves_header_only_failure_latch() -> Result<()> {
    use crate::{
        config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
        runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    for completion in [false, true] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            (scope.agent_id.len() as u64 + 256) * 8,
            64,
        )?;
        let request = InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: BTreeSet::from([Key::new(Family::Message, "pending")]),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: BTreeSet::from(["head".into()]),
            record_limit: 1024,
            byte_limit: 4 * 1024 * 1024,
        };
        let mut view = request.borrowed();
        let baseline = phase.finish()?;
        phase.read(|| {
            view.insert_admitted_root(Family::Message, "pending")?;
            view.insert_admitted_completion_brief("head")
        })?;
        assert_eq!(phase.finish()?, baseline);
        assert!(phase
            .read(|| {
                if completion {
                    view.insert_admitted_completion_brief("new-head")
                } else {
                    view.insert_admitted_root(Family::Turn, "pending")
                }
            })
            .is_err());
        assert!(phase
            .read(|| view.insert_admitted_root(Family::Message, "pending"))
            .is_err());
        assert!(phase
            .read(|| view.insert_admitted_completion_brief("head"))
            .is_err());
        assert!(matches!(view.mandatory_roots, Cow::Borrowed(_)));
        assert!(matches!(
            view.selected_completion_brief_ids,
            Cow::Borrowed(_)
        ));
    }
    Ok(())
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
fn message(id: &str) -> MessageEnvelope {
    let mut value = MessageEnvelope::new(
        "agent-a",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text: id.into() },
    );
    value.id = id.into();
    value.created_at = time();
    value
}
fn request(selector: InputWindowSelector, limit: usize) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        query: None,
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: Default::default(),
        windows: vec![InputWindowRequest { selector, limit }],
        mandatory_roots: BTreeSet::new(),
        record_limit: 1024,
        byte_limit: 1024 * 1024,
    }
}
fn current(snapshot: InputEvidenceBootstrapSnapshot) -> InputEvidenceCurrentSnapshot {
    match snapshot {
        InputEvidenceBootstrapSnapshot::Current(x) => x,
        other => panic!("unexpected snapshot: {other:?}"),
    }
}

#[test]
fn alias_statement_reuse_preserves_scope_keys_and_transaction_visibility() -> Result<()> {
    let (_home, db) = fresh()?;
    let left = Key::new(Family::Message, "left");
    let right = Key::new(Family::Message, "right");
    let foreign = InputEvidenceScope {
        identity_incarnation: 2,
        ..scope()
    };
    db.transaction(|tx| {
        assert!(alias_on(tx, &scope(), &left)?.is_none());
        assert!(alias_on(tx, &scope(), &right)?.is_none());
        let left_message = append_message_tx(tx, &message("left"))?.0;
        let right_message = append_message_tx(tx, &message("right"))?.0;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[
                CanonicalInputEvidence::Message(left_message),
                CanonicalInputEvidence::Message(right_message),
            ],
        )?;
        let before = alias_on(tx, &scope(), &left)?.context("left alias")?;
        let other = alias_on(tx, &scope(), &right)?.context("right alias")?;
        assert_ne!(before.digest, other.digest);
        assert!(alias_on(tx, &foreign, &left)?.is_none());
        assert!(alias_on(tx, &scope(), &Key::new(Family::Turn, "left"))?.is_none());
        assert!(alias_on(tx, &scope(), &Key::new(Family::Message, "missing"))?.is_none());
        assert_eq!(
            tx.execute("DELETE FROM messages WHERE evidence_id=?1", [&left.id])?,
            1
        );
        register_pruned_input_evidence_tx(tx, &scope(), std::slice::from_ref(&left))?;
        let after = alias_on(tx, &scope(), &left)?.context("pruned left alias")?;
        assert!(!after.present);
        assert_eq!(after.revision, before.revision + 1);
        let unchanged = alias_on(tx, &scope(), &right)?.context("unchanged right alias")?;
        assert!(unchanged.present);
        assert_eq!(unchanged.revision, other.revision);
        assert_eq!(unchanged.digest, other.digest);
        Ok(())
    })?;
    db.transaction(|tx| {
        assert!(
            !alias_on(tx, &scope(), &left)?
                .context("committed prune")?
                .present
        );
        assert!(
            alias_on(tx, &scope(), &right)?
                .context("committed right")?
                .present
        );
        assert!(alias_on(tx, &foreign, &right)?.is_none());
        Ok(())
    })
}

#[test]
fn input_snapshot_consumes_original_scope_decode() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let scope = scope();
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        let state = read.facts().state.as_ref().unwrap().id.as_ptr();
        let identity = read.facts().identity.as_ref().unwrap().agent_id.as_ptr();
        let mut wanted = request(InputWindowSelector::GlobalTurns, 12);
        wanted.windows.clear();
        let snapshot = current(prepare_input_evidence_with_scope_read_tx(
            tx,
            &scope,
            wanted.borrowed(),
            false,
            read,
        )?);
        assert_eq!(snapshot.facts.state.as_ref().unwrap().id.as_ptr(), state);
        assert_eq!(
            snapshot.facts.identity.as_ref().unwrap().agent_id.as_ptr(),
            identity
        );
        Ok(())
    })
}

#[test]
fn input_snapshot_scope_transfer_rejects_foreign_transaction_and_canonical_drift() -> Result<()> {
    let (_home, db) = fresh()?;
    let (_other_home, other) = fresh()?;
    db.transaction(|tx| {
        let scope = scope();
        let wanted = request(InputWindowSelector::GlobalTurns, 12);
        other.transaction(|other_tx| {
            let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
            assert_eq!(
                prepare_input_evidence_with_scope_read_tx(
                    other_tx,
                    &scope,
                    wanted.borrowed(),
                    false,
                    read
                )
                .unwrap_err()
                .to_string(),
                "context scope read belongs to another transaction or scope"
            );
            Ok(())
        })?;
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        tx.execute(
            "UPDATE agent_states SET control_revision=control_revision+1",
            [],
        )?;
        assert_eq!(
            prepare_input_evidence_with_scope_read_tx(tx, &scope, wanted.borrowed(), false, read)
                .unwrap_err()
                .to_string(),
            "context scope changed after its validated read"
        );
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
}

fn legacy(
    prepare: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut connection, migration)?;
    }
    {
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(&tx, &identity())?;
        upsert_agent_state_tx(&tx, &AgentState::new("agent-a"))?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[time().to_rfc3339()])?;
        prepare(&tx)?;
        tx.commit()?;
    }
    drop(connection);
    let db = RuntimeDb::open_and_migrate(&path, directory.path().join("runtime.lock"))?;
    db.transaction(|tx| {
        tx.execute_batch(INPUT_EVIDENCE_SCHEMA)?;
        capture_input_evidence_upgrade_tx(tx, time())
    })?;
    Ok((directory, db))
}
fn fresh() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let directory = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        tx.execute_batch(INPUT_EVIDENCE_SCHEMA)?;
        capture_input_evidence_upgrade_tx(tx, time())?;
        upsert_agent_identity_tx(tx, &identity())?;
        upsert_agent_state_tx(tx, &AgentState::new("agent-a"))
    })?;
    Ok((directory, db))
}
fn tool() -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: "tool-a".into(),
        agent_id: "agent-a".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: None,
        tool_name: "ExecCommand".into(),
        created_at: time(),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({"cmd":"printf hello"}),
        output: serde_json::json!({"result":{"exit_status":0,"stdout_preview":"x".repeat(10*1024*1024),"stdout_artifact":{"id":"artifact-a"}}}),
        summary: "command completed".into(),
        invocation_surface: None,
    }
}

#[test]
fn fixed_eligibility_is_metadata_only_and_tool_resident_does_not_pin_stdout() -> Result<()> {
    let (_directory, db) = legacy(|tx| insert_tool_evidence_tx(tx, &tool()))?;
    let count:i64=db.connection()?.query_row("SELECT COUNT(*) FROM input_evidence_eligibility WHERE family='tool_execution' AND length(fingerprint_json)<1024",[],|r|r.get(0))?;
    assert_eq!(count, 1);
    let mut policy = request(InputWindowSelector::RecentTools, 1);
    policy.byte_limit = 16 * 1024;
    let prepared =
        db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &policy))?;
    let captured = current(prepared).resident.capture()?;
    assert_eq!(captured.records.len(), 1);
    let record = &captured.records[0];
    assert!(record.resident_bytes < 4096);
    assert_eq!(record.locator.content_digest.len(), 64);
    let PreparedInputEvidence::ToolExecution(projected) = &record.record else {
        panic!("expected bounded tool")
    };
    assert!(projected.command.has_output_evidence);
    assert!(projected.command.has_artifact);
    assert_eq!(projected.command.exit_status, Some(0));
    assert!(!serde_json::to_string(projected)?.contains(&"x".repeat(1024)));
    Ok(())
}
#[test]
fn fixed_global_and_live_owner_tails_do_not_extend_on_later_calls() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        let mut work = WorkItemRecord::new("agent-a", "retained old owner", WorkItemState::Open);
        work.id = "work-old".into();
        work.created_at = time();
        tx.execute("INSERT INTO work_items(work_item_id,agent_id,state,objective,revision,created_at,updated_at,payload_json)
            VALUES(?1,?2,'open',?3,1,?4,?4,?5)",
            params![work.id,work.agent_id,work.objective,time().to_rfc3339(),serde_json::to_string(&work)?])?;
        let mut state = AgentState::new("agent-a");
        state.current_work_item_id = Some("work-old".into());
        upsert_agent_state_tx(tx, &state)?;
        for index in 1..=515 {
            let mut turn = TurnRecord::new("agent-a", format!("turn-{index:04}"), index);
            turn.created_at = time();
            turn.terminal = Some(crate::types::TurnTerminalSummary {
                kind: crate::types::TurnTerminalKind::Completed,
                reason: None,
                no_brief_reason: None,
                completed_at: time(),
                duration_ms: 0,
            });
            if index == 1 {
                turn.current_work_item_id = Some("work-old".into());
                turn.owner = Some(TurnOwner::WorkItem {
                    work_item_id: "work-old".into(),
                });
            }
            upsert_turn_record_tx(tx, &turn)?;
        }
        Ok(())
    })?;
    let eligible = |id: &str| -> Result<bool> {
        Ok(db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE family='turn' AND record_id=?1)",[id],|r|r.get(0))?)
    };
    assert!(eligible("turn-0001")?);
    assert!(!eligible("turn-0002")?);
    assert!(eligible("turn-0515")?);
    db.transaction(|tx| {
        upsert_turn_record_tx(tx, &TurnRecord::new("agent-a", "late-unqualified", 999))?;
        capture_input_evidence_upgrade_tx(tx, time())
    })?;
    assert!(!eligible("late-unqualified")?);
    let snapshot = db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 3),
        )
    })?;
    let captured = current(snapshot).resident.capture()?;
    assert_eq!(
        captured.windows[0]
            .keys
            .iter()
            .map(|key| key.id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-0515", "turn-0514", "turn-0513"]
    );
    assert!(!captured.windows[0].is_exhaustive);
    assert!(db
        .transaction(|tx| prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 514)
        ))
        .is_err());
    Ok(())
}
#[test]
fn fresh_namespace_does_not_inherit_same_named_history_but_original_writer_qualifies() -> Result<()>
{
    let (_directory, db) = fresh()?;
    let turn = TurnRecord::new("agent-a", "old-unqualified", 1);
    db.transaction(|tx| upsert_turn_record_tx(tx, &turn))?;
    let captured = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 3),
        )
    })?)
    .resident
    .capture()?;
    assert!(captured.records.is_empty());
    let delta = db.transaction(|tx| {
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(turn.clone())],
        )
    })?;
    assert_eq!(delta.records.len(), 1);
    let prepared_current = current(db.transaction(|tx| {
        load_input_evidence_snapshot_tx(tx, &scope(), &request(InputWindowSelector::GlobalTurns, 3))
    })?);
    assert_eq!(prepared_current.resident.capture()?.records.len(), 1);
    let mut successor = identity();
    successor.incarnation = 2;
    db.transaction(|tx| upsert_agent_identity_tx(tx, &successor))?;
    let new_scope = InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 2,
    };
    let successor = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &new_scope,
            &request(InputWindowSelector::GlobalTurns, 3),
        )
    })?);
    assert!(successor.resident.capture()?.records.is_empty());
    assert!(matches!(
        db.transaction(|tx| load_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 3)
        ))?,
        InputEvidenceBootstrapSnapshot::Retired { .. }
    ));
    Ok(())
}
#[test]
fn unchanged_writer_ack_promotes_legacy_preparation_without_new_content_revision() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        append_message_tx(tx, &message("legacy-writer"))?;
        Ok(())
    })?;
    let key = Key::new(Family::Message, "legacy-writer");
    let mut wanted = request(InputWindowSelector::GlobalTurns, 1);
    wanted.windows.clear();
    wanted.mandatory_roots.insert(key.clone());
    let prepared =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted))?);
    let original = prepared
        .resident
        .capture()?
        .records
        .into_iter()
        .find(|record| record.locator.key == key)
        .context("legacy Message was not prepared")?
        .locator
        .clone();
    let qualification = || -> Result<String> {
        Ok(db.connection()?.query_row(
            "SELECT qualification FROM input_evidence_current WHERE family='message' AND record_id='legacy-writer'",
            [], |row| row.get(0),
        )?)
    };
    assert_eq!(qualification()?, "legacy_prepared");
    let rolled_back: Result<()> = db.transaction(|tx| {
        let actual = append_message_tx(tx, &message("legacy-writer"))?.0;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(actual)],
        )?;
        anyhow::bail!("rollback writer proof")
    });
    assert!(rolled_back.is_err());
    assert_eq!(qualification()?, "legacy_prepared");
    let delta = db.transaction(|tx| {
        let actual = append_message_tx(tx, &message("legacy-writer"))?.0;
        let mut wrong = actual.clone();
        wrong.body = MessageBody::Text {
            text: "not canonical".into(),
        };
        assert!(register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(wrong)],
        )
        .is_err());
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(actual)],
        )
    })?;
    assert_eq!(delta.records[0].locator, original);
    assert_eq!(qualification()?, "committed_writer");
    db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted))?;
    assert_eq!(qualification()?, "committed_writer");
    Ok(())
}

#[test]
fn qualified_registration_rolls_back_with_canonical_sql_and_rejects_candidate_mismatch(
) -> Result<()> {
    let (_directory, db) = fresh()?;
    let failed: Result<()> = db.transaction(|tx| {
        let actual = append_message_tx(tx, &message("rolled-back"))?.0;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(actual)],
        )?;
        anyhow::bail!("rollback")
    });
    assert!(failed.is_err());
    let count: i64 = db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM messages)+(SELECT COUNT(*) FROM input_evidence_current)",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    db.transaction(|tx| {
        append_message_tx(tx, &message("message-a"))?;
        Ok(())
    })?;
    let wrong = message("message-a");
    assert!(db
        .transaction(|tx| register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(wrong.clone())]
        ))
        .is_err());
    Ok(())
}
#[test]
fn current_alias_revision_changes_without_mutating_or_pinning_old_allocations() -> Result<()> {
    let (_directory, db) = fresh()?;
    let old = TurnRecord::new("agent-a", "turn-a", 1);
    db.transaction(|tx| {
        upsert_turn_record_tx(tx, &old)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(old.clone())],
        )
    })?;
    let root = Arc::new(
        current(db.transaction(|tx| {
            load_input_evidence_snapshot_tx(
                tx,
                &scope(),
                &request(InputWindowSelector::GlobalTurns, 1),
            )
        })?)
        .resident,
    );
    let weak_root = Arc::downgrade(&root);
    let captured = root.capture()?;
    let old_record = captured.records[0].clone();
    drop(root);
    assert!(weak_root.upgrade().is_none());
    let mut updated = old.clone();
    updated.waiting_condition_ids.push("wait-a".into());
    let delta = db.transaction(|tx| {
        upsert_turn_record_tx(tx, &updated)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(updated.clone())],
        )
    })?;
    assert!(delta.records[0].locator.revision > old_record.locator.revision);
    let PreparedInputEvidence::Turn(old_turn) = &old_record.record else {
        panic!()
    };
    assert!(old_turn.waiting_condition_ids.is_empty());
    let latest = current(db.transaction(|tx| {
        load_input_evidence_snapshot_tx(tx, &scope(), &request(InputWindowSelector::GlobalTurns, 1))
    })?)
    .resident
    .capture()?;
    let PreparedInputEvidence::Turn(new_turn) = &latest.records[0].record else {
        panic!()
    };
    assert_eq!(new_turn.waiting_condition_ids, vec!["wait-a".to_string()]);
    Ok(())
}
#[test]
fn prune_keeps_old_snapshot_bytes_and_refuses_missing_mandatory_capture() -> Result<()> {
    let (_directory, db) = fresh()?;
    let input = db.transaction(|tx| {
        let actual = append_message_tx(tx, &message("message-a"))?.0;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(actual.clone())],
        )?;
        Ok(actual)
    })?;
    let mut policy = request(InputWindowSelector::TrustedOperator, 1);
    policy
        .mandatory_roots
        .insert(Key::new(Family::Message, &input.id));
    let old = current(db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &policy))?)
        .resident
        .capture()?;
    let removal = db.transaction(|tx| {
        tx.execute("DELETE FROM messages WHERE evidence_id=?1", [&input.id])?;
        register_pruned_input_evidence_tx(tx, &scope(), &[Key::new(Family::Message, &input.id)])
    })?;
    assert_eq!(removal.removals.len(), 1);
    assert!(db
        .transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &policy))
        .is_err());
    assert_eq!(old.records[0].locator.key.id, input.id);
    let PreparedInputEvidence::Message(retained) = &old.records[0].record else {
        panic!()
    };
    assert_eq!(retained.body, input.body);
    assert!(db
        .transaction(|tx| {
            let actual = append_message_tx(tx, &message("present"))?.0;
            register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[CanonicalInputEvidence::Message(actual)],
            )?;
            register_pruned_input_evidence_tx(tx, &scope(), &[Key::new(Family::Message, "present")])
        })
        .is_err());
    Ok(())
}
#[test]
fn coherent_read_transaction_preserves_old_alias_and_canonical_payload_together() -> Result<()> {
    let (_directory, db) = fresh()?;
    let old = TurnRecord::new("agent-a", "turn-a", 1);
    db.transaction(|tx| {
        upsert_turn_record_tx(tx, &old)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(old.clone())],
        )
    })?;
    let mut reader = db.connection()?;
    let tx = reader.transaction()?;
    let first = current(load_input_evidence_snapshot_tx(
        &tx,
        &scope(),
        &request(InputWindowSelector::GlobalTurns, 1),
    )?);
    let mut next = old.clone();
    next.waiting_condition_ids.push("wait-a".into());
    db.transaction(|writer| {
        upsert_turn_record_tx(writer, &next)?;
        register_committed_input_evidence_tx(
            writer,
            &scope(),
            &[CanonicalInputEvidence::Turn(next.clone())],
        )
    })?;
    let second = current(load_input_evidence_snapshot_tx(
        &tx,
        &scope(),
        &request(InputWindowSelector::GlobalTurns, 1),
    )?);
    assert_eq!(
        first.resident.capture()?.records[0].locator,
        second.resident.capture()?.records[0].locator
    );
    tx.commit()?;
    let fresh = current(db.transaction(|tx| {
        load_input_evidence_snapshot_tx(tx, &scope(), &request(InputWindowSelector::GlobalTurns, 1))
    })?);
    assert_ne!(
        fresh.resident.capture()?.records[0].locator,
        first.resident.capture()?.records[0].locator
    );
    Ok(())
}

#[test]
fn selected_turn_and_message_closure_materialize_frozen_full_transcript() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        let input = message("accepted-old-turn");
        append_message_tx(tx, &input)?;
        for index in 1..=2 {
            let mut turn = TurnRecord::new("agent-a", format!("turn-{index}"), index);
            if index == 1 {
                turn.input_message_ids.push(input.id.clone());
            }
            if index == 2 {
                turn.replay = Some(crate::types::TurnReplayProvenance {
                    source_message_id: input.id.clone(),
                    source_turn_id: "turn-1".into(),
                    reason: "retry".into(),
                    prior_terminal: None,
                });
            }
            upsert_turn_record_tx(tx, &turn)?;
            let mut entry = TranscriptEntry::new(
                "agent-a",
                TranscriptEntryKind::AssistantRound,
                None,
                None,
                serde_json::json!({"turn_id":format!("turn-{index}"),"text":format!("trace-{index}")}),
            );
            entry.id = format!("trace-{index}");
            entry.created_at = time();
            append_transcript_entry_tx(tx, &entry)?;
        }
        Ok(())
    })?;
    // Replay closes over the source message's immutable actual assignment,
    // rather than granting a body from its preallocation hint.
    let replay_capture = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 1),
        )
    })?)
    .resident
    .capture()?;
    assert!(replay_capture
        .lookup(&Key::new(Family::Turn, "turn-1"))?
        .is_some());
    assert!(replay_capture
        .lookup(&Key::new(Family::Message, "accepted-old-turn"))?
        .is_some());
    assert_eq!(
        replay_capture
            .transcripts_for_turns(&["turn-1".into()])?
            .len(),
        1
    );
    let mut policy = request(InputWindowSelector::GlobalTurns, 1);
    policy
        .mandatory_roots
        .insert(Key::new(Family::Message, "accepted-old-turn"));
    let prepared =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &policy))?);
    let captured = prepared.resident.capture()?;
    let ids = captured
        .records
        .iter()
        .map(|x| x.locator.key.id.as_str())
        .collect::<BTreeSet<_>>();
    assert!(ids.contains("turn-1") && ids.contains("turn-2"));
    assert!(ids.contains("trace-1") && ids.contains("trace-2"));
    let read =
        current(db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &policy))?);
    assert_eq!(read.resident.capture()?.records, captured.records);
    let old = captured;
    let delta = db.transaction(|tx| {
        tx.execute(
            "DELETE FROM transcript_entries WHERE evidence_id='trace-1'",
            [],
        )?;
        register_pruned_input_evidence_tx(tx, &scope(), &[Key::new(Family::Transcript, "trace-1")])
    })?;
    assert!(prepared
        .resident
        .with_committed(&delta, policy.mandatory_roots.clone())?
        .is_none());
    assert!(db
        .transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &policy))
        .is_err());
    assert!(old.records.iter().any(|x| x.locator.key.id == "trace-1"));
    Ok(())
}

#[test]
fn qualification_window_accumulator_rejects_before_collecting_unbounded_owners() -> Result<()> {
    let mut selected = BTreeSet::new();
    let mut bytes = 0;
    for owner in 0..128 {
        extend_metadata_selection(
            &mut selected,
            &mut bytes,
            (0..512).map(|turn| Key::new(Family::Turn, format!("owner-{owner}-turn-{turn}"))),
        )?;
    }
    assert_eq!(selected.len(), MAX_RECORDS);
    let error = extend_metadata_selection(
        &mut selected,
        &mut bytes,
        (0..512).map(|turn| Key::new(Family::Turn, format!("owner-129-turn-{turn}"))),
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ResourceExhausted(_))
    ));
    assert_eq!(selected.len(), MAX_RECORDS);
    Ok(())
}

#[test]
fn command_preview_is_safe_and_writer_cold_projection_match() -> Result<()> {
    for command in [
        "curl -H 'Authorization: Bearer secret-value' https://example.invalid".to_string(),
        "cat <<EOF\nsecret body\nEOF".into(),
        "x".repeat(MAX_COMMAND_PREVIEW_INPUT_BYTES + 1),
    ] {
        let mut tool = tool();
        tool.input = serde_json::json!({"cmd":command});
        let (_directory, db) = legacy(|tx| insert_tool_evidence_tx(tx, &tool))?;
        let cold = current(db.transaction(|tx| {
            prepare_input_evidence_snapshot_tx(
                tx,
                &scope(),
                &request(InputWindowSelector::RecentTools, 1),
            )
        })?)
        .resident
        .capture()?;
        let expected = project_tool_record(&tool)?;
        let Some(PreparedInputEvidence::ToolExecution(actual)) = cold
            .lookup(&Key::new(Family::ToolExecution, "tool-a"))?
            .map(|r| &r.record)
        else {
            panic!("missing Tool projection")
        };
        assert_eq!(actual, &expected);
        let rendered = actual.command.command_preview.as_ref().unwrap();
        assert!(!rendered.contains("secret-value"));
        if command.len() > MAX_COMMAND_PREVIEW_INPUT_BYTES {
            assert_eq!(rendered, OVERSIZED_COMMAND_PREVIEW);
        }
    }
    Ok(())
}

#[test]
fn qualified_related_projection_refresh_uses_original_transaction() -> Result<()> {
    let (_directory, db) = fresh()?;
    let input = db.transaction(|tx| {
        let actual = append_message_tx(tx, &message("message-a"))?.0;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(actual.clone())],
        )?;
        Ok(actual)
    })?;
    let mut turn = TurnRecord::new("agent-a", "turn-a", 1);
    turn.input_message_ids.push(input.id.clone());
    let delta = db.transaction(|tx| {
        upsert_turn_record_tx(tx, &turn)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(turn.clone())],
        )
    })?;
    assert_eq!(
        delta.message_turn_keys.get(&input.id),
        Some(&Some("turn-a".into()))
    );
    assert!(delta
        .records
        .iter()
        .any(|r| r.locator.key.family == Family::Message && r.locator.revision == 2));
    let policy = request(InputWindowSelector::GlobalTurns, 1);
    let before =
        current(db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &policy))?);
    let mut trace = TranscriptEntry::new(
        "agent-a",
        TranscriptEntryKind::AssistantRound,
        None,
        None,
        serde_json::json!({"turn_id":"turn-a"}),
    );
    trace.id = "trace-a".into();
    trace.created_at = time();
    let delta = db.transaction(|tx| {
        let actual = append_transcript_entry_tx(tx, &trace)?.0;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Transcript(actual)],
        )
    })?;
    // The new child expands the Turn's closure. Its unchanged canonical body
    // keeps the same qualification version despite a display revision bump.
    assert!(delta
        .records
        .iter()
        .any(|r| r.locator.key.family == Family::Turn && r.locator.revision == 1));
    let after = before
        .resident
        .with_committed(&delta, BTreeSet::new())?
        .unwrap()
        .capture()?;
    assert_eq!(after.transcripts_for_turns(&["turn-a".into()])?.len(), 1);
    assert_eq!(
        current(db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &policy))?)
            .resident
            .capture()?
            .records,
        after.records
    );
    Ok(())
}

#[test]
fn pending_preallocated_task_result_follows_typed_reply_but_not_turn_hint() -> Result<()> {
    use crate::types::{AdmissionContext, MessageDeliverySurface};
    let mut reply = message("reply-original");
    reply.kind = MessageKind::InternalFollowup;
    let mut result = message("message:agent-message-wait:task-a");
    result.kind = MessageKind::TaskResult;
    result.origin = MessageOrigin::Task {
        task_id: "task-a".into(),
    };
    result.authority_class = AuthorityClass::RuntimeInstruction;
    result.admission_context = Some(AdmissionContext::RuntimeOwned);
    result.delivery_surface = Some(MessageDeliverySurface::TaskRejoin);
    result.task_id = Some("task-a".into());
    result.turn_id = Some("turn:agent-message-wait:task-a".into());
    result.metadata = Some(
        serde_json::json!({"task_id":"task-a","task_kind":crate::types::AGENT_MESSAGE_WAIT_TASK_KIND,
        "task_status":"completed","task_detail":{"reply_content_source":"original_message","message_id":"reply-original"}}),
    );
    let (_directory, db) = fresh()?;
    db.transaction(|tx| {
        let original = append_message_tx(tx, &reply)?.0;
        let actual = append_message_tx(tx, &result)?.0;
        crate::runtime_db::repositories::upsert_queue_entry_tx(
            tx,
            &crate::types::QueueEntryRecord {
                message_id: actual.id.clone(),
                agent_id: actual.agent_id.clone(),
                priority: actual.priority.clone(),
                status: crate::types::QueueEntryStatus::Queued,
                created_at: actual.created_at,
                updated_at: actual.created_at,
            },
        )?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[
                CanonicalInputEvidence::Message(actual.clone()),
                CanonicalInputEvidence::Message(original),
            ],
        )?;
        assert_eq!(
            task_reply_reference_on(tx, "agent-a", &actual.id)?,
            Some(reply.id.clone())
        );
        Ok(())
    })?;
    // An accepted queued input with an unassigned preallocated Turn hint.
    let mut policy = request(InputWindowSelector::GlobalTurns, 1);
    policy
        .mandatory_roots
        .insert(Key::new(Family::Message, &result.id));
    let captured =
        current(db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &policy))?)
            .resident
            .capture()?;
    assert!(captured
        .lookup(&Key::new(Family::Message, &reply.id))?
        .is_some());
    assert_eq!(captured.message_turn_keys.get(&result.id), Some(&None));
    assert!(captured
        .lookup(&Key::new(Family::Turn, result.turn_id.as_ref().unwrap()))
        .is_err());
    let mut untrusted = result.clone();
    untrusted.admission_context = None;
    assert!(!PreparedInputEvidence::Message(untrusted)
        .references()
        .contains(&Key::new(Family::Message, &reply.id)));
    Ok(())
}

#[test]
fn trigger_only_turn_closes_over_its_canonical_message() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        let input = append_message_tx(tx, &message("trigger-only-message"))?.0;
        let mut turn = TurnRecord::new("agent-a", "trigger-only-turn", 1);
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&input));
        assert!(turn.input_message_ids.is_empty());
        upsert_turn_record_tx(tx, &turn)
    })?;
    let captured = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 1),
        )
    })?)
    .resident
    .capture()?;
    assert!(captured
        .lookup(&Key::new(Family::Message, "trigger-only-message"))?
        .is_some());
    assert_eq!(
        captured.message_turn_keys.get("trigger-only-message"),
        Some(&None)
    );
    Ok(())
}

#[test]
fn actual_related_writer_refreshes_fixed_unmaterialized_member_but_not_fresh_history() -> Result<()>
{
    for adopted in [true, false] {
        let setup = |tx: &Transaction<'_>| {
            upsert_turn_record_tx(tx, &TurnRecord::new("agent-a", "old-turn", 1))
        };
        let (_directory, db) = if adopted {
            legacy(setup)?
        } else {
            let pair = fresh()?;
            pair.1.transaction(setup)?;
            pair
        };
        let mut entry = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::AssistantRound,
            None,
            None,
            serde_json::json!({"turn_id":"old-turn"}),
        );
        entry.id = "new-transcript".into();
        let delta = db.transaction(|tx| {
            let actual = append_transcript_entry_tx(tx, &entry)?.0;
            register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[CanonicalInputEvidence::Transcript(actual)],
            )
        })?;
        assert_eq!(
            delta
                .records
                .iter()
                .any(|r| r.locator.key == Key::new(Family::Turn, "old-turn")),
            adopted
        );
        let captured = current(db.transaction(|tx| {
            prepare_input_evidence_snapshot_tx(
                tx,
                &scope(),
                &request(InputWindowSelector::GlobalTurns, 1),
            )
        })?)
        .resident
        .capture()?;
        let refreshed = db.transaction(|tx| {
            refresh_related_input_evidence_tx(tx, &scope(), &[Key::new(Family::Turn, "old-turn")])
        })?;
        assert_eq!(
            refreshed
                .records
                .iter()
                .any(|r| r.locator.key == Key::new(Family::Turn, "old-turn")),
            adopted
        );
        if adopted {
            assert!(captured
                .lookup(&Key::new(Family::Turn, "old-turn"))?
                .is_some());
            assert_eq!(
                captured.transcripts_for_turns(&["old-turn".into()])?.len(),
                1
            );
        } else {
            assert!(captured.records.is_empty());
        }
    }
    Ok(())
}

fn claim_only_attempt(db: &RuntimeDb) -> Result<ExecutionAttempt> {
    use crate::domain::execution_protocol::*;
    db.transaction(|tx| {
        let mut source = message("claim-source");
        source.turn_id = Some("claim-turn".into());
        let (source, _) = append_message_tx(tx, &source)?;
        let mut state = AgentState::new("agent-a");
        state.status = crate::types::AgentStatus::AwakeRunning;
        state.current_run_id = Some("claim-run".into());
        upsert_agent_state_tx(tx, &state)?;
        crate::runtime_db::repositories::upsert_queue_entry_tx(
            tx,
            &crate::types::QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: "agent-a".into(),
                priority: Priority::Normal,
                status: crate::types::QueueEntryStatus::Dequeued,
                created_at: time(),
                updated_at: time(),
            },
        )?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(source.clone())],
        )?;
        let attempt = ExecutionAttempt {
            attempt_id: "claim-attempt".into(),
            agent_id: "agent-a".into(),
            source_message_id: Some(source.id.clone()),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::QueueMessage {
                    message_id: source.id.clone(),
                },
                generation: source.message_seq.unwrap(),
            },
            binding: ExecutionBinding::Conversation {
                interaction_id: source.id,
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::Operator,
                trust: ExecutionTrust::OperatorInstruction,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: source.message_seq.unwrap(),
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            turn_id: Some("claim-turn".into()),
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: time().to_rfc3339(),
            terminal_at: None,
        };
        store_attempt(tx, &attempt)?;
        let delta = register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Attempt(attempt.clone())],
        )?;
        let observed = delta
            .records
            .iter()
            .find(|r| r.locator.key.family == Family::Attempt)
            .unwrap();
        assert_eq!(observed.attempt_turn_keys, Some(BTreeSet::new()));
        Ok(attempt)
    })
}
fn store_attempt(tx: &Transaction<'_>, attempt: &ExecutionAttempt) -> Result<()> {
    tx.execute("INSERT INTO execution_protocol_partitions(agent_id,updated_at) VALUES(?1,?2) ON CONFLICT(agent_id) DO NOTHING", params![attempt.agent_id, time().to_rfc3339()])?;
    tx.execute("INSERT INTO execution_protocol_attempts(agent_id,attempt_id,lifecycle_state,source_identity_json,source_generation,recovery_of_attempt_id,terminal_outcome_id,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(agent_id,attempt_id) DO UPDATE SET lifecycle_state=excluded.lifecycle_state,terminal_outcome_id=excluded.terminal_outcome_id,payload_json=excluded.payload_json",
        params![attempt.agent_id,attempt.attempt_id,serde_json::to_value(attempt.state)?.as_str().unwrap(),serde_json::to_string(&attempt.source.identity)?,i64::try_from(attempt.source.generation)?,attempt.recovery_of_attempt_id,attempt.terminal_outcome_id,serde_json::to_string(attempt)?])?;
    Ok(())
}
fn interrupt_claim(db: &RuntimeDb, mut attempt: ExecutionAttempt) -> Result<()> {
    use crate::domain::execution_protocol::{
        ConversationOutcome, ExecutionOutcome, ExecutionOutcomeRecord,
    };
    db.transaction(|tx| {
        let outcome = ExecutionOutcomeRecord {
            outcome_id: "claim-interruption".into(), attempt_id: attempt.attempt_id.clone(),
            outcome: ExecutionOutcome::Conversation(ConversationOutcome::Interrupted { reason: "fixture interruption".into() }),
            created_at: time().to_rfc3339(),
        };
        attempt.state = crate::domain::execution_protocol::ExecutionAttemptState::Interrupted;
        attempt.terminal_at = Some(time().to_rfc3339());
        attempt.terminal_outcome_id = Some(outcome.outcome_id.clone());
        store_attempt(tx, &attempt)?;
        tx.execute("INSERT INTO execution_protocol_outcomes(agent_id,outcome_id,attempt_id,payload_json) VALUES(?1,?2,?3,?4)",
            params![attempt.agent_id, outcome.outcome_id, outcome.attempt_id, serde_json::to_string(&outcome)?])?;
        let mut state = AgentState::new("agent-a");
        state.status = crate::types::AgentStatus::Stopped;
        state.pending = 1;
        upsert_agent_state_tx(tx, &state)?;
        crate::runtime_db::repositories::upsert_queue_entry_tx(tx, &crate::types::QueueEntryRecord {
            message_id: "claim-source".into(), agent_id: "agent-a".into(), priority: Priority::Normal,
            status: crate::types::QueueEntryStatus::Interrupted, created_at: time(), updated_at: time(),
        })?;
        let source_key = Key::new(Family::Message, "claim-source");
        let source = decode_canonical(&source_key, &payload_on(tx, "agent-a", &source_key)?.context("fixture source")?)?;
        register_committed_input_evidence_tx(tx, &scope(), &[
            source, CanonicalInputEvidence::Attempt(attempt.clone()), CanonicalInputEvidence::Outcome(outcome),
        ])?;
        Ok(())
    })
}
#[test]
fn claim_only_interruption_and_restart_keep_verified_no_turn() -> Result<()> {
    let (_dir, db) = fresh()?;
    let attempt = claim_only_attempt(&db)?;
    interrupt_claim(&db, attempt)?;
    db.transaction(|tx| {
        let current = current(load_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 12),
        )?);
        let capture = current.resident.capture()?;
        let record = capture
            .records
            .iter()
            .find(|r| r.locator.key.family == Family::Attempt)
            .unwrap();
        assert_eq!(record.attempt_turn_keys, Some(BTreeSet::new()));
        assert!(!record
            .references()
            .contains(&Key::new(Family::Turn, "claim-turn")));
        Ok(())
    })
}
#[test]
fn begin_turn_promotes_claim_metadata_before_interruption() -> Result<()> {
    let (_dir, db) = fresh()?;
    let attempt = claim_only_attempt(&db)?;
    db.transaction(|tx| {
        let mut turn = TurnRecord::new("agent-a", "claim-turn", 1);
        turn.input_message_ids.push("claim-source".into());
        turn.run_id = Some("claim-run".into());
        upsert_turn_record_tx(tx, &turn)?;
        let delta = register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(turn)],
        )?;
        let record = delta
            .records
            .iter()
            .find(|r| r.locator.key.family == Family::Attempt)
            .unwrap();
        assert!(record
            .attempt_turn_keys
            .as_ref()
            .unwrap()
            .contains(&Key::new(Family::Turn, "claim-turn")));
        Ok(())
    })?;
    interrupt_claim(&db, attempt)?;
    db.transaction(|tx| {
        let current = current(load_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 12),
        )?);
        assert!(current
            .resident
            .capture()?
            .records
            .iter()
            .any(|r| r.locator.key == Key::new(Family::Turn, "claim-turn")));
        Ok(())
    })
}
#[test]
fn actual_assignment_with_missing_turn_cannot_reuse_claim_only_proof() -> Result<()> {
    let (_dir, db) = fresh()?;
    let _attempt = claim_only_attempt(&db)?;
    db.connection()?.execute("INSERT INTO conversation_input_assignments(message_id,agent_id,turn_id,revision,assigned_at) VALUES('claim-source','agent-a','missing-started-turn',1,?1)",[time().to_rfc3339()])?;
    assert!(db
        .transaction(|tx| load_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 12)
        ))
        .is_err());
    Ok(())
}
#[test]
fn successor_cannot_reuse_original_incarnation_claim_only_proof() -> Result<()> {
    let (_dir, db) = fresh()?;
    let attempt = claim_only_attempt(&db)?;
    db.transaction(|tx| {
        let mut successor = ContextDurableRead::new(tx, "agent-a")
            .scope_facts()?
            .identity
            .context("original fixture identity missing")?;
        successor.incarnation = 2;
        successor.updated_at += chrono::Duration::days(1);
        upsert_agent_identity_tx(tx, &successor)?;
        assert_eq!(
            ContextDurableRead::new(tx, "agent-a")
                .scope_facts()?
                .identity
                .unwrap()
                .incarnation,
            2
        );
        let next = InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: 2,
        };
        assert!(attempt_turn::for_current(tx, &next, &binding_on(tx, &next)?, &attempt).is_err());
        Ok(())
    })?;
    let count: i64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE identity_incarnation=2",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    Ok(())
}
#[test]
fn legacy_fixed_boundary_freezes_claim_only_relation() -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        // Reuse the canonical fixture through an explicit fixed-boundary setup:
        // absence of a materialized Turn is qualified by the actual Open claim.
        let mut source = message("legacy-claim");
        source.turn_id = Some("legacy-hint".into());
        let (source, _) = append_message_tx(tx, &source)?;
        let mut state = AgentState::new("agent-a");
        state.status = crate::types::AgentStatus::AwakeRunning;
        state.current_run_id = Some("legacy-run".into());
        upsert_agent_state_tx(tx, &state)?;
        crate::runtime_db::repositories::upsert_queue_entry_tx(
            tx,
            &crate::types::QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: "agent-a".into(),
                priority: Priority::Normal,
                status: crate::types::QueueEntryStatus::Dequeued,
                created_at: time(),
                updated_at: time(),
            },
        )?;
        use crate::domain::execution_protocol::*;
        store_attempt(
            tx,
            &ExecutionAttempt {
                attempt_id: "legacy-attempt".into(),
                agent_id: "agent-a".into(),
                source_message_id: Some(source.id.clone()),
                source: ExecutionSource {
                    identity: ExecutionSourceIdentity::QueueMessage {
                        message_id: source.id.clone(),
                    },
                    generation: source.message_seq.unwrap(),
                },
                binding: ExecutionBinding::Conversation {
                    interaction_id: source.id,
                },
                provenance: ExecutionProvenance {
                    origin: ExecutionOrigin::Operator,
                    trust: ExecutionTrust::OperatorInstruction,
                    priority: ExecutionPriority::Normal,
                    correlation_id: None,
                    causation_id: None,
                },
                admitted_fences: AdmittedFences {
                    source_revision: source.message_seq.unwrap(),
                    work_item_source_revision: None,
                    work_item_generation: None,
                    rejoin: None,
                    agent_control_revision: 1,
                    host_registry_revision: 1,
                },
                state: ExecutionAttemptState::Open,
                run_id: None,
                turn_id: Some("legacy-hint".into()),
                recovery_of_attempt_id: None,
                terminal_outcome_id: None,
                admitted_at: time().to_rfc3339(),
                terminal_at: None,
            },
        )
    })?;
    db.transaction(|tx| {
        let current = current(prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 12),
        )?);
        assert!(current
            .resident
            .capture()?
            .records
            .iter()
            .any(
                |r| r.locator.key == Key::new(Family::Attempt, "legacy-attempt")
                    && r.attempt_turn_keys == Some(BTreeSet::new())
            ));
        Ok(())
    })
}

#[test]
fn explicit_absent_execution_work_survives_snapshot_and_request_capture() -> Result<()> {
    use crate::runtime::resident_input_evidence::ObservedWorkExecution;

    let (_directory, db) = fresh()?;
    let work_key = Key::new(Family::WorkExecution, "not-yet-registered-work");
    db.transaction(|tx| {
        let mut source = message("accepted-source-with-new-work");
        source.work_item_id = Some(work_key.id.clone());
        let (saved, inserted) = append_message_tx(tx, &source)?;
        assert!(inserted);
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(saved)],
        )?;
        Ok(())
    })?;
    // No Message or Work body is selected: only the exact negative root proves
    // that RegisterWorkItem is permitted, so both trim paths must retain it.
    let wanted = InputEvidenceLoadRequest {
        query: None,
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: Default::default(),
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([work_key.clone()]),
        record_limit: 16,
        byte_limit: 16 * 1024,
    };
    let snapshot =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted))?);
    assert!(matches!(
        snapshot
            .resident
            .capture()?
            .execution_work_item(&work_key.id)?,
        ObservedWorkExecution::Absent
    ));
    let captured = snapshot.resident.capture_for_request(&wanted)?;
    assert!(captured.records.is_empty());
    assert!(captured.unavailable.contains(&work_key));
    drop(snapshot);
    assert!(matches!(
        captured.execution_work_item(&work_key.id)?,
        ObservedWorkExecution::Absent
    ));
    assert!(captured.execution_work_item("unobserved-work").is_err());
    Ok(())
}

#[test]
fn explicit_interrupted_source_request_keeps_all_candidates_and_rejects_missing_body() -> Result<()>
{
    use crate::runtime::resident_input_evidence::InputEvidenceReadError;

    let (_directory, db) = fresh()?;
    let original = claim_only_attempt(&db)?;
    interrupt_claim(&db, original.clone())?;
    let mut second = original;
    second.attempt_id = "second-claim-attempt".into();
    second.admitted_at = (time() + chrono::Duration::seconds(1)).to_rfc3339();
    db.transaction(|tx| {
        let mut state = AgentState::new("agent-a");
        state.status = crate::types::AgentStatus::AwakeRunning;
        state.current_run_id = Some("claim-run".into());
        upsert_agent_state_tx(tx, &state)?;
        crate::runtime_db::repositories::upsert_queue_entry_tx(
            tx,
            &crate::types::QueueEntryRecord {
                message_id: "claim-source".into(),
                agent_id: "agent-a".into(),
                priority: Priority::Normal,
                status: crate::types::QueueEntryStatus::Dequeued,
                created_at: time(),
                updated_at: time(),
            },
        )?;
        store_attempt(tx, &second)?;
        // Queue consumption advances this Message's canonical projection too.
        let source_key = Key::new(Family::Message, "claim-source");
        let source = decode_canonical(
            &source_key,
            &payload_on(tx, "agent-a", &source_key)?.context("fixture claim source")?,
        )?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[source, CanonicalInputEvidence::Attempt(second.clone())],
        )?;
        Ok(())
    })?;
    db.transaction(|tx| {
        use crate::domain::execution_protocol::{ConversationOutcome, ExecutionOutcome, ExecutionOutcomeRecord};
        let outcome = ExecutionOutcomeRecord {
            outcome_id: "second-interruption".into(), attempt_id: second.attempt_id.clone(),
            outcome: ExecutionOutcome::Conversation(ConversationOutcome::Interrupted { reason: "second interruption".into() }),
            created_at: time().to_rfc3339(),
        };
        second.state = crate::domain::execution_protocol::ExecutionAttemptState::Interrupted;
        second.terminal_at = Some(time().to_rfc3339());
        second.terminal_outcome_id = Some(outcome.outcome_id.clone());
        store_attempt(tx, &second)?;
        tx.execute("INSERT INTO execution_protocol_outcomes(agent_id,outcome_id,attempt_id,payload_json) VALUES(?1,?2,?3,?4)",
            params![second.agent_id, outcome.outcome_id, outcome.attempt_id, serde_json::to_string(&outcome)?])?;
        register_committed_input_evidence_tx(tx, &scope(), &[
            CanonicalInputEvidence::Attempt(second.clone()), CanonicalInputEvidence::Outcome(outcome),
        ])?;
        Ok(())
    })?;
    let wanted = InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([Key::new(Family::Message, "claim-source")]),
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: BTreeSet::from(["claim-source".into()]),
        record_limit: 32,
        byte_limit: 256 * 1024,
    };
    let snapshot =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted))?);
    let captured = snapshot.resident.capture_for_request(&wanted)?;
    assert_eq!(
        captured
            .execution_interrupted_attempts_for_source("claim-source")?
            .len(),
        2
    );
    assert!(captured
        .lookup(&Key::new(Family::Attempt, "claim-attempt"))?
        .is_some());
    assert!(captured
        .lookup(&Key::new(Family::Attempt, "second-claim-attempt"))?
        .is_some());

    // Model a selected partial domain with the same complete metadata. Without
    // an explicit source request, only the newest attempt is a required body.
    let partial = ResidentInputEvidence::bootstrap(
        scope(),
        captured
            .records
            .iter()
            .filter(|record| record.locator.key != Key::new(Family::Attempt, "claim-attempt"))
            .cloned()
            .collect(),
        InputEvidenceCoverage {
            selected_completion_brief_ids: Default::default(),
            completion_source_observations: Default::default(),
            recovery_source_absences: Default::default(),
            recovery_lineage_messages: Default::default(),
            execution: captured.execution.clone(),
            windows: Vec::new(),
            mandatory_roots: wanted.mandatory_roots.clone(),
            turn_transcript_keys: captured.turn_transcript_keys.clone(),
            message_turn_keys: captured.message_turn_keys.as_ref().clone(),
            unavailable: BTreeSet::new(),
            record_limit: 32,
            byte_limit: 256 * 1024,
        },
    )?;
    let error = partial.capture_for_request(&wanted).unwrap_err();
    assert!(
        matches!(error.downcast_ref::<InputEvidenceReadError>(),
        Some(InputEvidenceReadError::NeedsPreparation(key)) if key == &Key::new(Family::Attempt, "claim-attempt")),
        "{error:#}"
    );
    let mut unobserved = wanted;
    unobserved.interrupted_sources = BTreeSet::from(["unprepared-source".into()]);
    assert!(partial.capture_for_request(&unobserved).is_err());
    Ok(())
}

fn provider_lineage_recovery(id: &str, source: &str, turn: &str) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "agent-a",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "model_lineage_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "continue provider input".into(),
        },
    )
    .with_admission(
        crate::types::MessageDeliverySurface::RuntimeSystem,
        crate::types::AdmissionContext::RuntimeOwned,
    );
    message.id = id.into();
    message.created_at = time();
    message.causation_id = Some(source.into());
    message
        .source_refs
        .insert("source_message_id".into(), source.into());
    message
        .source_refs
        .insert("source_turn_id".into(), turn.into());
    message.metadata = Some(serde_json::json!({"provider_recovery": {
        "fallback_model_ref":"anthropic/claude-sonnet-4-6", "source_message_id":source,
        "source_turn_id":turn, "source_terminal_kind":"deferred_to_fallback", "source_round":1
    }}));
    message
}

fn install_provider_lineage(tx: &Transaction<'_>) -> Result<MessageEnvelope> {
    use crate::domain::execution_protocol::*;
    let mut first = message("lineage-original");
    first.turn_id = Some("lineage-turn-0".into());
    let mut recovery = provider_lineage_recovery("lineage-recovery-1", &first.id, "lineage-turn-0");
    recovery.turn_id = Some("lineage-turn-1".into());
    let second = provider_lineage_recovery("lineage-recovery-2", &recovery.id, "lineage-turn-1");
    let messages = [first, recovery, second]
        .into_iter()
        .map(|message| append_message_tx(tx, &message).map(|(stored, _)| stored))
        .collect::<Result<Vec<_>>>()?;
    let mut evidence = messages
        .iter()
        .cloned()
        .map(CanonicalInputEvidence::Message)
        .collect::<Vec<_>>();
    for (index, source) in messages.iter().take(2).enumerate() {
        let turn_id = format!("lineage-turn-{index}");
        let mut turn = TurnRecord::new("agent-a", &turn_id, index as u64 + 1);
        turn.owner = Some(TurnOwner::AgentLifecycle {
            agent_id: "agent-a".into(),
        });
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(source));
        turn.input_message_ids = vec![source.id.clone()];
        turn.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::DeferredToFallback,
            reason: None,
            no_brief_reason: None,
            completed_at: time(),
            duration_ms: 1,
        });
        upsert_turn_record_tx(tx, &turn)?;
        let attempt_id = format!("lineage-attempt-{index}");
        let outcome_id = format!("lineage-outcome-{index}");
        let attempt = ExecutionAttempt {
            attempt_id: attempt_id.clone(),
            agent_id: "agent-a".into(),
            source_message_id: Some(source.id.clone()),
            source: ExecutionSource {
                identity: if index == 0 {
                    ExecutionSourceIdentity::QueueMessage {
                        message_id: source.id.clone(),
                    }
                } else {
                    ExecutionSourceIdentity::RuntimeRecovery {
                        recovery_id: source.id.clone(),
                    }
                },
                generation: source.message_seq.unwrap(),
            },
            binding: ExecutionBinding::AgentLifecycle {
                agent_id: "agent-a".into(),
            },
            provenance: ExecutionProvenance {
                origin: if index == 0 {
                    ExecutionOrigin::Operator
                } else {
                    ExecutionOrigin::RuntimeRecovery
                },
                trust: if index == 0 {
                    ExecutionTrust::OperatorInstruction
                } else {
                    ExecutionTrust::RuntimeInstruction
                },
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: source.causation_id.clone(),
            },
            admitted_fences: AdmittedFences {
                source_revision: source.message_seq.unwrap(),
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Interrupted,
            run_id: None,
            turn_id: Some(turn_id),
            recovery_of_attempt_id: (index == 1).then(|| "lineage-attempt-0".into()),
            terminal_outcome_id: Some(outcome_id.clone()),
            admitted_at: time().to_rfc3339(),
            terminal_at: Some(time().to_rfc3339()),
        };
        let outcome = ExecutionOutcomeRecord {
            outcome_id,
            attempt_id,
            outcome: ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                reason: "provider failed".into(),
            }),
            created_at: time().to_rfc3339(),
        };
        store_attempt(tx, &attempt)?;
        tx.execute("INSERT INTO execution_protocol_outcomes(agent_id,outcome_id,attempt_id,payload_json) VALUES(?1,?2,?3,?4)",
                params![attempt.agent_id, outcome.outcome_id, outcome.attempt_id, serde_json::to_string(&outcome)?])?;
        evidence.extend([
            CanonicalInputEvidence::Turn(
                ContextDurableRead::new(tx, "agent-a")
                    .turn_by_id(&turn.turn_id)?
                    .context("fixture Turn")?,
            ),
            CanonicalInputEvidence::Attempt(attempt),
            CanonicalInputEvidence::Outcome(outcome),
        ]);
    }
    register_committed_input_evidence_tx(tx, &scope(), &evidence)?;
    Ok(messages[2].clone())
}

fn provider_lineage_fixture() -> Result<(tempfile::TempDir, RuntimeDb, MessageEnvelope)> {
    let (directory, db) = fresh()?;
    let latest = db.transaction(install_provider_lineage)?;
    Ok((directory, db, latest))
}

fn provider_lineage_request(latest: &MessageEnvelope) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([Key::new(Family::Message, &latest.id)]),
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: BTreeSet::from([latest.id.clone()]),
        record_limit: 64,
        byte_limit: 1024 * 1024,
    }
}

#[test]
fn cold_multihop_provider_lineage_prepares_complete_sources_without_history_windows() -> Result<()>
{
    let (_directory, db, latest) = provider_lineage_fixture()?;
    let wanted = provider_lineage_request(&latest);
    // No old resident/previous request is carried into this fresh SQLite view.
    let snapshot =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted))?);
    let captured = snapshot.resident.capture()?;
    assert!(captured.execution.sources.contains_key("lineage-original"));
    assert!(captured
        .execution
        .sources
        .contains_key("lineage-recovery-1"));
    assert!(captured
        .lookup(&Key::new(Family::Attempt, "lineage-attempt-0"))?
        .is_some());
    let protocol = captured
        .execution_state_if_initialized()?
        .context("fixture protocol")?;
    let resolved =
        crate::runtime::provider_recovery::resolve_from_captured(&captured, &latest, &protocol)?;
    assert_eq!(resolved.predecessor.attempt_id, "lineage-attempt-1");
    assert_eq!(resolved.root_message.id, "lineage-original");
    // Reading an already-prepared namespace also follows the same finite graph.
    let loaded =
        current(db.transaction(|tx| load_input_evidence_snapshot_tx(tx, &scope(), &wanted))?);
    assert!(loaded
        .resident
        .capture()?
        .execution
        .sources
        .contains_key("lineage-original"));
    Ok(())
}

#[test]
fn multihop_provider_lineage_missing_body_is_not_an_empty_candidate_set() -> Result<()> {
    let (_directory, db, latest) = provider_lineage_fixture()?;
    db.connection()?.execute(
        "DELETE FROM execution_protocol_outcomes WHERE outcome_id='lineage-outcome-0'",
        [],
    )?;
    let error = db
        .transaction(|tx| {
            prepare_input_evidence_snapshot_tx(tx, &scope(), &provider_lineage_request(&latest))
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::NeedsPreparation(_))
        ),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn multihop_provider_lineage_requires_actual_assignment_and_does_not_adopt_old_body() -> Result<()>
{
    let (_directory, db, latest) = provider_lineage_fixture()?;
    db.connection()?.execute(
        "DELETE FROM conversation_input_assignments WHERE message_id='lineage-original'",
        [],
    )?;
    assert!(db
        .transaction(|tx| prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &provider_lineage_request(&latest)
        ))
        .is_err());
    let (_directory, db, latest) = provider_lineage_fixture()?;
    db.connection()?.execute("DELETE FROM input_evidence_current WHERE family='message' AND record_id='lineage-original'", [])?;
    assert!(db
        .transaction(|tx| prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &provider_lineage_request(&latest)
        ))
        .is_err());
    let adopted: bool = db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE family='message' AND record_id='lineage-original')", [], |row| row.get(0))?;
    assert!(!adopted);
    Ok(())
}

#[test]
fn malformed_provider_lineage_provenance_never_names_hydration_roots() -> Result<()> {
    let (_directory, db, mut latest) = provider_lineage_fixture()?;
    latest.source_refs.insert(
        "source_message_id".into(),
        "unqualified-unrelated-history".into(),
    );
    db.transaction(|tx| {
        // Keep canonical normalized fields/hash/revisions valid; only provider
        // provenance is malformed, so hydration must not follow the forged ref.
        crate::runtime_db::evidence::upsert_message_tx(tx, &latest)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(latest.clone())],
        )?;
        Ok(())
    })?;
    let snapshot = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(tx, &scope(), &provider_lineage_request(&latest))
    })?);
    let captured = snapshot.resident.capture()?;
    assert!(!captured
        .execution
        .sources
        .contains_key("unqualified-unrelated-history"));
    assert!(!captured
        .records
        .iter()
        .any(|record| record.locator.key.id == "unqualified-unrelated-history"));
    let protocol = captured
        .execution_state_if_initialized()?
        .context("fixture protocol")?;
    assert!(crate::runtime::provider_recovery::resolve_from_captured(
        &captured, &latest, &protocol
    )
    .is_err());
    Ok(())
}

#[test]
fn claim_only_replay_does_not_require_an_unstarted_turn_hint_after_restart() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        let mut input = message("claim-only-replay-input");
        input.turn_id = Some("turn-never-started".into());
        append_message_tx(tx, &input)?;
        let mut replay = TurnRecord::new("agent-a", "turn-actual-replay", 2);
        replay.trigger = Some(crate::types::TurnTriggerSummary::from_message(&input));
        replay.input_message_ids = vec![input.id.clone()];
        replay.replay = Some(crate::types::TurnReplayProvenance {
            source_message_id: input.id,
            source_turn_id: "turn-never-started".into(),
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: None,
        });
        upsert_turn_record_tx(tx, &replay)
    })?;
    let captured = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 1),
        )
    })?)
    .resident
    .capture()?;
    assert_eq!(
        captured.message_turn_keys.get("claim-only-replay-input"),
        Some(&Some("turn-actual-replay".into()))
    );
    assert!(captured
        .lookup(&Key::new(Family::Turn, "turn-actual-replay"))?
        .is_some());
    assert!(captured
        .lookup(&Key::new(Family::Turn, "turn-never-started"))
        .is_err());
    Ok(())
}

#[test]
fn replay_hint_cannot_adopt_an_unrelated_turn_outside_the_fixed_window() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        let mut unrelated = TurnRecord::new("agent-a", "turn-private-unrelated", 0);
        unrelated.waiting_condition_ids = vec!["private-content-must-not-be-adopted".into()];
        unrelated.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: time(),
            duration_ms: 0,
        });
        upsert_turn_record_tx(tx, &unrelated)?;
        for index in 1..=512 {
            let mut filler = TurnRecord::new("agent-a", format!("filler-{index:04}"), index);
            filler.terminal = unrelated.terminal.clone();
            upsert_turn_record_tx(tx, &filler)?;
        }
        let mut input = message("replay-with-unrelated-hint");
        input.turn_id = Some(unrelated.turn_id.clone());
        append_message_tx(tx, &input)?;
        let mut replay = TurnRecord::new("agent-a", "turn-actual-replay", 513);
        replay.trigger = Some(crate::types::TurnTriggerSummary::from_message(&input));
        replay.input_message_ids = vec![input.id.clone()];
        replay.replay = Some(crate::types::TurnReplayProvenance {
            source_message_id: input.id,
            source_turn_id: unrelated.turn_id,
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: None,
        });
        upsert_turn_record_tx(tx, &replay)
    })?;
    let adopted: i64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_eligibility WHERE family='turn' AND record_id='turn-private-unrelated'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(adopted, 0);
    let captured = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 1),
        )
    })?)
    .resident
    .capture()?;
    assert_eq!(
        captured.message_turn_keys.get("replay-with-unrelated-hint"),
        Some(&Some("turn-actual-replay".into()))
    );
    assert!(captured
        .lookup(&Key::new(Family::Turn, "turn-private-unrelated"))
        .is_err());
    assert!(!captured.records.iter().any(|record| {
        matches!(&record.record, PreparedInputEvidence::Turn(turn)
            if turn.waiting_condition_ids.iter().any(|id| id == "private-content-must-not-be-adopted"))
    }));
    Ok(())
}

#[test]
fn queue_observer_revisions_do_not_invalidate_qualified_input_bodies() -> Result<()> {
    use crate::runtime_db::repositories::{try_claim_queued_message_tx, upsert_queue_entry_tx};
    use crate::types::{QueueEntryRecord, QueueEntryStatus};
    let queued = QueueEntryRecord {
        message_id: "queue-only-source".into(),
        agent_id: "agent-a".into(),
        priority: Priority::Normal,
        status: QueueEntryStatus::Queued,
        created_at: time(),
        updated_at: time(),
    };
    let (_directory, db) = legacy(|tx| {
        let actual = append_message_tx(tx, &message(&queued.message_id))?.0;
        let mut turn = TurnRecord::new("agent-a", "queue-only-turn", 1);
        turn.created_at = time();
        turn.input_message_ids.push(actual.id);
        turn.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: time(),
            duration_ms: 0,
        });
        upsert_turn_record_tx(tx, &turn)?;
        upsert_queue_entry_tx(tx, &queued)?;
        Ok(())
    })?;
    let revisions = || -> Result<(i64, i64, i64)> {
        Ok(db.connection()?.query_row(
            "SELECT (SELECT revision FROM conversation_source_revisions WHERE source_kind='operator' AND source_id='queue-only-source'),summary_revision,detail_revision FROM conversation_turn_revisions WHERE agent_id='agent-a' AND turn_id='queue-only-turn'",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        )?)
    };
    let before = revisions()?;
    // No input/evidence writer runs here: only the existing queue transition.
    db.transaction(|tx| {
        assert!(try_claim_queued_message_tx(tx, &queued)?);
        Ok(())
    })?;
    let after = revisions()?;
    assert!(after.0 > before.0 && after.1 > before.1 && after.2 > before.2);
    let mut policy = request(InputWindowSelector::GlobalTurns, 1);
    policy
        .mandatory_roots
        .insert(Key::new(Family::Message, &queued.message_id));
    let prepared =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &policy))?);
    let captured = prepared.resident.capture()?;
    assert!(captured
        .records
        .iter()
        .any(|x| x.locator.key == Key::new(Family::Message, &queued.message_id)));
    assert_eq!(
        captured.message_turn_keys.get(&queued.message_id),
        Some(&Some("queue-only-turn".into()))
    );
    // A real assignment change remains a changed input dependency, unlike UI status.
    db.transaction(|tx| {
        tx.execute("UPDATE conversation_input_assignments SET turn_id='foreign-turn' WHERE agent_id='agent-a' AND message_id=?1", [&queued.message_id])?;
        let result = load_input_evidence_snapshot_tx(tx, &scope(), &policy);
        assert!(matches!(result.unwrap_err().downcast_ref::<InputEvidenceSnapshotError>(), Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == &queued.message_id));
        Ok(())
    })?;
    Ok(())
}

#[test]
fn fixed_unprepared_turn_rejects_equal_length_body_change_after_budget_preflight() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        let mut turn = TurnRecord::new("agent-a", "unprepared-turn", 1);
        turn.created_at = time();
        turn.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::Completed,
            reason: Some("before".into()),
            no_brief_reason: None,
            completed_at: time(),
            duration_ms: 0,
        });
        upsert_turn_record_tx(tx, &turn)
    })?;
    let key = Key::new(Family::Turn, "unprepared-turn");
    db.transaction(|tx| {
        let before = payload_on(tx, "agent-a", &key)?.unwrap();
        let mut value: TurnRecord = serde_json::from_str(&before)?;
        value.terminal.as_mut().unwrap().reason = Some("after!".into());
        let after = serde_json::to_string(&value)?;
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        tx.execute(
            "UPDATE turn_records SET payload_json=?1 WHERE agent_id='agent-a' AND turn_id=?2",
            params![after, key.id],
        )?;
        // Frozen metadata still matches; only its fixed body digest distinguishes the rows.
        let binding = binding_on(tx, &scope())?;
        let fingerprint = fingerprint_on(tx, "agent-a", &key)?.unwrap();
        assert!(require_eligible_metadata(tx, &binding, &key, &fingerprint).is_ok());
        assert!(require_eligible(tx, &binding, &key, &fingerprint).is_err());
        assert!(validate_existing_input_evidence_tx(tx, &scope(), &key).is_err());
        Ok(())
    })?;
    let mut policy = request(InputWindowSelector::GlobalTurns, 1);
    policy.byte_limit = 1;
    let too_large = db
        .transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &policy))
        .unwrap_err();
    assert!(matches!(
        too_large.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ResourceExhausted(_))
    ));
    policy.byte_limit = 1024 * 1024;
    let changed = db
        .transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &policy))
        .unwrap_err();
    assert!(
        matches!(changed.downcast_ref::<InputEvidenceSnapshotError>(), Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == &key.id)
    );
    let aliases: i64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE family='turn' AND record_id=?1",
        [&key.id],
        |row| row.get(0),
    )?;
    assert_eq!(aliases, 0);
    Ok(())
}

#[test]
fn fixed_message_rejects_equal_length_body_change_with_stale_canonical_hash() -> Result<()> {
    let (_directory, db) = legacy(|tx| {
        let mut input = message("stale-hash-message");
        input.body = MessageBody::Text {
            text: "before".into(),
        };
        append_message_tx(tx, &input)?;
        Ok(())
    })?;
    let key = Key::new(Family::Message, "stale-hash-message");
    db.transaction(|tx| {
        let before = payload_on(tx, "agent-a", &key)?.unwrap();
        let after = before.replace("\"before\"", "\"after!\"");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        tx.execute(
            "UPDATE messages SET payload_json=?1 WHERE evidence_id=?2",
            params![after, key.id],
        )?;
        let fingerprint = fingerprint_on(tx, "agent-a", &key)?.unwrap();
        assert!(fingerprint.content_hash.is_some());
        assert!(
            require_eligible_metadata(tx, &binding_on(tx, &scope())?, &key, &fingerprint).is_ok()
        );
        assert!(validate_existing_input_evidence_tx(tx, &scope(), &key).is_err());
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            prepare_input_evidence_snapshot_tx(
                tx,
                &scope(),
                &request(InputWindowSelector::TrustedOperator, 1),
            )
        })
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<InputEvidenceSnapshotError>(), Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == &key.id)
    );
    assert!(db.transaction(|tx| alias_on(tx, &scope(), &key))?.is_none());
    Ok(())
}

#[test]
fn nullable_hash_fixed_messages_validate_original_pretty_bytes_before_first_alias() -> Result<()> {
    use crate::runtime_db::repositories::upsert_queue_entry_tx;
    use crate::types::{QueueEntryRecord, QueueEntryStatus};
    let (_directory, db) = legacy(|tx| {
        for id in ["nullable-bad", "nullable-good"] {
            let mut input = message(id);
            input.body = MessageBody::Text {
                text: "before".into(),
            };
            let actual = append_message_tx(tx, &input)?.0;
            // Legitimate pre-upgrade raw formatting must be hashed as stored,
            // never replaced by reserializing a typed Message at capture time.
            let pretty = serde_json::to_string_pretty(&serde_json::to_value(&actual)?)?;
            tx.execute(
                "UPDATE messages SET content_hash=NULL,payload_json=?1 WHERE evidence_id=?2",
                params![pretty, id],
            )?;
            upsert_queue_entry_tx(
                tx,
                &QueueEntryRecord {
                    message_id: id.into(),
                    agent_id: "agent-a".into(),
                    priority: Priority::Normal,
                    status: QueueEntryStatus::Queued,
                    created_at: time(),
                    updated_at: time(),
                },
            )?;
        }
        let mut state = AgentState::new("agent-a");
        state.pending = 2;
        state.total_message_count = 2;
        upsert_agent_state_tx(tx, &state)
    })?;
    let good = Key::new(Family::Message, "nullable-good");
    let bad = Key::new(Family::Message, "nullable-bad");
    let snapshot = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::TrustedOperator, 1),
        )
    })?);
    let captured = snapshot.resident.capture()?;
    assert_eq!(captured.records.len(), 1);
    assert_eq!(captured.records[0].locator.key, good);
    let original_good = db
        .transaction(|tx| payload_on(tx, "agent-a", &good))?
        .unwrap();
    assert_eq!(
        captured.records[0].locator.content_digest,
        bytes_digest(original_good.as_bytes())
    );
    assert!(db.transaction(|tx| alias_on(tx, &scope(), &bad))?.is_none());
    db.transaction(|tx| {
        let before = payload_on(tx, "agent-a", &bad)?.unwrap();
        let after = before.replace("\"before\"", "\"after!\"");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        tx.execute(
            "UPDATE messages SET payload_json=?1 WHERE evidence_id=?2",
            params![after, bad.id],
        )?;
        let fingerprint = fingerprint_on(tx, "agent-a", &bad)?.unwrap();
        assert!(fingerprint.content_hash.is_none());
        assert!(
            require_eligible_metadata(tx, &binding_on(tx, &scope())?, &bad, &fingerprint).is_ok()
        );
        assert!(validate_existing_input_evidence_tx(tx, &scope(), &bad).is_err());
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            prepare_input_evidence_snapshot_tx(
                tx,
                &scope(),
                &request(InputWindowSelector::TrustedOperator, 2),
            )
        })
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<InputEvidenceSnapshotError>(), Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == &bad.id)
    );
    assert!(db.transaction(|tx| alias_on(tx, &scope(), &bad))?.is_none());
    assert_eq!(
        db.transaction(|tx| payload_on(tx, "agent-a", &good))?
            .as_deref(),
        Some(original_good.as_str())
    );
    Ok(())
}

#[test]
fn asserted_missing_provider_source_is_focused_authority_evidence_not_lookup_absence() -> Result<()>
{
    use crate::runtime::resident_input_evidence::InputEvidenceReadError;

    let (_directory, db) = fresh()?;
    let (incoming, accepted) = db.transaction(|tx| {
        let incoming = append_message_tx(
            tx,
            &provider_lineage_recovery(
                "lineage-recovery-2",
                "lineage-recovery-1",
                "lineage-turn-1",
            ),
        )?
        .0;
        let accepted = register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(incoming.clone())],
        )?;
        Ok((incoming, accepted))
    })?;
    let wanted = provider_lineage_request(&incoming);
    let mut execution = crate::runtime::resident_input_evidence::ExecutionInputCoverage::default();
    execution.apply(&accepted.execution)?;
    let accepted = ResidentInputEvidence::bootstrap(
        scope(),
        accepted.records,
        InputEvidenceCoverage {
            selected_completion_brief_ids: Default::default(),
            completion_source_observations: Default::default(),
            recovery_source_absences: Default::default(),
            recovery_lineage_messages: Default::default(),
            execution,
            windows: Vec::new(),
            mandatory_roots: wanted.mandatory_roots.clone(),
            turn_transcript_keys: accepted.turn_transcript_keys,
            message_turn_keys: accepted.message_turn_keys,
            unavailable: BTreeSet::new(),
            record_limit: wanted.record_limit,
            byte_limit: wanted.byte_limit,
        },
    )?;
    // A normal Message writer qualified the body, not its asserted source IDs.
    // The owner preparation fast path must request one coherent observation.
    let error = accepted.capture_for_request(&wanted).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceReadError>(),
            Some(InputEvidenceReadError::NeedsPreparation(key)) if key == &Key::new(Family::Message, &incoming.id)
        ),
        "{error:#}"
    );
    let before =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted))?);
    let captured = before.resident.capture_for_request(&wanted)?;
    let proof = &captured.recovery_source_absences[&incoming.id];
    assert_eq!(
        proof.absent,
        BTreeSet::from([
            Key::new(Family::Message, "lineage-recovery-1"),
            Key::new(Family::Turn, "lineage-turn-1")
        ])
    );
    // This is dedicated authority evidence, never general-purpose None.
    assert!(captured
        .lookup(&Key::new(Family::Message, "lineage-recovery-1"))
        .is_err());
    let rejection = crate::runtime::provider_recovery::invalid_source_reason_from_captured(
        &captured, &incoming,
    )?;
    assert!(rejection.is_some());
    let old_error = crate::runtime::provider_recovery::resolve_from_captured(
        &captured,
        &incoming,
        &crate::domain::execution_protocol::ExecutionProtocolState::empty("agent-a"),
    )
    .unwrap_err();
    assert!(old_error
        .downcast_ref::<crate::runtime::resident_input_evidence::InputEvidenceReadError>()
        .is_none());
    assert!(old_error
        .downcast_ref::<InputEvidenceSnapshotError>()
        .is_none());

    // An accepted writer between planning and terminalization supplies the
    // real multihop sources. The newly captured view must replan, not drop on
    // the previous source_invalid reason. No consumer performs SQL fallback.
    let delta = db.transaction(|tx| {
        let actual = install_provider_lineage(tx)?;
        assert_eq!(actual, incoming);
        let records = [
            "lineage-original",
            "lineage-recovery-1",
            "lineage-recovery-2",
        ]
        .into_iter()
        .map(|id| {
            ContextDurableRead::new(tx, "agent-a")
                .message_by_id(id)?
                .context("fixture Message")
                .map(CanonicalInputEvidence::Message)
        })
        .collect::<Result<Vec<_>>>()?;
        register_committed_input_evidence_tx(tx, &scope(), &records)
    })?;
    // Any relevant source write invalidates the old absence proof. It cannot
    // survive as a stale authority rejection in the next resident generation.
    let updated = before
        .resident
        .with_committed(&delta, wanted.mandatory_roots.clone())?;
    if let Some(updated) = updated {
        assert!(updated.capture()?.recovery_source_absences.is_empty());
    }
    let fresh =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted))?);
    let fresh = fresh.resident.capture_for_request(&wanted)?;
    assert!(fresh.recovery_source_absences.is_empty());
    for id in ["lineage-attempt-0", "lineage-attempt-1"] {
        assert!(fresh.lookup(&Key::new(Family::Attempt, id))?.is_some());
    }
    assert!(
        crate::runtime::provider_recovery::invalid_source_reason_from_captured(&fresh, &incoming)?
            .is_none()
    );
    assert!(
        crate::runtime::provider_recovery::invalid_source_reason_from_captured(
            &captured, &incoming
        )?
        .is_some()
    );
    Ok(())
}

#[test]
fn asserted_existing_unqualified_or_pruned_provider_source_is_not_negative_authority() -> Result<()>
{
    for scenario in ["unqualified", "pruned", "foreign", "foreign_pruned"] {
        let (_directory, db) = fresh()?;
        let incoming = db.transaction(|tx| {
            let mut source = message("asserted-source");
            if scenario.starts_with("foreign") {
                source.agent_id = "foreign-agent".into();
            }
            let actual = append_message_tx(tx, &source)?.0;
            if scenario == "pruned" {
                register_committed_input_evidence_tx(
                    tx,
                    &scope(),
                    &[CanonicalInputEvidence::Message(actual)],
                )?;
                tx.execute(
                    "DELETE FROM messages WHERE evidence_id='asserted-source'",
                    [],
                )?;
                register_pruned_input_evidence_tx(
                    tx,
                    &scope(),
                    &[Key::new(Family::Message, "asserted-source")],
                )?;
            } else if scenario == "foreign_pruned" {
                // A real foreign tombstone also prevents a never-existed proof.
                let mut foreign_identity = identity();
                foreign_identity.agent_id = "foreign-agent".into();
                upsert_agent_identity_tx(tx, &foreign_identity)?;
                let foreign_scope = InputEvidenceScope {
                    agent_id: "foreign-agent".into(),
                    identity_incarnation: 1,
                };
                register_committed_input_evidence_tx(
                    tx,
                    &foreign_scope,
                    &[CanonicalInputEvidence::Message(actual)],
                )?;
                tx.execute(
                    "DELETE FROM messages WHERE evidence_id='asserted-source'",
                    [],
                )?;
                register_pruned_input_evidence_tx(
                    tx,
                    &foreign_scope,
                    &[Key::new(Family::Message, "asserted-source")],
                )?;
            }
            let incoming = append_message_tx(
                tx,
                &provider_lineage_recovery("asserted-recovery", "asserted-source", "missing-turn"),
            )?
            .0;
            register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[CanonicalInputEvidence::Message(incoming.clone())],
            )?;
            Ok(incoming)
        })?;
        let result = db.transaction(|tx| {
            prepare_input_evidence_snapshot_tx(tx, &scope(), &provider_lineage_request(&incoming))
        });
        assert!(
            result.is_err(),
            "{scenario} must remain strict preparation debt"
        );
    }
    Ok(())
}

#[test]
fn history_selected_recovery_message_does_not_acquire_unrequested_authority_closure() -> Result<()>
{
    let (_directory, db) = fresh()?;
    db.transaction(|tx| {
        let old = append_message_tx(
            tx,
            &provider_lineage_recovery(
                "history-recovery",
                "unrequested-source",
                "unrequested-turn",
            ),
        )?
        .0;
        let mut turn = TurnRecord::new("agent-a", "history-hosting-turn", 1);
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&old));
        turn.input_message_ids = vec![old.id.clone()];
        upsert_turn_record_tx(tx, &turn)?;
        let turn = ContextDurableRead::new(tx, "agent-a")
            .turn_by_id(&turn.turn_id)?
            .context("history Turn")?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[
                CanonicalInputEvidence::Message(old),
                CanonicalInputEvidence::Turn(turn),
            ],
        )
    })?;
    let captured = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(
            tx,
            &scope(),
            &request(InputWindowSelector::GlobalTurns, 1),
        )
    })?)
    .resident
    .capture()?;
    assert!(captured
        .lookup(&Key::new(Family::Message, "history-recovery"))?
        .is_some());
    assert!(captured.recovery_source_absences.is_empty());
    for key in [
        Key::new(Family::Message, "unrequested-source"),
        Key::new(Family::Turn, "unrequested-turn"),
    ] {
        assert!(captured.lookup(&key).is_err());
        assert!(!captured
            .records
            .iter()
            .any(|record| record.locator.key == key));
    }
    Ok(())
}

#[test]
fn fixed_upgrade_missing_table_freezes_absence_without_late_input_adoption() -> Result<()> {
    let mut turn_schema = String::new();
    let (_directory, db) = legacy(|tx| {
        turn_schema = tx.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='turn_records'",
            [],
            |row| row.get(0),
        )?;
        tx.execute_batch("DROP TABLE turn_records")?;
        Ok(())
    })?;
    let policy = request(InputWindowSelector::GlobalTurns, 12);
    assert!(
        db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &policy))
            .is_err(),
        "ordinary preparation must not tolerate a missing canonical table"
    );
    db.transaction(|tx| {
        tx.execute_batch(&turn_schema)?;
        let turn = TurnRecord::new("agent-a", "post-cutover-turn", 1);
        upsert_turn_record_tx(tx, &turn)?;
        capture_input_evidence_upgrade_tx(tx, time())?;
        let eligible: i64 = tx.query_row("SELECT COUNT(*) FROM input_evidence_seed_membership WHERE family='turn' AND record_id='post-cutover-turn'", [], |row| row.get(0))?;
        assert_eq!(eligible, 0);
        Ok(())
    })?;
    let loaded =
        current(db.transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &policy))?);
    assert!(loaded.resident.capture()?.records.is_empty());
    let mut explicit = policy;
    explicit
        .mandatory_roots
        .insert(Key::new(Family::Turn, "post-cutover-turn"));
    let error = db
        .transaction(|tx| prepare_input_evidence_snapshot_tx(tx, &scope(), &explicit))
        .unwrap_err();
    // A present canonical row outside the frozen seed is an unqualified
    // projection, not a prepared member whose body only needs loading.
    assert!(
        matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == "post-cutover-turn"),
        "{error:#}"
    );
    db.transaction(|tx| {
        let eligible: i64 = tx.query_row(
            "SELECT COUNT(*) FROM input_evidence_seed_membership WHERE family='turn' AND record_id='post-cutover-turn'",
            [],
            |row| row.get(0),
        )?;
        let aliases: i64 = tx.query_row(
            "SELECT COUNT(*) FROM input_evidence_current WHERE family='turn' AND record_id='post-cutover-turn'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(eligible, 0);
        assert_eq!(aliases, 0);
        Ok(())
    })?;
    Ok(())
}

#[path = "input_evidence_work_cleanup_tests.rs"]
mod work_cleanup_tests;

#[test]
fn dispatch_briefs_read_fixed_members_without_alias_hydration_or_late_adoption() -> Result<()> {
    use crate::{runtime_db::evidence::insert_brief_evidence_tx, types::BriefKind};
    let mut fixed = BriefRecord::new("agent-a", BriefKind::Result, "fixed success", None, None);
    fixed.id = "fixed-dispatch-success".into();
    fixed.created_at = time();
    let (_directory, db) = legacy(|tx| insert_brief_evidence_tx(tx, &fixed))?;
    db.transaction(|tx| {
        let mut late = fixed.clone();
        late.id = "raw-late-dispatch-success".into();
        late.created_at += chrono::Duration::seconds(1);
        insert_brief_evidence_tx(tx, &late)?;
        let before: i64 =
            tx.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |row| {
                row.get(0)
            })?;
        let briefs = qualified_dispatch_briefs_tx(tx, &scope(), 64, 1024 * 1024)?;
        assert_eq!(briefs, vec![fixed.clone()]);
        let after: i64 =
            tx.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |row| {
                row.get(0)
            })?;
        assert_eq!(before, after);
        assert!(qualified_dispatch_briefs_tx(tx, &scope(), 64, 1).is_err());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn dispatch_briefs_follow_current_digest_and_pruning_and_never_successor_history() -> Result<()> {
    use crate::{runtime_db::evidence::insert_brief_evidence_tx, types::BriefKind};
    let (_directory, db) = fresh()?;
    let mut value = BriefRecord::new("agent-a", BriefKind::Result, "current success", None, None);
    value.id = "current-dispatch-success".into();
    value.created_at = time();
    db.transaction(|tx| {
        insert_brief_evidence_tx(tx, &value)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Brief(value.clone())],
        )?;
        assert_eq!(
            qualified_dispatch_briefs_tx(tx, &scope(), 64, 1024 * 1024)?,
            vec![value.clone()]
        );
        let mut wrong = value.clone();
        wrong.text = "conflicting restore".into();
        tx.execute(
            "UPDATE briefs SET payload_json=?2 WHERE evidence_id=?1",
            params![value.id, serde_json::to_string(&wrong)?],
        )?;
        assert!(qualified_dispatch_briefs_tx(tx, &scope(), 64, 1024 * 1024).is_err());
        tx.execute(
            "UPDATE briefs SET payload_json=?2 WHERE evidence_id=?1",
            params![value.id, serde_json::to_string(&value)?],
        )?;
        tx.execute("DELETE FROM briefs WHERE evidence_id=?1", [&value.id])?;
        register_pruned_input_evidence_tx(tx, &scope(), &[Key::new(Family::Brief, &value.id)])?;
        assert!(qualified_dispatch_briefs_tx(tx, &scope(), 64, 1024 * 1024)?.is_empty());
        let mut retained = value.clone();
        retained.id = "retained-prior-inc-success".into();
        insert_brief_evidence_tx(tx, &retained)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Brief(retained)],
        )?;
        assert_eq!(
            qualified_dispatch_briefs_tx(tx, &scope(), 64, 1024 * 1024)?.len(),
            1
        );
        Ok(())
    })?;
    let mut successor = identity();
    successor.incarnation = 2;
    db.transaction(|tx| upsert_agent_identity_tx(tx, &successor))?;
    db.transaction(|tx| {
        let successor_scope = InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: 2,
        };
        assert!(qualified_dispatch_briefs_tx(tx, &successor_scope, 64, 1024 * 1024)?.is_empty());
        assert!(qualified_dispatch_briefs_tx(tx, &scope(), 64, 1024 * 1024).is_err());
        Ok(())
    })?;
    Ok(())
}
