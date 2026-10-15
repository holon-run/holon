use super::*;
use crate::runtime::resident_input_evidence::InputEvidenceFamily;
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "actual-selector-owner".into(),
        identity_incarnation: 1,
    }
}
fn key(id: &str) -> InputEvidenceKey {
    InputEvidenceKey::new(InputEvidenceFamily::Message, id)
}
const LIMITS: (usize, usize) = (1024 * 1024, 4096);
#[test]
fn actual_live_input_roles_merge_earliest_cursor_and_retain_original_leases() -> Result<()> {
    let registry = InputQueryRegistry::default();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let first = registry.register(
        &scope(),
        7,
        InputQuerySpec {
            required_keys: &[key("first")],
            birth_after: Some(17),
        },
        &ledger,
        LIMITS,
    )?;
    let second = registry.register(
        &scope(),
        7,
        InputQuerySpec {
            required_keys: &[key("second")],
            birth_after: Some(31),
        },
        &ledger,
        LIMITS,
    )?;
    let old = registry.register(
        &scope(),
        6,
        InputQuerySpec {
            required_keys: &[key("old-epoch")],
            birth_after: Some(1),
        },
        &ledger,
        LIMITS,
    )?;
    let snapshot = registry.snapshot(&scope(), 7, &ledger, LIMITS)?;
    assert_eq!(snapshot.selection.birth_after, Some(17));
    assert_eq!(
        snapshot.selection.required_keys,
        BTreeSet::from([key("first"), key("second")])
    );
    let weak = Arc::downgrade(&first.0);
    drop(first);
    assert!(weak.upgrade().is_some());
    drop(snapshot);
    assert!(weak.upgrade().is_none());
    let snapshot = registry.snapshot(&scope(), 7, &ledger, LIMITS)?;
    assert_eq!(snapshot.selection.birth_after, Some(31));
    assert_eq!(
        snapshot.selection.required_keys,
        BTreeSet::from([key("second")])
    );
    drop(snapshot);
    drop(second);
    drop(old);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn input_selector_failure_precedes_first_copy_and_keeps_no_dead_budget() -> Result<()> {
    let registry = InputQueryRegistry::default();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let oversized = key(&"selector".repeat(8192));
    assert!(registry
        .register(
            &scope(),
            1,
            InputQuerySpec {
                required_keys: &[oversized],
                birth_after: Some(0)
            },
            &ledger,
            (4096, 4096)
        )
        .is_err());
    assert!(registry.0.slots.lock().unwrap().is_empty());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let lease = registry.register(&scope(), 1, InputQuerySpec::default(), &ledger, LIMITS)?;
    let held = registry.0.slots.lock().unwrap();
    drop(lease);
    assert_eq!(held.len(), 1);
    assert_eq!(
        ledger.report()?.observed_bytes,
        serialized_bytes(&scope())? * 4 + 512,
        "only the actual dead map slot and its scope remain observed"
    );
    drop(held);
    assert!(registry.snapshot(&scope(), 1, &ledger, (1, 1)).is_err());
    assert_eq!(
        ledger.report()?.observed_bytes,
        0,
        "cleanup precedes failed admission"
    );
    let lease = registry.register(&scope(), 1, InputQuerySpec::default(), &ledger, LIMITS)?;
    drop(lease);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn input_query_owner_scope_registry_and_actual_ledger_are_all_exact() -> Result<()> {
    let registry = InputQueryRegistry::default();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let other = InputEvidenceBudgetLedger::new(Default::default())?;
    let lease = registry.register(&scope(), 7, InputQuerySpec::default(), &ledger, LIMITS)?;
    assert!(registry
        .validate_lease(&lease, &scope(), 7, &ledger)
        .is_ok());
    assert!(registry
        .validate_lease(&lease, &scope(), 8, &ledger)
        .is_err());
    assert!(registry
        .validate_lease(&lease, &scope(), 7, &other)
        .is_err());
    assert!(InputQueryRegistry::default()
        .validate_lease(&lease, &scope(), 7, &ledger)
        .is_err());
    let mut successor = scope();
    successor.identity_incarnation += 1;
    assert!(registry
        .validate_lease(&lease, &successor, 7, &ledger)
        .is_err());
    assert!(registry.snapshot(&scope(), 7, &other, LIMITS).is_err());
    assert_eq!(other.report()?.observed_bytes, 0);
    drop(lease);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn input_query_coverage_requires_a_complete_accepted_birth_frontier() -> Result<()> {
    let selection = InputQuerySelection {
        required_keys: BTreeSet::new(),
        birth_after: Some(17),
    };
    assert!(InputQueryCoverage::default()
        .validate_selection(&selection)
        .is_err());
    let mut coverage = InputQueryCoverage {
        required_keys: BTreeSet::new(),
        birth_cursor: Some(100_001),
        birth_after: Some(17),
        born_key_seqs: BTreeMap::from([(key("accepted-new"), 100_001)]),
    };
    coverage.validate_selection(&selection)?;
    coverage.birth_after = Some(18);
    assert!(coverage.validate_selection(&selection).is_err());
    coverage.birth_after = Some(17);
    coverage.born_key_seqs.insert(key("bad-old"), 17);
    assert!(coverage.validate_selection(&selection).is_err());
    coverage.born_key_seqs.remove(&key("bad-old"));
    coverage.birth_cursor = None;
    assert!(coverage.validate_selection(&selection).is_err());
    Ok(())
}

#[tokio::test]
async fn actual_held_root_input_registration_rejects_unprepared_birth_and_stale_foreign_roots(
) -> Result<()> {
    use crate::runtime::{
        tests::support::{context_config, tempdir},
        RuntimeHandle,
    };
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "input-birth-held-source",
        home.path().into(),
        context_config(),
    )?;
    runtime.quiesce_context_resource_observer_task_for_test().await?;
    // A real unsupported evidence ACK invalidates the genuine bootstrap
    // cursor. This exercises the missing Source gate without editing a DTO.
    let entry=crate::types::TranscriptEntry::new(runtime.instance_agent_id(),
        crate::types::TranscriptEntryKind::AssistantRound,Some(0),None,
        serde_json::json!({"text":"actual ACK establishes incomplete Input"}));
    runtime.persist_transcript_evidence(&entry).await?;
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let original = reservation
        .capture_ready_snapshot()?
        .context("actual Input root")?;
    let error = runtime
        .register_input_birth_session_from_snapshot(&reservation, &original)
        .err()
        .context("unpublished birth metadata cannot become a ready baseline")?;
    assert!(format!("{error:#}").contains("incomplete"), "{error:#}");
    drop(reservation);
    let created = runtime
        .create_work_item_owned_result("actual later publication".into(), None, None, Vec::new())
        .await?;
    drop(created);
    runtime.stop_context_resource_observer().await?;
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    assert!(runtime
        .register_input_birth_session_from_snapshot(&reservation, &original)
        .is_err());
    let current = reservation
        .capture_ready_snapshot()?
        .context("actual later Input root")?;
    let other_home = tempdir()?;
    let other = RuntimeHandle::new_isolated_resource_fixture(
        "input-birth-foreign-source",
        other_home.path().into(),
        context_config(),
    )?;
    other.stop_context_resource_observer().await?;
    let other_reservation = other.inner.mutation_owner.reserve().await?;
    let foreign = other_reservation
        .capture_ready_snapshot()?
        .context("actual foreign Input root")?;
    assert!(runtime
        .register_input_birth_session_from_snapshot(&reservation, &foreign)
        .is_err());
    runtime.inner.runtime_db.connection()?.execute_batch(
        "DROP TABLE messages; DROP TABLE agent_states; DROP TABLE agent_identities;",
    )?;
    let error = runtime
        .register_input_birth_session_from_snapshot(&reservation, &current)
        .err()
        .context("Input source stays incomplete under denied body tables")?;
    assert!(format!("{error:#}").contains("incomplete"), "{error:#}");
    drop(other_reservation);
    drop(reservation);
    Ok(())
}

#[test]
fn actual_accepted_high_water_proves_empty_interval_without_a_prior_live_role() -> Result<()> {
    let coverage = InputQueryCoverage {
        required_keys: BTreeSet::new(),
        birth_cursor: Some(73),
        birth_after: None,
        born_key_seqs: BTreeMap::new(),
    };
    coverage.validate_selection(&InputQuerySelection {
        required_keys: BTreeSet::new(),
        birth_after: Some(73),
    })?;
    assert!(coverage
        .validate_selection(&InputQuerySelection {
            required_keys: BTreeSet::new(),
            birth_after: Some(72)
        })
        .is_err());
    assert!(coverage
        .validate_selection(&InputQuerySelection {
            required_keys: BTreeSet::new(),
            birth_after: Some(74)
        })
        .is_err());
    Ok(())
}

#[tokio::test]
async fn original_owner_slot_shares_input_roles_without_runtime_and_fences_replacement() -> Result<()> {
    use crate::runtime::mutation_owner::MutationOwnerRegistry;
    let owners=MutationOwnerRegistry::default();
    let scope=scope();
    let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let owner=owners.claim(&scope.agent_id,scope.identity_incarnation)?;
    let reserved=owner.reserve().await?;
    assert!(reserved.input_query_roles(&scope,&ledger,LIMITS)?.is_none());
    let lease=owner.input_queries()?.register(&scope,reserved.owner_epoch(),InputQuerySpec{
        required_keys:&[key("actual-held-input")],birth_after:Some(7),
    },&ledger,LIMITS)?;
    let weak=Arc::downgrade(&lease.0);
    drop(reserved);
    // This is the same Host owner slot; no Runtime/root/DB is constructed.
    let cold_owner=owners.current_owner(&scope.agent_id,scope.identity_incarnation)?;
    let reserved=cold_owner.reserve().await?;
    let snapshot=reserved.input_query_roles(&scope,&ledger,LIMITS)?.context("cold actual owner lost its live role")?;
    snapshot.validate_binding(&scope,reserved.owner_epoch(),&ledger)?;
    assert_eq!(snapshot.selection.birth_after,Some(7));
    assert_eq!(snapshot.selection.required_keys,BTreeSet::from([key("actual-held-input")]));
    let other=InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(reserved.input_query_roles(&scope,&other,LIMITS).is_err());
    assert!(snapshot.validate_binding(&scope,reserved.owner_epoch()+1,&ledger).is_err());
    let mut successor_scope=scope.clone();successor_scope.identity_incarnation+=1;
    assert!(reserved.input_query_roles(&successor_scope,&ledger,LIMITS).is_err());
    assert_eq!(other.report()?.observed_bytes,0);
    drop(lease);assert!(weak.upgrade().is_some(),"actual snapshot owns the original selector lifetime");
    drop(snapshot);assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes,0);
    let old=owner.input_queries()?.register(&scope,reserved.owner_epoch(),InputQuerySpec::default(),&ledger,LIMITS)?;
    drop(reserved);
    let replacement=owners.claim(&scope.agent_id,scope.identity_incarnation)?;
    assert!(owner.input_queries().is_err());
    let reserved=replacement.reserve().await?;
    assert!(reserved.input_query_roles(&scope,&ledger,LIMITS)?.is_none(),"a same-incarnation new owner cannot inherit old-epoch roles");
    assert!(replacement.input_queries()?.validate_lease(&old,&scope,reserved.owner_epoch(),&ledger).is_err());
    drop((old,reserved,replacement,cold_owner,owner));
    assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}


#[test]
fn input_birth_metadata_streams_struct_keys_and_old_empty_metadata_stays_incomplete() -> Result<()> {
    let actual = InputQueryCoverage {
        required_keys: BTreeSet::from([key("\0")]),
        birth_cursor: Some(2),
        birth_after: Some(0),
        born_key_seqs: BTreeMap::from([(key("\0"), 1), (key("native-second"), 2)]),
    };
    let encoded = serde_json::to_string(&actual)?;
    assert_eq!(serialized_bytes(&actual)?, u64::try_from(encoded.len())?);
    assert_eq!(serde_json::from_str::<InputQueryCoverage>(&encoded)?, actual);
    let old = serde_json::from_value::<InputQueryCoverage>(serde_json::json!({
        "birth_cursor":null,"birth_after":null,"born_key_seqs":{}
    }))?;
    assert!(old.required_keys.is_empty());
    assert!(old.validate_selection(&InputQuerySelection::default()).is_err());
    let duplicated = serde_json::json!({
        "birth_cursor":2,"birth_after":0,
        "born_key_seqs":[[key("same"),1],[key("same"),2]]
    });
    assert!(serde_json::from_value::<InputQueryCoverage>(duplicated).is_err());
    Ok(())
}
