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

#[cfg(test)]
#[path = "agent_deletion_cleanup/supervised_admission_tests.rs"]
mod supervised_admission_tests;

const REASON: &str = "agent_deleted";

// Exact sqlite_master SQL for the branch's fixed Scheduling/Task DELETE guards.
// Only fenced, settled purge may suspend these, inside its existing writer TX.
const FIXED_DELETE_GUARDS: &[(&str, &str, &str)] = &[
    (
        "scheduling_fixed_manifest_delete",
        "scheduling_fixed_manifests",
        "CREATE TRIGGER scheduling_fixed_manifest_delete BEFORE DELETE ON scheduling_fixed_manifests WHEN OLD.sealed=1 BEGIN SELECT RAISE(ABORT,'Scheduling fixed manifest immutable'); END",
    ),
    (
        "scheduling_fixed_member_delete",
        "scheduling_fixed_members",
        "CREATE TRIGGER scheduling_fixed_member_delete BEFORE DELETE ON scheduling_fixed_members WHEN (SELECT sealed FROM scheduling_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'Scheduling fixed member immutable'); END",
    ),
    (
        "scheduling_capture_delete",
        "scheduling_domain_captures",
        "CREATE TRIGGER scheduling_capture_delete BEFORE DELETE ON scheduling_domain_captures BEGIN SELECT RAISE(ABORT,'Scheduling fixed capture immutable'); END",
    ),
    (
        "task_query_fixed_input_delete",
        "task_query_fixed_input_members",
        "CREATE TRIGGER task_query_fixed_input_delete BEFORE DELETE ON task_query_fixed_input_members WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task output Input members are immutable'); END",
    ),
    (
        "task_query_fixed_manifest_immutable_delete",
        "task_query_fixed_manifests",
        "CREATE TRIGGER task_query_fixed_manifest_immutable_delete BEFORE DELETE ON task_query_fixed_manifests WHEN OLD.sealed=1 BEGIN SELECT RAISE(ABORT,'fixed Task query inventory is immutable'); END",
    ),
    (
        "task_query_fixed_task_delete",
        "task_query_fixed_tasks",
        "CREATE TRIGGER task_query_fixed_task_delete BEFORE DELETE ON task_query_fixed_tasks WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END",
    ),
    (
        "task_query_fixed_result_delete",
        "task_query_fixed_results",
        "CREATE TRIGGER task_query_fixed_result_delete BEFORE DELETE ON task_query_fixed_results WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END",
    ),
    (
        "task_query_fixed_message_delete",
        "task_query_fixed_messages",
        "CREATE TRIGGER task_query_fixed_message_delete BEFORE DELETE ON task_query_fixed_messages WHEN (SELECT sealed FROM task_query_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'fixed Task query members are immutable'); END",
    ),
    (
        "task_status_fixed_controls_immutable_delete",
        "task_status_fixed_controls",
        "CREATE TRIGGER task_status_fixed_controls_immutable_delete BEFORE DELETE ON task_status_fixed_controls WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END",
    ),
    (
        "task_status_fixed_delegations_immutable_delete",
        "task_status_fixed_delegations",
        "CREATE TRIGGER task_status_fixed_delegations_immutable_delete BEFORE DELETE ON task_status_fixed_delegations WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END",
    ),
];

// Child/derived rows precede source rows. Optional memory tables share this DB.
// Jointly owned records (reports, delegations, workspaces) are not in this list.
const PRIVATE_TABLES: &[(&str, &str)] = &[
    // Domain projections are private content, not retained identity fences.
    ("current_read_qualification", "agent_id"),
    ("current_read_revisions", "agent_id"),
    ("input_query_birth_members", "agent_id"),
    ("input_query_birth_cursors", "agent_id"),
    ("input_evidence_execution_current_sources", "agent_id"),
    ("input_evidence_execution_current_work", "agent_id"),
    ("input_evidence_current", "agent_id"),
    ("work_domain_records", "agent_id"),
    ("work_domain_result_heads", "agent_id"),
    ("work_domain_wait_edges", "agent_id"),
    ("work_domain_current", "agent_id"),
    ("task_domain_records", "agent_id"),
    ("task_domain_current", "agent_id"),
    ("task_domain_import_inventory", "agent_id"),
    ("task_message_sources", "agent_id"),
    ("task_message_order_headers", "agent_id"),
    ("task_message_source_inventory", "agent_id"),
    ("task_output_current_input_relations", "agent_id"),
    ("task_query_task_members", "agent_id"),
    ("task_query_task_births", "agent_id"),
    ("task_query_birth_cursors", "agent_id"),
    ("task_query_task_inventory", "agent_id"),
    ("task_status_control_sources", "agent_id"),
    ("task_status_fixed_controls", "agent_id"),
    ("task_status_delegation_sources", "child_agent_id"),
    ("task_status_fixed_delegations", "child_agent_id"),
    ("task_status_child_observers", "agent_id"),
    ("scheduling_domain_records", "agent_id"),
    ("scheduling_domain_current", "agent_id"),
    ("scheduling_import_inventory", "agent_id"),
    ("dispatch_memory_checkpoints", "agent_id"),
    ("dispatch_memory_legacy_dispatches", "agent_id"),
    ("dispatch_memory_retired_handoffs", "agent_id"),
    ("episode_memory_results", "agent_id"),
    ("control_cancellation_obligations", "agent_id"),
    ("control_command_receipts", "agent_id"),
    ("task_stop_effects", "agent_id"),
    ("command_task_creation_sources", "agent_id"),
    ("provider_queue_parking", "agent_id"),
    ("agent_episode_continuity", "agent_id"),
    // All seed children, including unbound legacy cuts, precede these sources.
    ("episode_continuity_seeds", "agent_id"),
    ("task_query_fixed_manifests", "agent_id"),
    ("scheduling_fixed_manifests", "agent_id"),
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

const SEED_OWNER: &str =
    "seed_id IN (SELECT seed_id FROM episode_continuity_seeds WHERE agent_id = ?1)";
const TASK_INVENTORY_OWNER: &str =
    "inventory_id IN (SELECT inventory_id FROM task_query_fixed_manifests WHERE agent_id = ?1)";
const SCHEDULING_INVENTORY_OWNER: &str =
    "inventory_id IN (SELECT inventory_id FROM scheduling_fixed_manifests WHERE agent_id = ?1)";
const RECEIPT_OWNER: &str =
    "EXISTS(SELECT 1 FROM json_each(scopes_json) s WHERE json_extract(s.value,'$.agent_id') = ?1)";

// Private dependent projections, child-before-source for both archive/purge.
// Original seed ownership, not today's binding, owns legacy/unbound captures.
const DOMAIN_DEPENDENT_TABLES: &[(&str, &str)] = &[
    ("input_evidence_eligibility", SEED_OWNER),
    ("input_evidence_execution_seed_sources", SEED_OWNER),
    ("input_evidence_execution_seed_source_recovery", SEED_OWNER),
    ("input_evidence_execution_seed_work", SEED_OWNER),
    ("input_evidence_seed_attempt_turns", SEED_OWNER),
    ("input_evidence_seed_recovery_current", SEED_OWNER),
    ("input_evidence_captures", SEED_OWNER),
    ("work_domain_eligibility", SEED_OWNER),
    ("work_domain_seed_recovery_current", SEED_OWNER),
    ("work_domain_captures", SEED_OWNER),
    ("task_domain_eligibility", SEED_OWNER),
    ("task_domain_seed_recovery_current", SEED_OWNER),
    ("task_domain_captures", SEED_OWNER),
    ("scheduling_seed_recovery_current", SEED_OWNER),
    ("scheduling_domain_captures", SEED_OWNER),
    ("episode_continuity_members", SEED_OWNER),
    ("episode_continuity_imports", SEED_OWNER),
    ("task_query_fixed_tasks", TASK_INVENTORY_OWNER),
    ("task_query_fixed_results", TASK_INVENTORY_OWNER),
    ("task_query_fixed_messages", TASK_INVENTORY_OWNER),
    ("task_query_fixed_input_members", TASK_INVENTORY_OWNER),
    ("task_output_fixed_input_prunes", TASK_INVENTORY_OWNER),
    ("scheduling_fixed_members", SCHEDULING_INVENTORY_OWNER),
    (
        "task_result_recovery_admissions",
        "result_identity IN (SELECT result_identity FROM task_result_settlements WHERE agent_id = ?1)",
    ),
    (
        "provider_recovery_requests",
        "episode_key IN (SELECT episode_key FROM provider_recovery_episodes WHERE json_extract(binding_json,'$.agent_id') = ?1)",
    ),
    (
        "provider_recovery_episodes",
        "json_extract(binding_json,'$.agent_id') = ?1",
    ),
];

// Effects reference original receipts, so these sources are deleted last.
const COMMAND_DEPENDENT_TABLES: &[(&str, &str)] = &[
    (
        "dispatch_memory_auto_commands",
        "command_nonce IN (SELECT command_nonce FROM input_mutation_receipts WHERE EXISTS(SELECT 1 FROM json_each(scopes_json) s WHERE json_extract(s.value,'$.agent_id') = ?1))",
    ),
    ("input_mutation_receipts", RECEIPT_OWNER),
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
        let guards = checked_fixed_delete_guards_tx(tx)?;
        for (name, _) in &guards {
            tx.execute_batch(&format!("DROP TRIGGER {}", quoted(name)))?;
        }
        for (table, predicate) in private_history_selections_tx(tx)? {
            tx.execute(
                &format!("DELETE FROM {} WHERE {predicate}", quoted(table)),
                [agent_id],
            )
            .with_context(|| format!("clearing deletion-private table {table}"))?;
        }
        for (_, sql) in &guards {
            tx.execute_batch(sql)?;
        }
        ensure!(
            checked_fixed_delete_guards_tx(tx)? == guards,
            "deletion guards were not restored verbatim"
        );
        write_proof_tx(tx, agent_id, expected_incarnation, true, now)?;
        assert_private_history_cleared_tx(tx, agent_id)
    })
}

fn checked_fixed_delete_guards_tx(tx: &Transaction<'_>) -> Result<Vec<(String, String)>> {
    let mut guards = Vec::new();
    for &(name, table, expected_sql) in FIXED_DELETE_GUARDS {
        let actual: Option<(String, String)> = tx
            .query_row(
                "SELECT tbl_name,sql FROM sqlite_master WHERE type='trigger' AND name=?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (actual_table, sql) =
            actual.with_context(|| format!("missing expected deletion guard {name}"))?;
        ensure!(
            actual_table == table && sql == expected_sql,
            "changed deletion guard {name}"
        );
        guards.push((name.to_owned(), sql));
    }
    let mut statement = tx.prepare(
        "SELECT name,tbl_name,sql FROM sqlite_master WHERE type='trigger'
         AND (tbl_name GLOB 'scheduling_*' OR tbl_name GLOB 'task_*')
         AND tbl_name != 'task_status_source_inventory'",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (name, table, sql) = row?;
        // Conservative even for DELETE in a body/comment; never interpret
        // an unfamiliar trigger as a removable fixed guard.
        if sql.to_ascii_uppercase().contains("DELETE") {
            ensure!(
                FIXED_DELETE_GUARDS
                    .iter()
                    .any(|(known, _, _)| *known == name),
                "unknown deletion guard {name} on {table}"
            );
        }
    }
    Ok(guards)
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
    for &(table, predicate) in DOMAIN_DEPENDENT_TABLES.iter().chain(DEPENDENT_TABLES) {
        if table_exists(tx, table)? {
            selections.push((table, predicate.to_owned()));
        }
    }
    for &(table, owner) in PRIVATE_TABLES {
        if table_exists(tx, table)? {
            selections.push((table, format!("{} = ?1", quoted(owner))));
        }
    }
    for &(table, predicate) in COMMAND_DEPENDENT_TABLES {
        if table_exists(tx, table)? {
            selections.push((table, predicate.to_owned()));
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
    for &(table, predicate) in DOMAIN_DEPENDENT_TABLES
        .iter()
        .chain(COMMAND_DEPENDENT_TABLES)
    {
        if table_exists(tx, table)? {
            ensure!(
                !exists(
                    tx,
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM {} WHERE {predicate})",
                        quoted(table)
                    ),
                    agent_id,
                )?,
                "private history remains in {table}"
            );
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
    assert_command_ownership_tx(tx, agent_id)?;
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
            || DEPENDENT_TABLES
                .iter()
                .chain(DOMAIN_DEPENDENT_TABLES)
                .chain(COMMAND_DEPENDENT_TABLES)
                .any(|(name, _)| *name == table)
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
        let table_columns = columns(tx, &table)?;
        for owner in table_columns.iter().filter(|column| {
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
        // A future ownerless capture must not bypass classification.
        for (column, predicate) in [
            ("seed_id", SEED_OWNER),
            ("inventory_id", TASK_INVENTORY_OWNER),
            ("inventory_id", SCHEDULING_INVENTORY_OWNER),
        ] {
            if table_columns.iter().any(|name| name == column) {
                ensure!(
                    !exists(
                        tx,
                        &format!(
                            "SELECT EXISTS(SELECT 1 FROM {} WHERE {predicate})",
                            quoted(&table)
                        ),
                        agent_id,
                    )?,
                    "unclassified deletion ownership in {table}"
                );
            }
        }
    }
    Ok(())
}

// No receipt rewrite or per-scope deletion: the accepted child owns the
// complete, checksum-validated original receipt, including its parent scope.
fn assert_child_receipt_owner_tx(
    tx: &Transaction<'_>,
    purge_agent_id: &str,
    nonce: &str,
    descriptor: &str,
) -> Result<()> {
    use super::input_mutation_receipts::{
        lookup_input_mutation_receipt_tx, InputMutationResult, PreparedInputMutation,
    };
    use crate::runtime::resident_input_evidence::InputEvidenceScope;
    use crate::types::{AgentDeletionJob, AgentDeletionMode, AgentKind, TaskKind, TaskTargetKind};

    let prepared: PreparedInputMutation = serde_json::from_str(descriptor)?;
    ensure!(
        prepared.command_nonce() == nonce,
        "receipt nonce differs from descriptor"
    );
    // Reuse normalized-column, descriptor, full outcome and result-digest checks.
    let receipt = lookup_input_mutation_receipt_tx(tx, &prepared)?
        .context("original shared receipt disappeared")?;
    let (parent, child, parent_task_id, admission, terminal) = match &receipt.outcome.result {
        InputMutationResult::ChildTaskTerminal { ack } => {
            let deletion = ack
                .deletion
                .as_ref()
                .context("child terminal ACK has no deletion")?;
            (
                &ack.original.scope,
                InputEvidenceScope {
                    agent_id: deletion.identity.agent_id.clone(),
                    identity_incarnation: deletion.identity.incarnation,
                },
                &ack.original.admitted.key.id,
                None,
                Some((ack, deletion)),
            )
        }
        InputMutationResult::SupervisedInvocationAdmission { ack } => (
            &ack.parent_scope,
            ack.child_scope.clone(),
            &ack.parent_task_id,
            Some(ack),
            None,
        ),
        _ => bail!("shared receipt is not an original supervised child ACK"),
    };
    ensure!(
        // Preserve the parent Task source until the child's complete receipt
        // is purged. Parent-first cleanup must remain fail-closed.
        child.agent_id == purge_agent_id
            && parent.agent_id != child.agent_id
            && prepared.scopes().len() == 2
            && prepared.scopes().contains(parent)
            && prepared.scopes().contains(&child),
        "child receipt is not the exact parent/child group"
    );
    let parent_identity = identity_tx(tx, &parent.agent_id)?;
    let current = fenced_identity_tx(tx, &child.agent_id, child.identity_incarnation)?;
    ensure!(
        parent_identity.incarnation == parent.identity_incarnation
            && current.kind == AgentKind::Child
            && current.parent_agent_id.as_deref() == Some(parent.agent_id.as_str())
            && current.delegated_from_task_id.as_deref() == Some(parent_task_id.as_str()),
        "child receipt lineage differs from the original parent Task"
    );
    if let Some((ack, deletion)) = terminal {
        let raw_job: String = tx.query_row(
            "SELECT payload_json FROM agent_deletion_jobs WHERE agent_id=?1 AND deletion_id=?2",
            params![child.agent_id, deletion.job.deletion_id],
            |row| row.get(0),
        )?;
        let job: AgentDeletionJob = serde_json::from_str(&raw_job)?;
        ensure!(
            job.agent_id == child.agent_id
                && job.deletion_id == deletion.job.deletion_id
                && job.deletion_id == format!("deletion:child-terminal:{}", ack.original.lease_id)
                && job.mode == AgentDeletionMode::Delete
                && deletion.job.mode == AgentDeletionMode::Delete
                && job.expected_identity_revision == deletion.job.expected_identity_revision
                && job.requested_by == deletion.job.requested_by,
            "child terminal receipt does not bind the current deletion job"
        );
    }
    let (owner, linked_child, kind, status, payload): (
        String,
        Option<String>,
        String,
        String,
        String,
    ) = tx.query_row(
        "SELECT owner_agent_id,child_agent_id,kind,status,payload_json FROM tasks WHERE task_id=?1",
        [parent_task_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let task = super::repositories::decode_task_payload(&payload)?;
    let target = task
        .target()
        .context("canonical parent terminal Task target is absent or malformed")?;
    let detail = task.detail.as_ref();
    let legacy_incarnation = detail.and_then(|d| d.get("target_identity_incarnation"));
    // An explicit typed target is authoritative; unknown typed incarnation
    // cannot be filled from legacy fields or the child's current identity.
    let target_incarnation = if detail.and_then(|d| d.get("target")).is_some() {
        target.incarnation
    } else {
        legacy_incarnation.and_then(serde_json::Value::as_u64)
    };
    ensure!(
        task.id == *parent_task_id
            && task.agent_id == parent.agent_id
            && owner == task.agent_id
            && task.kind.as_str() == kind
            && serde_json::to_value(&task.status)?.as_str() == Some(status.as_str())
            // Admission binds ActorInvocation without the legacy creation marker.
            // Terminal ACKs retain their existing child-task classification.
            && (task.is_child_agent_task()
                || (admission.is_some() && task.kind == TaskKind::ActorInvocation))
            && super::repositories::is_terminal_task_status(&task.status)
            && linked_child.as_deref() == Some(child.agent_id.as_str())
            && task
                .detail
                .as_ref()
                .and_then(|d| d.get("child_agent_id"))
                .and_then(serde_json::Value::as_str)
                == Some(child.agent_id.as_str())
            && target.kind == TaskTargetKind::ChildAgent
            && target.agent_id == child.agent_id
            && target_incarnation == Some(child.identity_incarnation)
            && legacy_incarnation
                .is_none_or(|value| { value.as_u64() == Some(child.identity_incarnation) }),
        "canonical parent terminal Task differs from the child ACK"
    );
    if let Some(admission) = admission {
        let relations =
            super::agent_relations::canonical_relations_from_connection(tx, &child.agent_id)?
                .context("supervised admission child has no canonical relations")?;
        ensure!(
            current.visibility == crate::types::AgentVisibility::Private
                && current.ownership() == crate::types::AgentOwnership::ParentSupervised
                && relations
                    .lineage
                    .as_ref()
                    .is_some_and(|lineage| lineage.parent_agent_id == parent.agent_id)
                && relations.supervision.as_ref().is_some_and(|supervision| {
                    supervision.supervisor_agent_id == parent.agent_id
                        && supervision.delegated_from_task_id.as_deref()
                            == Some(parent_task_id.as_str())
                }),
            "supervised admission canonical child lineage differs"
        );
        ensure!(
            detail.and_then(|d| d.get("source_message_id"))
                == Some(&serde_json::Value::String(
                    admission.source_message_id.clone()
                ))
                && detail.and_then(|d| d.get("source_turn_id"))
                    == Some(&serde_json::to_value(&admission.source_turn_id)?),
            "canonical parent terminal Task differs from the original admission source"
        );
    }
    // The child terminal producer is not an automatic dispatch writer. Keep
    // the existing automatic-command selector safe for both scope members.
    if table_exists(tx, "dispatch_memory_auto_commands")? {
        ensure!(
            !exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM dispatch_memory_auto_commands WHERE command_nonce=?1)",
                nonce
            )?,
            "child terminal receipt unexpectedly owns automatic command history"
        );
    }
    Ok(())
}

fn assert_command_ownership_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
    if !table_exists(tx, "input_mutation_receipts")? {
        return Ok(());
    }
    let candidates = {
        let mut statement = tx.prepare(&format!(
            "SELECT command_nonce,prepared_json FROM input_mutation_receipts
             WHERE {RECEIPT_OWNER}
               AND EXISTS(SELECT 1 FROM json_each(scopes_json) s
                 WHERE json_type(s.value,'$.agent_id') IS NOT 'text'
                    OR json_extract(s.value,'$.agent_id') != ?1)"
        ))?;
        let rows = statement
            .query_map([agent_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (nonce, descriptor) in candidates {
        assert_child_receipt_owner_tx(tx, agent_id, &nonce, &descriptor)
            .context("shared command history requires explicit ownership resolution")?;
    }
    if table_exists(tx, "dispatch_memory_auto_commands")? {
        ensure!(
            !exists(
                tx,
                "SELECT EXISTS(SELECT 1 FROM dispatch_memory_auto_commands a
                 LEFT JOIN input_mutation_receipts r USING(command_nonce)
                 WHERE (EXISTS(SELECT 1 FROM json_each(a.payload_json,'$.base.scopes') s
                           WHERE json_extract(s.value,'$.agent_id') = ?1)
                     OR EXISTS(SELECT 1 FROM json_each(r.scopes_json) s
                           WHERE json_extract(s.value,'$.agent_id') = ?1))
                   AND (r.command_nonce IS NULL
                     OR json(json_extract(a.payload_json,'$.base.scopes')) IS NOT json(r.scopes_json)
                     OR json(json_extract(a.payload_json,'$.final_descriptor.scopes')) IS NOT json(r.scopes_json)))",
                agent_id,
            )?,
            "automatic command history has no matching original ownership source"
        );
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

    fn legacy_fixture(bound: bool) -> Result<(tempfile::TempDir, RuntimeDb)> {
        use super::super::{
            evidence::{append_message_tx, upsert_agent_state_tx},
            migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
            repositories::{upsert_task_tx, upsert_timer_tx},
        };
        use crate::types::{TimerRecord, TimerStatus};

        let dir = tempfile::tempdir()?;
        let path = dir.path().join("runtime.sqlite");
        let mut connection = rusqlite::Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 89)
        {
            apply_migration(&mut connection, migration)?;
        }
        let a = AgentIdentityRecord::new(
            "a",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        let tx = connection.transaction()?;
        for agent in ["a", "b"] {
            let mut identity = a.clone();
            identity.agent_id = agent.into();
            if bound || agent == "b" {
                upsert_agent_identity_tx(&tx, &identity)?;
            }
            let mut state = AgentState::new(agent);
            state.status = AgentStatus::Stopped;
            upsert_agent_state_tx(&tx, &state)?;
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
                    text: format!("legacy private content of {agent}"),
                },
            );
            message.id = format!("legacy-message-{agent}");
            append_message_tx(&tx, &message)?;
            upsert_task_tx(
                &tx,
                &TaskRecord {
                    id: format!("legacy-task-{agent}"),
                    agent_id: agent.into(),
                    kind: TaskKind::CommandTask,
                    status: TaskStatus::Completed,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    parent_message_id: None,
                    work_item_id: None,
                    summary: Some(format!("legacy private result of {agent}")),
                    detail: None,
                    recovery: None,
                },
            )?;
            upsert_timer_tx(
                &tx,
                &TimerRecord {
                    id: format!("legacy-timer-{agent}"),
                    agent_id: agent.into(),
                    created_at: Utc::now(),
                    calendar: None,
                    duration_ms: 1000,
                    interval_ms: None,
                    repeat: false,
                    status: TimerStatus::Completed,
                    summary: Some(format!("legacy private timer of {agent}")),
                    next_fire_at: None,
                    last_fired_at: None,
                    fire_count: 1,
                },
            )?;
        }
        tx.commit()?;
        drop(connection);
        let db = RuntimeDb::open_and_migrate(path, dir.path().join("runtime.lock"))?;
        if !bound {
            db.transaction(|tx| upsert_agent_identity_tx(tx, &a))?;
        }
        Ok((dir, db))
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

    fn fixed_delete_guards(db: &RuntimeDb) -> Result<Vec<(String, String)>> {
        // INSERT/UPDATE and unrelated triggers must survive too.
        db.transaction(|tx| {
            let mut statement = tx.prepare(
                "SELECT name,sql FROM sqlite_master WHERE type='trigger'
                 ORDER BY name",
            )?;
            let guards = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(guards)
        })
    }

    fn private_rows(db: &RuntimeDb, agent_id: &str) -> Result<Vec<(String, Vec<String>)>> {
        db.transaction(|tx| {
            let mut snapshot = Vec::new();
            for (table, predicate) in private_history_selections_tx(tx)? {
                let mut statement = tx.prepare(&format!(
                    "SELECT * FROM {} WHERE {predicate}",
                    quoted(table)
                ))?;
                let count = statement.column_count();
                let mut rows = statement
                    .query_map([agent_id], |row| {
                        (0..count)
                            .map(|column| row.get::<_, rusqlite::types::Value>(column))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })?
                    .map(|row| row.map(|values| format!("{values:?}")))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.sort();
                snapshot.push((table.to_owned(), rows));
            }
            Ok(snapshot)
        })
    }

    #[test]
    fn new_domain_schema_has_explicit_deletion_ownership() {
        // Only the shared cut's counts/digest/observed flags are content-free;
        // its per-agent fixed rows still require private deletion selectors.
        for schema in [
            super::super::episode_snapshot::EPISODE_CONTINUITY_SCHEMA,
            super::super::input_evidence_snapshot::INPUT_EVIDENCE_SCHEMA,
            super::super::work_snapshot::WORK_DOMAIN_SCHEMA,
            super::super::task_snapshot::TASK_DOMAIN_SCHEMA,
            super::super::scheduling_snapshot::SCHEDULING_SCHEMA,
            super::super::dispatch_memory::DISPATCH_MEMORY_SCHEMA,
            super::super::input_mutation_receipts::INPUT_MUTATION_RECEIPT_SCHEMA,
            super::super::current_read_writes::SCHEMA,
            super::super::command_task_creation::SCHEMA,
            super::super::task_stop::TASK_STOP_SCHEMA,
            include_str!("control_transition/schema.sql"),
            include_str!("input_native_birth.rs"),
            include_str!("episode_snapshot_task_queries.rs"),
        ] {
            for declaration in schema
                .lines()
                .filter_map(|line| line.trim().strip_prefix("CREATE TABLE "))
            {
                let name = declaration
                    .strip_prefix("IF NOT EXISTS ")
                    .unwrap_or(declaration)
                    .split_whitespace()
                    .next()
                    .unwrap();
                assert!(
                    name == "task_status_source_inventory"
                        || PRIVATE_TABLES
                            .iter()
                            .chain(DOMAIN_DEPENDENT_TABLES)
                            .chain(COMMAND_DEPENDENT_TABLES)
                            .any(|(table, _)| *table == name),
                    "unclassified domain table {name}"
                );
            }
        }
    }

    #[test]
    fn legacy_seed_archive_and_purge_clear_bound_and_unbound_domains_without_unsealing(
    ) -> Result<()> {
        for bound in [true, false] {
            let (_dir, db) = legacy_fixture(bound)?;
            db.transaction(|tx| {
                let source: String = tx.query_row(
                    "SELECT source_kind FROM episode_continuity_seeds WHERE agent_id='a'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(
                    source,
                    if bound {
                        "upgrade_identity"
                    } else {
                        "upgrade_unbound"
                    }
                );
                Ok(())
            })?;
            fence(&db)?;
            settle_execution(&db, "a", 1, Utc::now())?;
            let guards = fixed_delete_guards(&db)?;
            let foreign = private_rows(&db, "b")?;
            let before = snapshot(&db)?;
            let archive = export_private_history(&db, "a", 1)?;
            assert_eq!(before, snapshot(&db)?, "archive must remain read-only");
            let exported = archive["tables"].as_object().unwrap();
            for table in [
                "episode_continuity_seeds",
                "input_evidence_captures",
                "work_domain_captures",
                "task_domain_captures",
                "scheduling_domain_captures",
                "scheduling_fixed_members",
                "task_query_fixed_tasks",
                "task_query_fixed_messages",
            ] {
                assert!(!exported[table].as_array().unwrap().is_empty(), "{table}");
            }
            assert!(serde_json::to_string(&archive)?.contains("legacy private content of a"));
            assert!(!serde_json::to_string(&archive)?.contains("legacy private content of b"));
            let counts = db.transaction(|tx| {
                exported
                    .iter()
                    .map(|(table, rows)| {
                        let count: usize = tx.query_row(
                            &format!("SELECT COUNT(*) FROM {}", quoted(table)),
                            [],
                            |row| row.get(0),
                        )?;
                        Ok((table.clone(), count, rows.as_array().unwrap().len()))
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
            purge_private_history(&db, "a", 1, Utc::now())?;
            assert_private_history_cleared(&db, "a")?;
            assert_eq!(guards, fixed_delete_guards(&db)?);
            assert_eq!(foreign, private_rows(&db, "b")?);
            db.transaction(|tx| {
                for (table, before, removed) in &counts {
                    let remaining: usize = tx.query_row(
                        &format!("SELECT COUNT(*) FROM {}", quoted(table)),
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(remaining, before - removed, "{table}");
                }
                assert!(
                    tx.execute(
                        "DELETE FROM scheduling_fixed_members WHERE inventory_id IN
                     (SELECT inventory_id FROM scheduling_fixed_manifests WHERE agent_id='b')",
                        [],
                    )
                    .is_err(),
                    "foreign fixed Scheduling rows must remain sealed"
                );
                assert!(
                    tx.execute(
                        "DELETE FROM task_query_fixed_tasks WHERE inventory_id IN
                     (SELECT inventory_id FROM task_query_fixed_manifests WHERE agent_id='b')",
                        [],
                    )
                    .is_err(),
                    "foreign fixed Task rows must remain sealed"
                );
                assert!(
                    tx.execute(
                        "DELETE FROM scheduling_domain_captures WHERE seed_id IN
                     (SELECT seed_id FROM episode_continuity_seeds WHERE agent_id='b')",
                        [],
                    )
                    .is_err(),
                    "foreign fixed captures must remain immutable"
                );
                Ok(())
            })?;
            purge_private_history(&db, "a", 1, Utc::now())?;
        }
        Ok(())
    }

    #[test]
    fn fixed_domain_delete_guards_are_restored_on_purge_failure() -> Result<()> {
        for fault in ["delete", "proof"] {
            let (_dir, db) = legacy_fixture(true)?;
            fence(&db)?;
            settle_execution(&db, "a", 1, Utc::now())?;
            let guards = fixed_delete_guards(&db)?;
            db.transaction(|tx| {
                let event = if fault == "delete" {
                    "BEFORE DELETE ON messages"
                } else {
                    "BEFORE UPDATE ON runtime_metadata WHEN NEW.key='agent_deletion_cleanup:a'"
                };
                tx.execute_batch(&format!(
                    "CREATE TRIGGER reject_domain_purge {event}
                     BEGIN SELECT RAISE(ABORT,'domain-purge-fault'); END;"
                ))?;
                Ok(())
            })?;
            let failure_guards = fixed_delete_guards(&db)?;
            let own = private_rows(&db, "a")?;
            let foreign = private_rows(&db, "b")?;
            let error = purge_private_history(&db, "a", 1, Utc::now()).unwrap_err();
            assert!(format!("{error:#}").contains("domain-purge-fault"));
            assert_eq!(failure_guards, fixed_delete_guards(&db)?);
            assert_eq!(own, private_rows(&db, "a")?);
            assert_eq!(foreign, private_rows(&db, "b")?);
            db.transaction(|tx| {
                assert!(!proof_tx(tx, "a")?.unwrap().purged);
                assert!(exists(
                    tx,
                    "SELECT EXISTS(SELECT 1 FROM episode_continuity_seeds WHERE agent_id=?1)",
                    "a"
                )?);
                assert!(tx
                    .execute(
                        "DELETE FROM scheduling_fixed_manifests WHERE agent_id='a'",
                        []
                    )
                    .is_err());
                tx.execute_batch("DROP TRIGGER reject_domain_purge")?;
                Ok(())
            })?;
            purge_private_history(&db, "a", 1, Utc::now())?;
            assert_eq!(guards, fixed_delete_guards(&db)?);
        }
        Ok(())
    }

    #[test]
    fn fixed_domain_delete_guard_schema_drift_rejects_without_partial_purge() -> Result<()> {
        for drift in ["missing", "changed", "unknown", "unknown_domain"] {
            let (_dir, db) = legacy_fixture(true)?;
            fence(&db)?;
            settle_execution(&db, "a", 1, Utc::now())?;
            db.transaction(|tx| {
                match drift {
                    "missing" => tx.execute_batch("DROP TRIGGER scheduling_capture_delete")?,
                    "changed" => tx.execute_batch(
                        "DROP TRIGGER scheduling_capture_delete;
                         CREATE TRIGGER scheduling_capture_delete
                         BEFORE DELETE ON scheduling_domain_captures
                         BEGIN SELECT RAISE(ABORT,'changed-guard'); END;",
                    )?,
                    _ => {
                        let table = if drift == "unknown" {
                            "scheduling_fixed_members"
                        } else {
                            "task_domain_records"
                        };
                        tx.execute_batch(&format!(
                            "CREATE TRIGGER future_fixed_delete BEFORE DELETE ON {table}
                             BEGIN SELECT RAISE(ABORT,'unknown-guard'); END;"
                        ))?;
                    }
                }
                Ok(())
            })?;
            let guards = fixed_delete_guards(&db)?;
            let own = private_rows(&db, "a")?;
            let foreign = private_rows(&db, "b")?;
            let error = purge_private_history(&db, "a", 1, Utc::now()).unwrap_err();
            assert!(format!("{error:#}").contains("deletion guard"), "{error:#}");
            assert_eq!(guards, fixed_delete_guards(&db)?, "{drift}");
            assert_eq!(own, private_rows(&db, "a")?, "{drift}");
            assert_eq!(foreign, private_rows(&db, "b")?, "{drift}");
            db.transaction(|tx| {
                assert!(!proof_tx(tx, "a")?.unwrap().purged);
                Ok(())
            })?;
        }
        Ok(())
    }

    #[test]
    fn unknown_seed_content_and_shared_command_history_fail_closed() -> Result<()> {
        for shared in [false, true] {
            let (_dir, db) = legacy_fixture(true)?;
            db.transaction(|tx| {
                if shared {
                    tx.execute(
                        "INSERT INTO input_mutation_receipts VALUES(
                         'shared','queue',1,?1,?1,?2,'now','{}','{}','now',?1)",
                        params![
                            "0".repeat(64),
                            r#"[{"agent_id":"a","identity_incarnation":1},{"agent_id":"b","identity_incarnation":1}]"#,
                        ],
                    )?;
                } else {
                    tx.execute_batch(
                        "CREATE TABLE future_seed_history(seed_id TEXT,body TEXT);
                         INSERT INTO future_seed_history
                         SELECT seed_id,'unknown seed content' FROM episode_continuity_seeds
                         WHERE agent_id='a';",
                    )?;
                }
                Ok(())
            })?;
            fence(&db)?;
            settle_execution(&db, "a", 1, Utc::now())?;
            let guards = fixed_delete_guards(&db)?;
            let before = snapshot(&db)?;
            let expected = if shared {
                "shared command history"
            } else {
                "unclassified deletion ownership in future_seed_history"
            };
            assert!(
                format!("{:#}", export_private_history(&db, "a", 1).unwrap_err())
                    .contains(expected)
            );
            assert!(format!(
                "{:#}",
                purge_private_history(&db, "a", 1, Utc::now()).unwrap_err()
            )
            .contains(expected));
            assert_eq!(guards, fixed_delete_guards(&db)?);
            assert_eq!(before, snapshot(&db)?);
            db.transaction(|tx| {
                assert!(!proof_tx(tx, "a")?.unwrap().purged);
                Ok(())
            })?;
        }
        Ok(())
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
