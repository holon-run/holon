use super::*;
use crate::domain::execution_protocol::{
    ExecutionAttemptState, ExecutionOrigin, ExecutionPriority, ExecutionProvenance,
    ExecutionSourceIdentity, ExecutionTrust,
};

mod auth_contract;
mod payment_required;
mod repair_blocks;
mod revision_diagnostics;

fn now() -> DateTime<Utc> {
    "2026-10-24T17:00:00Z".parse().unwrap()
}

fn reopen(dir: &tempfile::TempDir) -> Result<RuntimeDb> {
    RuntimeDb::open_and_migrate(
        &dir.path().join("runtime.sqlite"),
        &dir.path().join("runtime.lock"),
    )
}

fn database() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let db = reopen(&dir)?;
    Ok((dir, db))
}

fn root() -> ExecutionAttempt {
    ExecutionAttempt {
        attempt_id: "root-attempt".into(),
        agent_id: "agent-a".into(),
        source_message_id: Some("operator-message".into()),
        source: ExecutionSource {
            identity: ExecutionSourceIdentity::QueueMessage {
                message_id: "operator-message".into(),
            },
            generation: 1,
        },
        binding: ExecutionBinding::WorkItem {
            work_item_id: "work-a".into(),
        },
        provenance: ExecutionProvenance {
            origin: ExecutionOrigin::Operator,
            trust: ExecutionTrust::OperatorInstruction,
            priority: ExecutionPriority::Normal,
            correlation_id: None,
            causation_id: None,
        },
        admitted_fences: AdmittedFences {
            source_revision: 1,
            work_item_source_revision: Some(1),
            work_item_generation: Some(1),
            rejoin: None,
            agent_control_revision: 1,
            host_registry_revision: 1,
        },
        state: ExecutionAttemptState::Open,
        run_id: None,
        turn_id: None,
        recovery_of_attempt_id: None,
        input_projection: None,
        terminal_outcome_id: None,
        admitted_at: timestamp(now()),
        terminal_at: None,
    }
}

fn binding() -> ProviderRecoveryBinding {
    ProviderRecoveryBinding::from_root_attempt(&root(), "chain-v1").unwrap()
}

fn transport(value: &str) -> ProviderAdmissionScope {
    ProviderAdmissionScope::transport("connection", value).unwrap()
}

fn quota(value: &str) -> ProviderAdmissionScope {
    ProviderAdmissionScope::quota(&ProviderQuotaIdentity::exact("test", value).unwrap())
}

fn lease(db: &RuntimeDb, scopes: &[ProviderAdmissionScope]) -> Result<ProviderAdmissionLease> {
    match db.reserve_provider_admission(scopes, now(), Duration::minutes(2))? {
        ProviderAdmissionReservation::Granted(lease) => Ok(lease),
        _ => anyhow::bail!("expected admission"),
    }
}

fn episode(db: &RuntimeDb, kind: ProviderRecoveryBudgetKind) -> Result<ProviderRecoveryEpisode> {
    db.open_provider_recovery_episode(&binding(), kind, now())
}

fn refresh(db: &RuntimeDb, episode: &ProviderRecoveryEpisode) -> Result<ProviderRecoveryEpisode> {
    db.provider_recovery_episode(episode.key())?
        .context("episode missing")
}

fn charge(
    db: &RuntimeDb,
    episode: &ProviderRecoveryEpisode,
    lease: &ProviderAdmissionLease,
    id: &str,
    cursor: u32,
) -> Result<ProviderRecoveryDispatch> {
    match db.charge_provider_recovery_dispatch(episode, lease, id, cursor, now())? {
        ProviderRecoveryDispatchResult::Charged(dispatch) => Ok(dispatch),
        denied => anyhow::bail!("expected charged dispatch: {denied:?}"),
    }
}

fn assert_denied(result: ProviderRecoveryDispatchResult, reason: ProviderRecoveryDispatchDenial) {
    match result {
        ProviderRecoveryDispatchResult::Denied(actual) => assert_eq!(actual, reason),
        other => panic!("expected {reason:?}, got {other:?}"),
    }
}

fn canonical_root(db: &RuntimeDb) -> Result<ExecutionAttempt> {
    use crate::domain::execution_protocol::ExecutionProtocolState;
    use crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx;
    use crate::types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility,
    };

    let mut attempt = root();
    db.agent_states()
        .upsert_for_fixture(&AgentState::new(&attempt.agent_id))?;
    db.agent_identities().upsert(&AgentIdentityRecord::new(
        &attempt.agent_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    ))?;
    let authority = db
        .transitions()
        .load_execution_authority_fences(&attempt.agent_id)?;
    attempt.admitted_fences.agent_control_revision = authority.agent_control_revision;
    attempt.admitted_fences.host_registry_revision = authority.host_registry_revision;
    let mut work = WorkItemRecord::new(&attempt.agent_id, "provider recovery", WorkItemState::Open);
    work.id = "work-a".into();
    db.work_items().insert_new(&work)?;
    let mut protocol = ExecutionProtocolState::empty(&attempt.agent_id);
    protocol.work_items.insert(
        work.id,
        WorkItemExecutionRecord {
            source_revision: 1,
            state: WorkItemExecutionState::InFlight {
                generation: 1,
                attempt_id: attempt.attempt_id.clone(),
            },
        },
    );
    protocol
        .attempts
        .insert(attempt.attempt_id.clone(), attempt.clone());
    db.transaction(|tx| persist_state_tx(tx, &protocol))?;
    Ok(attempt)
}

fn canonical_episode(
    db: &RuntimeDb,
    attempt: &ExecutionAttempt,
) -> Result<ProviderRecoveryEpisode> {
    db.open_provider_recovery_episode(
        &ProviderRecoveryBinding::from_root_attempt(attempt, "chain-v1")?,
        ProviderRecoveryBudgetKind::Network,
        now(),
    )
}

fn assert_not_charged(db: &RuntimeDb, episode: &ProviderRecoveryEpisode) -> Result<()> {
    let persisted = refresh(db, episode)?;
    assert_eq!(persisted.request_count, 0);
    assert_eq!(persisted.total_requests, 0);
    assert_eq!(persisted.candidate_cursor, 0);
    db.with_read(|connection| {
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM provider_recovery_requests WHERE episode_key = ?1",
            [episode.key()],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
        Ok(())
    })
}

fn dispatch_authorization_snapshot(db: &RuntimeDb) -> Result<Vec<(String, String)>> {
    db.with_read(|connection| {
        let mut query = connection.prepare(
            "SELECT 'attempt:' || agent_id || ':' || attempt_id || ':' || lifecycle_state
                      || ':' || source_generation, payload_json FROM execution_protocol_attempts
             UNION ALL
             SELECT 'episode:' || episode_key, binding_json FROM provider_recovery_episodes
             UNION ALL
             SELECT 'lane:' || agent_id || ':' || work_item_id, payload_json
               FROM execution_protocol_work_items
             UNION ALL
             SELECT 'owner:' || agent_id || ':' || work_item_id, payload_json FROM work_items
             ORDER BY 1",
        )?;
        let records = query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records)
    })
}

fn assert_dispatch_rejected(
    db: &RuntimeDb,
    episode: &ProviderRecoveryEpisode,
    admission: &ProviderAdmissionLease,
    attempt: &ExecutionAttempt,
    reason: ProviderDispatchRejection,
) -> Result<()> {
    let before = dispatch_authorization_snapshot(db)?;
    assert_denied(
        db.charge_provider_recovery_dispatch_for_attempt(
            episode,
            admission,
            "rejected-request",
            1,
            attempt,
            now(),
        )?,
        ProviderRecoveryDispatchDenial::StaleExecution(reason),
    );
    assert_not_charged(db, episode)?;
    assert_eq!(dispatch_authorization_snapshot(db)?, before);
    Ok(())
}

fn store_dispatch_attempt(db: &RuntimeDb, attempt: &ExecutionAttempt) -> Result<()> {
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO execution_protocol_attempts (
               agent_id, attempt_id, lifecycle_state, source_identity_json,
               source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(agent_id, attempt_id) DO UPDATE SET
               lifecycle_state = excluded.lifecycle_state,
               source_identity_json = excluded.source_identity_json,
               source_generation = excluded.source_generation,
               recovery_of_attempt_id = excluded.recovery_of_attempt_id,
               terminal_outcome_id = excluded.terminal_outcome_id,
               payload_json = excluded.payload_json",
            params![
                attempt.agent_id,
                attempt.attempt_id,
                serde_json::to_value(attempt.state)?.as_str().unwrap(),
                serde_json::to_string(&attempt.source.identity)?,
                attempt.source.generation,
                attempt.recovery_of_attempt_id,
                attempt.terminal_outcome_id,
                serde_json::to_string(attempt)?,
            ],
        )?;
        Ok(())
    })
}

#[test]
fn canonical_dispatch_survives_restart_and_benign_control_revision_changes() -> Result<()> {
    use crate::types::AgentState;
    let (dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let episode = canonical_episode(&db, &attempt)?;
    let admission = lease(&db, &[transport("canonical")])?;
    db.agent_states()
        .upsert_for_fixture(&AgentState::new(&attempt.agent_id))?;
    drop(db);
    let db = reopen(&dir)?;
    assert!(matches!(
        db.charge_provider_recovery_dispatch_for_attempt(
            &episode,
            &admission,
            "canonical-request",
            2,
            &attempt,
            now(),
        )?,
        ProviderRecoveryDispatchResult::Charged(_)
    ));
    assert_eq!(refresh(&db, &episode)?.request_count, 1);
    Ok(())
}

#[test]
fn preclaim_guard_accepts_canonical_run_and_turn_ids_filled_before_dispatch() -> Result<()> {
    use crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx;

    let (dir, db) = database()?;
    let preclaim = canonical_root(&db)?;
    let episode = canonical_episode(&db, &preclaim)?;
    let admission = lease(&db, &[transport("postclaim")])?;
    let mut protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized(&preclaim.agent_id)?
        .unwrap();
    let postclaim = protocol.attempts.get_mut(&preclaim.attempt_id).unwrap();
    postclaim.run_id = Some("claimed-run".into());
    postclaim.turn_id = Some("started-turn".into());
    assert_eq!(
        ProviderRecoveryBinding::from_root_attempt(postclaim, "chain-v1")?,
        ProviderRecoveryBinding::from_root_attempt(&preclaim, "chain-v1")?,
    );
    db.transaction(|tx| persist_state_tx(tx, &protocol))?;
    drop(db);
    let db = reopen(&dir)?;
    assert!(matches!(
        db.charge_provider_recovery_dispatch_for_attempt(
            &episode,
            &admission,
            "postclaim-request",
            1,
            &preclaim,
            now(),
        )?,
        ProviderRecoveryDispatchResult::Charged(_)
    ));
    assert_eq!(refresh(&db, &episode)?.request_count, 1);
    Ok(())
}

#[test]
fn changed_canonical_attempt_or_unpersisted_claim_is_denied_without_charging() -> Result<()> {
    for (case, reason) in [
        ("missing", ProviderDispatchRejection::AttemptMissing),
        ("source", ProviderDispatchRejection::SnapshotSourceMismatch),
        (
            "source_message",
            ProviderDispatchRejection::SnapshotSourceMismatch,
        ),
        ("binding", ProviderDispatchRejection::SnapshotOwnerMismatch),
        ("fences", ProviderDispatchRejection::SnapshotFencesMismatch),
        (
            "provenance",
            ProviderDispatchRejection::SnapshotProvenanceMismatch,
        ),
        (
            "ancestry",
            ProviderDispatchRejection::SnapshotAncestryMismatch,
        ),
        ("closed", ProviderDispatchRejection::AttemptNotOpen),
        ("snapshot_closed", ProviderDispatchRejection::AttemptNotOpen),
    ] {
        let (_dir, db) = database()?;
        let attempt = canonical_root(&db)?;
        let episode = canonical_episode(&db, &attempt)?;
        let admission = lease(&db, &[transport(case)])?;
        let mut supplied = attempt.clone();
        let mut changed = attempt.clone();
        match case {
            "missing" => supplied.attempt_id = "not-persisted".into(),
            "source" => {
                changed.source.generation += 1;
                changed.admitted_fences.source_revision += 1;
            }
            "source_message" => changed.source_message_id = Some("different-message".into()),
            "binding" => changed.binding = ExecutionBinding::Command,
            "fences" => changed.admitted_fences.host_registry_revision += 1,
            "provenance" => changed.provenance.trust = ExecutionTrust::RuntimeInstruction,
            "ancestry" => changed.recovery_of_attempt_id = Some("other-root".into()),
            "closed" => changed.state = ExecutionAttemptState::Interrupted,
            "snapshot_closed" => supplied.state = ExecutionAttemptState::Interrupted,
            _ => unreachable!(),
        }
        if case != "missing" {
            store_dispatch_attempt(&db, &changed)?;
        }
        assert_dispatch_rejected(&db, &episode, &admission, &supplied, reason)?;
    }
    Ok(())
}

#[test]
fn canonical_attempt_invariants_have_typed_rejections_without_charging() -> Result<()> {
    for (case, reason) in [
        ("terminal_time", ProviderDispatchRejection::AttemptTerminal),
        (
            "terminal_outcome",
            ProviderDispatchRejection::AttemptTerminal,
        ),
        (
            "source_revision",
            ProviderDispatchRejection::SourceGenerationMismatch,
        ),
        (
            "control_fence",
            ProviderDispatchRejection::InvalidAdmittedFences,
        ),
        (
            "host_fence",
            ProviderDispatchRejection::InvalidAdmittedFences,
        ),
        (
            "work_revision",
            ProviderDispatchRejection::WorkItemRevisionMismatch,
        ),
        (
            "missing_work_revision",
            ProviderDispatchRejection::WorkItemRevisionMismatch,
        ),
        (
            "future_work_revision",
            ProviderDispatchRejection::WorkItemRevisionMismatch,
        ),
        ("lifecycle_owner", ProviderDispatchRejection::OwnerMismatch),
    ] {
        let (_dir, db) = database()?;
        let mut attempt = canonical_root(&db)?;
        let episode = canonical_episode(&db, &attempt)?;
        let admission = lease(&db, &[transport(case)])?;
        match case {
            "terminal_time" => attempt.terminal_at = Some(timestamp(now())),
            "terminal_outcome" => attempt.terminal_outcome_id = Some("terminal-outcome".into()),
            "source_revision" => attempt.admitted_fences.source_revision += 1,
            "control_fence" => attempt.admitted_fences.agent_control_revision = 0,
            "host_fence" => attempt.admitted_fences.host_registry_revision = 0,
            "work_revision" => attempt.admitted_fences.work_item_source_revision = Some(0),
            "missing_work_revision" => attempt.admitted_fences.work_item_source_revision = None,
            "future_work_revision" => attempt.admitted_fences.work_item_source_revision = Some(2),
            "lifecycle_owner" => {
                attempt.binding = ExecutionBinding::AgentLifecycle {
                    agent_id: "other-agent".into(),
                };
            }
            _ => unreachable!(),
        }
        store_dispatch_attempt(&db, &attempt)?;
        assert_dispatch_rejected(&db, &episode, &admission, &attempt, reason)?;
    }
    Ok(())
}

#[test]
fn stopped_agent_or_invalid_owner_is_denied_without_charging() -> Result<()> {
    use crate::types::{AgentRegistryStatus, AgentState, AgentStatus};
    for (case, reason) in [
        ("stopped", ProviderDispatchRejection::AuthorityStopped),
        (
            "identity",
            ProviderDispatchRejection::AuthorityIdentityInactive,
        ),
        (
            "control_missing",
            ProviderDispatchRejection::AuthorityControlMissing,
        ),
        (
            "identity_missing",
            ProviderDispatchRejection::AuthorityIdentityMissing,
        ),
        (
            "identity_revision",
            ProviderDispatchRejection::AuthorityFenceMismatch,
        ),
        ("completed", ProviderDispatchRejection::OwnerNotOpen),
        ("deleted", ProviderDispatchRejection::OwnerMissing),
        ("owner_identity", ProviderDispatchRejection::OwnerMismatch),
        ("lane", ProviderDispatchRejection::LaneGenerationMismatch),
        (
            "generation",
            ProviderDispatchRejection::LaneGenerationMismatch,
        ),
        (
            "revision",
            ProviderDispatchRejection::WorkItemRevisionMismatch,
        ),
    ] {
        let (_dir, db) = database()?;
        let attempt = canonical_root(&db)?;
        let episode = canonical_episode(&db, &attempt)?;
        let admission = lease(&db, &[transport(case)])?;
        match case {
            "stopped" => {
                let mut state = AgentState::new(&attempt.agent_id);
                state.status = AgentStatus::Stopped;
                db.agent_states().upsert_for_fixture(&state)?;
            }
            "identity" => {
                let mut identity = db.agent_identities().latest(&attempt.agent_id)?.unwrap();
                identity.status = AgentRegistryStatus::Deleting;
                db.agent_identities().upsert(&identity)?;
            }
            "control_missing" | "identity_missing" => {
                db.transaction(|tx| {
                    let sql = if case == "control_missing" {
                        "DELETE FROM agent_states WHERE agent_id = 'agent-a'"
                    } else {
                        "DELETE FROM agent_identities WHERE agent_id = 'agent-a'"
                    };
                    tx.execute(sql, [])?;
                    Ok(())
                })?;
            }
            "identity_revision" => {
                let mut identity = db.agent_identities().latest(&attempt.agent_id)?.unwrap();
                identity.revision += 1;
                db.agent_identities().upsert(&identity)?;
            }
            "completed" => {
                let mut work = db.work_items().latest("work-a")?.unwrap();
                work.state = WorkItemState::Completed;
                db.transaction(|tx| {
                    tx.execute(
                        "UPDATE work_items SET state = 'completed', payload_json = ?1
                         WHERE work_item_id = 'work-a'",
                        [serde_json::to_string(&work)?],
                    )?;
                    Ok(())
                })?;
            }
            "deleted" => {
                db.transaction(|tx| {
                    tx.execute("DELETE FROM work_items WHERE work_item_id = 'work-a'", [])?;
                    Ok(())
                })?;
            }
            "owner_identity" => {
                let mut work = db.work_items().latest("work-a")?.unwrap();
                work.agent_id = "other-agent".into();
                db.transaction(|tx| {
                    tx.execute(
                        "UPDATE work_items SET payload_json = ?1 WHERE work_item_id = 'work-a'",
                        [serde_json::to_string(&work)?],
                    )?;
                    Ok(())
                })?;
            }
            "lane" | "generation" | "revision" => {
                let record = WorkItemExecutionRecord {
                    source_revision: if case == "revision" { 2 } else { 1 },
                    state: WorkItemExecutionState::InFlight {
                        generation: if case == "generation" { 2 } else { 1 },
                        attempt_id: if case == "lane" {
                            "replacement".into()
                        } else {
                            attempt.attempt_id.clone()
                        },
                    },
                };
                db.transaction(|tx| {
                    tx.execute(
                        "UPDATE execution_protocol_work_items SET payload_json = ?1
                         WHERE agent_id = 'agent-a' AND work_item_id = 'work-a'",
                        [serde_json::to_string(&record)?],
                    )?;
                    Ok(())
                })?;
            }
            _ => unreachable!(),
        }
        assert_dispatch_rejected(&db, &episode, &admission, &attempt, reason)?;
    }
    Ok(())
}

#[test]
fn canonical_dispatch_requires_the_persisted_episode_root() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let mut unrelated = attempt.clone();
    unrelated.attempt_id = "unrelated-root".into();
    let same_source_episode = canonical_episode(&db, &unrelated)?;
    let admission = lease(&db, &[transport("wrong-root")])?;
    assert_dispatch_rejected(
        &db,
        &same_source_episode,
        &admission,
        &attempt,
        ProviderDispatchRejection::RootMismatch,
    )?;
    unrelated.source_message_id = Some("unrelated-message".into());
    unrelated.source.identity = ExecutionSourceIdentity::QueueMessage {
        message_id: "unrelated-message".into(),
    };
    let episode = canonical_episode(&db, &unrelated)?;
    assert_dispatch_rejected(
        &db,
        &episode,
        &admission,
        &attempt,
        ProviderDispatchRejection::RootMismatch,
    )
}

#[test]
fn episode_binding_and_root_mismatches_do_not_change_authorization_or_charge() -> Result<()> {
    for (case, reason) in [
        ("binding", ProviderDispatchRejection::EpisodeBindingMismatch),
        (
            "root_source",
            ProviderDispatchRejection::RootBindingMismatch,
        ),
        (
            "root_fences",
            ProviderDispatchRejection::RootBindingMismatch,
        ),
    ] {
        let (_dir, db) = database()?;
        let attempt = canonical_root(&db)?;
        let mut episode_root = attempt.clone();
        match case {
            "root_source" => episode_root.source_message_id = Some("other-root-message".into()),
            "root_fences" => episode_root.admitted_fences.agent_control_revision += 1,
            _ => {}
        }
        let episode = canonical_episode(&db, &episode_root)?;
        let admission = lease(&db, &[transport(case)])?;
        if case == "binding" {
            let mut invalid = ProviderRecoveryBinding::from_root_attempt(&attempt, "chain-v1")?;
            invalid.owner = ExecutionBinding::Command;
            db.transaction(|tx| {
                tx.execute(
                    "UPDATE provider_recovery_episodes SET binding_json = ?1 WHERE episode_key = ?2",
                    params![serde_json::to_string(&invalid)?, episode.key()],
                )?;
                Ok(())
            })?;
        }
        assert_dispatch_rejected(&db, &episode, &admission, &attempt, reason)?;
    }
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let episode = canonical_episode(&db, &attempt)?;
    let admission = lease(&db, &[transport("missing-episode-binding")])?;
    let mut missing = episode.clone();
    missing.key = "missing-episode".into();
    let before = dispatch_authorization_snapshot(&db)?;
    assert_denied(
        db.charge_provider_recovery_dispatch_for_attempt(
            &missing,
            &admission,
            "missing-episode-binding",
            1,
            &attempt,
            now(),
        )?,
        ProviderRecoveryDispatchDenial::StaleExecution(
            ProviderDispatchRejection::EpisodeBindingMissing,
        ),
    );
    assert_not_charged(&db, &episode)?;
    assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    Ok(())
}

#[test]
fn invalid_recovery_ancestry_has_typed_rejections_without_mutation_or_charge() -> Result<()> {
    for (case, reason) in [
        ("missing", ProviderDispatchRejection::AncestorMissing),
        ("open", ProviderDispatchRejection::AncestorNotClosed),
        ("owner", ProviderDispatchRejection::AncestorOwnerMismatch),
        ("cycle", ProviderDispatchRejection::AncestryCycle),
        ("wrong_root", ProviderDispatchRejection::RootMismatch),
    ] {
        let (_dir, db) = database()?;
        let mut attempt = canonical_root(&db)?;
        let episode = canonical_episode(&db, &attempt)?;
        let admission = lease(&db, &[transport(case)])?;
        let mut ancestor = attempt.clone();
        ancestor.attempt_id = "ancestor".into();
        ancestor.state = ExecutionAttemptState::Interrupted;
        attempt.recovery_of_attempt_id = Some(ancestor.attempt_id.clone());
        match case {
            "open" => attempt.recovery_of_attempt_id = Some(attempt.attempt_id.clone()),
            "owner" => ancestor.binding = ExecutionBinding::Command,
            "cycle" => ancestor.recovery_of_attempt_id = Some(ancestor.attempt_id.clone()),
            _ => {}
        }
        if !matches!(case, "missing" | "open") {
            store_dispatch_attempt(&db, &ancestor)?;
        }
        store_dispatch_attempt(&db, &attempt)?;
        assert_dispatch_rejected(&db, &episode, &admission, &attempt, reason)?;
    }
    Ok(())
}

#[test]
fn dispatch_storage_and_decode_errors_are_not_lifecycle_denials() -> Result<()> {
    for case in [
        "attempt_json",
        "owner_json",
        "lane_json",
        "episode_json",
        "authority_status",
        "identity_json",
        "identity_overflow",
        "authority_sql",
    ] {
        let (_dir, db) = database()?;
        let attempt = canonical_root(&db)?;
        let episode = canonical_episode(&db, &attempt)?;
        let admission = lease(&db, &[transport(case)])?;
        db.transaction(|tx| {
            // Pass JSON storage constraints, then fail typed canonical decoding.
            let sql = match case {
                "attempt_json" => {
                    "UPDATE execution_protocol_attempts SET payload_json = 'null' WHERE agent_id = 'agent-a'"
                }
                "owner_json" => "UPDATE work_items SET payload_json = 'null' WHERE work_item_id = 'work-a'",
                "lane_json" => {
                    "UPDATE execution_protocol_work_items SET payload_json = 'null' WHERE agent_id = 'agent-a'"
                }
                "episode_json" => "UPDATE provider_recovery_episodes SET binding_json = 'null'",
                "authority_status" => "UPDATE agent_states SET status = 'invalid' WHERE agent_id = 'agent-a'",
                "identity_json" => "UPDATE agent_identities SET payload_json = 'null' WHERE agent_id = 'agent-a'",
                "authority_sql" => "ALTER TABLE agent_identities RENAME TO unavailable_agent_identities",
                "identity_overflow" => return Ok(()),
                _ => unreachable!(),
            };
            tx.execute(sql, [])?;
            Ok(())
        })?;
        if case == "identity_overflow" {
            let mut identity = db.agent_identities().latest(&attempt.agent_id)?.unwrap();
            identity.revision = u64::MAX;
            db.transaction(|tx| {
                tx.execute(
                    "UPDATE agent_identities SET payload_json = ?1 WHERE agent_id = 'agent-a'",
                    [serde_json::to_string(&identity)?],
                )?;
                Ok(())
            })?;
        }
        let before = dispatch_authorization_snapshot(&db)?;
        let error = db
            .charge_provider_recovery_dispatch_for_attempt(
                &episode,
                &admission,
                "storage-error",
                1,
                &attempt,
                now(),
            )
            .expect_err("storage corruption must remain an error, not a provider denial");
        if case == "authority_sql" {
            assert!(error.downcast_ref::<rusqlite::Error>().is_some());
        } else if case != "identity_overflow" {
            assert!(error.downcast_ref::<serde_json::Error>().is_some());
        }
        assert_not_charged(&db, &episode)?;
        assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    }
    Ok(())
}

#[test]
fn canonical_recovery_descendant_charges_root_but_replaced_attempt_cannot() -> Result<()> {
    use crate::domain::execution_protocol::{
        admit_execution, settle_execution, AdmitExecution, ExecutionOutcome,
        ExecutionOutcomeRecord, SettleExecution, WorkItemOutcome,
    };
    use crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx;

    let (_dir, db) = database()?;
    let root = canonical_root(&db)?;
    let episode = canonical_episode(&db, &root)?;
    let admission = lease(&db, &[transport("recovery-descendant")])?;
    let protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized(&root.agent_id)?
        .unwrap();
    let protocol = settle_execution(
        &protocol,
        &SettleExecution {
            outcome: ExecutionOutcomeRecord {
                outcome_id: "root-failed".into(),
                attempt_id: root.attempt_id.clone(),
                outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Continue),
                created_at: timestamp(now()),
            },
        },
    )
    .map_err(anyhow::Error::msg)?
    .state;
    db.transaction(|tx| persist_state_tx(tx, &protocol))?;
    let mut recovery = root.clone();
    recovery.attempt_id = "recovery-attempt".into();
    recovery.source_message_id = Some("recovery-message".into());
    recovery.source = ExecutionSource {
        identity: ExecutionSourceIdentity::RuntimeRecovery {
            recovery_id: "provider-recovery".into(),
        },
        generation: 2,
    };
    recovery.recovery_of_attempt_id = Some(root.attempt_id.clone());
    recovery.provenance.origin = ExecutionOrigin::RuntimeRecovery;
    recovery.provenance.trust = ExecutionTrust::RuntimeInstruction;
    recovery.admitted_fences.source_revision = 2;
    recovery.admitted_fences.work_item_generation =
        Some(protocol.work_items["work-a"].generation());
    let protocol = admit_execution(
        &protocol,
        &AdmitExecution {
            attempt: recovery.clone(),
        },
    )
    .map_err(anyhow::Error::msg)?
    .state;
    db.transaction(|tx| persist_state_tx(tx, &protocol))?;
    assert_dispatch_rejected(
        &db,
        &episode,
        &admission,
        &root,
        ProviderDispatchRejection::AttemptNotOpen,
    )?;
    assert!(matches!(
        db.charge_provider_recovery_dispatch_for_attempt(
            &episode,
            &admission,
            "recovery-request",
            1,
            &recovery,
            now(),
        )?,
        ProviderRecoveryDispatchResult::Charged(_)
    ));
    assert_eq!(refresh(&db, &episode)?.request_count, 1);
    Ok(())
}

#[test]
fn three_requests_total_survive_restart_and_replayed_open() -> Result<()> {
    let (dir, db) = database()?;
    let mut current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    for index in 0..3 {
        let scope = transport(&format!("endpoint-{index}"));
        let reservation = lease(&db, std::slice::from_ref(&scope))?;
        let dispatched = charge(
            &db,
            &current,
            &reservation,
            &format!("request-{index}"),
            index + 1,
        )?;
        assert!(db.record_provider_recovery_request_outcome(
            &dispatched,
            ProviderRecoveryRequestOutcome::NetworkFailure(&scope),
            false,
            now(),
        )?);
        current = refresh(&db, &current)?;
    }
    assert_eq!((current.request_count, current.total_requests), (3, 3));
    assert_eq!(current.state, ProviderRecoveryBudgetState::BudgetExhausted);
    let original_floor = current.not_before;
    drop(db);
    let db = reopen(&dir)?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    assert_eq!(current.request_count, 3);
    assert_eq!(current.kind, ProviderRecoveryBudgetKind::Network);
    assert_eq!(current.not_before, original_floor);
    assert!(db.provider_recovery_candidate_attempted(&current, &[transport("endpoint-0")])?);
    assert!(!db.provider_recovery_candidate_attempted(&current, &[transport("untried")])?);
    assert!(!db.provider_recovery_candidate_attempted(&current, &[])?);
    let unchanged = refresh(&db, &current)?;
    assert_eq!((unchanged.request_count, unchanged.total_requests), (3, 3));
    assert_eq!(unchanged.revision, current.revision);
    assert_eq!(unchanged.candidate_cursor, current.candidate_cursor);
    assert_eq!(unchanged.not_before, current.not_before);
    let reservation = lease(&db, &[transport("untried")])?;
    assert_denied(
        db.charge_provider_recovery_dispatch(&current, &reservation, "request-4", 4, now())?,
        ProviderRecoveryDispatchDenial::BudgetExhausted,
    );
    assert_eq!(refresh(&db, &current)?.total_requests, 3);
    Ok(())
}

#[test]
fn stale_admission_and_preflight_release_do_not_charge_or_infer_failure() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let scope = transport("endpoint");
    let reservation = lease(&db, std::slice::from_ref(&scope))?;
    db.release_provider_admission(&reservation)?;
    assert_eq!(refresh(&db, &current)?.total_requests, 0);
    assert_eq!(
        refresh(&db, &current)?.state,
        ProviderRecoveryBudgetState::Ready
    );
    assert!(db.record_provider_admission_failure(
        &reservation,
        &scope,
        ProviderCooldownPolicy::Transport,
        now(),
        None,
        "model",
    )?);
    assert_denied(
        db.charge_provider_recovery_dispatch(&current, &reservation, "request", 1, now())?,
        ProviderRecoveryDispatchDenial::StaleAdmission,
    );
    assert_eq!(refresh(&db, &current)?.total_requests, 0);
    Ok(())
}

#[test]
fn duplicate_or_unknown_dispatch_is_never_refunded_or_reauthorized() -> Result<()> {
    let (dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let scopes = [transport("endpoint")];
    let reservation = lease(&db, &scopes)?;
    let _unknown = charge(&db, &current, &reservation, "request", 1)?;
    drop(db);
    let db = reopen(&dir)?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    assert_eq!(current.request_count, 1);
    let reservation = lease(&db, &scopes)?;
    assert_denied(
        db.charge_provider_recovery_dispatch(&current, &reservation, "request", 1, now())?,
        ProviderRecoveryDispatchDenial::DuplicateRequest,
    );
    assert!(db
        .begin_provider_recovery_probe_epoch(&current, now() + Duration::hours(1))?
        .is_none());
    assert_eq!(refresh(&db, &current)?.total_requests, 1);
    Ok(())
}

#[test]
fn local_dispatch_rejection_never_refunds_an_unknown_charged_request() -> Result<()> {
    let (dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let episode = canonical_episode(&db, &attempt)?;
    let admission = lease(&db, &[transport("unknown-request")])?;
    assert!(matches!(
        db.charge_provider_recovery_dispatch_for_attempt(
            &episode,
            &admission,
            "unknown-request",
            2,
            &attempt,
            now(),
        )?,
        ProviderRecoveryDispatchResult::Charged(_)
    ));
    // No outcome is recorded: the transport may have sent the charged request.
    drop(db);
    let db = reopen(&dir)?;
    let episode = refresh(&db, &episode)?;
    let mut stale = attempt;
    stale.admitted_fences.host_registry_revision += 1;
    let before = dispatch_authorization_snapshot(&db)?;
    assert_denied(
        db.charge_provider_recovery_dispatch_for_attempt(
            &episode,
            &admission,
            "locally-rejected-request",
            3,
            &stale,
            now(),
        )?,
        ProviderRecoveryDispatchDenial::StaleExecution(
            ProviderDispatchRejection::SnapshotFencesMismatch,
        ),
    );
    let persisted = refresh(&db, &episode)?;
    assert_eq!(persisted.request_count, 1);
    assert_eq!(persisted.total_requests, 1);
    assert_eq!(persisted.candidate_cursor, 2);
    db.with_read(|connection| {
        let requests: Vec<(String, Option<String>)> = connection
            .prepare(
                "SELECT request_id, outcome FROM provider_recovery_requests
                 WHERE episode_key = ?1",
            )?
            .query_map([episode.key()], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(requests, vec![("unknown-request".into(), None)]);
        Ok(())
    })?;
    assert_eq!(dispatch_authorization_snapshot(&db)?, before);
    Ok(())
}

#[test]
fn actual_failed_scope_not_all_applicable_scopes_controls_network_deduplication() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let common = transport("shared-path");
    let read_a = transport("read-model-a");
    let reservation = lease(&db, &[common.clone(), read_a.clone()])?;
    let first = charge(&db, &current, &reservation, "a", 1)?;
    db.record_provider_recovery_request_outcome(
        &first,
        ProviderRecoveryRequestOutcome::NetworkFailure(&read_a),
        false,
        now(),
    )?;
    let current = refresh(&db, &current)?;
    assert!(!db.provider_recovery_candidate_attempted(&current, std::slice::from_ref(&common))?);
    assert!(db.provider_recovery_candidate_attempted(&current, std::slice::from_ref(&read_a))?);
    assert_denied(
        db.charge_provider_recovery_dispatch(&current, &reservation, "a-again", 1, now())?,
        ProviderRecoveryDispatchDenial::ScopeAlreadyAttempted,
    );
    let reservation_b = lease(&db, &[common.clone(), transport("read-model-b")])?;
    let second = charge(&db, &current, &reservation_b, "b", 2)?;
    db.record_provider_recovery_request_outcome(
        &second,
        ProviderRecoveryRequestOutcome::NetworkFailure(&common),
        false,
        now(),
    )?;
    let current = refresh(&db, &current)?;
    let reservation_c = lease(&db, &[common, quota("different-account")])?;
    assert_denied(
        db.charge_provider_recovery_dispatch(&current, &reservation_c, "c", 3, now())?,
        ProviderRecoveryDispatchDenial::ScopeAlreadyAttempted,
    );
    assert_eq!(refresh(&db, &current)?.request_count, 2);
    Ok(())
}

#[test]
fn rate_limited_never_refills_budget_and_deduplicates_only_its_scope() -> Result<()> {
    for kind in [
        ProviderRecoveryBudgetKind::Network,
        ProviderRecoveryBudgetKind::Operator,
    ] {
        for scope_kind in 0..3 {
            for protocol_response in [false, true] {
                let (dir, db) = database()?;
                let mut current = episode(&db, kind)?;
                let common = transport("healthy-connection");
                let limited = match scope_kind {
                    0 => quota("account-a"),
                    1 => ProviderAdmissionScope::route_throttle("provider/endpoint/model-a")?,
                    _ => ProviderAdmissionScope::candidate("provider/endpoint/credential/model-a")?,
                };
                let first = lease(&db, &[common.clone(), limited.clone()])?;
                let dispatch = charge(&db, &current, &first, "limited", 1)?;
                assert!(db.record_provider_recovery_request_outcome(
                    &dispatch,
                    if scope_kind == 2 {
                        ProviderRecoveryRequestOutcome::PaymentRequired(&limited)
                    } else {
                        ProviderRecoveryRequestOutcome::RateLimited(&limited)
                    },
                    false,
                    now(),
                )?);
                current = refresh(&db, &current)?;
                assert_eq!(current.state, ProviderRecoveryBudgetState::Ready);
                assert!(db.provider_recovery_candidate_attempted(
                    &current,
                    std::slice::from_ref(&limited)
                )?);
                assert!(!db.provider_recovery_candidate_attempted(
                    &current,
                    std::slice::from_ref(&common)
                )?);
                assert_denied(
                    db.charge_provider_recovery_dispatch(&current, &first, "repeat", 2, now())?,
                    ProviderRecoveryDispatchDenial::ScopeAlreadyAttempted,
                );
                for index in 1..3 {
                    let next = match scope_kind {
                        0 => quota(&format!("account-{index}")),
                        1 => ProviderAdmissionScope::route_throttle(&format!(
                            "provider/endpoint/model-{index}"
                        ))?,
                        _ => ProviderAdmissionScope::candidate(&format!(
                            "provider/endpoint/credential/model-{index}"
                        ))?,
                    };
                    let reservation = lease(&db, &[common.clone(), next])?;
                    let dispatch = charge(
                        &db,
                        &current,
                        &reservation,
                        &format!("next-{index}"),
                        index + 1,
                    )?;
                    assert!(db.record_provider_recovery_request_outcome(
                        &dispatch,
                        if protocol_response {
                            ProviderRecoveryRequestOutcome::NonRecoveryResponse
                        } else {
                            ProviderRecoveryRequestOutcome::OtherFailure
                        },
                        false,
                        now(),
                    )?);
                    current = refresh(&db, &current)?;
                    assert_ne!(current.state, ProviderRecoveryBudgetState::Resolved);
                }
                assert_eq!(current.state, ProviderRecoveryBudgetState::BudgetExhausted);
                let (revision, floor) = (current.revision, current.not_before);
                drop(db);
                let db = reopen(&dir)?;
                let current = episode(&db, kind)?;
                assert_eq!(
                    (
                        current.request_count,
                        current.total_requests,
                        current.epoch,
                        current.candidate_cursor
                    ),
                    (3, 3, 1, 3)
                );
                assert_eq!((current.revision, current.not_before), (revision, floor));
                assert!(db.provider_recovery_candidate_attempted(&current, &[limited])?);
            }
        }
    }
    Ok(())
}

#[test]
fn rate_limited_outcome_rejects_transport_and_unadmitted_scopes() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let path = transport("healthy-path");
    let limited = ProviderAdmissionScope::route_throttle("route-a")?;
    let reservation = lease(&db, &[path.clone(), limited.clone()])?;
    let dispatch = charge(&db, &current, &reservation, "a", 1)?;
    for invalid in [
        path,
        ProviderAdmissionScope::route_throttle("foreign")?,
        quota("foreign"),
    ] {
        assert!(db
            .record_provider_recovery_request_outcome(
                &dispatch,
                ProviderRecoveryRequestOutcome::RateLimited(&invalid),
                false,
                now(),
            )
            .is_err());
    }
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::RateLimited(&limited),
        false,
        now(),
    )?);
    assert!(!db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::AvailableResponse,
        false,
        now(),
    )?);
    assert_eq!(
        refresh(&db, &current)?.state,
        ProviderRecoveryBudgetState::Ready
    );
    Ok(())
}

#[test]
fn operator_budget_deduplicates_quota_not_healthy_shared_transport() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    let common = transport("healthy-path");
    let first = lease(&db, &[common.clone(), quota("account-a")])?;
    let dispatch = charge(&db, &current, &first, "a", 1)?;
    db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::OtherFailure,
        false,
        now(),
    )?;
    let current = refresh(&db, &current)?;
    assert_eq!(current.state, ProviderRecoveryBudgetState::Ready);
    assert!(!db.provider_recovery_candidate_attempted(&current, std::slice::from_ref(&common))?);
    assert!(db.provider_recovery_candidate_attempted(&current, &[quota("account-a")])?);
    assert!(!db.provider_recovery_candidate_attempted(&current, &[quota("account-b")])?);
    assert_denied(
        db.charge_provider_recovery_dispatch(&current, &first, "a-again", 2, now())?,
        ProviderRecoveryDispatchDenial::ScopeAlreadyAttempted,
    );
    let second = lease(&db, &[common, quota("account-b")])?;
    let _next = charge(&db, &current, &second, "b", 2)?;
    assert_eq!(refresh(&db, &current)?.request_count, 2);
    Ok(())
}

#[test]
fn known_non_recovery_http_does_not_limit_healthy_operator_requests() -> Result<()> {
    let (_dir, db) = database()?;
    let mut current = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    let reservation = lease(&db, &[quota("healthy")])?;
    for index in 0..5 {
        let dispatched = charge(&db, &current, &reservation, &format!("protocol-{index}"), 1)?;
        db.record_provider_recovery_request_outcome(
            &dispatched,
            ProviderRecoveryRequestOutcome::NonRecoveryResponse,
            false,
            now(),
        )?;
        current = refresh(&db, &current)?;
        assert_eq!(current.state, ProviderRecoveryBudgetState::Resolved);
        current = db
            .begin_provider_recovery_probe_epoch(&current, now())?
            .unwrap();
    }
    assert_eq!(current.total_requests, 5);
    Ok(())
}

#[test]
fn known_non_recovery_http_never_refills_an_active_failure_episode() -> Result<()> {
    for kind in [
        ProviderRecoveryBudgetKind::Network,
        ProviderRecoveryBudgetKind::Operator,
    ] {
        let (_dir, db) = database()?;
        let current = episode(&db, kind)?;
        let failed = transport("failed");
        let reservation = lease(&db, std::slice::from_ref(&failed))?;
        let first = charge(&db, &current, &reservation, "failure", 1)?;
        db.record_provider_recovery_request_outcome(
            &first,
            ProviderRecoveryRequestOutcome::NetworkFailure(&failed),
            false,
            now(),
        )?;
        let current = refresh(&db, &current)?;
        let another = lease(&db, &[transport("another")])?;
        let second = charge(&db, &current, &another, "auxiliary", 2)?;
        db.record_provider_recovery_request_outcome(
            &second,
            ProviderRecoveryRequestOutcome::NonRecoveryResponse,
            false,
            now(),
        )?;
        let current = refresh(&db, &current)?;
        assert_ne!(current.state, ProviderRecoveryBudgetState::Resolved);
        assert_eq!((current.request_count, current.total_requests), (2, 2));
        assert!(db
            .begin_provider_recovery_probe_epoch(&current, now())?
            .is_none());
    }
    Ok(())
}

#[test]
fn unknown_http_followed_by_protocol_result_cannot_refill_recovery_budget() -> Result<()> {
    for outcome in [
        ProviderRecoveryRequestOutcome::NonRecoveryResponse,
        ProviderRecoveryRequestOutcome::OtherFailure,
    ] {
        let (_dir, db) = database()?;
        let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
        let first = lease(&db, &[transport("unknown")])?;
        let _unknown = charge(&db, &current, &first, "unknown", 1)?;
        let current = refresh(&db, &current)?;
        let second = lease(&db, &[transport("protocol")])?;
        let dispatched = charge(&db, &current, &second, "protocol", 2)?;
        db.record_provider_recovery_request_outcome(&dispatched, outcome, false, now())?;
        let current = refresh(&db, &current)?;
        assert_eq!((current.request_count, current.total_requests), (2, 2));
        assert_ne!(current.state, ProviderRecoveryBudgetState::Resolved);
        assert!(db
            .begin_provider_recovery_probe_epoch(&current, now())?
            .is_none());
    }
    Ok(())
}

#[test]
fn candidate_screening_uses_persisted_policy_and_rejects_stale_snapshots() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let healthy = transport("healthy-path");
    let failed = transport("failed-model");
    let reservation = lease(&db, &[healthy.clone(), failed.clone()])?;
    let dispatch = charge(&db, &current, &reservation, "request", 1)?;
    db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::NetworkFailure(&failed),
        false,
        now(),
    )?;
    assert!(db
        .provider_recovery_candidate_attempted(&current, std::slice::from_ref(&failed))
        .is_err());
    let mut tampered = refresh(&db, &current)?;
    tampered.kind = ProviderRecoveryBudgetKind::Operator;
    assert!(
        db.provider_recovery_candidate_attempted(&tampered, &[healthy.clone(), failed.clone()])?
    );
    assert!(!db.provider_recovery_candidate_attempted(&tampered, &[healthy])?);
    assert!(db.provider_recovery_candidate_attempted(&tampered, &[failed])?);
    tampered.epoch += 1;
    assert!(db
        .provider_recovery_candidate_attempted(&tampered, &[])
        .is_err());
    Ok(())
}

#[test]
fn candidate_screening_counts_dispatched_operator_transport_probes() -> Result<()> {
    let (_dir, db) = database()?;
    let scope = transport("probed-path");
    let initial = lease(&db, std::slice::from_ref(&scope))?;
    db.record_provider_admission_failure(
        &initial,
        &scope,
        ProviderCooldownPolicy::Transport,
        now(),
        None,
        "model",
    )?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    let ProviderRecoveryAdmissionReservation::Granted(probe) = db
        .reserve_provider_recovery_admission(
            &current,
            std::slice::from_ref(&scope),
            now(),
            Duration::minutes(2),
        )?
    else {
        anyhow::bail!("expected operator transport probe");
    };
    assert!(!db.provider_recovery_candidate_attempted(&current, std::slice::from_ref(&scope))?);
    let _dispatch = charge(&db, &current, &probe, "probe", 1)?;
    let current = refresh(&db, &current)?;
    assert!(db.provider_recovery_candidate_attempted(&current, &[scope])?);
    assert_eq!(refresh(&db, &current)?.request_count, 1);
    Ok(())
}

#[test]
fn due_epoch_cas_preserves_cursor_and_cannot_regrant_operator_override() -> Result<()> {
    let (dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    let scope = transport("endpoint");
    let reservation = lease(&db, std::slice::from_ref(&scope))?;
    let dispatch = charge(&db, &current, &reservation, "a", 2)?;
    db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::NetworkFailure(&scope),
        true,
        now(),
    )?;
    let current = refresh(&db, &current)?;
    db.park_provider_recovery_episode(&current, now(), false, now())?;
    let current = refresh(&db, &current)?;
    let until = current.not_before.unwrap();
    assert!(db
        .begin_provider_recovery_probe_epoch(&current, until - Duration::milliseconds(1))?
        .is_none());
    let other = reopen(&dir)?;
    let next = db
        .begin_provider_recovery_probe_epoch(&current, until)?
        .unwrap();
    assert!(other
        .begin_provider_recovery_probe_epoch(&current, until)?
        .is_none());
    assert_eq!(
        (
            next.request_count,
            next.total_requests,
            next.candidate_cursor
        ),
        (0, 1, 2)
    );
    assert!(next.side_effect_boundary);
    assert_eq!(next.kind, ProviderRecoveryBudgetKind::Network);
    assert_eq!(
        episode(&db, ProviderRecoveryBudgetKind::Operator)?.kind,
        ProviderRecoveryBudgetKind::Network
    );
    assert!(!db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::AvailableResponse,
        false,
        until,
    )?);
    Ok(())
}

#[test]
fn zero_dispatch_parking_uses_candidate_deadline_without_failed_epoch() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let deadline = now() + Duration::milliseconds(1);
    assert!(db.park_provider_recovery_episode(&current, deadline, true, now())?);
    let parked = refresh(&db, &current)?;
    assert_eq!(parked.not_before, Some(deadline));
    assert_eq!(parked.failed_epochs, 0);
    assert_eq!((parked.request_count, parked.total_requests), (0, 0));
    assert_eq!(parked.state, ProviderRecoveryBudgetState::Cooling);
    Ok(())
}

#[test]
fn zero_dispatch_probe_epochs_do_not_restart_failed_epoch_floor() -> Result<()> {
    let (dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let scope = transport("failed-before-shared-cooldown");
    let reservation = lease(&db, std::slice::from_ref(&scope))?;
    let dispatch = charge(&db, &current, &reservation, "failed-request", 2)?;
    db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::NetworkFailure(&scope),
        true,
        now(),
    )?;
    let current = refresh(&db, &current)?;
    assert!(db.park_provider_recovery_episode(&current, now(), false, now())?);
    let mut parked = refresh(&db, &current)?;
    assert_eq!(parked.failed_epochs, 1);
    assert_eq!(parked.not_before, Some(episode_floor(&parked, now())?));
    let reopened = reopen(&dir)?;
    for _ in 0..6 {
        let due = parked.not_before.unwrap();
        let next = reopened
            .begin_provider_recovery_probe_epoch(&parked, due)?
            .context("due epoch missing")?;
        assert!(db
            .begin_provider_recovery_probe_epoch(&parked, due)?
            .is_none());
        let deadline = due + Duration::seconds(1);
        assert!(db.park_provider_recovery_episode(&next, deadline, true, due)?);
        parked = refresh(&reopened, &next)?;
        assert_eq!(parked.not_before, Some(deadline));
        assert_eq!(parked.failed_epochs, 1);
        assert_eq!((parked.request_count, parked.total_requests), (0, 1));
        assert_eq!(parked.candidate_cursor, 2);
        assert!(parked.side_effect_boundary);
        assert_eq!(parked.state, ProviderRecoveryBudgetState::Cooling);
        assert!(db
            .begin_provider_recovery_probe_epoch(&parked, deadline - Duration::milliseconds(1))?
            .is_none());
        assert!(!db.park_provider_recovery_episode(
            &parked,
            deadline,
            true,
            deadline - Duration::milliseconds(1),
        )?);
        assert_eq!(refresh(&reopened, &parked)?.revision, parked.revision);
    }
    let due = parked.not_before.unwrap();
    let next = db
        .begin_provider_recovery_probe_epoch(&parked, due)?
        .context("next dispatch epoch missing")?;
    let ProviderAdmissionReservation::Granted(reservation) =
        db.reserve_provider_admission(std::slice::from_ref(&scope), due, Duration::minutes(2))?
    else {
        anyhow::bail!("expected due probe");
    };
    let ProviderRecoveryDispatchResult::Charged(dispatch) =
        db.charge_provider_recovery_dispatch(&next, &reservation, "second-failure", 2, due)?
    else {
        anyhow::bail!("expected charged probe");
    };
    db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::NetworkFailure(&scope),
        false,
        due,
    )?;
    let next = refresh(&db, &next)?;
    assert!(db.park_provider_recovery_episode(&next, due, false, due)?);
    let failed = refresh(&reopened, &next)?;
    assert_eq!(failed.failed_epochs, 2);
    assert_eq!((failed.request_count, failed.total_requests), (1, 2));
    assert_eq!(failed.not_before, Some(episode_floor(&failed, due)?));
    Ok(())
}

#[test]
fn zero_dispatch_without_future_deadline_paces_rechecks_without_escalation() -> Result<()> {
    let (_dir, db) = database()?;
    let mut current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let delay = ProviderCooldownPolicy::Transport.delay(1, current.key());
    let mut at = now();
    for _ in 0..6 {
        assert!(db.park_provider_recovery_episode(&current, at, false, at)?);
        let parked = refresh(&db, &current)?;
        assert_eq!(parked.not_before, Some(at + delay));
        assert_eq!(parked.failed_epochs, 0);
        assert_eq!((parked.request_count, parked.total_requests), (0, 0));
        assert_eq!(parked.state, ProviderRecoveryBudgetState::Cooling);
        assert!(!db.park_provider_recovery_episode(
            &parked,
            at,
            false,
            at + Duration::milliseconds(1),
        )?);
        at = parked.not_before.unwrap();
        current = db
            .begin_provider_recovery_probe_epoch(&parked, at)?
            .context("due local recheck missing")?;
    }
    Ok(())
}

#[test]
fn repeated_parking_and_wakes_do_not_extend_floor() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let deadline = now() + Duration::minutes(20);
    assert!(db.park_provider_recovery_episode(&current, deadline, false, now())?);
    let parked = refresh(&db, &current)?;
    assert_eq!(parked.not_before, Some(deadline));
    assert!(!db.park_provider_recovery_episode(
        &parked,
        deadline,
        false,
        now() + Duration::minutes(19),
    )?);
    assert_eq!(refresh(&db, &parked)?.revision, parked.revision);
    for _ in 0..5 {
        assert_eq!(
            episode(&db, ProviderRecoveryBudgetKind::Network)?.not_before,
            Some(deadline)
        );
    }
    Ok(())
}

#[test]
fn response_ends_network_sequence_without_replaying_old_result_or_limiting_healthy_rounds(
) -> Result<()> {
    let (_dir, db) = database()?;
    let mut current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let reservation = lease(&db, &[transport("endpoint")])?;
    for round in 0..5 {
        let dispatch = charge(&db, &current, &reservation, &format!("round-{round}"), 2)?;
        let outcome = if round == 0 {
            ProviderRecoveryRequestOutcome::OtherFailure
        } else {
            ProviderRecoveryRequestOutcome::AvailableResponse
        };
        assert!(db.record_provider_recovery_request_outcome(&dispatch, outcome, false, now())?);
        current = refresh(&db, &current)?;
        assert_eq!(current.state, ProviderRecoveryBudgetState::Resolved);
        current = db
            .begin_provider_recovery_probe_epoch(&current, now())?
            .unwrap();
        assert_eq!((current.candidate_cursor, current.request_count), (0, 0));
        assert!(!db.record_provider_recovery_request_outcome(
            &dispatch,
            ProviderRecoveryRequestOutcome::OtherFailure,
            false,
            now(),
        )?);
    }
    assert_eq!(current.total_requests, 5);
    Ok(())
}

#[test]
fn root_owner_source_and_chain_are_bound_and_recovery_attempt_cannot_be_root() -> Result<()> {
    let (_dir, db) = database()?;
    let initial = binding();
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let mut attempt = root();
    attempt.binding = ExecutionBinding::WorkItem {
        work_item_id: "work-b".into(),
    };
    assert_ne!(
        initial.key()?,
        ProviderRecoveryBinding::from_root_attempt(&attempt, "chain-v1")?.key()?
    );
    attempt = root();
    attempt.source.generation += 1;
    assert_ne!(
        initial.key()?,
        ProviderRecoveryBinding::from_root_attempt(&attempt, "chain-v1")?.key()?
    );
    assert_ne!(
        initial.key()?,
        ProviderRecoveryBinding::from_root_attempt(&root(), "chain-v2")?.key()?
    );
    attempt.recovery_of_attempt_id = Some("root-attempt".into());
    assert!(ProviderRecoveryBinding::from_root_attempt(&attempt, "chain-v1").is_err());
    assert_eq!(refresh(&db, &current)?.total_requests, 0);
    Ok(())
}

#[test]
fn corrupt_deadline_and_unadmitted_failure_scope_fail_closed() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let reservation = lease(&db, &[transport("endpoint")])?;
    let dispatch = charge(&db, &current, &reservation, "a", 1)?;
    assert!(db
        .record_provider_recovery_request_outcome(
            &dispatch,
            ProviderRecoveryRequestOutcome::NetworkFailure(&transport("foreign")),
            false,
            now(),
        )
        .is_err());
    let current = refresh(&db, &current)?;
    assert_eq!(current.request_count, 1);
    db.transaction(|tx| {
        tx.execute(
            "UPDATE provider_recovery_episodes SET state = 'cooling', not_before = 'invalid'
             WHERE episode_key = ?1",
            [current.key()],
        )?;
        Ok(())
    })?;
    assert!(db.provider_recovery_episode(current.key()).is_err());
    Ok(())
}

#[test]
fn non_network_failure_cannot_refill_an_existing_network_recovery_sequence() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let scope = transport("endpoint-a");
    let reservation = lease(&db, std::slice::from_ref(&scope))?;
    let first = charge(&db, &current, &reservation, "a", 1)?;
    db.record_provider_recovery_request_outcome(
        &first,
        ProviderRecoveryRequestOutcome::NetworkFailure(&scope),
        false,
        now(),
    )?;
    let current = refresh(&db, &current)?;
    let reservation = lease(&db, &[transport("endpoint-b")])?;
    let second = charge(&db, &current, &reservation, "b", 2)?;
    db.record_provider_recovery_request_outcome(
        &second,
        ProviderRecoveryRequestOutcome::OtherFailure,
        false,
        now(),
    )?;
    let current = refresh(&db, &current)?;
    assert_eq!(current.request_count, 2);
    assert_eq!(current.state, ProviderRecoveryBudgetState::Ready);
    assert!(db
        .begin_provider_recovery_probe_epoch(&current, now())?
        .is_none());
    Ok(())
}

#[test]
fn operator_policy_probe_shares_lease_and_failure_never_shortens_original_cooldown() -> Result<()> {
    let (dir, db) = database()?;
    let scope = quota("shared-account");
    let initial = lease(&db, std::slice::from_ref(&scope))?;
    db.record_provider_admission_failure(
        &initial,
        &scope,
        ProviderCooldownPolicy::HardQuota,
        now(),
        None,
        "a",
    )?;
    let original = db
        .provider_admission_blocked(std::slice::from_ref(&scope), now())?
        .unwrap();
    let current = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    let ProviderRecoveryAdmissionReservation::Granted(probe) = db
        .reserve_provider_recovery_admission(
            &current,
            std::slice::from_ref(&scope),
            now(),
            Duration::minutes(2),
        )?
    else {
        anyhow::bail!("operator should receive one early policy probe")
    };
    let other = reopen(&dir)?;
    let mut other_binding = binding();
    other_binding.agent_id = "agent-b".into();
    other_binding.root_message_id = "operator-b".into();
    let other_episode = other.open_provider_recovery_episode(
        &other_binding,
        ProviderRecoveryBudgetKind::Operator,
        now(),
    )?;
    assert!(matches!(
        other.reserve_provider_recovery_admission(
            &other_episode,
            std::slice::from_ref(&scope),
            now(),
            Duration::minutes(2),
        )?,
        ProviderRecoveryAdmissionReservation::Blocked(_)
    ));
    assert!(db.record_provider_admission_failure(
        &probe,
        &scope,
        ProviderCooldownPolicy::TransientQuota,
        now(),
        None,
        "a",
    )?);
    let next = db
        .provider_admission_blocked(std::slice::from_ref(&scope), now())?
        .unwrap();
    assert!(next.not_before >= original.not_before);
    Ok(())
}

#[test]
fn operator_cannot_override_server_not_before_or_tamper_with_snapshot_to_gain_override(
) -> Result<()> {
    let (_dir, db) = database()?;
    let scope = quota("account");
    let initial = lease(&db, std::slice::from_ref(&scope))?;
    let server = now() + Duration::hours(2);
    db.record_provider_admission_failure(
        &initial,
        &scope,
        ProviderCooldownPolicy::HardQuota,
        now(),
        Some(server),
        "model",
    )?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    let blocked = db.reserve_provider_recovery_admission(
        &current,
        std::slice::from_ref(&scope),
        now(),
        Duration::minutes(2),
    )?;
    assert!(
        matches!(blocked, ProviderRecoveryAdmissionReservation::Blocked(b) if b.not_before >= server)
    );
    let mut auto_binding = binding();
    auto_binding.root_message_id = "system-message".into();
    let local_scope = quota("policy-only");
    let local = lease(&db, std::slice::from_ref(&local_scope))?;
    db.record_provider_admission_failure(
        &local,
        &local_scope,
        ProviderCooldownPolicy::HardQuota,
        now(),
        None,
        "model",
    )?;
    let mut automatic = db.open_provider_recovery_episode(
        &auto_binding,
        ProviderRecoveryBudgetKind::Network,
        now(),
    )?;
    automatic.kind = ProviderRecoveryBudgetKind::Operator;
    assert!(matches!(
        db.reserve_provider_recovery_admission(
            &automatic,
            &[local_scope],
            now(),
            Duration::minutes(2),
        )?,
        ProviderRecoveryAdmissionReservation::Blocked(_)
    ));
    Ok(())
}
