//! Fenced deletion only: settle execution first, then erase private projections.
//!
//! Ownership is explicit rather than inferred from timestamps or turn incarnation.
//! Identity reservations and content-free canonical lineage/supervision facts survive.

use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::{
    evidence::upsert_agent_state_tx,
    legacy_scheduler_wire::{ActivationLifecycleState, AdmitActivationCommand},
    repositories::{upsert_queue_entry_tx, upsert_turn_record_tx, upsert_wait_condition_tx},
    task_result_settlement::{settle_activation_tx, TaskResultSettlementDisposition},
    transitions::execution_protocol_repository::retire_agent_execution_tx,
    RuntimeDb,
};
use crate::types::{
    AgentIdentityRecord, AgentRegistryStatus, AgentState, AgentStatus, QueueEntryRecord,
    QueueEntryStatus, TurnNoBriefReason, TurnRecord, TurnTerminalKind, TurnTerminalSummary,
    WaitConditionRecord, WaitConditionStatus,
};

const REASON: &str = "agent_deleted";

// Child/derived rows precede source rows. Optional memory tables share this DB.
// Jointly owned records (reports, delegations, workspaces) are not in this list.
const PRIVATE_TABLES: &[(&str, &str)] = &[
    ("memory_index_rebuild_seen", "agent_id"),
    ("memory_index_rebuild_jobs", "agent_id"),
    ("memory_index_cursors", "agent_id"),
    ("memory_index_checkpoints", "agent_id"),
    ("memory_index_meta", "agent_id"),
    ("memory_index_pending_sources", "agent_id"),
    ("memory_index_source_state", "agent_id"),
    ("memory_documents", "agent_id"),
    ("conversation_input_assignments", "agent_id"),
    ("conversation_source_revisions", "agent_id"),
    ("conversation_turn_revisions", "agent_id"),
    ("runtime_index_outbox", "agent_id"),
    ("runtime_index_outbox_watermarks", "agent_id"),
    ("queue_head_no_progress", "agent_id"),
    ("source_revision_repair_blocks", "agent_id"),
    ("brief_created_linkage_uncertain", "agent_id"),
    ("agent_brief_read_cursors", "agent_id"),
    ("operator_notifications", "agent_id"),
    ("operator_delivery_records", "agent_id"),
    ("operator_transport_bindings", "target_agent_id"),
    ("agent_message_deliveries", "target_agent_id"),
    ("agent_message_wait_outcomes", "owner_agent_id"),
    ("task_result_settlements", "agent_id"),
    ("scheduler_continuation_admissions", "agent_id"),
    ("scheduler_activation_settlements", "agent_id"),
    ("scheduler_activation_sources", "agent_id"),
    ("scheduler_activation_inputs", "agent_id"),
    ("scheduler_activations", "agent_id"),
    ("scheduler_protocol_command_results", "agent_id"),
    ("execution_protocol_command_results", "agent_id"),
    ("execution_protocol_outcomes", "agent_id"),
    ("execution_protocol_attempts", "agent_id"),
    ("execution_protocol_work_items", "agent_id"),
    ("execution_protocol_partitions", "agent_id"),
    ("work_item_continuations", "agent_id"),
    ("context_episode_anchors", "agent_id"),
    ("wait_conditions", "agent_id"),
    ("queue_entries", "agent_id"),
    ("timers", "agent_id"),
    ("tasks", "owner_agent_id"),
    ("work_items", "agent_id"),
    ("transcript_entries", "agent_id"),
    ("tool_executions", "agent_id"),
    ("model_requests", "agent_id"),
    ("model_responses", "agent_id"),
    ("delivery_summaries", "agent_id"),
    ("artifact_metadata", "agent_id"),
    ("briefs", "agent_id"),
    ("messages", "agent_id"),
    ("turn_records", "agent_id"),
    ("audit_events", "agent_id"),
    // Callback messages/audits use agent_id; capabilities use target_agent_id.
    ("external_triggers", "target_agent_id"),
    ("agent_states", "agent_id"),
    ("agent_bootstraps", "agent_id"),
    ("agent_capability_policy_rules", "agent_id"),
    ("agent_capability_policy_records", "agent_id"),
    ("agent_message_policy_rules", "agent_id"),
    ("agent_message_policy_records", "agent_id"),
    ("agent_durability_records", "agent_id"),
    ("agent_lifecycle_attachment_records", "agent_id"),
    ("agent_relation_backfill_outcomes", "agent_id"),
    ("agents", "agent_id"),
];

// Resolve ownerless projections through source rows, never their payload.
// Export and purge must use exactly the same selectors.
const DEPENDENT_TABLES: &[(&str, &str)] = &[
    (
        "memory_documents_fts",
        "document_key IN (SELECT document_key FROM memory_documents WHERE agent_id = ?1)",
    ),
    (
        "memory_documents_fts_rows",
        "document_key IN (SELECT document_key FROM memory_documents WHERE agent_id = ?1)",
    ),
    (
        "timer_wakes",
        "timer_id IN (SELECT timer_id FROM timers WHERE agent_id = ?1)",
    ),
    (
        "subagent_cleanup_failure_notices",
        "brief_id IN (SELECT evidence_id FROM briefs WHERE agent_id = ?1)",
    ),
    (
        "destructive_operations",
        "owner_turn_id IN (SELECT turn_id FROM turn_records WHERE agent_id = ?1)",
    ),
    // Parent-owned reminders are not the child's private history.
    ("subagent_cleanup_outbox", "parent_agent_id = ?1"),
    ("subagent_cleanup_observations", "parent_agent_id = ?1"),
    ("workspace_occupancies", "holder_agent_id = ?1"),
    (
        "scheduler_protocol_command_conflict_attempts",
        "partition_kind = 'agent' AND partition_key = ?1",
    ),
];

#[derive(Debug, Deserialize, Serialize)]
struct CleanupProof {
    incarnation: u64,
    settled: bool,
    purged: bool,
}

/// The host must fence admission and unload execution before calling this.
pub(crate) fn settle_execution(
    db: &RuntimeDb,
    agent_id: &str,
    expected_incarnation: u64,
    now: DateTime<Utc>,
) -> Result<()> {
    db.transaction(|tx| {
        fenced_identity_tx(tx, agent_id, expected_incarnation)?;
        if proof_tx(tx, agent_id)?
            .is_some_and(|proof| proof.incarnation == expected_incarnation && proof.purged)
        {
            return assert_private_history_cleared_tx(tx, agent_id);
        }
        for mut wait in payloads::<WaitConditionRecord>(tx, "wait_conditions", agent_id)? {
            ensure!(wait.agent_id == agent_id, "wait ownership index mismatch");
            if matches!(
                wait.status,
                WaitConditionStatus::Active | WaitConditionStatus::Triggered
            ) {
                wait.status = WaitConditionStatus::Cancelled;
                wait.updated_at = now;
                wait.cancelled_at = Some(now);
                upsert_wait_condition_tx(tx, &wait)?;
            }
        }
        // Close recovery ownership, not externally dispatched side effects.
        // Deletion neither replays the command nor fabricates verification.
        tx.execute(
            "UPDATE destructive_operations SET phase = 'owner_closed', updated_at = ?2
             WHERE owner_turn_id IN (SELECT turn_id FROM turn_records WHERE agent_id = ?1)
               AND phase IN ('planned','scheduled','daemon_interrupted')",
            params![agent_id, now.to_rfc3339()],
        )?;
        retire_agent_execution_tx(tx, agent_id)?;
        for mut command in
            payloads::<AdmitActivationCommand>(tx, "scheduler_activations", agent_id)?
        {
            let activation = &mut command.activation;
            ensure!(
                activation.agent_id == agent_id,
                "activation ownership index mismatch"
            );
            if matches!(
                activation.state,
                ActivationLifecycleState::Admitted
                    | ActivationLifecycleState::Running
                    | ActivationLifecycleState::SettlementMissing
            ) {
                // Only the compatibility wire type survives the scheduler retirement.
                activation.state = ActivationLifecycleState::Cancelled;
                let activation_id = activation.id.clone();
                tx.execute(
                    "UPDATE scheduler_activations SET lifecycle_state = 'cancelled',
                     payload_json = ?3, updated_at = ?4 WHERE agent_id = ?1 AND activation_id = ?2",
                    params![
                        agent_id,
                        activation_id,
                        serde_json::to_string(&command)?,
                        now.to_rfc3339()
                    ],
                )?;
            }
        }

        for mut turn in payloads::<TurnRecord>(tx, "turn_records", agent_id)? {
            ensure!(turn.agent_id == agent_id, "turn ownership index mismatch");
            if turn.terminal.is_some() {
                continue;
            }
            let has_brief: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM briefs WHERE agent_id = ?1 AND turn_id = ?2)",
                params![agent_id, turn.turn_id],
                |row| row.get(0),
            )?;
            turn.terminal = Some(TurnTerminalSummary {
                kind: TurnTerminalKind::Interrupted,
                reason: Some(REASON.into()),
                no_brief_reason: (!has_brief).then_some(TurnNoBriefReason::Interrupted),
                completed_at: now,
                duration_ms: 0,
            });
            upsert_turn_record_tx(tx, &turn)?;
        }
        for mut queue in payloads::<QueueEntryRecord>(tx, "queue_entries", agent_id)? {
            ensure!(queue.agent_id == agent_id, "queue ownership index mismatch");
            if matches!(
                queue.status,
                QueueEntryStatus::Queued
                    | QueueEntryStatus::Interrupted
                    | QueueEntryStatus::Dequeued
            ) {
                queue.status = QueueEntryStatus::Aborted;
                queue.updated_at = now;
                upsert_queue_entry_tx(tx, &queue)?;
            }
        }
        let activations = {
            let mut statement = tx.prepare(
                "SELECT DISTINCT activation_id FROM task_result_settlements
                 WHERE agent_id = ?1 AND state = 'caller_admitted'",
            )?;
            let activations = statement
                .query_map([agent_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            activations
        };
        for activation in activations {
            settle_activation_tx(
                tx,
                agent_id,
                &activation,
                TaskResultSettlementDisposition::OwnerClosed,
                now,
            )?;
        }
        for mut state in payloads::<AgentState>(tx, "agent_states", agent_id)? {
            ensure!(state.id == agent_id, "runtime state ownership mismatch");
            if state.current_run_id.is_some()
                || state.current_turn_id.is_some()
                || state.current_execution_binding.is_some()
            {
                state.current_run_id = None;
                state.current_turn_id = None;
                state.current_turn_work_item_id = None;
                state.current_execution_binding = None;
                state.current_turn_operator_binding_id = None;
                state.current_turn_operator_reply_route_id = None;
                state.status = AgentStatus::Stopped;
                upsert_agent_state_tx(tx, &state)?;
            }
        }
        assert_execution_settled_tx(tx, agent_id)?;
        write_proof_tx(tx, agent_id, expected_incarnation, false, now)
    })
}

/// Erase only proven private rows, atomically. Live resources must already be cancelled.
pub(crate) fn purge_private_history(
    db: &RuntimeDb,
    agent_id: &str,
    expected_incarnation: u64,
    now: DateTime<Utc>,
) -> Result<()> {
    db.transaction(|tx| {
        fenced_identity_tx(tx, agent_id, expected_incarnation)?;
        let proof = proof_tx(tx, agent_id)?.context("deletion execution has not been settled")?;
        ensure!(
            proof.incarnation == expected_incarnation && proof.settled,
            "deletion execution settlement proof does not match incarnation"
        );
        assert_execution_settled_tx(tx, agent_id)?;
        assert_no_live_resources_tx(tx, agent_id)?;
        assert_safe_ownership_tx(tx, agent_id)?;
        assert_payload_ownership_tx(tx, agent_id)?;
        assert_search_schema_tx(tx)?;

        let operation_ids = {
            let mut statement = tx.prepare(
                "SELECT operation_id FROM destructive_operations
                 WHERE owner_turn_id IN (SELECT turn_id FROM turn_records WHERE agent_id = ?1)",
            )?;
            let ids = statement
                .query_map([agent_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids
        };
        for operation_id in operation_ids {
            super::destructive_operations::retain_closed_operation_fence_tx(
                tx,
                &operation_id,
                now,
            )?;
        }
        for (table, predicate) in private_history_selections_tx(tx)? {
            tx.execute(
                &format!("DELETE FROM {} WHERE {predicate}", quoted(table)),
                [agent_id],
            )
            .with_context(|| format!("clearing deletion-private table {table}"))?;
        }
        write_proof_tx(tx, agent_id, expected_incarnation, true, now)?;
        assert_private_history_cleared_tx(tx, agent_id)
    })
}

/// Logical archive of exactly the rows purge owns, without changing the DB.
/// Text (including payload_json) is verbatim; BLOBs use a tagged byte array.
pub(crate) fn export_private_history(
    db: &RuntimeDb,
    agent_id: &str,
    expected_incarnation: u64,
) -> Result<serde_json::Value> {
    // Serialize the identity fence and every SELECT with admission and purge.
    // This writer snapshot issues no writes, including no cleanup proof.
    db.transaction(|tx| {
        fenced_identity_tx(tx, agent_id, expected_incarnation)?;
        assert_safe_ownership_tx(tx, agent_id)?;
        assert_payload_ownership_tx(tx, agent_id)?;
        assert_search_schema_tx(tx)?;
        let mut tables = serde_json::Map::new();
        for (table, predicate) in private_history_selections_tx(tx)? {
            let mut statement = tx.prepare(&format!(
                "SELECT * FROM {} WHERE {predicate}",
                quoted(table)
            ))?;
            let names: Vec<String> = statement
                .column_names()
                .into_iter()
                .map(String::from)
                .collect();
            let mut rows = statement.query([agent_id])?;
            let mut exported = Vec::new();
            while let Some(row) = rows.next()? {
                let mut object = serde_json::Map::new();
                for (index, name) in names.iter().enumerate() {
                    use rusqlite::types::ValueRef;
                    let value = match row.get_ref(index)? {
                        ValueRef::Null => serde_json::Value::Null,
                        ValueRef::Integer(value) => value.into(),
                        ValueRef::Real(value) => serde_json::Number::from_f64(value)
                            .context("non-finite SQLite number cannot be archived")?
                            .into(),
                        ValueRef::Text(value) => std::str::from_utf8(value)?.into(),
                        ValueRef::Blob(value) => serde_json::json!({"$sqlite_blob": value}),
                    };
                    object.insert(name.clone(), value);
                }
                exported.push(serde_json::Value::Object(object));
            }
            tables.insert(table.to_owned(), exported.into());
        }
        Ok(serde_json::json!({
            "format": "holon.agent-private-history.v1",
            "agent_id": agent_id,
            "incarnation": expected_incarnation,
            "tables": tables,
        }))
    })
}

fn private_history_selections_tx(tx: &Transaction<'_>) -> Result<Vec<(&'static str, String)>> {
    let mut selections = Vec::new();
    for &(table, predicate) in DEPENDENT_TABLES {
        if table_exists(tx, table)? {
            selections.push((table, predicate.to_owned()));
        }
    }
    for &(table, owner) in PRIVATE_TABLES {
        if table_exists(tx, table)? {
            selections.push((table, format!("{} = ?1", quoted(owner))));
        }
    }
    Ok(selections)
}

/// Read-only recovery gate; missing proof or open execution is never repaired.
pub(crate) fn assert_execution_settled(db: &RuntimeDb, agent_id: &str) -> Result<()> {
    db.with_read(|connection| {
        let tx = connection.transaction()?;
        let identity = identity_tx(&tx, agent_id)?;
        fenced_identity_tx(&tx, agent_id, identity.incarnation)?;
        let proof = proof_tx(&tx, agent_id)?.context("deletion execution has not been settled")?;
        ensure!(
            proof.incarnation == identity.incarnation && proof.settled,
            "deletion execution settlement proof does not match incarnation"
        );
        assert_execution_settled_tx(&tx, agent_id)
    })
}

/// True when this incarnation's cleanup proof already settled execution and
/// purged private history. A cleanup repair in this state must not replay
/// settlement writers; re-purging residue is the only remaining work.
pub(crate) fn private_history_purge_proof_is_current(
    db: &RuntimeDb,
    agent_id: &str,
    expected_incarnation: u64,
) -> bool {
    db.with_read(|connection| {
        let tx = connection.transaction()?;
        Ok(proof_tx(&tx, agent_id)?.is_some_and(|proof| {
            proof.incarnation == expected_incarnation && proof.settled && proof.purged
        }))
    })
    .unwrap_or(false)
}

/// Read-only legacy completion audit. Errors mean cleanup is not proven.
pub(crate) fn assert_private_history_cleared(db: &RuntimeDb, agent_id: &str) -> Result<()> {
    db.with_read(|connection| {
        let tx = connection.transaction()?;
        assert_private_history_cleared_tx(&tx, agent_id)
    })
}

/// Reincarnation must call this inside its identity replacement writer transaction.
/// A completed legacy job alone is not proof that content was erased.
pub(crate) fn assert_private_history_cleared_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<()> {
    let identity = identity_tx(tx, agent_id)?;
    fenced_identity_tx(tx, agent_id, identity.incarnation)?;
    let proof = proof_tx(tx, agent_id)?.context("private history cleanup proof is missing")?;
    ensure!(
        proof.incarnation == identity.incarnation && proof.settled && proof.purged,
        "private history cleanup proof is stale or incomplete"
    );
    assert_safe_ownership_tx(tx, agent_id)?;
    assert_search_schema_tx(tx)?;
    for (table, owner) in PRIVATE_TABLES {
        if table_exists(tx, table)?
            && exists(
                tx,
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {owner} = ?1)"),
                agent_id,
            )?
        {
            bail!("private history remains in {table}");
        }
    }
    for (table, owner) in [
        ("subagent_cleanup_outbox", "parent_agent_id"),
        ("subagent_cleanup_observations", "parent_agent_id"),
        ("workspace_occupancies", "holder_agent_id"),
    ] {
        ensure!(
            !exists(
                tx,
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {owner} = ?1)"),
                agent_id
            )?,
            "private history remains in {table}"
        );
    }
    ensure!(
        !exists(
            tx,
            "SELECT EXISTS(SELECT 1 FROM scheduler_protocol_command_conflict_attempts
                     WHERE partition_kind = 'agent' AND partition_key = ?1)",
            agent_id
        )?,
        "private scheduler conflict history remains"
    );
    Ok(())
}

fn identity_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<AgentIdentityRecord> {
    let payload: String = tx.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
        [agent_id],
        |row| row.get(0),
    )?;
    let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
    ensure!(
        identity.agent_id == agent_id,
        "canonical identity ownership mismatch"
    );
    Ok(identity)
}

fn fenced_identity_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    expected_incarnation: u64,
) -> Result<AgentIdentityRecord> {
    let identity = identity_tx(tx, agent_id)?;
    ensure!(
        identity.incarnation == expected_incarnation
            && matches!(
                identity.status,
                AgentRegistryStatus::Deleting | AgentRegistryStatus::Deleted
            ),
        "private cleanup requires the expected deleting/deleted incarnation"
    );
    ensure!(
        exists(
            tx,
            "SELECT EXISTS(SELECT 1 FROM agent_identities
                    WHERE agent_id = ?1 AND status IN ('deleting','deleted'))",
            agent_id
        )?,
        "canonical identity status index mismatch"
    );
    Ok(identity)
}

fn assert_execution_settled_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
    for (table, predicate) in [
        ("turn_records", "terminal_kind IS NULL"),
        (
            "queue_entries",
            "status IN ('queued','dequeued','interrupted')",
        ),
        ("execution_protocol_attempts", "lifecycle_state = 'open'"),
        (
            "execution_protocol_work_items",
            "lifecycle_state = 'in_flight'",
        ),
        ("task_result_settlements", "state = 'caller_admitted'"),
        ("wait_conditions", "status IN ('active','triggered')"),
        (
            "scheduler_activations",
            "lifecycle_state IN ('admitted','running','settlement_missing')",
        ),
    ] {
        ensure!(
            !exists(
                tx,
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {table} WHERE agent_id = ?1 AND {predicate})"
                ),
                agent_id
            )?,
            "unsettled deletion execution in {table}"
        );
    }
    ensure!(
        !exists(
            tx,
            "SELECT EXISTS(SELECT 1 FROM destructive_operations d
             JOIN turn_records t ON t.turn_id = d.owner_turn_id
             WHERE t.agent_id = ?1 AND d.phase NOT IN ('verified','owner_closed'))",
            agent_id
        )?,
        "unsettled deletion destructive operations"
    );
    Ok(())
}

fn assert_no_live_resources_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
    for sql in [
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE owner_agent_id = ?1 AND status IN ('queued','running','cancelling'))",
        "SELECT EXISTS(SELECT 1 FROM work_items WHERE agent_id = ?1 AND state != 'completed')",
        "SELECT EXISTS(SELECT 1 FROM timers WHERE agent_id = ?1 AND status = 'active')",
        "SELECT EXISTS(SELECT 1 FROM wait_conditions WHERE agent_id = ?1 AND status IN ('active','triggered'))",
        "SELECT EXISTS(SELECT 1 FROM workspace_occupancies WHERE holder_agent_id = ?1 AND released_at IS NULL)",
        "SELECT EXISTS(SELECT 1 FROM destructive_operations d JOIN turn_records t ON t.turn_id = d.owner_turn_id
         WHERE t.agent_id = ?1 AND d.phase NOT IN ('verified','owner_closed'))",
        "SELECT EXISTS(SELECT 1 FROM agent_message_deliveries WHERE target_agent_id = ?1 AND state IN ('queued','dispatched'))",
    ] {
        ensure!(!exists(tx, sql, agent_id)?, "deletion still has live resources");
    }
    Ok(())
}

// Shared registries and monotonic counters contain no conversation content.
// Their references remain valid because the identity tombstone is retained.
const RETAINED_TABLES: &[&str] = &[
    "agent_identities",
    "agent_identity_reservations",
    "agent_deletion_jobs",
    "agent_lineages",
    "agent_supervisions",
    "workspace_entries",
];

fn columns(tx: &Transaction<'_>, table: &str) -> Result<Vec<String>> {
    // Reader authorization forbids PRAGMAs, including table-valued variants.
    let statement = tx.prepare(&format!("SELECT * FROM {} LIMIT 0", quoted(table)))?;
    Ok(statement
        .column_names()
        .into_iter()
        .map(String::from)
        .collect())
}

fn tables(tx: &Transaction<'_>) -> Result<Vec<String>> {
    let mut statement =
        tx.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let result = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(result)
}

fn quoted(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn assert_safe_ownership_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
    // These records belong jointly to another principal, not just to this agent.
    for sql in [
        "SELECT EXISTS(SELECT 1 FROM content_reports WHERE agent_id = ?1)",
        "SELECT EXISTS(SELECT 1 FROM work_item_delegations WHERE parent_agent_id = ?1 OR child_agent_id = ?1)",
        "SELECT EXISTS(SELECT 1 FROM agent_supervisions s
         JOIN agent_identities i ON i.agent_id = s.child_agent_id
         WHERE s.supervisor_agent_id = ?1 AND s.state != 'closed'
           AND i.status != 'deleted')",
    ] {
        ensure!(!exists(tx, sql, agent_id)?, "shared history requires explicit ownership resolution");
    }
    for table in tables(tx)? {
        if PRIVATE_TABLES.iter().any(|(name, _)| *name == table)
            || RETAINED_TABLES.contains(&table.as_str())
            || matches!(
                table.as_str(),
                "subagent_cleanup_observations"
                    | "subagent_cleanup_outbox"
                    | "workspace_occupancies"
                    | "content_reports"
                    | "work_item_delegations"
            )
        {
            continue;
        }
        for owner in columns(tx, &table)?.iter().filter(|column| {
            matches!(
                column.as_str(),
                "agent_id"
                    | "owner_agent_id"
                    | "target_agent_id"
                    | "holder_agent_id"
                    | "parent_agent_id"
                    | "child_agent_id"
                    | "supervisor_agent_id"
            )
        }) {
            ensure!(
                !exists(
                    tx,
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM {} WHERE {} = ?1)",
                        quoted(&table),
                        quoted(owner)
                    ),
                    agent_id
                )?,
                "unclassified deletion ownership in {table}"
            );
        }
    }
    Ok(())
}

// Historical IDs in another agent's payload do not own or pin these rows.
// Live invocation consumers resolve the durable identity fence before reading
// private evidence; their cancellation is settled by the normal task protocol.

fn assert_payload_ownership_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
    for (table, owner) in PRIVATE_TABLES {
        if !table_exists(tx, table)? || !columns(tx, table)?.iter().any(|c| c == "payload_json") {
            continue;
        }
        let key = match (*table, *owner) {
            ("agent_states", _) => "id",
            (_, "owner_agent_id") => "agent_id",
            (_, owner) => owner,
        };
        ensure!(
            !exists(
                tx,
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {table} WHERE {owner} = ?1
                 AND json_type(payload_json,'$.{key}') IS NOT NULL
                 AND (json_type(payload_json,'$.{key}') != 'text'
                      OR json_extract(payload_json,'$.{key}') != ?1))"
                ),
                agent_id
            )?,
            "private payload ownership index mismatch in {table}"
        );
    }
    Ok(())
}

fn assert_search_schema_tx(tx: &Transaction<'_>) -> Result<()> {
    for table in ["memory_documents_fts", "memory_documents_fts_rows"] {
        if !table_exists(tx, table)? {
            continue;
        }
        ensure!(
            table_exists(tx, "memory_documents")?,
            "memory projection has no ownership source"
        );
    }
    Ok(())
}

fn table_exists(tx: &Transaction<'_>, table: &str) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn exists(tx: &Transaction<'_>, sql: &str, agent_id: &str) -> Result<bool> {
    let mut statement = tx.prepare(sql)?;
    // A few ownership integrity checks are deliberately global, with no parameters.
    Ok(if statement.parameter_count() == 0 {
        statement.query_row([], |row| row.get(0))?
    } else {
        statement.query_row([agent_id], |row| row.get(0))?
    })
}

fn payloads<T: DeserializeOwned>(
    tx: &Transaction<'_>,
    table: &str,
    agent_id: &str,
) -> Result<Vec<T>> {
    let mut statement = tx.prepare(&format!(
        "SELECT payload_json FROM {table} WHERE agent_id = ?1"
    ))?;
    let payloads = statement
        .query_map([agent_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    payloads
        .iter()
        .map(|payload| serde_json::from_str(payload).map_err(Into::into))
        .collect()
}

fn proof_key(agent_id: &str) -> String {
    format!("agent_deletion_cleanup:{agent_id}")
}

fn proof_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<CleanupProof>> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [proof_key(agent_id)],
            |row| row.get(0),
        )
        .optional()?;
    payload
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(Into::into)
}

fn write_proof_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    incarnation: u64,
    purged: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO runtime_metadata(key,value,created_at,updated_at) VALUES (?1,?2,?3,?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![
            proof_key(agent_id),
            serde_json::to_string(&CleanupProof {
                incarnation,
                settled: true,
                purged
            })?,
            now.to_rfc3339()
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        domain::execution_protocol::{
            AdmittedFences, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding,
            ExecutionOrigin, ExecutionPriority, ExecutionProtocolState, ExecutionProvenance,
            ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
        },
        runtime_db::{
            evidence::upsert_agent_identity_tx,
            transitions::{
                execution_protocol_repository::load_state_unchecked_tx, persist_state_tx,
            },
        },
        types::{
            AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility, AuditEvent,
            AuthorityClass, BriefKind, BriefRecord, CallbackDeliveryMode, ExternalTriggerStatus,
            MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, TaskKind,
            TaskRecord, TaskStatus,
        },
    };

    fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        for id in ["a", "b"] {
            let identity = AgentIdentityRecord::new(
                id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            db.transaction(|tx| upsert_agent_identity_tx(tx, &identity))?;
        }
        Ok((dir, db))
    }

    fn fence(db: &RuntimeDb) -> Result<()> {
        let revision = db.transaction(|tx| Ok(identity_tx(tx, "a")?.revision))?;
        db.agent_deletions().begin("a", revision, "test", false)?;
        Ok(())
    }

    fn message(db: &RuntimeDb, agent: &str, id: &str) -> Result<()> {
        let mut message = MessageEnvelope::new(
            agent,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: format!("private content of {agent}"),
            },
        );
        message.id = id.into();
        db.messages().upsert_with_index_changes(
            &message,
            &[super::super::index_outbox::RuntimeIndexChange::for_message(
                &message,
            )],
        )?;
        Ok(())
    }

    fn callback_message(db: &RuntimeDb, agent: &str, trigger_id: &str) -> Result<()> {
        let mut message = MessageEnvelope::new(
            agent,
            MessageKind::CallbackEvent,
            MessageOrigin::Callback {
                descriptor_id: trigger_id.into(),
                source: None,
            },
            AuthorityClass::IntegrationSignal,
            Priority::Normal,
            MessageBody::Text {
                text: format!("private content of {agent}"),
            },
        );
        message.id = format!("callback-message-{agent}");
        db.messages().upsert_with_index_changes(
            &message,
            &[super::super::index_outbox::RuntimeIndexChange::for_message(
                &message,
            )],
        )?;
        Ok(())
    }

    fn attempt(db: &RuntimeDb) -> Result<()> {
        let attempt = ExecutionAttempt {
            input_projection: None,
            attempt_id: "attempt-a".into(),
            agent_id: "a".into(),
            source_message_id: Some("message-a".into()),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::QueueMessage {
                    message_id: "message-a".into(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::Conversation {
                interaction_id: "interaction-a".into(),
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
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Open,
            run_id: Some("run-a".into()),
            turn_id: Some("turn-open".into()),
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: Utc::now().to_rfc3339(),
            terminal_at: None,
        };
        let state = ExecutionProtocolState {
            agent_id: "a".into(),
            attempts: BTreeMap::from([(attempt.attempt_id.clone(), attempt)]),
            work_items: BTreeMap::new(),
            outcomes: BTreeMap::new(),
        };
        db.transaction(|tx| persist_state_tx(tx, &state))
    }

    fn task(
        db: &RuntimeDb,
        agent: &str,
        id: &str,
        detail: Option<serde_json::Value>,
    ) -> Result<()> {
        let now = Utc::now();
        db.tasks().upsert(&TaskRecord {
            id: id.into(),
            agent_id: agent.into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Completed,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("private result".into()),
            detail,
            recovery: None,
        })?;
        Ok(())
    }

    fn memory(db: &RuntimeDb) -> Result<()> {
        db.transaction(|tx| {
            tx.execute_batch(
                "CREATE TABLE memory_documents (
                    document_key TEXT PRIMARY KEY, agent_id TEXT NOT NULL, body TEXT NOT NULL);
                 CREATE VIRTUAL TABLE memory_documents_fts USING fts5(document_key UNINDEXED,body);
                 CREATE TABLE memory_documents_fts_rows(document_key TEXT PRIMARY KEY,fts_rowid INTEGER);
                 CREATE TABLE memory_index_pending_sources(document_key TEXT PRIMARY KEY,agent_id TEXT,source_id TEXT);
                 CREATE TABLE memory_index_source_state(document_key TEXT PRIMARY KEY,agent_id TEXT,source_id TEXT);
                 CREATE TABLE memory_index_cursors(agent_id TEXT PRIMARY KEY,last_change_seq INTEGER);
                 CREATE TABLE memory_index_rebuild_jobs(agent_id TEXT PRIMARY KEY,generation TEXT);",
            )?;
            for agent in ["a", "b"] {
                tx.execute("INSERT INTO memory_documents VALUES (?1,?1,'secret body')", [agent])?;
                tx.execute("INSERT INTO memory_documents_fts(document_key,body) VALUES (?1,'secret body')", [agent])?;
                tx.execute("INSERT INTO memory_documents_fts_rows VALUES (?1,1)", [agent])?;
                for table in ["memory_index_pending_sources", "memory_index_source_state"] {
                    tx.execute(&format!("INSERT INTO {table} VALUES (?1,?1,?2)"),
                               params![agent,format!("message-{agent}")])?;
                }
                tx.execute("INSERT INTO memory_index_cursors VALUES (?1,9)", [agent])?;
                tx.execute("INSERT INTO memory_index_rebuild_jobs VALUES (?1,'rebuild')", [agent])?;
            }
            Ok(())
        })
    }

    fn snapshot(db: &RuntimeDb) -> Result<Vec<String>> {
        db.transaction(|tx| {
            let mut result = Vec::new();
            for table in [
                "turn_records",
                "queue_entries",
                "execution_protocol_attempts",
                "execution_protocol_outcomes",
                "conversation_turn_revisions",
                "conversation_source_revisions",
                "external_triggers",
                "messages",
                "wait_conditions",
                "destructive_operations",
                "audit_events",
                "runtime_index_outbox",
            ] {
                let value = match table {
                    "conversation_turn_revisions" => "json_object('turn',turn_id,'summary',summary_revision,'detail',detail_revision,'settled',result_settled)",
                    "conversation_source_revisions" => "json_object('source',source_id,'revision',revision)",
                    "audit_events" => "json(data_json)",
                    "runtime_index_outbox" => "json_object('seq',change_seq,'agent',agent_id,'source',source_ref)",
                    "destructive_operations" => "json_object('operation',operation_id,'turn',owner_turn_id,'phase',phase,'updated',updated_at)",
                    _ => "json(payload_json)",
                };
                let mut statement = tx.prepare(&format!("SELECT json_group_array({value}) FROM {table}"))?;
                result.push(statement.query_row([], |row| row.get(0))?);
            }
            Ok(result)
        })
    }

    #[test]
    fn export_is_read_only_and_contains_only_the_purge_owned_rows() -> Result<()> {
        let (_dir, db) = fixture()?;
        for agent in ["a", "b"] {
            db.external_triggers().ensure_default_for_agent(
                agent,
                CallbackDeliveryMode::WakeHint,
                Utc::now(),
                format!("ingress-{agent}-revoked"),
                format!("old-token-{agent}"),
            )?;
            let (active, revoked) = db.external_triggers().reset_default_for_agent(
                agent,
                CallbackDeliveryMode::WakeHint,
                Utc::now(),
                format!("ingress-{agent}-default"),
                format!("current-token-{agent}"),
            )?;
            assert_eq!(active.status, ExternalTriggerStatus::Active);
            assert_eq!(revoked.unwrap().status, ExternalTriggerStatus::Revoked);
            callback_message(&db, agent, &active.external_trigger_id)?;
            task(&db, agent, &format!("task-{agent}"), None)?;
            db.turn_records()
                .upsert(&TurnRecord::new(agent, format!("turn-{agent}"), 1))?;
            let mut brief = BriefRecord::new(agent, BriefKind::Result, agent, None, None);
            brief.id = format!("brief-{agent}");
            db.evidence().append_brief(&brief)?;
            db.transaction(|tx| {
                super::super::evidence::insert_audit_event_tx(
                    tx,
                    Some(agent),
                    &AuditEvent::legacy(
                        "callback_delivered",
                        serde_json::json!({
                            "agent_id": agent,
                            "external_trigger_id": active.external_trigger_id,
                        }),
                    ),
                )?;
                tx.execute(
                    "INSERT INTO timers(timer_id,agent_id,status,created_at,duration_ms,
                     updated_at,payload_json) VALUES (?1,?1,'completed','now',1,'now','{}')",
                    [agent],
                )?;
                tx.execute(
                    "INSERT INTO timer_wakes(timer_id,message_id,fire_count,status,created_at,updated_at)
                     VALUES (?1,?1,1,'incorporated','now','now')",
                    [agent],
                )?;
                tx.execute(
                    "INSERT INTO subagent_cleanup_failure_notices VALUES (?1,?2)",
                    params![agent, brief.id],
                )?;
                tx.execute(
                    "INSERT INTO destructive_operations(operation_id,owner_turn_id,phase,
                     verification_target,recovery_policy,created_at,updated_at)
                     VALUES (?1,?2,'verified',?1,'verify','now','now')",
                    params![agent, format!("turn-{agent}")],
                )?;
                tx.execute(
                    "INSERT INTO subagent_cleanup_observations VALUES (?1,?1,'evidence',
                     'now','now',NULL,0,NULL)",
                    [agent],
                )?;
                tx.execute(
                    "INSERT INTO subagent_cleanup_outbox VALUES (?1,?1,'now','settled','{}','[]')",
                    [agent],
                )?;
                tx.execute(
                    "INSERT INTO workspace_occupancies VALUES (?1,'root','workspace',?1,
                     'read','now','now','{}')",
                    [agent],
                )?;
                tx.execute(
                    "INSERT INTO scheduler_protocol_command_conflict_attempts(
                     partition_kind,partition_key,command_kind,command_identity,
                     canonical_schema_version,existing_payload_hash,incoming_payload_hash,
                     conflict_kind,conflict_code,created_at)
                     VALUES ('agent',?1,'test',?1,1,'old','new','test','test','now')",
                    [agent],
                )?;
                Ok(())
            })?;
        }
        memory(&db)?;
        let retained_triggers: Vec<_> = db
            .external_triggers()
            .latest_all()?
            .into_iter()
            .filter(|record| record.target_agent_id == "b")
            .collect();
        fence(&db)?;
        let before = snapshot(&db)?;
        let changes: i64 =
            db.transaction(|tx| Ok(tx.query_row("SELECT total_changes()", [], |row| row.get(0))?))?;
        let archive = export_private_history(&db, "a", 1)?;
        assert_eq!(archive["format"], "holon.agent-private-history.v1");
        assert_eq!(archive["agent_id"], "a");
        assert_eq!(archive["incarnation"], 1);
        let exported = archive["tables"].as_object().unwrap();
        for table in [
            "messages",
            "tasks",
            "memory_documents",
            "memory_documents_fts",
            "memory_documents_fts_rows",
            "timers",
            "audit_events",
        ] {
            assert_eq!(exported[table].as_array().unwrap().len(), 1, "{table}");
        }
        for &(table, _) in DEPENDENT_TABLES {
            assert_eq!(exported[table].as_array().unwrap().len(), 1, "{table}");
        }
        for &table in RETAINED_TABLES {
            assert!(!exported.contains_key(table), "{table}");
        }
        assert_eq!(exported["messages"][0]["agent_id"], "a");
        let triggers = exported["external_triggers"].as_array().unwrap();
        assert_eq!(triggers.len(), 2);
        for status in ["active", "revoked"] {
            assert!(triggers
                .iter()
                .any(|row| row["target_agent_id"] == "a" && row["status"] == status));
        }
        assert_eq!(exported["memory_documents_fts"][0]["document_key"], "a");
        assert!(!serde_json::to_string(&archive)?.contains("private content of b"));
        assert!(!serde_json::to_string(&archive)?.contains("current-token-b"));
        assert!(!serde_json::to_string(&archive)?.contains("ingress-b-"));
        assert_eq!(archive, export_private_history(&db, "a", 1)?);
        assert_eq!(before, snapshot(&db)?);
        db.transaction(|tx| {
            let after: i64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
            assert_eq!(changes, after);
            assert!(proof_tx(tx, "a")?.is_none());
            let original: String = tx.query_row(
                "SELECT payload_json FROM messages WHERE agent_id = 'a'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(exported["messages"][0]["payload_json"], original);
            Ok(())
        })?;
        settle_execution(&db, "a", 1, Utc::now())?;
        purge_private_history(&db, "a", 1, Utc::now())?;
        let after = export_private_history(&db, "a", 1)?;
        for (table, rows) in after["tables"].as_object().unwrap() {
            assert!(rows.as_array().unwrap().is_empty(), "{table}");
        }
        assert_eq!(db.external_triggers().latest_all()?, retained_triggers);
        db.transaction(|tx| {
            for (table, value) in [("messages", "payload_json"), ("audit_events", "data_json")] {
                let payload: String = tx.query_row(
                    &format!("SELECT {value} FROM {table} WHERE agent_id = 'b'"),
                    [],
                    |row| row.get(0),
                )?;
                assert!(payload.contains("ingress-b-default"), "{table}");
            }
            for &(table, predicate) in DEPENDENT_TABLES {
                assert!(
                    exists(
                        tx,
                        &format!(
                            "SELECT EXISTS(SELECT 1 FROM {} WHERE {predicate})",
                            quoted(table)
                        ),
                        "b"
                    )?,
                    "{table}"
                );
            }
            Ok(())
        })
    }

    #[test]
    fn external_trigger_ownership_and_unknown_schema_still_fail_closed() -> Result<()> {
        for case in ["payload", "unknown"] {
            let (_dir, db) = fixture()?;
            db.external_triggers().ensure_default_for_agent(
                "a",
                CallbackDeliveryMode::WakeHint,
                Utc::now(),
                "ingress-a".into(),
                "token-a".into(),
            )?;
            db.transaction(|tx| {
                if case == "payload" {
                    tx.execute("UPDATE external_triggers SET payload_json = json_set(payload_json,'$.target_agent_id','b')", [])?;
                } else {
                    tx.execute_batch("CREATE TABLE unclassified_trigger_history(target_agent_id TEXT,external_trigger_id TEXT);
                        INSERT INTO unclassified_trigger_history VALUES ('a','ingress-a');")?;
                }
                Ok(())
            })?;
            fence(&db)?;
            settle_execution(&db, "a", 1, Utc::now())?;
            let before = snapshot(&db)?;
            assert!(export_private_history(&db, "a", 1).is_err());
            assert!(purge_private_history(&db, "a", 1, Utc::now()).is_err());
            assert_eq!(before, snapshot(&db)?);
        }
        Ok(())
    }

    #[test]
    fn settlement_recovery_gate_is_read_only_and_requires_fenced_current_proof() -> Result<()> {
        for case in [
            "active",
            "missing",
            "unsettled",
            "stale",
            "open",
            "settled",
            "purged",
        ] {
            let (_dir, db) = fixture()?;
            db.turn_records()
                .upsert(&TurnRecord::new("a", "turn-a", 1))?;
            if case != "active" {
                fence(&db)?;
                if case != "missing" {
                    settle_execution(&db, "a", 1, Utc::now())?;
                }
            }
            db.transaction(|tx| {
                match case {
                    "unsettled" | "stale" => {
                        let (key, value) = if case == "unsettled" {
                            ("$.settled", "false")
                        } else {
                            ("$.incarnation", "2")
                        };
                        tx.execute(
                            "UPDATE runtime_metadata SET value = json_set(value,?2,json(?3))
                             WHERE key = ?1",
                            params![proof_key("a"), key, value],
                        )?;
                    }
                    "open" => {
                        tx.execute(
                            "UPDATE turn_records SET terminal_kind = NULL WHERE agent_id = 'a'",
                            [],
                        )?;
                    }
                    _ => {}
                }
                Ok(())
            })?;
            if case == "purged" {
                purge_private_history(&db, "a", 1, Utc::now())?;
            }
            let checkpoint = || -> Result<_> {
                let history = snapshot(&db)?;
                db.transaction(|tx| {
                    let proof = proof_tx(tx, "a")?.map(serde_json::to_value).transpose()?;
                    let changes: i64 =
                        tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
                    Ok((history.clone(), proof, changes))
                })
            };
            let before = checkpoint()?;
            let result = assert_execution_settled(&db, "a");
            assert_eq!(
                result.is_ok(),
                matches!(case, "settled" | "purged"),
                "{case}"
            );
            if case == "open" {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("unsettled deletion execution in turn_records"));
            }
            assert_eq!(before, checkpoint()?, "{case}");
        }
        Ok(())
    }

    #[test]
    fn export_fences_identity_and_refuses_unknown_or_disagreeing_ownership() -> Result<()> {
        let (_dir, db) = fixture()?;
        message(&db, "a", "message-a")?;
        assert!(export_private_history(&db, "a", 1).is_err());
        fence(&db)?;
        assert!(export_private_history(&db, "a", 2).is_err());
        assert!(export_private_history(&db, "missing", 1).is_err());
        db.transaction(|tx| {
            tx.execute(
                "UPDATE messages SET payload_json = json_set(payload_json,'$.agent_id','b')
                 WHERE agent_id = 'a'",
                [],
            )?;
            Ok(())
        })?;
        assert!(export_private_history(&db, "a", 1)
            .unwrap_err()
            .to_string()
            .contains("ownership index mismatch"));
        db.transaction(|tx| {
            tx.execute(
                "UPDATE messages SET payload_json = json_set(payload_json,'$.agent_id','a')
                 WHERE agent_id = 'a'",
                [],
            )?;
            tx.execute_batch(
                "CREATE TABLE unclassified_archive_history(agent_id TEXT,payload TEXT);
                 INSERT INTO unclassified_archive_history VALUES ('a','private');",
            )?;
            Ok(())
        })?;
        assert!(export_private_history(&db, "a", 1)
            .unwrap_err()
            .to_string()
            .contains("unclassified deletion ownership"));
        Ok(())
    }

    #[test]
    fn export_preserves_sqlite_cell_types_and_rejects_non_finite_numbers() -> Result<()> {
        let (_dir, db) = fixture()?;
        memory(&db)?;
        fence(&db)?;
        db.transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE memory_documents ADD COLUMN blob_value BLOB;
                 ALTER TABLE memory_documents ADD COLUMN integer_value INTEGER;
                 ALTER TABLE memory_documents ADD COLUMN real_value REAL;
                 ALTER TABLE memory_documents ADD COLUMN null_value TEXT;
                 UPDATE memory_documents SET blob_value = x'00ff',
                    integer_value = 9223372036854775807,real_value = 1.25 WHERE agent_id = 'a';",
            )?;
            Ok(())
        })?;
        let archive = export_private_history(&db, "a", 1)?;
        let row = &archive["tables"]["memory_documents"][0];
        assert_eq!(
            row["blob_value"],
            serde_json::json!({"$sqlite_blob":[0,255]})
        );
        assert_eq!(row["integer_value"], serde_json::json!(i64::MAX));
        assert_eq!(row["real_value"], serde_json::json!(1.25));
        assert!(row["null_value"].is_null());
        db.transaction(|tx| {
            tx.execute(
                "UPDATE memory_documents SET real_value = ?1 WHERE agent_id = 'a'",
                [f64::INFINITY],
            )?;
            Ok(())
        })?;
        assert!(export_private_history(&db, "a", 1)
            .unwrap_err()
            .to_string()
            .contains("non-finite SQLite number"));
        Ok(())
    }

    #[test]
    fn settlement_is_terminal_safe_typed_and_idempotent() -> Result<()> {
        let (_dir, db) = fixture()?;
        message(&db, "a", "message-a")?;
        attempt(&db)?;
        let mut completed = TurnRecord::new("a", "turn-completed", 1);
        completed.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: Utc::now(),
            duration_ms: 15,
        });
        db.turn_records().upsert(&completed)?;
        db.turn_records()
            .upsert(&TurnRecord::new("a", "turn-open", 2))?;
        db.turn_records()
            .upsert(&TurnRecord::new("a", "turn-brief", 3))?;
        let mut brief = BriefRecord::new("a", BriefKind::Result, "existing result", None, None);
        brief.turn_id = Some("turn-brief".into());
        brief.turn_index = Some(3);
        db.evidence().append_brief(&brief)?;
        for (n, status) in [
            QueueEntryStatus::Queued,
            QueueEntryStatus::Dequeued,
            QueueEntryStatus::Interrupted,
            QueueEntryStatus::Processed,
            QueueEntryStatus::Aborted,
            QueueEntryStatus::Quarantined,
        ]
        .into_iter()
        .enumerate()
        {
            let now = Utc::now();
            db.queue_entries().upsert(&QueueEntryRecord {
                message_id: format!("queue-{n}"),
                agent_id: "a".into(),
                priority: Priority::Normal,
                status,
                created_at: now,
                updated_at: now,
            })?;
        }
        fence(&db)?;
        let mut job = db.agent_deletions().latest_for_agent("a")?.unwrap();
        job.mode = crate::types::AgentDeletionMode::ParentCleanup;
        db.agent_deletions().update(&job)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            let turns = payloads::<TurnRecord>(tx, "turn_records", "a")?;
            assert_eq!(
                turns
                    .iter()
                    .find(|t| t.turn_id == "turn-completed")
                    .unwrap(),
                &completed
            );
            let terminal = turns
                .iter()
                .find(|t| t.turn_id == "turn-open")
                .unwrap()
                .terminal
                .as_ref()
                .unwrap();
            assert_eq!(terminal.kind, TurnTerminalKind::Interrupted);
            assert_eq!(
                terminal.no_brief_reason,
                Some(TurnNoBriefReason::Interrupted)
            );
            assert!(turns
                .iter()
                .find(|t| t.turn_id == "turn-brief")
                .unwrap()
                .terminal
                .as_ref()
                .unwrap()
                .no_brief_reason
                .is_none());
            let queues = payloads::<QueueEntryRecord>(tx, "queue_entries", "a")?;
            assert_eq!(
                queues
                    .iter()
                    .filter(|q| q.status == QueueEntryStatus::Aborted)
                    .count(),
                4
            );
            assert_eq!(
                load_state_unchecked_tx(tx, "a")?.attempts["attempt-a"].state,
                ExecutionAttemptState::Interrupted
            );
            assert!(exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM conversation_turn_revisions
                      WHERE agent_id = ?1 AND turn_id = 'turn-open' AND result_settled = 1)",
                "a"
            )?);
            Ok(())
        })?;
        let before = snapshot(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        assert_eq!(snapshot(&db)?, before);
        Ok(())
    }

    #[test]
    fn settlement_failure_rolls_back_wait_operation_attempt_turn_and_proof() -> Result<()> {
        let (_dir, db) = fixture()?;
        attempt(&db)?;
        db.turn_records()
            .upsert(&TurnRecord::new("a", "turn-open", 1))?;
        let now = Utc::now();
        db.wait_conditions().upsert(&WaitConditionRecord {
            id: "rollback-wait".into(),
            agent_id: "a".into(),
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: crate::types::WaitConditionKind::External,
            source: None,
            subject_ref: None,
            waiting_for: "external".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some("turn-open".into()),
            trigger_message_id: None,
            triggered_at: None,
        })?;
        db.destructive_operations()
            .plan_or_get(&super::super::DestructiveOperationRecord {
                operation_id: "rollback-operation".into(),
                owner_turn_id: "turn-open".into(),
                owner_work_item_id: None,
                command_digest: "fixture".into(),
                phase: super::super::DestructiveOperationPhase::Scheduled,
                verification_target: "false".into(),
                recovery_policy: "verify_only".into(),
                created_at: now,
                updated_at: now,
            })?;
        fence(&db)?;
        let before = snapshot(&db)?;
        db.transaction(|tx| {
            tx.execute_batch(
                "CREATE TRIGGER fail_settlement BEFORE UPDATE ON turn_records
                              BEGIN SELECT RAISE(ABORT,'injected settlement failure'); END;",
            )?;
            Ok(())
        })?;
        let error = settle_execution(&db, "a", 1, Utc::now()).unwrap_err();
        assert!(format!("{error:#}").contains("injected settlement failure"));
        assert_eq!(snapshot(&db)?, before);
        db.transaction(|tx| {
            assert!(proof_tx(tx, "a")?.is_none());
            Ok(())
        })
    }

    #[test]
    fn purge_clears_private_sources_and_shared_projections_by_owner() -> Result<()> {
        let (_dir, db) = fixture()?;
        for agent in ["a", "b"] {
            message(&db, agent, &format!("message-{agent}"))?;
            task(&db, agent, &format!("task-{agent}"), None)?;
        }
        memory(&db)?;
        fence(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        purge_private_history(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            assert_private_history_cleared_tx(tx, "a")?;
            assert_eq!(identity_tx(tx, "a")?.incarnation, 1);
            for table in ["memory_documents_fts", "memory_documents_fts_rows"] {
                let count: i64 =
                    tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(count, 1);
                assert!(exists(
                    tx,
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE document_key = ?1)"),
                    "b"
                )?);
            }
            assert!(exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM runtime_index_outbox WHERE agent_id = ?1)",
                "b"
            )?);
            assert!(exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM messages WHERE agent_id = ?1)",
                "b"
            )?);
            assert!(exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE owner_agent_id = ?1)",
                "b"
            )?);
            let proof: String = tx.query_row(
                "SELECT value FROM runtime_metadata WHERE key = ?1",
                [proof_key("a")],
                |row| row.get(0),
            )?;
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&proof)?,
                serde_json::json!({"incarnation":1,"settled":true,"purged":true})
            );
            Ok(())
        })?;
        purge_private_history(&db, "a", 1, Utc::now())?;
        settle_execution(&db, "a", 1, Utc::now())
    }

    #[test]
    fn purge_fault_rolls_back_sources_fts_and_proof() -> Result<()> {
        let (_dir, db) = fixture()?;
        message(&db, "a", "message-a")?;
        memory(&db)?;
        db.turn_records()
            .upsert(&TurnRecord::new("a", "operation-turn", 1))?;
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO destructive_operations(operation_id,owner_turn_id,phase,
                 verification_target,recovery_policy,created_at,updated_at)
                 VALUES ('rollback-fence','operation-turn','scheduled','false','verify_only','now','now')",
                [],
            )?;
            Ok(())
        })?;
        fence(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            tx.execute_batch(
                "CREATE TRIGGER fail_purge BEFORE DELETE ON messages
                              BEGIN SELECT RAISE(ABORT,'injected purge failure'); END;",
            )?;
            Ok(())
        })?;
        let error = purge_private_history(&db, "a", 1, Utc::now()).unwrap_err();
        assert!(format!("{error:#}").contains("injected purge failure"));
        db.transaction(|tx| {
            assert!(!proof_tx(tx, "a")?.unwrap().purged);
            let fences: i64 = tx.query_row(
                "SELECT COUNT(*) FROM runtime_metadata
                 WHERE key LIKE 'destructive_operation_closed:%'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                fences, 0,
                "a failed purge must roll back dispatch tombstones"
            );
            let phase: String = tx.query_row(
                "SELECT phase FROM destructive_operations WHERE operation_id = 'rollback-fence'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                phase, "owner_closed",
                "the private operation must survive failed purge"
            );
            assert!(exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM messages WHERE agent_id = ?1)",
                "a"
            )?);
            assert!(exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM memory_documents WHERE agent_id = ?1)",
                "a"
            )?);
            assert!(exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM memory_documents_fts WHERE document_key = ?1)",
                "a"
            )?);
            Ok(())
        })
    }

    #[test]
    fn foreign_history_is_preserved_without_pinning_private_rows() -> Result<()> {
        let (_dir, db) = fixture()?;
        message(&db, "a", "message-a")?;
        task(&db, "a", "task-a", None)?;
        task(
            &db,
            "b",
            "task-b",
            Some(serde_json::json!({
                "result_consumer":{"task_id":"task-a"},
                "source_message_id":"message-a",
                "nested":{"unknown_reference":"message-a"}
            })),
        )?;
        callback_message(&db, "b", "message-a")?;
        db.transaction(|tx| {
            // A malformed historical tool payload must not even be parsed by deletion.
            tx.execute(
                "INSERT INTO tool_executions(evidence_id,agent_id,created_at,kind,payload_json)
                VALUES ('foreign-tool','b','now','Inspect','not-json')",
                [],
            )?;
            super::super::evidence::insert_audit_event_tx(
                tx,
                Some("b"),
                &AuditEvent::legacy(
                    "historical_reference",
                    serde_json::json!({"task_id":"task-a"}),
                ),
            )?;
            Ok(())
        })?;
        let foreign = db.tasks().latest("task-b")?.unwrap();
        fence(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        export_private_history(&db, "a", 1)?;
        purge_private_history(&db, "a", 1, Utc::now())?;
        assert!(db.tasks().latest("task-a")?.is_none());
        assert_eq!(db.tasks().latest("task-b")?, Some(foreign));
        db.with_read(|connection| {
            let tool: String = connection.query_row(
                "SELECT payload_json FROM tool_executions WHERE evidence_id='foreign-tool'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(tool, "not-json");
            Ok(())
        })
    }

    #[test]
    fn active_or_replacement_incarnation_cannot_be_cleaned() -> Result<()> {
        let (_dir, db) = fixture()?;
        assert!(settle_execution(&db, "a", 1, Utc::now()).is_err());
        assert!(purge_private_history(&db, "a", 1, Utc::now()).is_err());
        fence(&db)?;
        assert!(settle_execution(&db, "a", 2, Utc::now()).is_err());
        settle_execution(&db, "a", 1, Utc::now())?;
        purge_private_history(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            let mut identity = identity_tx(tx, "a")?;
            identity.incarnation = 2;
            identity.status = AgentRegistryStatus::Active;
            tx.execute(
                "UPDATE agent_identities SET status = 'active',payload_json = ?1
                        WHERE agent_id = 'a'",
                [serde_json::to_string(&identity)?],
            )?;
            Ok(())
        })?;
        message(&db, "a", "replacement-message")?;
        for incarnation in [1, 2] {
            assert!(settle_execution(&db, "a", incarnation, Utc::now()).is_err());
            assert!(purge_private_history(&db, "a", incarnation, Utc::now()).is_err());
        }
        assert!(db
            .transaction(|tx| assert_private_history_cleared_tx(tx, "a"))
            .is_err());
        Ok(())
    }

    #[test]
    fn completed_legacy_job_and_late_history_are_not_cleanup_proof() -> Result<()> {
        let (_dir, db) = fixture()?;
        message(&db, "a", "old-message")?;
        fence(&db)?;
        let mut job = db.agent_deletions().latest_for_agent("a")?.unwrap();
        // Persist the pre-proof terminal shape directly: new finalization
        // correctly refuses to manufacture this legacy residue.
        job.status = crate::types::AgentDeletionStatus::Completed;
        job.phase = crate::types::AgentDeletionPhase::Finalize;
        job.completed_at = Some(Utc::now());
        job.updated_at = job.completed_at.unwrap();
        db.agent_deletions().update(&job)?;
        let before = snapshot(&db)?;
        assert!(assert_private_history_cleared(&db, "a").is_err());
        assert_eq!(before, snapshot(&db)?);
        settle_execution(&db, "a", 1, Utc::now())?;
        purge_private_history(&db, "a", 1, Utc::now())?;
        assert_private_history_cleared(&db, "a")?;
        message(&db, "a", "late-stale-message")?;
        let before = snapshot(&db)?;
        assert!(assert_private_history_cleared(&db, "a").is_err());
        assert_eq!(before, snapshot(&db)?);
        Ok(())
    }

    #[test]
    fn unknown_owner_schema_fails_closed() -> Result<()> {
        let (_dir, db) = fixture()?;
        db.transaction(|tx| {
            tx.execute_batch("CREATE TABLE future_private_history(agent_id TEXT,body TEXT);
                              INSERT INTO future_private_history VALUES ('a','unknown private body');
                              INSERT INTO future_private_history VALUES ('b','other body');")?;
            Ok(())
        })?;
        fence(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        assert!(purge_private_history(&db, "a", 1, Utc::now())
            .unwrap_err()
            .to_string()
            .contains("unclassified deletion ownership"));
        db.transaction(|tx| {
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM future_private_history", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(count, 2);
            Ok(())
        })
    }

    #[test]
    fn legacy_activation_and_result_claim_settle_without_rewriting_terminal_evidence() -> Result<()>
    {
        let (_dir, db) = fixture()?;
        let now = Utc::now();
        let command: AdmitActivationCommand = serde_json::from_value(serde_json::json!({
            "authority_id":"authority-a",
            "expected_scheduling_generation":1,
            "expected_dispatch_revision":0,
            "activation":{
                "id":"activation-a","agent_id":"a","state":"running",
                "cause":{"kind":"operator_input","message_id":"message-a","resume":null},
                "binding":{"kind":"work_item","work_item_id":"legacy-work"},
                "priority":"normal","preemption":"allow_operator_interjection",
                "source_revision":1,"idempotency_key":"legacy-input-a",
                "provenance":{"origin":"operator","trust":"operator_instruction",
                    "source_id":"message-a","correlation_id":null,"causation_id":null}
            }
        }))?;
        let claim: super::super::task_result_settlement::TaskResultSettlementRecord =
            serde_json::from_value(serde_json::json!({
                "result_identity":"result-a","agent_id":"a","task_id":"task-a",
                "message_id":"result-message","work_item_id":null,"rejoin_generation":1,
                "parent_turn_id":"turn-open","task_status":"completed",
                "state":"caller_admitted","activation_id":"activation-a","disposition":null,
                "created_at":now,"updated_at":now,"admitted_at":now,"settled_at":null
            }))?;
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO scheduler_activations(
                    agent_id,activation_id,authority_id,owner_kind,owner_id,work_item_id,
                    admitted_generation,admission_kind,lifecycle_state,idempotency_key,
                    payload_json,created_at,updated_at)
                 VALUES ('a','activation-a','authority-a','work_item','legacy-work','legacy-work',
                         1,'scheduling','running','legacy-input-a',?1,?2,?2)",
                params![serde_json::to_string(&command)?,now.to_rfc3339()],
            )?;
            tx.execute(
                "INSERT INTO task_result_settlements(
                    result_identity,agent_id,task_id,message_id,rejoin_generation,
                    parent_turn_id,state,activation_id,created_at,updated_at,admitted_at,payload_json)
                 VALUES ('result-a','a','task-a','result-message',1,'turn-open','caller_admitted',
                         'activation-a',?1,?1,?1,?2)",
                params![now.to_rfc3339(),serde_json::to_string(&claim)?],
            )?;
            Ok(())
        })?;
        fence(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            let commands = payloads::<AdmitActivationCommand>(tx, "scheduler_activations", "a")?;
            assert_eq!(
                commands[0].activation.state,
                ActivationLifecycleState::Cancelled
            );
            assert_eq!(commands[0].activation.cause, command.activation.cause);
            let claims = payloads::<
                super::super::task_result_settlement::TaskResultSettlementRecord,
            >(tx, "task_result_settlements", "a")?;
            assert_eq!(
                claims[0].state,
                super::super::task_result_settlement::TaskResultSettlementState::Settled
            );
            assert_eq!(
                claims[0].disposition,
                Some(TaskResultSettlementDisposition::OwnerClosed)
            );
            Ok(())
        })?;
        let before = snapshot(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        assert_eq!(snapshot(&db)?, before);
        purge_private_history(&db, "a", 1, Utc::now())
    }

    #[test]
    fn canonical_lineage_and_other_agents_history_survive_purge() -> Result<()> {
        let (_dir, db) = fixture()?;
        let lineage = crate::types::AgentLineageRecord {
            child_agent_id: "b".into(),
            parent_agent_id: "a".into(),
            creation_cause: crate::types::AgentLineageCreationCause::CreateAgent,
            revision: 1,
            created_at: Utc::now(),
        };
        db.agent_canonical_relations().upsert_lineage(&lineage)?;
        message(&db, "b", "parent-owned-history")?;
        fence(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        purge_private_history(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            let payload: String = tx.query_row(
                "SELECT payload_json FROM agent_lineages WHERE child_agent_id = 'b'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                serde_json::from_str::<crate::types::AgentLineageRecord>(&payload)?,
                lineage
            );
            assert!(exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM messages WHERE agent_id = ?1)",
                "b"
            )?);
            Ok(())
        })
    }

    #[test]
    fn payload_ownership_disagreement_and_live_resources_fail_closed() -> Result<()> {
        let (_dir, db) = fixture()?;
        message(&db, "a", "message-a")?;
        fence(&db)?;
        settle_execution(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE messages SET payload_json = json_set(payload_json,'$.agent_id','b')
                 WHERE agent_id = 'a'",
                [],
            )?;
            Ok(())
        })?;
        assert!(purge_private_history(&db, "a", 1, Utc::now())
            .unwrap_err()
            .to_string()
            .contains("ownership index mismatch"));
        db.transaction(|tx| {
            tx.execute(
                "UPDATE messages SET payload_json = json_set(payload_json,'$.agent_id','a')
                 WHERE agent_id = 'a'",
                [],
            )?;
            Ok(())
        })?;
        let now = Utc::now();
        db.tasks().upsert(&TaskRecord {
            id: "live-task".into(),
            agent_id: "a".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        })?;
        assert!(purge_private_history(&db, "a", 1, Utc::now())
            .unwrap_err()
            .to_string()
            .contains("live resources"));
        Ok(())
    }

    #[test]
    fn purge_requires_settlement_but_does_not_audit_unrelated_search_orphans() -> Result<()> {
        let (_dir, db) = fixture()?;
        memory(&db)?;
        fence(&db)?;
        assert!(purge_private_history(&db, "a", 1, Utc::now()).is_err());
        settle_execution(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO memory_documents_fts(document_key,body) VALUES ('orphan','unknown')",
                [],
            )?;
            Ok(())
        })?;
        purge_private_history(&db, "a", 1, Utc::now())?;
        db.transaction(|tx| {
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM memory_documents_fts WHERE document_key='orphan'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                count, 1,
                "unattributed search residue belongs to index repair"
            );
            Ok(())
        })?;
        Ok(())
    }
}
