use super::*;
use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        input_queries::{source_capture_for_fixture, source_roles_for_fixture, InputQuerySpec},
        mutation_owner::MutationOwner,
    },
    runtime_db::{
        evidence::{self, upsert_agent_identity_tx, upsert_agent_state_tx},
        RuntimeDb,
    },
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentState, AgentVisibility,
        TranscriptEntryKind,
    },
};
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "input-query-actual-source".into(),
        identity_incarnation: 1,
    }
}
fn policy() -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: MAX_METADATA_BYTES,
        record_limit: MAX_RECORDS,
    }
}
fn fresh() -> Result<(tempfile::TempDir, RuntimeDb, InputEvidenceBudgetLedger)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        capture_input_evidence_upgrade_tx(tx, Utc::now())?;
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
        InputEvidenceBudgetLedger::new(Default::default())?,
    ))
}
fn entry(id: &str) -> TranscriptEntry {
    let mut entry = TranscriptEntry::new(
        scope().agent_id,
        TranscriptEntryKind::AssistantRound,
        Some(0),
        None,
        serde_json::json!({"text":"actual accepted source member"}),
    );
    entry.id = id.into();
    entry
}
fn write(db: &RuntimeDb, ledger: &InputEvidenceBudgetLedger, ids: &[String]) -> Result<()> {
    let rows = ids.iter().map(|id| entry(id)).collect::<Vec<_>>();
    db.transaction(|tx| {
        let proof = native_birth::NativeInputWriteProof::prepare_tx(
            tx,
            &scope(),
            |visit| {
                for row in &rows {
                    visit(Family::Transcript, &row.id)?;
                }
                Ok(())
            },
            ledger,
            (MAX_METADATA_BYTES, MAX_RECORDS),
        )?
        .context("actual installed source absent")?;
        let mut actual = Vec::new();
        for row in &rows {
            let (record, inserted) = evidence::append_transcript_entry_tx(tx, row)?;
            assert!(inserted);
            actual.push(CanonicalInputEvidence::Transcript(record));
        }
        let delta = register_committed_input_evidence_tx(tx, &scope(), &actual)?;
        proof.finish_tx(tx, &delta.records)
    })
}

#[test]
fn input_query_actual_frontier_exceeds_windows_without_body_sql_and_keeps_pruned_births(
) -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let ids = (0..130)
        .map(|i| format!("actual-query-{i:03}"))
        .collect::<Vec<_>>();
    write(&db, &ledger, &ids)?;
    let owner = MutationOwner::standalone(&scope().agent_id, 1);
    let (held, lease, roles) = source_roles_for_fixture(
        &owner,
        &scope(),
        InputQuerySpec {
            birth_after: Some(0),
            ..Default::default()
        },
        &ledger,
        (MAX_METADATA_BYTES, MAX_RECORDS),
    )?;
    let before = ledger.report()?.observed_bytes;
    db.transaction(|tx| {
        let actual_scope = scope();
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &policy(), &ledger, (0, 0))?;
        // Metadata is the original native source, with no canonical body access.
        tx.execute_batch("ALTER TABLE transcript_entries RENAME TO forbidden_query_body")?;
        let source = prepare_tx(
            tx,
            &actual_scope,
            InputSourceRead {
                roles: Some(&roles),
                owner_epoch: held.owner_epoch(),
            },
            &mut material,
        )?;
        assert_eq!(source.coverage.birth_cursor, Some(130));
        assert_eq!(source.coverage.birth_after, Some(0));
        assert_eq!(source.coverage.born_key_seqs.len(), 130);
        assert_eq!(source.roots.len(), 130);
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(
                source
                    .coverage
                    .born_key_seqs
                    .get(&Key::new(Family::Transcript, id)),
                Some(&(u64::try_from(index)? + 1))
            );
        }
        tx.execute_batch("ALTER TABLE forbidden_query_body RENAME TO transcript_entries")?;
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, before);
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM transcript_entries WHERE evidence_id=?1",
            [&ids[129]],
        )?;
        register_pruned_input_evidence_tx(
            tx,
            &scope(),
            &[Key::new(Family::Transcript, &ids[129])],
        )?;
        let actual_scope = scope();
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &policy(), &ledger, (0, 0))?;
        let source = prepare_tx(
            tx,
            &actual_scope,
            InputSourceRead {
                roles: Some(&roles),
                owner_epoch: held.owner_epoch(),
            },
            &mut material,
        )?;
        assert_eq!(
            source.coverage.born_key_seqs.len(),
            130,
            "pruning cannot erase an accepted birth"
        );
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            tx.execute(
                "DELETE FROM input_query_birth_members WHERE record_id=?1",
                [&ids[64]],
            )?;
            let actual_scope = scope();
            let mut material =
                read_material::InputReadMaterial::new(&actual_scope, &policy(), &ledger, (0, 0))?;
            prepare_tx(
                tx,
                &actual_scope,
                InputSourceRead {
                    roles: Some(&roles),
                    owner_epoch: held.owner_epoch(),
                },
                &mut material,
            )
        })
        .err()
        .context("a gap cannot be called a complete frontier")?;
    assert!(
        matches!(
            error.downcast_ref::<InputQueryError>(),
            Some(InputQueryError::Incomplete)
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    drop((roles, lease, held, owner));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn named_input_query_collects_only_actual_bodies_and_preserves_typed_unavailable_and_nul_ids(
) -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let ids = vec![
        "actual-source-a".to_owned(),
        "\0".to_owned(),
        "actual-source-pruned".to_owned(),
    ];
    write(&db, &ledger, &ids)?;
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM transcript_entries WHERE evidence_id=?1",
            [&ids[2]],
        )?;
        register_pruned_input_evidence_tx(tx, &scope(), &[Key::new(Family::Transcript, &ids[2])])?;
        Ok(())
    })?;
    let owner = MutationOwner::standalone(&scope().agent_id, 1);
    let (held, lease, roles) = source_roles_for_fixture(
        &owner,
        &scope(),
        InputQuerySpec {
            birth_after: Some(0),
            ..Default::default()
        },
        &ledger,
        (MAX_METADATA_BYTES, MAX_RECORDS),
    )?;
    let (resident, guards) = db.transaction(|tx| {
        let actual_scope = scope();
        let policy = policy();
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
        let current = match prepare_input_evidence_snapshot_with_query_material_tx(
            tx,
            &actual_scope,
            &policy,
            false,
            &mut material,
            Some(&roles),
            held.owner_epoch(),
        )? {
            InputEvidenceBootstrapSnapshot::Current(current) => current,
            _ => anyhow::bail!("actual scope unexpectedly retired"),
        };
        Ok((
            current.resident.with_budget(ledger.clone())?,
            material.into_guards(),
        ))
    })?;
    drop(guards); // Original named ACK has consumed its temporary preparation.
    db.connection()?
        .execute_batch("ALTER TABLE transcript_entries RENAME TO forbidden_actual_query_body")?;
    let (capture, query, pins) = source_capture_for_fixture(&resident, &lease)?;
    assert_eq!(query.birth_cursor, Some(3));
    assert_eq!(query.born_key_seqs.len(), 3);
    assert!(capture
        .lookup(&Key::new(Family::Transcript, &ids[0]))?
        .is_some());
    assert!(capture
        .lookup(&Key::new(Family::Transcript, &ids[1]))?
        .is_some());
    assert!(capture
        .lookup(&Key::new(Family::Transcript, &ids[2]))?
        .is_none());
    assert!(capture
        .unavailable
        .contains(&Key::new(Family::Transcript, &ids[2])));
    assert_eq!(
        query
            .born_key_seqs
            .get(&Key::new(Family::Transcript, &ids[2])),
        Some(&3)
    );
    let strict = InputEvidenceLoadRequest {
        mandatory_roots: BTreeSet::from([Key::new(Family::Transcript, &ids[2])]),
        ..policy()
    };
    let error = resident
        .capture_for_request(&strict)
        .err()
        .context("query observation cannot weaken original mandatory-body capture")?;
    assert!(
        format!("{error:#}").contains("mandatory evidence unavailable"),
        "{error:#}"
    );
    db.connection()?
        .execute_batch("ALTER TABLE forbidden_actual_query_body RENAME TO transcript_entries")?;
    // The same key on both roles must still obey the original body contract.
    let error = db
        .transaction(|tx| {
            let actual_scope = scope();
            let mut material =
                read_material::InputReadMaterial::new(&actual_scope, &strict, &ledger, (0, 0))?;
            prepare_input_evidence_snapshot_with_query_material_tx(
                tx,
                &actual_scope,
                &strict,
                false,
                &mut material,
                Some(&roles),
                held.owner_epoch(),
            )
        })
        .err()
        .context("born Pruned cannot mask an original mandatory root")?;
    assert!(
        format!("{error:#}").contains("mandatory evidence unavailable"),
        "{error:#}"
    );
    let unqualified = entry("raw-late-query");
    db.transaction(|tx| evidence::append_transcript_entry_tx(tx, &unqualified))?;
    let required = [Key::new(Family::Transcript, &unqualified.id)];
    drop((roles, lease, held));
    let (held, lease, roles) = source_roles_for_fixture(
        &owner,
        &scope(),
        InputQuerySpec {
            required_keys: &required,
            ..Default::default()
        },
        &ledger,
        (MAX_METADATA_BYTES, MAX_RECORDS),
    )?;
    let before = ledger.report()?.observed_bytes;
    let error = db
        .transaction(|tx| {
            let actual_scope = scope();
            let policy = policy();
            let mut material =
                read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
            prepare_input_evidence_snapshot_with_query_material_tx(
                tx,
                &actual_scope,
                &policy,
                false,
                &mut material,
                Some(&roles),
                held.owner_epoch(),
            )
        })
        .err()
        .context("raw late address must remain outside original qualification")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::NeedsPreparation(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_evidence_current WHERE record_id=?1",
            [&unqualified.id],
            |r| r.get::<_, usize>(0)
        )?,
        0
    );
    drop((capture, query, pins, resident, roles, lease, held, owner));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn input_query_actual_selector_prior_is_counted_once_and_scope_failures_do_not_create_ready(
) -> Result<()> {
    let (_home, db, ledger) = fresh()?;
    let owner = MutationOwner::standalone(&scope().agent_id, 1);
    let required = [Key::new(
        Family::Transcript,
        "long-actual-selector".repeat(128),
    )];
    let (held, lease, roles) = source_roles_for_fixture(
        &owner,
        &scope(),
        InputQuerySpec {
            required_keys: &required,
            ..Default::default()
        },
        &ledger,
        (MAX_METADATA_BYTES, MAX_RECORDS),
    )?;
    let actual_scope = scope();
    let policy = policy();
    let initial =
        read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?.usage();
    let mut original = (0u64, 0usize);
    roles.visit_material(|_, bytes, records| {
        original.0 += bytes;
        original.1 += records;
        Ok(())
    })?;
    let mut limited = policy.clone();
    limited.byte_limit = initial.0 + usize::try_from(original.0)? - 1;
    assert!(initial.0 < limited.byte_limit && usize::try_from(original.0)? < limited.byte_limit);
    let baseline = ledger.report()?.observed_bytes;
    let error = {
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &limited, &ledger, (0, 0))?;
        material
            .observe_query_prior(&roles, held.owner_epoch())
            .err()
            .context("held selectors received another full allowance")?
    };
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    {
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
        material.observe_query_prior(&roles, held.owner_epoch())?;
        let once = material.usage();
        material.observe_query_prior(&roles, held.owner_epoch())?;
        assert_eq!(material.usage(), once);
        assert_eq!(
            once,
            (
                initial.0 + usize::try_from(original.0)?,
                initial.1 + original.1
            )
        );
    }
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    {
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
        assert!(material
            .observe_query_prior(&roles, held.owner_epoch() + 1)
            .is_err());
    }
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let other = InputEvidenceBudgetLedger::new(Default::default())?;
    {
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &policy, &other, (0, 0))?;
        assert!(material
            .observe_query_prior(&roles, held.owner_epoch())
            .is_err());
    }
    assert_eq!(other.report()?.observed_bytes, 0);
    db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&actual_scope.agent_id],
    )?;
    let error = db
        .transaction(|tx| {
            let mut material =
                read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
            prepare_input_evidence_snapshot_with_query_material_tx(
                tx,
                &actual_scope,
                &policy,
                false,
                &mut material,
                Some(&roles),
                held.owner_epoch(),
            )
        })
        .err()
        .context("NoState source must not become Ready")?;
    assert!(
        matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::NeedsPreparation(message)) if message=="Input query owner has no State"),
        "{error:#}"
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_query_birth_cursors", [], |r| {
                r.get::<_, usize>(0)
            })?,
        0
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let mut successor = AgentIdentityRecord::new(
        scope().agent_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    successor.incarnation = 2;
    db.transaction(|tx| {
        upsert_agent_identity_tx(tx, &successor)?;
        upsert_agent_state_tx(tx, &AgentState::new(scope().agent_id))?;
        tx.execute(
            "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
            params![
                "[".to_owned() + &"x".repeat(9 * 1024 * 1024),
                actual_scope.agent_id
            ],
        )?;
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
        let error = prepare_input_evidence_snapshot_with_query_material_tx(
            tx,
            &actual_scope,
            &policy,
            false,
            &mut material,
            Some(&roles),
            held.owner_epoch(),
        )
        .err()
        .context("materialized old scope must fail before successor State")?;
        assert!(
            matches!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::RetiredScope)
            ),
            "{error:#}"
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_query_birth_cursors", [], |r| {
                r.get::<_, usize>(0)
            })?,
        0
    );
    drop((roles, lease, held, owner));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn original_named_query_capture_keeps_lease_and_metadata_in_the_actual_input_phase() -> Result<()> {
    use crate::runtime::{
        input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate},
        input_queries::source_capture_with_limits_for_fixture,
        resident_input_evidence::InputCaptureError,
    };
    let (_home, db, ledger) = fresh()?;
    let ids = vec!["joint-original-a".to_owned(), "joint-original-b".to_owned()];
    write(&db, &ledger, &ids)?;
    let owner = MutationOwner::standalone(&scope().agent_id, 1);
    let (held, lease, roles) = source_roles_for_fixture(
        &owner,
        &scope(),
        InputQuerySpec {
            birth_after: Some(0),
            ..Default::default()
        },
        &ledger,
        (MAX_METADATA_BYTES, MAX_RECORDS),
    )?;
    let resident = db.transaction(|tx| {
        let actual_scope = scope();
        let policy = policy();
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
        let snapshot = prepare_input_evidence_snapshot_with_query_material_tx(
            tx,
            &actual_scope,
            &policy,
            false,
            &mut material,
            Some(&roles),
            held.owner_epoch(),
        )?;
        let InputEvidenceBootstrapSnapshot::Current(current) = snapshot else {
            anyhow::bail!("original named preparation unexpectedly retired")
        };
        current.resident.with_budget(ledger.clone())
    })?;
    let full = (MAX_METADATA_BYTES, MAX_RECORDS);
    let (capture, query, guard, own) =
        source_capture_with_limits_for_fixture(&resident, &lease, full, (0, 0))?;
    assert_eq!(capture.records.len(), 2);
    assert_eq!(query.birth_cursor, Some(2));
    drop((capture, query, guard));
    let prior = (128 * 1024u64, 32usize);
    let prior_guard = ledger.register_owned_bytes(
        resident.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let physical = vec![6u8; usize::try_from(prior.0)?];
    let baseline = ledger.report()?.observed_bytes;
    db.connection()?.execute_batch(
        "ALTER TABLE transcript_entries RENAME TO query_joint_forbidden_transcript;
         ALTER TABLE messages RENAME TO query_joint_forbidden_messages;
         ALTER TABLE agent_states RENAME TO query_joint_forbidden_state;",
    )?;
    // Find the exact original algorithm's live peak, including scratch. The
    // returned own estimate intentionally excludes already-destroyed scratch.
    let accepts = |cap| -> Result<bool> {
        let accepted = match source_capture_with_limits_for_fixture(&resident, &lease, cap, prior) {
            Ok(value) => {
                drop(value);
                true
            }
            Err(error) => {
                ensure!(
                    matches!(
                        error.downcast_ref::<InputQueryError>(),
                        Some(InputQueryError::ResourceExhausted(_))
                    ) || matches!(
                        error.downcast_ref::<InputCaptureError>(),
                        Some(InputCaptureError::ResourceExhausted)
                    ),
                    "{error:#}"
                );
                false
            }
        };
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        Ok(accepted)
    };
    assert!(accepts(full)?);
    let (mut low, mut high) = (1usize, full.0);
    while low < high {
        let midpoint = low + (high - low) / 2;
        if accepts((midpoint, full.1))? {
            high = midpoint;
        } else {
            low = midpoint + 1;
        }
    }
    let bytes = low;
    let (mut low, mut high) = (1usize, full.1);
    while low < high {
        let midpoint = low + (high - low) / 2;
        if accepts((bytes, midpoint))? {
            high = midpoint;
        } else {
            low = midpoint + 1;
        }
    }
    let exact = (bytes, low);
    assert!(exact.0 >= usize::try_from(own.0 + prior.0)? && exact.1 >= own.1 + prior.1);
    assert!(!accepts((exact.0 - 1, exact.1))?);
    assert!(!accepts((exact.0, exact.1 - 1))?);
    let (capture, query, guard, actual) =
        source_capture_with_limits_for_fixture(&resident, &lease, exact, prior)?;
    assert_eq!(
        actual, own,
        "caller prior or request limits entered own material"
    );
    assert_eq!(query.born_key_seqs.len(), 2);
    let last = guard.clone();
    drop((capture, query, guard));
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop((prior_guard, physical, resident, roles, lease, held, owner));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn logical_body_bytes_do_not_replace_the_actual_capture_phase_allowance() -> Result<()> {
    use crate::runtime::{
        input_queries::source_capture_with_limits_for_fixture,
        resident_input_evidence::InputCaptureError,
    };
    let (_home, db, ledger) = fresh()?;
    let ids = vec!["logical-small-original".to_owned()];
    write(&db, &ledger, &ids)?;
    let owner = MutationOwner::standalone(&scope().agent_id, 1);
    let (held, lease, roles) = source_roles_for_fixture(
        &owner,
        &scope(),
        InputQuerySpec {
            birth_after: Some(0),
            ..Default::default()
        },
        &ledger,
        (MAX_METADATA_BYTES, MAX_RECORDS),
    )?;
    let mut logical = policy();
    logical.byte_limit = 4096;
    logical.record_limit = 8;
    let key = Key::new(Family::Transcript, &ids[0]);
    logical.mandatory_roots.insert(key.clone());
    // The real original SQL actor owns its explicit material phase. Its
    // selected body's logical policy is independently carried into the view.
    let resident = db.transaction(|tx| {
        let actual_scope = scope();
        let phase_policy = policy();
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, &phase_policy, &ledger, (0, 0))?;
        let snapshot = prepare_input_evidence_snapshot_with_query_material_tx(
            tx,
            &actual_scope,
            &logical,
            false,
            &mut material,
            Some(&roles),
            held.owner_epoch(),
        )?;
        let InputEvidenceBootstrapSnapshot::Current(current) = snapshot else {
            anyhow::bail!("actual logical source unexpectedly retired")
        };
        current.resident.with_budget(ledger.clone())
    })?;
    db.connection()?.execute_batch(
        "ALTER TABLE transcript_entries RENAME TO logical_capture_forbidden_transcript;
         ALTER TABLE messages RENAME TO logical_capture_forbidden_messages;
         ALTER TABLE agent_states RENAME TO logical_capture_forbidden_state;",
    )?;
    let baseline = ledger.report()?.observed_bytes;
    let all = resident.capture()?;
    let selected = resident.capture_for_request(&logical)?;
    assert_eq!(all.records.len(), 1);
    assert_eq!(selected.records.len(), 1);
    assert_eq!(selected.records[0].locator.key, key);
    assert!(Arc::ptr_eq(&all.records[0], &selected.records[0]));
    drop((all, selected));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let (capture, query, guard) = source_capture_for_fixture(&resident, &lease)?;
    assert_eq!(capture.records.len(), 1);
    assert_eq!(query.birth_cursor, Some(1));
    drop((capture, query, guard));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    for query_capture in [false, true] {
        let error = if query_capture {
            source_capture_with_limits_for_fixture(&resident, &lease, (4096, MAX_RECORDS), (0, 0))
                .err()
        } else {
            resident
                .capture_for_request_with_material(
                    &logical,
                    false,
                    (4096, MAX_RECORDS),
                    (0, 0),
                    &ledger,
                )
                .err()
        }
        .context("explicit phase limit was replaced by the implicit ledger allowance")?;
        assert!(
            matches!(
                error.downcast_ref::<InputCaptureError>(),
                Some(InputCaptureError::ResourceExhausted)
            ) || matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let mut too_small = logical.clone();
    too_small.byte_limit = 1;
    let error = resident
        .capture_for_request(&too_small)
        .err()
        .context("original logical selected-body limit was discarded")?;
    assert!(
        error
            .to_string()
            .contains("input evidence resident byte limit exceeded"),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop((resident, roles, lease, held, owner));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
