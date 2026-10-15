use super::*;
use crate::{
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        repositories::upsert_queue_entry_tx,
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, ContextEpisodeRecord, MessageBody, MessageEnvelope,
        MessageKind, MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus,
    },
};

struct Fixture {
    _home: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
    identity: AgentIdentityRecord,
    state: AgentState,
    episode: ContextEpisodeRecord,
    message: MessageEnvelope,
}
impl Fixture {
    fn new(fixed: bool) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "episode-pending-joint".into(),
            identity_incarnation: 1,
        };
        let identity = AgentIdentityRecord::new(
            &scope.agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        let mut state = AgentState::new(&scope.agent_id);
        state.pending = 1;
        let at: chrono::DateTime<chrono::Utc> = "2026-10-14T01:02:03Z".parse()?;
        let episode: ContextEpisodeRecord = serde_json::from_value(serde_json::json!({
            "id":"original-episode","agent_id":scope.agent_id,"created_at":at,"finalized_at":at,
            "start_turn_index":1,"end_turn_index":1,"start_message_count":0,"end_message_count":1,
            "boundary_reason":"wait_boundary","objective":"original Episode",
        }))?;
        let message = db.transaction(|tx| {
            upsert_agent_identity_tx(tx,&identity)?;
            upsert_agent_state_tx(tx,&state)?;
            let message = MessageEnvelope::new(&scope.agent_id,MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id:None,actor_display_name:None },AuthorityClass::OperatorInstruction,
                Priority::Normal,MessageBody::Text { text:"original pending body".into() });
            let (message, inserted) = append_message_tx(tx,&message)?;
            assert!(inserted);
            upsert_queue_entry_tx(tx,&QueueEntryRecord { message_id:message.id.clone(),agent_id:scope.agent_id.clone(),
                priority:message.priority.clone(),status:QueueEntryStatus::Queued,created_at:message.created_at,updated_at:message.created_at })?;
            let payload = serde_json::to_string(&episode)?;
            tx.execute("INSERT INTO context_episode_anchors(episode_id,agent_id,workspace_id,work_item_id,boundary_reason,start_turn_index,end_turn_index,started_at,ended_at,payload_json) VALUES(?1,?2,?3,NULL,'wait_boundary',1,1,?4,?4,?5)",
                params![episode.id,scope.agent_id,episode.workspace_id,at.to_rfc3339(),payload])?;
            if fixed {
                use sha2::{Digest,Sha256};
                tx.execute("INSERT INTO episode_continuity_seeds(seed_id,agent_id,policy_version,boundary_id,identity_incarnation,source_kind,identity_digest,roots_json,candidate_payloads_json,import_required,captured_at) VALUES('joint-original-seed',?1,1,'original-boundary',1,'upgrade_identity',NULL,'{}','[]',0,?2)",params![scope.agent_id,at.to_rfc3339()])?;
                tx.execute("UPDATE agent_episode_continuity SET mode='legacy_adopted',seed_id='joint-original-seed' WHERE agent_id=?1 AND identity_incarnation=1",[&scope.agent_id])?;
                tx.execute("INSERT INTO episode_continuity_members(seed_id,capture_phase,episode_id,content_digest,qualification_json) VALUES('joint-original-seed','upgrade',?1,?2,'{}')",params![episode.id,format!("{:x}",Sha256::digest(payload.as_bytes()))])?;
            } else {
                let phase = crate::runtime_db::episode_memory::EPISODE_MEMORY_PHASE;
                let invocation = crate::runtime_db::episode_memory::EpisodeMemoryInvocation { message_id:message.id.clone(),turn_index:1 };
                let receipt: crate::runtime_db::episode_memory::EpisodeMemoryReceipt = serde_json::from_value(serde_json::json!({
                    "phase":phase,"key":{"agent_id":scope.agent_id,"identity_incarnation":1,"actual_dispatch_turn_id":"original-turn"},
                    "invocation":invocation,"command_digest":"original-command",
                    "outcome":{"episode_memory_changed":true,"business_fields_changed":true,"boundary_reason":"wait_boundary",
                        "finalized_episode_id":episode.id,"previous_builder_id":null,"active_builder_id":null,
                        "compression_epoch_before":0,"compression_epoch_after":0,"archived_episode_count_before":0,"archived_episode_count_after":1},
                    "agent_control_revision":1,"audit_bindings":[],"index_bindings":[],"committed_at":at,
                }))?;
                tx.execute("INSERT INTO episode_memory_results(agent_id,identity_incarnation,turn_id,phase_version,command_digest,invocation_json,finalized_episode_id,result_json,committed_at) VALUES(?1,1,'original-turn',?2,'original-command',?3,?4,?5,?6)",
                    params![scope.agent_id,phase,serde_json::to_string(&invocation)?,episode.id,serde_json::to_string(&receipt)?,at.to_rfc3339()])?;
            }
            Ok(message)
        })?;
        Ok(Self {
            _home: home,
            db,
            scope,
            identity,
            state,
            episode,
            message,
        })
    }
}
fn policy(bytes: usize, records: usize) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: bytes,
        record_limit: records,
    }
}
fn resource(error: anyhow::Error) {
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
}
fn read_both(
    f: &Fixture,
    request: &InputEvidenceLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(usize, usize)> {
    f.db.transaction(|tx| {
        let mut input = InputReadMaterial::new(&f.scope, request, ledger, (0, 0))?;
        input.observe_projected(1024 * 1024, 1)?;
        let held = vec![0u8; 1024 * 1024];
        let metadata = input.pending_metadata_tx(tx, &f.scope, ledger, false)?;
        assert_eq!(metadata.count, 1);
        let current = input.episode_snapshot_tx(tx, &f.scope, ledger, 12, false)?;
        let pending = input.pending_queue_inputs_tx(tx, &f.scope, ledger, false)?;
        assert_eq!(current.identity, f.identity);
        assert_eq!(current.state.as_ref(), Some(&f.state));
        assert_eq!(current.episodes, vec![f.episode.clone()]);
        assert_eq!(pending.messages, vec![f.message.clone()]);
        assert_eq!(pending.metadata.facts.state.as_ref(), Some(&f.state));
        assert_eq!(held.len(), 1024 * 1024);
        let usage = input.usage();
        let last = input
            .guards
            .last()
            .context("actual pending guard missing")?
            .clone();
        let weak = Arc::downgrade(&last);
        drop((pending, current, metadata, input, held));
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
        Ok(usage)
    })
}

#[test]
fn actual_episode_and_pending_sql_share_original_input_remaining_and_last_guards() -> Result<()> {
    for fixed in [false, true] {
        let f = Fixture::new(fixed)?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
        let usage = read_both(&f, &full, &ledger)?;
        assert!(usage.0 > 1024 * 1024 && usage.1 > 0);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        for nodes in [false, true] {
            let limited = policy(
                if nodes { full.byte_limit } else { usage.0 - 1 },
                if nodes {
                    usage.1 - 1
                } else {
                    full.record_limit
                },
            );
            resource(
                read_both(&f, &limited, &ledger)
                    .err()
                    .context("Episode/Pending received separate full caps")?,
            );
            assert_eq!(ledger.report()?.observed_bytes, 0);
            assert_eq!(ledger.report()?.pending.count, 0);
        }
        assert_eq!(read_both(&f, &policy(usage.0, usage.1), &ledger)?, usage);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let aliases: usize = f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_evidence_current",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(aliases, 0, "read budgets cannot mint Input qualification");
    }
    Ok(())
}

#[test]
fn original_selected_raw_material_rejects_oversized_malformed_before_json() -> Result<()> {
    for target in ["episode", "receipt", "queue", "message"] {
        let f = Fixture::new(false)?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
        let baseline = f.db.transaction(|tx| {
            let mut input = InputReadMaterial::new(&f.scope, &full, &ledger, (0, 0))?;
            if matches!(target, "episode" | "receipt") {
                drop(input.episode_snapshot_tx(tx, &f.scope, &ledger, 12, false)?);
            } else {
                drop(input.pending_queue_inputs_tx(tx, &f.scope, &ledger, false)?);
            }
            Ok(input.usage())
        })?;
        let limited = policy(baseline.0 + 128 * 1024, MAX_RECORDS);
        f.db.connection()?.execute(
            match target {
                "episode" => "UPDATE context_episode_anchors SET payload_json=?1",
                "receipt" => "UPDATE episode_memory_results SET result_json=?1",
                "queue" => "UPDATE queue_entries SET payload_json=?1",
                _ => "UPDATE messages SET payload_json=?1",
            },
            ["[".to_owned() + &"x".repeat(1024 * 1024)],
        )?;
        let error =
            f.db.transaction(|tx| {
                let mut input = InputReadMaterial::new(&f.scope, &limited, &ledger, (0, 0))?;
                if matches!(target, "episode" | "receipt") {
                    input
                        .episode_snapshot_tx(tx, &f.scope, &ledger, 12, false)
                        .map(|_| ())
                } else {
                    input
                        .pending_queue_inputs_tx(tx, &f.scope, &ledger, false)
                        .map(|_| ())
                }
            })
            .err()
            .context("malformed selected source bypassed raw gate")?;
        resource(error);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(ledger.report()?.pending.count, 0);
    }
    Ok(())
}

#[test]
fn complete_native_object_keys_gate_episode_receipt_and_pending_before_serde() -> Result<()> {
    for target in ["episode", "invocation", "receipt", "queue", "message"] {
        let f = Fixture::new(false)?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
        let (select, update) = match target {
            "episode" => (
                "SELECT payload_json FROM context_episode_anchors",
                "UPDATE context_episode_anchors SET payload_json=?1",
            ),
            "invocation" => (
                "SELECT invocation_json FROM episode_memory_results",
                "UPDATE episode_memory_results SET invocation_json=?1",
            ),
            "receipt" => (
                "SELECT result_json FROM episode_memory_results",
                "UPDATE episode_memory_results SET result_json=?1",
            ),
            "queue" => (
                "SELECT payload_json FROM queue_entries",
                "UPDATE queue_entries SET payload_json=?1",
            ),
            _ => (
                "SELECT payload_json FROM messages",
                "UPDATE messages SET payload_json=?1",
            ),
        };
        let baseline = f.db.transaction(|tx| {
            let mut input = InputReadMaterial::new(&f.scope, &full, &ledger, (0, 0))?;
            if matches!(target, "episode" | "invocation" | "receipt") {
                drop(input.episode_snapshot_tx(tx, &f.scope, &ledger, 12, false)?);
            } else {
                drop(input.pending_queue_inputs_tx(tx, &f.scope, &ledger, false)?);
            }
            Ok(input.usage().1)
        })?;
        let raw: String = f.db.connection()?.query_row(select, [], |row| row.get(0))?;
        let mut value: serde_json::Value = serde_json::from_str(&raw)?;
        value["original_ignored_extra"] = serde_json::Value::Object(
            (0..5000)
                .map(|i| (format!("key-{i}"), serde_json::Value::Null))
                .collect(),
        );
        let raw = serde_json::to_string(&value)?;
        assert!(raw.len() * 8 < full.byte_limit);
        f.db.connection()?.execute(update, [raw])?;
        let limited = policy(full.byte_limit, baseline + 4096);
        let error =
            f.db.transaction(|tx| {
                let mut input = InputReadMaterial::new(&f.scope, &limited, &ledger, (0, 0))?;
                if matches!(target, "episode" | "invocation" | "receipt") {
                    input
                        .episode_snapshot_tx(tx, &f.scope, &ledger, 12, false)
                        .map(|_| ())
                } else {
                    input
                        .pending_queue_inputs_tx(tx, &f.scope, &ledger, false)
                        .map(|_| ())
                }
            })
            .err()
            .context("small raw/high-node source reached original decoder")?;
        resource(error);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn original_episode_membership_and_recovery_scope_remain_strict_under_shared_material() -> Result<()>
{
    let f = Fixture::new(true)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
    // Unqualified late malformed rows never enter this fixed window.
    f.db.connection()?.execute("INSERT INTO context_episode_anchors SELECT 'raw-late',agent_id,workspace_id,work_item_id,boundary_reason,start_turn_index,end_turn_index,started_at,ended_at,'{' FROM context_episode_anchors WHERE episode_id=?1",[&f.episode.id])?;
    f.db.transaction(|tx| {
        let mut input = InputReadMaterial::new(&f.scope, &full, &ledger, (0, 0))?;
        assert_eq!(
            input
                .episode_snapshot_tx(tx, &f.scope, &ledger, 12, false)?
                .episodes,
            vec![f.episode.clone()]
        );
        Ok(())
    })?;
    let mut changed = f.episode.clone();
    changed.objective = Some("changed fixed source".into());
    f.db.connection()?.execute(
        "UPDATE context_episode_anchors SET payload_json=?1 WHERE episode_id=?2",
        params![serde_json::to_string(&changed)?, changed.id],
    )?;
    let error =
        f.db.transaction(|tx| {
            let mut input = InputReadMaterial::new(&f.scope, &full, &ledger, (0, 0))?;
            input.episode_snapshot_tx(tx, &f.scope, &ledger, 12, false)
        })
        .err()
        .context("changed original fixed source was adopted")?;
    assert!(
        matches!(
            error.downcast_ref::<EpisodeSnapshotError>(),
            Some(EpisodeSnapshotError::LegacyContentChanged { .. })
        ),
        "{error:#}"
    );
    let other = InputEvidenceBudgetLedger::new(Default::default())?;
    f.db.connection()?
        .execute_batch("ALTER TABLE agent_states RENAME TO episode_pending_state_forbidden")?;
    f.db.transaction(|tx| {
        let mut input = InputReadMaterial::new(&f.scope, &full, &ledger, (0, 0))?;
        assert!(input
            .pending_queue_inputs_tx(tx, &f.scope, &other, false)
            .err()
            .context("foreign Pending phase accepted")?
            .to_string()
            .contains("another original ledger"));
        let mut successor = f.identity.clone();
        successor.incarnation = 2;
        tx.execute(
            "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
            params![serde_json::to_string(&successor)?, f.scope.agent_id],
        )?;
        let error = input
            .episode_snapshot_tx(tx, &f.scope, &ledger, 12, true)
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::RetiredScope)
            ),
            "{error:#}"
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(other.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_deleting_recovery_keeps_scope_and_pending_logical_accepted_charge() -> Result<()> {
    let f = Fixture::new(false)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
    let original = f.db.transaction(|tx| {
        crate::runtime_db::bootstrap_inputs::pending_queue_inputs_and_charges_tx(
            tx, &f.scope, &ledger,
        )
    })?;
    assert_eq!(original.messages, vec![f.message.clone()]);
    let before = ledger.report()?.pending;
    assert_eq!(before.count, 1);
    let mut deleting = f.identity.clone();
    deleting.status = crate::types::AgentRegistryStatus::Deleting;
    deleting.updated_at = chrono::Utc::now();
    f.db.transaction(|tx| upsert_agent_identity_tx(tx, &deleting))?;
    f.db.transaction(|tx| {
        let mut input = InputReadMaterial::new(&f.scope, &full, &ledger, (0, 0))?;
        let current = input.episode_snapshot_tx(tx, &f.scope, &ledger, 12, true)?;
        assert_eq!(
            current.identity.status,
            crate::types::AgentRegistryStatus::Deleting
        );
        assert_eq!(current.episodes, vec![f.episode.clone()]);
        let pending = input.pending_queue_inputs_tx(tx, &f.scope, &ledger, true)?;
        assert_eq!(pending.messages, original.messages);
        let error = input
            .pending_metadata_tx(tx, &f.scope, &ledger, false)
            .err()
            .context("ordinary read accepted Deleting")?;
        assert!(
            matches!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::RetiredScope)
            ),
            "{error:#}"
        );
        Ok(())
    })?;
    assert_eq!(
        ledger.report()?.pending,
        before,
        "decoder accounting cannot re-install logical Accepted responsibility"
    );
    Ok(())
}

#[test]
fn unsealed_original_episode_import_precedes_selected_raw_and_native_material() -> Result<()> {
    for original_recovery in [false, true] {
        let f = Fixture::new(true)?;
        f.db.connection()?.execute(
            "UPDATE episode_continuity_seeds SET import_required=1 WHERE seed_id='joint-original-seed'",
            [],
        )?;
        let malformed = "[".to_owned() + &"x".repeat(9 * 1024 * 1024);
        assert!(malformed.len() * 8 > MAX_METADATA_BYTES);
        f.db.connection()?.execute(
            "UPDATE context_episode_anchors SET payload_json=?1 WHERE episode_id=?2",
            params![malformed, f.episode.id],
        )?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let full = policy(MAX_METADATA_BYTES, MAX_RECORDS);
        f.db.transaction(|tx| {
            let episode_scope = EpisodeMemoryLoadScope {
                agent_id: f.scope.agent_id.clone(),
                identity_incarnation: f.scope.identity_incarnation,
            };
            let original = if original_recovery {
                crate::runtime_db::episode_snapshot::load_episode_memory_for_input_recovery_in(
                    tx,
                    &episode_scope,
                    12,
                )
            } else {
                crate::runtime_db::episode_snapshot::load_episode_memory_snapshot_in(
                    tx,
                    &episode_scope,
                    12,
                    None,
                )
            };
            assert!(matches!(
                original
                    .err()
                    .context("original unsealed Episode import was accepted")?
                    .downcast_ref::<EpisodeSnapshotError>(),
                Some(EpisodeSnapshotError::LegacyImportPending)
            ));
            let mut input = InputReadMaterial::new(&f.scope, &full, &ledger, (0, 0))?;
            let error = input
                .episode_snapshot_tx(tx, &f.scope, &ledger, 12, original_recovery)
                .err()
                .context("joint loader read an unsealed selected Episode")?;
            assert!(
                matches!(
                    error.downcast_ref::<EpisodeSnapshotError>(),
                    Some(EpisodeSnapshotError::LegacyImportPending)
                ),
                "{error:#}"
            );
            assert!(
                input.usage().0 < 1024 * 1024,
                "unsealed import touched the 9MiB selected Episode"
            );
            let aliases: usize =
                tx.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(aliases, 0);
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(ledger.report()?.pending.count, 0);
        let raw_bytes: usize = f.db.connection()?.query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM context_episode_anchors WHERE episode_id=?1",
            [&f.episode.id], |r|r.get(0),
        )?;
        assert_eq!(raw_bytes, malformed.len());
    }
    Ok(())
}
