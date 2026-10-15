use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority,
    },
};
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "input-native-source".into(),
        identity_incarnation: 1,
    }
}
fn fresh() -> Result<(tempfile::TempDir, RuntimeDb, InputEvidenceBudgetLedger)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        super::super::capture_input_evidence_upgrade_tx(tx, Utc::now())?;
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                scope().agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new(scope().agent_id))
    })?;
    Ok((
        home,
        db,
        InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?,
    ))
}
fn message(id: &str) -> MessageEnvelope {
    let mut row = MessageEnvelope::new(
        scope().agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "actual native Input".into(),
        },
    );
    row.id = id.into();
    row
}
fn proof<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    rows: &[MessageEnvelope],
    ledger: &InputEvidenceBudgetLedger,
) -> Result<NativeInputWriteProof<'tx, 'conn>> {
    NativeInputWriteProof::prepare_tx(
        tx,
        &scope(),
        |visit| {
            for row in rows {
                visit(Family::Message, &row.id)?;
            }
            Ok(())
        },
        ledger,
        (MAX_METADATA_BYTES, MAX_RECORDS),
    )?
    .context("actual source not installed")
}
#[test]
fn input_native_members_are_complete_beyond_windows_and_updates_keep_original_births() -> Result<()>
{
    let (_home, db, ledger) = fresh()?;
    let rows = (0..130)
        .map(|i| message(&format!("native-{i:03}")))
        .collect::<Vec<_>>();
    db.transaction(|tx| {
        let native = proof(tx, &rows, &ledger)?;
        let mut facts = Vec::new();
        for row in &rows {
            let (actual, inserted) = append_message_tx(tx, row)?;
            assert!(inserted);
            facts.push(CanonicalInputEvidence::Message(actual));
        }
        let delta = super::super::register_committed_input_evidence_tx(tx, &scope(), &facts)?;
        native.finish_tx(tx, &delta.records)?;
        assert_eq!(
            tx.query_row(
                "SELECT high_water FROM input_query_birth_cursors",
                [],
                |r| r.get::<_, u64>(0)
            )?,
            130
        );
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM input_query_birth_members WHERE birth_seq>0",
                [],
                |r| r.get::<_, u64>(0)
            )?,
            130
        );
        Ok(())
    })?;
    db.transaction(|tx| {
        let native = proof(tx, &rows[..1], &ledger)?;
        let (actual, inserted) = append_message_tx(tx, &rows[0])?;
        assert!(!inserted);
        let delta = super::super::register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(actual)],
        )?;
        native.finish_tx(tx, &delta.records)?;
        assert_eq!(
            tx.query_row(
                "SELECT high_water FROM input_query_birth_cursors",
                [],
                |r| r.get::<_, u64>(0)
            )?,
            130
        );
        assert_eq!(
            tx.query_row(
                "SELECT birth_seq FROM input_query_birth_members WHERE record_id='native-000'",
                [],
                |r| r.get::<_, u64>(0)
            )?,
            1
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn after_sql_proof_and_old_tombstones_never_mint_another_native_birth() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let row = message("preexisting-before-proof");
    db.transaction(|tx| {
        let(actual,_)=append_message_tx(tx,&row)?;
        let native=proof(tx,std::slice::from_ref(&row),&ledger)?;
        assert!(native.first.is_empty());
        let delta=super::super::register_committed_input_evidence_tx(tx,&scope(),&[CanonicalInputEvidence::Message(actual)])?;
        native.finish_tx(tx,&delta.records)?;
        assert_eq!(tx.query_row("SELECT high_water FROM input_query_birth_cursors",[],|r|r.get::<_,u64>(0))?,0);
        assert_eq!(tx.query_row("SELECT birth_seq FROM input_query_birth_members",[],|r|r.get::<_,u64>(0))?,0);
        tx.execute("DELETE FROM messages WHERE evidence_id=?1",[&row.id])?;
        tx.execute("UPDATE input_evidence_current SET availability='pruned',content_digest=NULL WHERE family='message' AND record_id=?1",[&row.id])?;
        let next=proof(tx,std::slice::from_ref(&row),&ledger)?;assert!(next.first.is_empty());drop(next);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn native_source_rejects_other_actual_tx_and_installed_missing_schema() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let (_other_home, other, _other_ledger) = fresh()?;
    db.transaction(|tx| {
        let native = proof(tx, &[message("exact-original-tx")], &ledger)?;
        let error = other
            .transaction_once(|other_tx| native.finish_tx(other_tx, &[]))
            .unwrap_err();
        assert!(format!("{error:#}").contains("changed original transaction"));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let failed = db.transaction(|tx| {
        tx.execute_batch("DROP TABLE input_query_birth_members")?;
        proof(tx, &[message("installed-missing")], &ledger)?;
        Ok(())
    });
    assert!(
        format!("{:#}", failed.unwrap_err()).contains("installed Input birth source is incomplete")
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn original_borrowed_source_addresses_are_budgeted_before_first_owned_copy() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let huge = "x".repeat(16 * 1024);
    let failed: Result<()> = db.transaction(|tx| {
        NativeInputWriteProof::prepare_tx(
            tx,
            &scope(),
            |visit| visit(Family::Message, &huge),
            &ledger,
            (4096, MAX_RECORDS),
        )?;
        Ok(())
    });
    assert!(matches!(
        failed
            .unwrap_err()
            .downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ResourceExhausted(_))
    ));
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_query_birth_cursors", [], |r| {
                r.get::<_, u64>(0)
            })?,
        0
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_identity_native_nodes_and_a_changed_borrowed_pass_fail_before_owned_keys() -> Result<()>
{
    let (_home, db, ledger) = fresh()?;
    let huge = "x".repeat(16 * 1024);
    let failed: Result<()> = db.transaction(|tx| {
        let mut pass = 0;
        NativeInputWriteProof::prepare_tx(
            tx,
            &scope(),
            |visit| {
                pass += 1;
                visit(Family::Message, if pass == 1 { "tiny" } else { &huge })
            },
            &ledger,
            (MAX_METADATA_BYTES, MAX_RECORDS),
        )?;
        Ok(())
    });
    assert!(matches!(
        failed
            .unwrap_err()
            .downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ResourceExhausted(_))
    ));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let values = serde_json::to_string(&vec![0; 2000])?;
    db.connection()?.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.unconsumed',json(?1)) WHERE agent_id=?2",params![values,scope().agent_id])?;
    let failed: Result<()> = db.transaction(|tx| {
        NativeInputWriteProof::prepare_tx(
            tx,
            &scope(),
            |visit| visit(Family::Message, "native-node-limit"),
            &ledger,
            (MAX_METADATA_BYTES, 1024),
        )?;
        Ok(())
    });
    assert!(matches!(
        failed
            .unwrap_err()
            .downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ResourceExhausted(
            "original Input birth identity"
        ))
    ));
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_query_birth_cursors", [], |r| {
                r.get::<_, u64>(0)
            })?,
        0
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_identity_missing_incarnation_keeps_default_one_but_null_or_other_incarnation_rejects(
) -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    db.connection()?.execute("UPDATE agent_identities SET payload_json=json_remove(payload_json,'$.incarnation') WHERE agent_id=?1",[scope().agent_id])?;
    let raw: String = db.connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [scope().agent_id],
        |r| r.get(0),
    )?;
    let actual: AgentIdentityRecord = serde_json::from_str(&raw)?;
    assert_eq!(actual.incarnation, scope().identity_incarnation);
    db.transaction(|tx| {
        let native = proof(tx, &[message("legacy-default-one")], &ledger)?;
        assert_eq!(native.scope.identity_incarnation, actual.incarnation);
        native.finish_tx(tx, &[])
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for value in [
        serde_json::Value::Null,
        serde_json::json!(0),
        serde_json::json!(2),
        serde_json::json!("1"),
    ] {
        db.connection()?.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',json(?1)) WHERE agent_id=?2",params![serde_json::to_string(&value)?,scope().agent_id])?;
        let failed: Result<()> = db.transaction(|tx| {
            proof(tx, &[message("never-new-scope")], &ledger)?;
            Ok(())
        });
        assert!(matches!(
            failed
                .unwrap_err()
                .downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::RetiredScope)
        ));
        assert_eq!(
            db.connection()?.query_row(
                "SELECT high_water FROM input_query_birth_cursors",
                [],
                |r| r.get::<_, u64>(0)
            )?,
            0
        );
        assert_eq!(
            db.connection()?.query_row(
                "SELECT COUNT(*) FROM input_query_birth_members",
                [],
                |r| r.get::<_, u64>(0)
            )?,
            0
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn native_source_preserves_nul_only_canonical_message_ids() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let row = message("\0");
    db.transaction(|tx| {
        let native = proof(tx, std::slice::from_ref(&row), &ledger)?;
        assert!(native.first.contains(&Key::new(Family::Message, &row.id)));
        let (actual, inserted) = append_message_tx(tx, &row)?;
        assert!(inserted);
        assert_eq!(actual.id, row.id);
        assert!(actual.message_seq.is_some());
        let delta = super::super::register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(actual)],
        )?;
        assert_eq!(delta.records.len(), 1);
        assert_eq!(delta.records[0].locator.key.id, row.id);
        native.finish_tx(tx, &delta.records)?;
        let member: (String, u64, usize, usize) = tx.query_row(
            "SELECT record_id,birth_seq,length(record_id),length(CAST(record_id AS BLOB)) FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3",
            params![scope().agent_id, scope().identity_incarnation, row.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        assert_eq!(member, (row.id.clone(), 1, 0, 1));
        assert_eq!(tx.query_row(
            "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
            |r| r.get::<_, u64>(0),
        )?, 1);
        assert!(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3 AND availability='present' AND qualification='committed_writer')",
            params![scope().agent_id, scope().identity_incarnation, row.id],
            |r| r.get::<_, bool>(0),
        )?);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn native_source_and_member_schema_still_reject_empty_addresses() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let row = message("");
    let failed: Result<()> = db.transaction(|tx| {
        proof(tx, std::slice::from_ref(&row), &ledger)?;
        Ok(())
    });
    assert!(format!("{:#}", failed.unwrap_err()).contains("Input birth original address is empty"));
    db.transaction(|tx| {
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, u64>(0))?, 0);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| r.get::<_, u64>(0))?, 0);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_query_birth_cursors", [], |r| r.get::<_, u64>(0))?, 0);
        initialize(tx, &scope())?;
        let error = tx.execute(
            "INSERT INTO input_query_birth_members(agent_id,identity_incarnation,family,record_id,birth_seq) VALUES(?1,?2,'message',?3,1)",
            params![scope().agent_id, scope().identity_incarnation, row.id],
        ).unwrap_err();
        assert!(error.to_string().contains("CHECK constraint failed"));
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_query_birth_members", [], |r| r.get::<_, u64>(0))?, 0);
        assert_eq!(tx.query_row("SELECT high_water FROM input_query_birth_cursors", [], |r| r.get::<_, u64>(0))?, 0);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}


#[test]
fn original_native_advance_pins_exact_actual_frontier_until_last_ack_clone_drops() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let rows = (0..130).map(|i| message(&format!("advance-{i:03}"))).collect::<Vec<_>>();
    let delta = db.transaction_once(|tx| {
        let native = proof(tx, &rows, &ledger)?;
        let mut facts = Vec::new();
        for row in &rows {
            let (actual, inserted) = append_message_tx(tx, row)?;
            assert!(inserted);
            facts.push(CanonicalInputEvidence::Message(actual));
        }
        let mut delta = super::super::register_committed_input_evidence_tx(tx, &scope(), &facts)?;
        native.finish_delta_tx(tx, &mut delta)?;
        let advance = delta.query_advance.as_ref().context("actual outer writer lost advance")?;
        assert_eq!(advance.base_cursor, 0);
        assert_eq!(advance.cursor, 130);
        assert_eq!(advance.born.len(), 130);
        advance.validate_delta(&scope(), &delta.records)?;
        for (index, row) in rows.iter().enumerate() {
            let (seq, locator) = advance.born.get(&Key::new(Family::Message, &row.id))
                .context("post-window native member omitted")?;
            assert_eq!(*seq, u64::try_from(index)? + 1);
            assert_eq!(*locator, delta.records.iter().find(|record| record.locator.key.id == row.id)
                .context("actual D3 row omitted")?.locator);
        }
        Ok(delta)
    })?;
    let advance = delta.query_advance.as_ref().context("committed advance absent")?;
    let weak = Arc::downgrade(&advance._material[0]);
    let held = ledger.report()?.observed_bytes;
    assert!(held > 0, "original preproof guard must survive SQL commit");
    let mut changed = delta.records[0].as_ref().clone();
    changed.locator.revision += 1;
    let error = advance.validate_delta(&scope(), &[Arc::new(changed)]).unwrap_err();
    assert!(error.to_string().contains("exact actual D3 record"));
    let mut foreign = scope();
    foreign.identity_incarnation += 1;
    assert!(advance.validate_delta(&foreign, &delta.records).is_err());
    let retained = delta.clone();
    drop(delta);
    assert!(weak.upgrade().is_some());
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop(retained);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn native_advance_updates_and_preexisting_raw_rows_keep_birth_zero_and_whole_tx_rollback() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let row = message("advance-original");
    db.transaction(|tx| {
        let native = proof(tx, std::slice::from_ref(&row), &ledger)?;
        let (actual, _) = append_message_tx(tx, &row)?;
        let mut delta = super::super::register_committed_input_evidence_tx(tx, &scope(),
            &[CanonicalInputEvidence::Message(actual)])?;
        native.finish_delta_tx(tx, &mut delta)?;
        assert_eq!(delta.query_advance.as_ref().unwrap().cursor, 1);
        Ok(())
    })?;
    let raw = message("advance-raw-before-preflight");
    db.transaction(|tx| { append_message_tx(tx, &raw)?; Ok(()) })?;
    db.transaction(|tx| {
        let native = proof(tx, &[row.clone(), raw.clone()], &ledger)?;
        let mut facts = Vec::new();
        for original in [&row, &raw] {
            let (actual, inserted) = append_message_tx(tx, original)?;
            assert!(!inserted);
            facts.push(CanonicalInputEvidence::Message(actual));
        }
        let mut delta = super::super::register_committed_input_evidence_tx(tx, &scope(), &facts)?;
        native.finish_delta_tx(tx, &mut delta)?;
        let advance = delta.query_advance.as_ref().unwrap();
        assert_eq!((advance.base_cursor, advance.cursor), (1, 1));
        assert!(advance.born.is_empty());
        assert_eq!(tx.query_row("SELECT birth_seq FROM input_query_birth_members WHERE record_id=?1",
            [&raw.id], |row| row.get::<_,u64>(0))?, 0);
        Ok(())
    })?;
    let pending = message("advance-rolled-back");
    let failed: Result<()> = db.transaction(|tx| {
        let native = proof(tx, std::slice::from_ref(&pending), &ledger)?;
        let (actual, _) = append_message_tx(tx, &pending)?;
        let mut delta = super::super::register_committed_input_evidence_tx(tx, &scope(),
            &[CanonicalInputEvidence::Message(actual)])?;
        native.finish_delta_tx(tx, &mut delta)?;
        assert_eq!(delta.query_advance.as_ref().unwrap().cursor, 2);
        anyhow::bail!("original whole receipt failure")
    });
    assert!(failed.unwrap_err().to_string().contains("whole receipt failure"));
    assert_eq!(db.connection()?.query_row("SELECT high_water FROM input_query_birth_cursors", [],
        |row| row.get::<_,u64>(0))?, 1);
    assert!(!db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE evidence_id=?1)",
        [&pending.id], |row| row.get::<_,bool>(0))?);
    assert!(!db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM input_query_birth_members WHERE record_id=?1)",
        [&pending.id], |row| row.get::<_,bool>(0))?);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn native_advance_requires_unchanged_concrete_tx_base_and_refuses_different_delta_merge() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let rows = [message("advance-first"), message("advance-second")];
    let failed: Result<()> = db.transaction(|tx| {
        let first = proof(tx, &rows[..1], &ledger)?;
        let second = proof(tx, &rows[1..], &ledger)?;
        let (actual, _) = append_message_tx(tx, &rows[1])?;
        let mut delta = super::super::register_committed_input_evidence_tx(tx, &scope(),
            &[CanonicalInputEvidence::Message(actual)])?;
        second.finish_delta_tx(tx, &mut delta)?;
        first.finish_delta_tx(tx, &mut CommittedInputEvidenceDelta::default())?;
        Ok(())
    });
    assert!(failed.unwrap_err().to_string().contains("cursor changed within original transaction"));
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM input_query_birth_cursors", [],
        |row| row.get::<_,usize>(0))?, 0);
    let mut deltas = Vec::new();
    for row in &rows {
        deltas.push(db.transaction_once(|tx| {
            let native = proof(tx, std::slice::from_ref(row), &ledger)?;
            let (actual, _) = append_message_tx(tx, row)?;
            let mut delta = super::super::register_committed_input_evidence_tx(tx, &scope(),
                &[CanonicalInputEvidence::Message(actual)])?;
            native.finish_delta_tx(tx, &mut delta)?;
            Ok(delta)
        })?);
    }
    let mut merged = CommittedInputEvidenceDelta::default();
    super::super::super::input_evidence_writes::merge_input_evidence_delta(&mut merged, deltas[0].clone())?;
    super::super::super::input_evidence_writes::merge_input_evidence_delta(&mut merged, deltas[0].clone())?;
    let error = super::super::super::input_evidence_writes::merge_input_evidence_delta(&mut merged, deltas[1].clone())
        .unwrap_err();
    assert!(error.to_string().contains("different original birth advances"));
    drop((merged, deltas));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_writer_source_uses_the_actual_owner_and_retains_its_selectors_through_ack() -> Result<()> {
    use crate::runtime::{input_queries::{InputQuerySpec, source_roles_for_fixture}, mutation_owner::MutationOwnerRegistry};
    let (_home, db, ledger) = fresh()?;
    let owners = MutationOwnerRegistry::default();
    let owner = owners.claim(&scope().agent_id, scope().identity_incarnation)?;
    let required = [Key::new(Family::Message, "original-required")];
    let (reservation, lease, roles) = source_roles_for_fixture(
        &owner, &scope(), InputQuerySpec { required_keys: &required, birth_after: Some(0) },
        &ledger, (MAX_METADATA_BYTES, MAX_RECORDS),
    )?;
    drop(roles);
    let source = reservation.input_query_source(&scope(), &ledger, (MAX_METADATA_BYTES, MAX_RECORDS))?;
    let source_weak = Arc::downgrade(&source);
    let row = message("actual-writer-source");
    let delta = db.transaction_once(|tx| {
        let native = NativeInputWriteProof::prepare_with_query_source_tx(
            tx, &scope(), |visit| visit(Family::Message, &row.id), &ledger,
            (MAX_METADATA_BYTES, MAX_RECORDS), Some(&source),
        )?.context("actual source is installed")?;
        let (actual, inserted) = append_message_tx(tx, &row)?;
        assert!(inserted);
        let mut delta = super::super::register_committed_input_evidence_tx(
            tx, &scope(), &[CanonicalInputEvidence::Message(actual)],
        )?;
        native.finish_delta_tx(tx, &mut delta)?;
        assert!(Arc::ptr_eq(delta.query_advance.as_ref().context("advance absent")?
            ._query_source.as_ref().context("actual owner source lost")?, &source));
        Ok(delta)
    })?;
    drop((source, lease, reservation));
    assert!(source_weak.upgrade().is_some());
    let held = ledger.report()?.observed_bytes;
    assert!(held > 0);
    let retained = delta.clone();
    drop(delta);
    assert!(source_weak.upgrade().is_some());
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop(retained);
    assert!(source_weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_writer_source_debits_selectors_before_native_keys_and_rejects_foreign_ledger() -> Result<()> {
    use crate::runtime::{input_queries::{InputQuerySpec, source_roles_for_fixture}, mutation_owner::MutationOwnerRegistry};
    let (_home, db, ledger) = fresh()?;
    let owners = MutationOwnerRegistry::default();
    let owner = owners.claim(&scope().agent_id, scope().identity_incarnation)?;
    let required = [Key::new(Family::Message, "s".repeat(2048))];
    let (reservation, lease, roles) = source_roles_for_fixture(
        &owner, &scope(), InputQuerySpec { required_keys: &required, birth_after: Some(0) },
        &ledger, (MAX_METADATA_BYTES, MAX_RECORDS),
    )?;
    drop(roles);
    let source = reservation.input_query_source(&scope(), &ledger, (MAX_METADATA_BYTES, MAX_RECORDS))?;
    let source_cost = source.remaining_limits(&scope(), &ledger, (MAX_METADATA_BYTES, MAX_RECORDS))?;
    let source_bytes = MAX_METADATA_BYTES - source_cost.0;
    let row = message(&"n".repeat(4096));
    let native_bytes = db.transaction(|tx| {
        let native = proof(tx, std::slice::from_ref(&row), &ledger)?;
        let bytes = native._material.iter().try_fold(0usize, |sum, guard| -> Result<usize> {
            sum.checked_add(usize::try_from(guard.owned_material_bytes()?)?)
                .context("fixture native bytes overflow")
        })?;
        drop(native);
        Ok(bytes)
    })?;
    // Either allocation separately fits. Their actual simultaneous sum does not.
    let limit = native_bytes.max(source_bytes);
    assert!(native_bytes <= limit && source_bytes <= limit);
    assert!(source_bytes.checked_add(native_bytes).is_some_and(|sum| sum > limit));
    let baseline = ledger.report()?.observed_bytes;
    let failed: Result<()> = db.transaction(|tx| {
        NativeInputWriteProof::prepare_with_query_source_tx(
            tx, &scope(), |visit| visit(Family::Message, &row.id), &ledger,
            (limit, MAX_RECORDS), Some(&source),
        )?;
        anyhow::bail!("fixture must reject before business SQL")
    });
    assert!(matches!(failed.unwrap_err().downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ResourceExhausted(_))));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let foreign = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let rejected: Result<()> = db.transaction(|tx| {
        NativeInputWriteProof::prepare_with_query_source_tx(
            tx, &scope(), |visit| visit(Family::Message, &row.id), &foreign,
            (MAX_METADATA_BYTES, MAX_RECORDS), Some(&source),
        )?;
        Ok(())
    });
    assert!(rejected.is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    db.transaction(|tx| {
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_,u64>(0))?, 0);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM input_query_birth_members", [], |r| r.get::<_,u64>(0))?, 0);
        assert_eq!(tx.query_row("SELECT high_water FROM input_query_birth_cursors", [], |r| r.get::<_,u64>(0))?, 0);
        Ok(())
    })?;
    drop((source, lease, reservation));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_writer_source_observes_empty_roles_and_never_substitutes_a_successor_scope() -> Result<()> {
    use crate::runtime::mutation_owner::MutationOwnerRegistry;
    let (_home, _db, ledger) = fresh()?;
    let owners = MutationOwnerRegistry::default();
    let owner = owners.claim(&scope().agent_id, scope().identity_incarnation)?;
    let reservation = owner.try_reserve()?;
    assert!(reservation.input_query_roles(&scope(), &ledger, (MAX_METADATA_BYTES, MAX_RECORDS))?.is_none());
    let source = reservation.input_query_source(&scope(), &ledger, (MAX_METADATA_BYTES, MAX_RECORDS))?;
    let remaining = source.remaining_limits(&scope(), &ledger, (MAX_METADATA_BYTES, MAX_RECORDS))?;
    assert!(remaining.0 < MAX_METADATA_BYTES && remaining.1 < MAX_RECORDS);
    let baseline = ledger.report()?.observed_bytes;
    let mut successor = scope(); successor.identity_incarnation += 1;
    assert!(reservation.input_query_source(&successor, &ledger, (MAX_METADATA_BYTES, MAX_RECORDS)).is_err());
    assert!(source.remaining_limits(&successor, &ledger, (MAX_METADATA_BYTES, MAX_RECORDS)).is_err());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop((source, reservation));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn native_first_tool_insert_cannot_borrow_legacy_noop_qualification() -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let scope = scope();
    let at = Utc::now();
    let tool = ToolExecutionRecord {
        id: "forged-newborn-noop".into(), agent_id: scope.agent_id.clone(),
        work_item_id: None, turn_index: 0, turn_id: None,
        tool_name: crate::tool::names::COMPLETE_WORK_ITEM.into(), created_at: at,
        completed_at: Some(at), duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: crate::types::ToolExecutionStatus::Success,
        input: serde_json::json!({"work_item_id":"finite-original-work"}),
        output: serde_json::json!({"status":"completed"}),
        summary: "actual first Tool insert".into(), invocation_surface: None,
    };
    let failed: Result<()> = db.transaction(|tx| {
        let native = NativeInputWriteProof::prepare_tx(tx, &scope,
            |visit|visit(Family::ToolExecution, &tool.id), &ledger,
            (MAX_METADATA_BYTES, MAX_RECORDS))?.context("actual Native source missing")?;
        assert!(native.first.contains(&Key::new(Family::ToolExecution, &tool.id)));
        crate::runtime_db::evidence::insert_tool_evidence_tx(tx, &tool)?;
        let delta = super::super::register_committed_input_evidence_tx(tx, &scope,
            &[CanonicalInputEvidence::ToolExecution(tool.clone())])?;
        assert_eq!(delta.records.len(), 1);
        // Adversarial alias provenance is not the opaque original Focus no-op
        // readback. A real first insert must still require its writer ACK.
        assert_eq!(tx.execute("UPDATE input_evidence_current SET qualification='legacy_prepared' WHERE family='tool_execution' AND record_id=?1",[&tool.id])?, 1);
        native.finish_tx(tx, &delta.records)
    });
    assert!(format!("{:#}",failed.unwrap_err()).contains("lacks exact actual D3 writer ACK"));
    assert_eq!(db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM tool_executions),(SELECT COUNT(*) FROM input_evidence_current),(SELECT COUNT(*) FROM input_query_birth_members),(SELECT COUNT(*) FROM input_query_birth_cursors)",
        [], |row|Ok((row.get::<_,usize>(0)?,row.get::<_,usize>(1)?,row.get::<_,usize>(2)?,row.get::<_,usize>(3)?)))?, (0,0,0,0));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
