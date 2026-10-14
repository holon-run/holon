use super::*;
use crate::{
    config::StateMemoryLimits,
    domain::execution_protocol::{
        admit_execution, AdmitExecution, AdmittedFences, ConversationOutcome, ExecutionAttempt,
        ExecutionAttemptState, ExecutionBinding, ExecutionOrigin, ExecutionOutcome,
        ExecutionPriority, ExecutionProtocolState, ExecutionProvenance, ExecutionSource,
        ExecutionSourceIdentity, ExecutionTrust,
    },
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope},
    },
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::{
            prepare_original_input_recovery_snapshot_tx, InputEvidenceBootstrapSnapshot,
            InputEvidenceLoadRequest,
        },
        input_preparation::{
            prepare_existing_admission_cleanup_domains_tx, prepare_input_domains_tx,
        },
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        repositories::{upsert_queue_entry_tx, upsert_turn_record_tx},
        transitions::{
            AgentStateMutation, InputMutationExecution, PreparedInputTransition,
            PreparedQueueTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
        },
    },
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility, AuthorityClass, BriefKind,
        BriefRecord, MessageBody, MessageKind, MessageOrigin, Priority, QueueEntryRecord,
        QueueEntryStatus,
    },
};

const AGENT: &str = "fixed-deleting-upgrade";
const MESSAGE: &str = "message:fixed-deleting-admission";
const EPISODE: &str = "episode:fixed-deleting-checkpoint";
const ATTEMPT: &str = "activation:message:message:fixed-deleting-admission";
const STARTED_TURN: &str = "turn:fixed-deleting-started";
const STARTED_RUN: &str = "run:fixed-deleting-started";

fn at() -> DateTime<Utc> {
    // Startup uses the real clock; original admitted rows must precede recovery.
    "2026-10-01T00:00:00Z".parse().unwrap()
}
fn identity(status: AgentRegistryStatus, incarnation: u64) -> AgentIdentityRecord {
    let mut record = AgentIdentityRecord::new(
        AGENT,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    record.status = status;
    record.incarnation = incarnation;
    record
}
fn input_scope(incarnation: u64) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: incarnation,
    }
}
fn episode_scope(incarnation: u64) -> EpisodeMemoryLoadScope {
    EpisodeMemoryLoadScope {
        agent_id: AGENT.into(),
        identity_incarnation: incarnation,
    }
}
fn episode() -> ContextEpisodeRecord {
    serde_json::from_value(serde_json::json!({
        "id": EPISODE, "agent_id": AGENT, "workspace_id": "agent-home", "created_at": at(), "finalized_at": at(),
        "start_turn_index": 0, "end_turn_index": 0, "start_message_count": 0,
        "end_message_count": 1, "boundary_reason": "wait_boundary"
    }))
    .unwrap()
}
fn message(id: &str) -> MessageEnvelope {
    let mut record = MessageEnvelope::new(
        AGENT,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "accepted before fixed upgrade".into(),
        },
    );
    record.id = id.into();
    record.created_at = at();
    record
}
struct Fixture {
    _directory: tempfile::TempDir,
    db: RuntimeDb,
}
impl Fixture {
    /// Seed the actual pre-feature schema, then execute the real v82-v85 hooks.
    fn legacy(identity: Option<AgentIdentityRecord>, sealed: bool) -> Result<Self> {
        Self::legacy_with_started_turn(identity, sealed, false)
    }
    fn legacy_with_started_turn(
        identity: Option<AgentIdentityRecord>,
        sealed: bool,
        started_turn: bool,
    ) -> Result<Self> {
        Self::legacy_with_execution(identity, sealed, started_turn, false)
    }
    fn legacy_with_open_attempt() -> Result<Self> {
        Self::legacy_with_execution(
            Some(identity(AgentRegistryStatus::Deleting, 7)),
            true,
            true,
            true,
        )
    }
    fn legacy_with_execution(
        identity: Option<AgentIdentityRecord>,
        sealed: bool,
        started_turn: bool,
        open_attempt: bool,
    ) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime.sqlite");
        let mut connection = Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 81)
        {
            apply_migration(&mut connection, migration)?;
        }
        let tx = connection.transaction()?;
        if let Some(identity) = identity.as_ref() {
            let mut admission_identity = identity.clone();
            if open_attempt {
                admission_identity.status = AgentRegistryStatus::Active;
            }
            upsert_agent_identity_tx(&tx, &admission_identity)?;
        }
        let (source, _) = append_message_tx(&tx, &message(MESSAGE))?;
        upsert_queue_entry_tx(
            &tx,
            &QueueEntryRecord {
                message_id: MESSAGE.into(),
                agent_id: AGENT.into(),
                priority: Priority::Normal,
                status: if open_attempt {
                    QueueEntryStatus::Dequeued
                } else {
                    QueueEntryStatus::Queued
                },
                created_at: at(),
                updated_at: at(),
            },
        )?;
        let mut state = AgentState::new(AGENT);
        // An admitted Dequeued claim is no longer in the pending queue.
        state.pending = usize::from(!open_attempt);
        state.total_message_count = 1;
        if started_turn {
            state.turn_index = 1;
            state.current_turn_id = Some(STARTED_TURN.into());
            let mut turn = TurnRecord::new(AGENT, STARTED_TURN, 1);
            turn.created_at = at();
            turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
            turn.input_message_ids.push(source.id.clone());
            if open_attempt {
                state.status = crate::types::AgentStatus::AwakeRunning;
                state.current_run_id = Some(STARTED_RUN.into());
                turn.run_id = Some(STARTED_RUN.into());
                turn.owner = Some(crate::types::TurnOwner::Conversation {
                    interaction_id: source.id.clone(),
                });
            }
            upsert_turn_record_tx(&tx, &turn)?;
        }
        if open_attempt {
            let source_revision = source.message_seq.context("actual source sequence")?;
            let admitted = admit_execution(
                &ExecutionProtocolState::empty(AGENT),
                &AdmitExecution {
                    attempt: ExecutionAttempt {
                        attempt_id: ATTEMPT.into(),
                        agent_id: AGENT.into(),
                        source_message_id: Some(source.id.clone()),
                        source: ExecutionSource {
                            identity: ExecutionSourceIdentity::QueueMessage {
                                message_id: source.id.clone(),
                            },
                            generation: source_revision,
                        },
                        binding: ExecutionBinding::Conversation {
                            interaction_id: source.id.clone(),
                        },
                        provenance: ExecutionProvenance {
                            origin: ExecutionOrigin::Operator,
                            trust: ExecutionTrust::OperatorInstruction,
                            priority: ExecutionPriority::Normal,
                            correlation_id: None,
                            causation_id: None,
                        },
                        admitted_fences: AdmittedFences {
                            source_revision,
                            work_item_source_revision: None,
                            work_item_generation: None,
                            rejoin: None,
                            agent_control_revision: 1,
                            host_registry_revision: 1,
                        },
                        state: ExecutionAttemptState::Open,
                        run_id: Some(STARTED_RUN.into()),
                        turn_id: Some(STARTED_TURN.into()),
                        recovery_of_attempt_id: None,
                        terminal_outcome_id: None,
                        admitted_at: at().to_rfc3339(),
                        terminal_at: None,
                    },
                },
            )
            .map_err(anyhow::Error::msg)?;
            crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx(
                &tx,
                &admitted.state,
            )?;
            // Original admission preceded the same-incarnation deletion fence.
            upsert_agent_identity_tx(&tx, identity.as_ref().context("admitted identity")?)?;
        }
        upsert_agent_state_tx(&tx, &state)?;
        let record = episode();
        tx.execute(
            "INSERT INTO context_episode_anchors (episode_id,agent_id,workspace_id,work_item_id,boundary_reason,
             start_turn_index,end_turn_index,started_at,ended_at,payload_json)
             VALUES (?1,?2,'agent-home',NULL,'wait_boundary',0,0,?3,?3,?4)",
            params![record.id, AGENT, at().to_rfc3339(), serde_json::to_string(&record)?],
        )?;
        if sealed {
            tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at)
                VALUES('context_episode_anchors',1,'complete','db',?1)", [at().to_rfc3339()])?;
        }
        tx.commit()?;
        drop(connection);
        let db = RuntimeDb::open_and_migrate(path, directory.path().join("runtime.lock"))?;
        Ok(Self {
            _directory: directory,
            db,
        })
    }
    fn state(&self) -> Result<AgentState> {
        self.db
            .agent_states()
            .latest(AGENT)?
            .context("missing State")
    }
    fn request(&self) -> InputEvidenceLoadRequest {
        InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: BTreeSet::from([InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                MESSAGE,
            )]),
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: BTreeSet::new(),
            record_limit: 1024,
            byte_limit: 1024 * 1024,
        }
    }
    fn prepare_cleanup(&self, incarnation: u64) -> Result<()> {
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let state = self.state()?;
        let request = self.request();
        self.db.transaction(|tx| {
            let prepared = prepare_existing_admission_cleanup_domains_tx(
                tx,
                &input_scope(incarnation),
                &state,
                &request,
                12,
                &budget,
            )?;
            assert_eq!(prepared.pending.messages.len(), 1);
            assert_eq!(prepared.pending.messages[0].id, MESSAGE);
            assert_eq!(prepared.episodes.selected(12)?[0].id, EPISODE);
            assert!(prepared
                .inputs
                .capture_for_request(&request)?
                .lookup(&InputEvidenceKey::new(
                    InputEvidenceFamily::Message,
                    MESSAGE
                ))?
                .is_some());
            Ok(())
        })
    }
    fn cleanup(&self, incarnation: u64, nonce: &str) -> Result<PreparedInputTransition> {
        let expected: QueueEntryRecord = serde_json::from_str(&self.db.connection()?.query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id=?1",
            [MESSAGE],
            |row| row.get::<_, String>(0),
        )?)?;
        let mut record = expected.clone();
        // Queued -> Dropped is a legal original-admission settlement.
        record.status = QueueEntryStatus::Dropped;
        let before = self.state()?;
        let mut after = before.clone();
        after.pending = 0;
        let mut brief = BriefRecord::new(
            AGENT,
            BriefKind::Failure,
            "admitted cleanup finished",
            Some(MESSAGE.into()),
            None,
        );
        brief.id = "brief:fixed-deleting-cleanup".into();
        brief.created_at = at();
        PreparedInputTransition::queue(
            input_scope(incarnation),
            nonce.into(),
            at(),
            PreparedQueueTransition::new(QueueTransitionCommand {
                agent_id: AGENT.into(),
                operation: QueueOperation::Settle,
                mutation: QueueMutation::CompareAndSet { expected, record },
                scheduler_claim_work_item: None,
                agent_state: Some(AgentStateMutation {
                    expected: Some(Box::new(before)),
                    record: Box::new(after),
                }),
                message_evidence: Vec::new(),
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: vec![crate::types::brief_created_event_for(&brief)?],
                notify_scheduler: true,
                fault: None,
                brief_evidence: vec![brief],
            }),
        )
    }
    fn recover_startup(
        &self,
    ) -> Result<crate::runtime_db::transitions::StartupInputRecoveryCommit> {
        let lease_path = self._directory.path().join("runtime.maintenance.lock");
        let lease = crate::runtime_db::RuntimeDbLock::try_lock(&lease_path)?;
        let handoff = tokio::sync::Mutex::new(());
        let handoff_guard = handoff.try_lock().unwrap();
        let proof = crate::runtime_db::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery::for_pre_activation_host(
            &self.db, &lease, &lease_path, &handoff_guard,
        )?;
        let scopes = self.db.startup_input_recovery_scopes(&[AGENT.to_owned()])?;
        self.db
            .recover_interrupted_runtime_state_at_startup_in_scopes(&scopes, &proof)
    }
    fn recovery_rows(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let connection = self.db.connection()?;
        let mut snapshot = BTreeMap::new();
        for table in [
            "agent_states",
            "turn_records",
            "messages",
            "queue_entries",
            "audit_events",
            "execution_protocol_partitions",
            "execution_protocol_attempts",
            "execution_protocol_outcomes",
            "execution_protocol_command_results",
            "input_evidence_current",
            "input_evidence_execution_current_sources",
            "conversation_input_assignments",
            "conversation_source_revisions",
            "conversation_turn_revisions",
            "runtime_sequences",
        ] {
            let mut statement = connection.prepare(&format!("SELECT * FROM {table}"))?;
            let columns = statement.column_count();
            let mut rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get::<_, rusqlite::types::Value>(column))
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map(|row| format!("{row:?}"))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.sort();
            snapshot.insert(table.to_owned(), rows);
        }
        Ok(snapshot)
    }
    fn assert_recovered_attempt_qualified(&self) -> Result<()> {
        let outcome_id = format!("outcome:startup_interrupted:{ATTEMPT}");
        let mut request = self.request();
        request.mandatory_roots.extend([
            InputEvidenceKey::new(InputEvidenceFamily::Turn, STARTED_TURN),
            InputEvidenceKey::new(InputEvidenceFamily::Attempt, ATTEMPT),
            InputEvidenceKey::new(InputEvidenceFamily::Outcome, &outcome_id),
        ]);
        request.interrupted_sources.insert(MESSAGE.into());
        self.db.transaction(|tx| {
            let prepared =
                prepare_original_input_recovery_snapshot_tx(tx, &input_scope(7), &request)?;
            let InputEvidenceBootstrapSnapshot::Current(prepared) = prepared else {
                anyhow::bail!("original same-incarnation cleanup scope was retired");
            };
            let captured = prepared.resident.capture_for_request(&request)?;
            assert_eq!(captured.scope, input_scope(7));
            let protocol = captured.protocol_state()?;
            let attempt = &protocol.attempts[ATTEMPT];
            assert_eq!(attempt.state, ExecutionAttemptState::Interrupted);
            assert_eq!(attempt.source_message_id.as_deref(), Some(MESSAGE));
            assert_eq!(attempt.turn_id.as_deref(), Some(STARTED_TURN));
            assert_eq!(
                attempt.terminal_outcome_id.as_deref(),
                Some(outcome_id.as_str())
            );
            assert!(matches!(
                &protocol.outcomes[&outcome_id].outcome,
                ExecutionOutcome::Conversation(ConversationOutcome::Interrupted { .. })
            ));
            let turn = captured
                .lookup(&InputEvidenceKey::new(
                    InputEvidenceFamily::Turn,
                    STARTED_TURN,
                ))?
                .context("original recovered Turn body")?;
            let crate::runtime::resident_input_evidence::PreparedInputEvidence::Turn(turn) =
                &turn.record
            else {
                anyhow::bail!("recovered Turn locator has the wrong body family");
            };
            assert_eq!(
                turn.terminal.as_ref().unwrap().kind,
                crate::types::TurnTerminalKind::Interrupted
            );
            assert_eq!(turn.run_id.as_deref(), Some(STARTED_RUN));
            assert_eq!(turn.input_message_ids, vec![MESSAGE.to_owned()]);
            let source = captured.execution.source(MESSAGE)?;
            source.validate()?;
            assert_eq!(source.attempts.len(), 1);
            let observation = &source.attempts[ATTEMPT];
            assert_eq!(observation.state, ExecutionAttemptState::Interrupted);
            assert_eq!(
                observation.terminal_outcome_id.as_deref(),
                Some(outcome_id.as_str())
            );
            assert!(!observation.pruned);
            assert!(captured
                .lookup(&InputEvidenceKey::new(
                    InputEvidenceFamily::Message,
                    MESSAGE
                ))?
                .is_some());
            Ok(())
        })
    }
    fn assert_normal_rejected(&self, incarnation: u64) -> Result<()> {
        assert!(matches!(
            self.db
                .load_episode_memory_snapshot(&episode_scope(incarnation), 12, None)?,
            EpisodeMemoryBootstrapSnapshot::Retired { .. }
        ));
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let state = self.state()?;
        assert!(self
            .db
            .transaction(|tx| prepare_input_domains_tx(
                tx,
                &input_scope(incarnation),
                &state,
                &self.request(),
                12,
                &budget,
            ))
            .is_err());
        assert!(self
            .db
            .evidence()
            .append_message_in_scope(
                &input_scope(incarnation),
                &message("message:new-during-deleting")
            )
            .is_err());
        let new_count: i64 = self.db.connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE evidence_id='message:new-during-deleting'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(new_count, 0);
        Ok(())
    }
}

#[test]
fn real_upgrade_deleting_scope_preserves_admitted_cleanup_without_activation() -> Result<()> {
    let fixture = Fixture::legacy_with_started_turn(
        Some(identity(AgentRegistryStatus::Deleting, 7)),
        true,
        true,
    )?;
    fixture.assert_normal_rejected(7)?;
    // Use the original exclusive startup actor, with both genuine borrows held.
    // Its canonical interruption must advance the original qualified Turn too.
    let lease_path = fixture._directory.path().join("runtime.maintenance.lock");
    let lease = crate::runtime_db::RuntimeDbLock::try_lock(&lease_path)?;
    let handoff = tokio::sync::Mutex::new(());
    let handoff_guard = handoff.try_lock().unwrap();
    let proof = crate::runtime_db::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery::for_pre_activation_host(
        &fixture.db, &lease, &lease_path, &handoff_guard,
    )?;
    let scopes = fixture
        .db
        .startup_input_recovery_scopes(&[AGENT.to_owned()])?;
    let recovered = fixture
        .db
        .recover_interrupted_runtime_state_at_startup_in_scopes(&scopes, &proof)?;
    assert_eq!(recovered.report.daemon_restart_turns, 1);
    assert!(recovered
        .input_deltas
        .iter()
        .any(|(scope, delta)| scope == &input_scope(7)
            && delta.records.iter().any(|record| record.locator.key
                == InputEvidenceKey::new(
                    InputEvidenceFamily::Turn,
                    "turn:fixed-deleting-started"
                ))));
    let turn = fixture
        .db
        .transaction(|tx| {
            crate::runtime_db::repositories::committed_turn_record_tx(
                tx,
                AGENT,
                "turn:fixed-deleting-started",
            )
        })?
        .record;
    assert_eq!(
        turn.terminal.unwrap().kind,
        crate::types::TurnTerminalKind::Interrupted
    );
    drop(proof);
    drop(handoff_guard);
    drop(lease);
    fixture.prepare_cleanup(7)?;
    let prepared = fixture.cleanup(7, "fixed-upgrade-cleanup")?;
    let receipt = match fixture
        .db
        .transitions_in_scope(&input_scope(7))
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Committed { value, receipt } => {
            assert!(value.applied);
            assert!(value.effects.briefs[0].created_event_seq.is_some());
            assert!(value
                .effects
                .input_evidence_delta
                .records
                .iter()
                .any(|record| record.locator.key.id == "brief:fixed-deleting-cleanup"));
            receipt
        }
        _ => panic!("first cleanup must commit"),
    };
    assert_eq!(fixture.state()?.pending, 0);
    assert!(matches!(fixture.db.transitions_in_scope(&input_scope(7))
        .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { receipt: original, .. } if original == receipt));
    assert_eq!(
        fixture.db.agent_identities().latest(AGENT)?.unwrap().status,
        AgentRegistryStatus::Deleting
    );
    Ok(())
}

#[test]
fn fixed_upgrade_skips_deleted_and_reincarnation_never_inherits_deleting_seed() -> Result<()> {
    let deleted = Fixture::legacy(Some(identity(AgentRegistryStatus::Deleted, 7)), true)?;
    let count: i64 = deleted.db.connection()?.query_row(
        "SELECT COUNT(*) FROM episode_continuity_seeds WHERE agent_id=?1",
        [AGENT],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    assert!(deleted.prepare_cleanup(7).is_err());

    let fixture = Fixture::legacy(Some(identity(AgentRegistryStatus::Deleting, 7)), true)?;
    fixture.prepare_cleanup(7)?;
    let original = fixture.cleanup(7, "old-incarnation-cleanup")?;
    fixture
        .db
        .agent_identities()
        .upsert(&identity(AgentRegistryStatus::Active, 8))?;
    let current = fixture
        .db
        .load_episode_memory_snapshot(&episode_scope(8), 12, None)?
        .require_current()?;
    assert_eq!(current.continuity, EpisodeContinuityBinding::FreshEmpty);
    assert!(current.episodes.is_empty());
    assert!(fixture.prepare_cleanup(8).is_err());
    assert!(fixture
        .db
        .transitions_in_scope(&input_scope(7))
        .commit_prepared_transition(&original)
        .is_err());
    let old: MessageEnvelope = serde_json::from_str(&fixture.db.connection()?.query_row(
        "SELECT payload_json FROM messages WHERE evidence_id=?1",
        [MESSAGE],
        |row| row.get::<_, String>(0),
    )?)?;
    assert!(fixture
        .db
        .evidence()
        .append_message_in_scope(&input_scope(8), &old)
        .is_err());
    assert_eq!(fixture.state()?.pending, 1);
    Ok(())
}

#[test]
fn first_json_deleting_identity_binds_fixed_unbound_scope_only_through_import() -> Result<()> {
    let fixture = Fixture::legacy(None, false)?;
    let seed: String = fixture.db.connection()?.query_row(
        "SELECT seed_id FROM episode_continuity_seeds WHERE agent_id=?1",
        [AGENT],
        |row| row.get(0),
    )?;
    let imported = identity(AgentRegistryStatus::Deleting, 7);
    // Ordinary no-identity materialization must not gain this import authority.
    assert!(fixture
        .db
        .transaction(|tx| bind_unbound_episode_continuity_tx(tx, &imported, at()))
        .is_err());
    fixture
        .db
        .agent_identities()
        .import_legacy(vec![imported])?;
    assert!(fixture.prepare_cleanup(7).is_err());
    fixture
        .db
        .context_episodes()
        .import_legacy(vec![episode()])?;
    let bound: String = fixture.db.connection()?.query_row(
        "SELECT seed_id FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=7",
        [AGENT],
        |row| row.get(0),
    )?;
    assert_eq!(bound, seed);
    fixture.assert_normal_rejected(7)?;
    fixture.prepare_cleanup(7)?;
    let prepared = fixture.cleanup(7, "imported-deleting-cleanup")?;
    assert!(matches!(
        fixture
            .db
            .transitions_in_scope(&input_scope(7))
            .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}

#[test]
fn fixed_deleting_import_requires_one_time_seal_before_cleanup_preparation() -> Result<()> {
    let fixture = Fixture::legacy(Some(identity(AgentRegistryStatus::Deleting, 7)), false)?;
    let error = fixture.prepare_cleanup(7).unwrap_err();
    assert_eq!(
        error.downcast_ref::<EpisodeSnapshotError>(),
        Some(&EpisodeSnapshotError::LegacyImportPending)
    );
    fixture
        .db
        .context_episodes()
        .import_legacy(vec![episode()])?;
    fixture.prepare_cleanup(7)?;
    // A later import invocation cannot widen the fixed membership or rewrite its seal.
    let mut late = episode();
    late.id = "episode:late-after-deleting-import".into();
    fixture
        .db
        .context_episodes()
        .import_legacy(vec![late.clone()])?;
    let connection = fixture.db.connection()?;
    let seals: i64 = connection.query_row(
        "SELECT COUNT(*) FROM episode_continuity_imports",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(seals, 1);
    let late_rows: i64 = connection.query_row(
        "SELECT COUNT(*) FROM context_episode_anchors WHERE episode_id=?1",
        [&late.id],
        |row| row.get(0),
    )?;
    assert_eq!(late_rows, 0);
    drop(connection);
    let prepared = fixture.cleanup(7, "sealed-deleting-cleanup")?;
    assert!(matches!(
        fixture
            .db
            .transitions_in_scope(&input_scope(7))
            .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}

#[test]
fn deleting_upgrade_startup_qualifies_original_attempt_interruption_and_source() -> Result<()> {
    let fixture = Fixture::legacy_with_open_attempt()?;
    assert_eq!(fixture.state()?.pending, 0);
    let attempt_status: String = fixture.db.connection()?.query_row(
        "SELECT lifecycle_state FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",
        params![AGENT, ATTEMPT], |row| row.get(0))?;
    assert_eq!(attempt_status, "open");
    let recovered = fixture.recover_startup()?;
    assert_eq!(recovered.report.interrupted_turns, 1);
    assert_eq!(recovered.report.orphaned_claim_turns, 1);
    assert_eq!(recovered.report.interrupted_execution_attempts, 1);
    assert_eq!(recovered.report.queue_entries_changed, 1);
    let delta = &recovered
        .input_deltas
        .iter()
        .find(|(scope, _)| scope == &input_scope(7))
        .context("same-incarnation recovery delta")?
        .1;
    for key in [
        InputEvidenceKey::new(InputEvidenceFamily::Turn, STARTED_TURN),
        InputEvidenceKey::new(InputEvidenceFamily::Attempt, ATTEMPT),
        InputEvidenceKey::new(
            InputEvidenceFamily::Outcome,
            format!("outcome:startup_interrupted:{ATTEMPT}"),
        ),
    ] {
        assert!(
            delta.records.iter().any(|record| record.locator.key == key),
            "missing {key:?}"
        );
    }
    let status: String = fixture.db.connection()?.query_row(
        "SELECT status FROM queue_entries WHERE message_id=?1",
        [MESSAGE],
        |row| row.get(0),
    )?;
    assert_eq!(status, "interrupted");
    // Global startup intentionally does not normalize State or activate Deleting.
    assert_eq!(fixture.state()?.pending, 0);
    let preparation_error = fixture.prepare_cleanup(7).unwrap_err();
    assert!(format!("{preparation_error:#}").contains("canonical pending queue differs from State"));
    fixture.assert_recovered_attempt_qualified()?;
    fixture.assert_normal_rejected(7)?;
    assert_eq!(
        fixture.db.agent_identities().latest(AGENT)?.unwrap().status,
        AgentRegistryStatus::Deleting
    );
    Ok(())
}

#[test]
fn deleting_upgrade_startup_outcome_alias_failure_rolls_back_complete_recovery() -> Result<()> {
    let fixture = Fixture::legacy_with_open_attempt()?;
    let before = fixture.recovery_rows()?;
    assert_eq!(before["execution_protocol_attempts"].len(), 1);
    assert!(before["execution_protocol_outcomes"].is_empty());
    assert!(before["audit_events"].is_empty());
    fixture.db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER fail_deleting_startup_outcome_alias
            BEFORE INSERT ON input_evidence_current WHEN NEW.family='outcome'
            BEGIN SELECT RAISE(ABORT,'deleting startup outcome alias fault'); END;",
        )?;
        Ok(())
    })?;
    let error = fixture.recover_startup().unwrap_err();
    assert!(
        format!("{error:#}").contains("deleting startup outcome alias fault"),
        "{error:#}"
    );
    assert_eq!(
        fixture.recovery_rows()?,
        before,
        "failed qualification must roll back canonical recovery and its audit/sequence effects"
    );
    fixture.db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER fail_deleting_startup_outcome_alias")?;
        Ok(())
    })?;
    // The identical original Attempt remains eligible and can recover after rollback.
    let recovered = fixture.recover_startup()?;
    assert_eq!(recovered.report.interrupted_execution_attempts, 1);
    fixture.assert_recovered_attempt_qualified()?;
    Ok(())
}
