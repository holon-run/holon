//! Domain repository implementations and their transaction helpers.

use std::collections::BTreeMap;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, ToSql, Transaction};
use sha2::{Digest, Sha256};

use crate::runtime_db::evidence::*;
use crate::runtime_db::index_outbox::RuntimeIndexChange;
use crate::runtime_db::types::*;
use crate::runtime_db::write_queue::RuntimeDbWriteContext;
use crate::runtime_db::{
    RuntimeStateTransitionConflict, CONTEXT_EPISODE_ANCHORS_DOMAIN, TASK_PAYLOAD_ARRAY_LIMIT,
    TASK_PAYLOAD_STRING_LIMIT,
};
use crate::types::*;

impl WorkItemRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkItemRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("work_items", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_items", "jsonl", "db", |tx| {
                let mut latest = BTreeMap::<String, WorkItemRecord>::new();
                for record in records {
                    let should_replace = latest
                        .get(&record.id)
                        .is_none_or(|existing| newer_work_item_record(&record, existing));
                    if should_replace {
                        latest.insert(record.id.clone(), record);
                    }
                }
                for record in latest.values() {
                    import_work_item_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn insert_new(&self, record: &WorkItemRecord) -> Result<bool> {
        self.db
            .transaction(|tx| insert_new_work_item_tx(tx, record))
    }

    pub fn insert_new_with_index_changes(
        &self,
        record: &WorkItemRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            let inserted = insert_new_work_item_tx(tx, record)?;
            if inserted {
                insert_runtime_index_changes_tx(tx, changes)?;
            }
            Ok(inserted)
        })
    }

    pub fn update_expected(&self, record: &WorkItemRecord, expected_revision: u64) -> Result<bool> {
        self.db
            .transaction(|tx| update_expected_work_item_tx(tx, record, expected_revision))
    }

    pub fn update_expected_with_index_changes(
        &self,
        record: &WorkItemRecord,
        expected_revision: u64,
        changes: &[RuntimeIndexChange],
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            let updated = update_expected_work_item_tx(tx, record, expected_revision)?;
            if updated {
                insert_runtime_index_changes_tx(tx, changes)?;
            }
            Ok(updated)
        })
    }

    pub fn latest(&self, work_item_id: &str) -> Result<Option<WorkItemRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
                [work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_work_item_payload(&payload))
            .transpose()
    }

    pub fn latest_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<WorkItemRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, work_item_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_payload(&row?)).collect()
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             ORDER BY updated_at DESC, created_at DESC, work_item_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn due_blocked_rechecks(
        &self,
        agent_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<WorkItemRecord>> {
        let now_str = timestamp(now);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             WHERE agent_id = ?1
               AND state = 'open'
               AND blocked_by IS NOT NULL
               AND recheck_at IS NOT NULL
               AND recheck_at <= ?2
               AND (recheck_consumed_at IS NULL OR recheck_consumed_at < recheck_at)
             ORDER BY recheck_at ASC",
        )?;
        let rows =
            statement.query_map(params![agent_id, now_str], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_payload(&row?)).collect()
    }

    pub fn next_recheck_at(&self, agent_id: &str) -> Result<Option<DateTime<Utc>>> {
        let connection = self.db.connection()?;
        let result: Option<String> = connection
            .query_row(
                "SELECT MIN(recheck_at)
                 FROM work_items
                 WHERE agent_id = ?1
                   AND state = 'open'
                   AND blocked_by IS NOT NULL
                   AND recheck_at IS NOT NULL
                   AND (recheck_consumed_at IS NULL OR recheck_consumed_at < recheck_at)",
                [agent_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        result
            .map(|s| parse_timestamp(&s).context("parsing recheck_at from work_items query"))
            .transpose()
    }
}

impl AgentStateRepository<'_> {
    pub fn import_legacy(&self, record: Option<AgentState>) -> Result<()> {
        if self.db.storage_domain_is_complete("agent_states", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("agent_states", "json", "db", |tx| {
                if let Some(record) = record.as_ref() {
                    upsert_agent_state_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": usize::from(record.is_some()) }))
            })
    }

    pub fn upsert(&self, record: &AgentState) -> Result<()> {
        self.db.transaction(|tx| upsert_agent_state_tx(tx, record))
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentState>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_agent_state_payload(&payload))
            .transpose()
    }
}

impl WorkspaceEntryRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkspaceEntry>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("workspace_entries", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("workspace_entries", "jsonl", "db", |tx| {
                let latest = reduce_workspace_entry_records(records);
                for record in latest.values() {
                    upsert_workspace_entry_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkspaceEntry) -> Result<()> {
        self.db
            .transaction(|tx| upsert_workspace_entry_tx(tx, record))
    }

    pub fn upsert_with_index_changes(
        &self,
        record: &WorkspaceEntry,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            upsert_workspace_entry_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn latest_all(&self) -> Result<Vec<WorkspaceEntry>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM workspace_entries
             ORDER BY updated_at DESC, created_at DESC, workspace_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_workspace_entry_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl WorkspaceOccupancyRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkspaceOccupancyRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("workspace_occupancies", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("workspace_occupancies", "jsonl", "db", |tx| {
                let latest = reduce_workspace_occupancy_records(records);
                for record in latest.values() {
                    upsert_workspace_occupancy_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkspaceOccupancyRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_workspace_occupancy_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM workspace_occupancies
             ORDER BY acquired_at DESC, occupancy_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_workspace_occupancy_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl ExecutionRootEntryRepository<'_> {
    pub fn upsert(&self, record: &ExecutionRootEntry) -> Result<()> {
        self.db
            .transaction(|tx| upsert_execution_root_entry_tx(tx, record))
    }

    /// Look up an execution root entry by its `execution_root_id`.
    pub fn get(&self, execution_root_id: &str) -> Result<Option<ExecutionRootEntry>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM execution_root_entries
                 WHERE execution_root_id = ?1",
                [execution_root_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_execution_root_entry_payload(&payload))
            .transpose()
    }

    /// Soft-delete: mark an execution root entry as removed.
    pub fn mark_removed(&self, execution_root_id: &str) -> Result<bool> {
        let now = crate::runtime_db::migrations::timestamp(Utc::now());
        let affected = self.db.transaction(|tx| {
            // Read the current payload, set removed_at, and write it back
            // so that reads via decode_execution_root_entry_payload see the change.
            let payload: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM execution_root_entries
                     WHERE execution_root_id = ?1 AND removed_at IS NULL",
                    [execution_root_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(payload) = payload else {
                return Ok(0);
            };
            let mut json: serde_json::Value = serde_json::from_str(&payload)?;
            json["removed_at"] = serde_json::Value::String(now.clone());
            let updated_payload = serde_json::to_string(&json)?;
            let n = tx.execute(
                "UPDATE execution_root_entries
                 SET removed_at = ?1, payload_json = ?2
                 WHERE execution_root_id = ?3 AND removed_at IS NULL",
                params![now, updated_payload, execution_root_id],
            )?;
            Ok(n)
        })?;
        Ok(affected > 0)
    }

    /// Return all non-removed entries for a workspace.
    pub fn active_for_workspace(&self, workspace_id: &str) -> Result<Vec<ExecutionRootEntry>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json FROM execution_root_entries
                 WHERE workspace_id = ?1 AND removed_at IS NULL
                 ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map([workspace_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_execution_root_entry_payload(&row?))
            .collect()
    }

    /// Return all entries, including removed tombstones, in stable order.
    pub fn latest_all(&self) -> Result<Vec<ExecutionRootEntry>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json FROM execution_root_entries
             ORDER BY created_at ASC, execution_root_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_execution_root_entry_payload(&row?))
            .collect()
    }
}

impl AgentIdentityRepository<'_> {
    pub fn import_legacy(&self, records: Vec<AgentIdentityRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("agent_identities", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("agent_identities", "jsonl", "db", |tx| {
                let latest = reduce_agent_identity_records(records);
                for record in latest.values() {
                    upsert_agent_identity_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &AgentIdentityRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_agent_identity_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<AgentIdentityRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM agent_identities
             ORDER BY updated_at DESC, created_at DESC, agent_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_agent_identity_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentIdentityRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_agent_identity_payload(&payload))
            .transpose()
    }
}

impl WorkItemDelegationRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WorkItemDelegationRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("work_item_delegations", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_item_delegations", "jsonl", "db", |tx| {
                let latest = reduce_work_item_delegation_records(records);
                for record in latest.values() {
                    upsert_work_item_delegation_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkItemDelegationRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_work_item_delegation_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemDelegationRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_delegation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WorkItemDelegationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_delegation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemDelegationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_delegations
             WHERE parent_agent_id = ?1 OR child_agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_delegation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn latest_for_child(
        &self,
        child_agent_id: &str,
    ) -> Result<Option<WorkItemDelegationRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM work_item_delegations
                 WHERE child_agent_id = ?1
                 ORDER BY updated_at DESC, created_at DESC, delegation_id ASC
                 LIMIT 1",
                [child_agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_work_item_delegation_payload(&payload))
            .transpose()
    }
}

impl WorkItemContinuationRepository<'_> {
    pub fn import_empty(&self) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("work_item_continuations", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_item_continuations", "new-domain", "db", |_tx| {
                Ok(serde_json::json!({ "imported_records": 0 }))
            })
    }

    pub fn upsert(&self, record: &WorkItemContinuationFrame) -> Result<()> {
        self.db.transaction(|tx| {
            upsert_work_item_continuation_tx(tx, record)?;
            Ok(())
        })
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WorkItemContinuationFrame>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_continuation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemContinuationFrame>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_continuation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn active_for_agent(&self, agent_id: &str) -> Result<Vec<WorkItemContinuationFrame>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             WHERE agent_id = ?1 AND state = 'active'
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_continuation_payload(&row?))
            .collect()
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemContinuationFrame>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             ORDER BY updated_at DESC, created_at DESC, continuation_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_work_item_continuation_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl ContextEpisodeRepository<'_> {
    pub fn import_legacy(&self, records: Vec<ContextEpisodeRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete(CONTEXT_EPISODE_ANCHORS_DOMAIN, "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import(CONTEXT_EPISODE_ANCHORS_DOMAIN, "jsonl", "db", |tx| {
                let latest = reduce_context_episode_records(records);
                for record in latest.values() {
                    upsert_context_episode_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &ContextEpisodeRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_context_episode_tx(tx, record))
    }

    pub fn upsert_with_index_changes(
        &self,
        record: &ContextEpisodeRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            upsert_context_episode_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<ContextEpisodeRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM context_episode_anchors
             ORDER BY ended_at DESC, started_at DESC, episode_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_context_episode_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ContextEpisodeRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM context_episode_anchors
             WHERE agent_id = ?1
             ORDER BY ended_at DESC, started_at DESC, episode_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_context_episode_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl ExternalTriggerRepository<'_> {
    pub fn import_legacy(&self, records: Vec<ExternalTriggerRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("external_triggers", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("external_triggers", "jsonl", "db", |tx| {
                let latest = reduce_external_trigger_records(records);
                for record in latest.values() {
                    upsert_external_trigger_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &ExternalTriggerRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_external_trigger_tx(tx, record))
    }

    pub fn ensure_default_for_agent(
        &self,
        agent_id: &str,
        delivery_mode: CallbackDeliveryMode,
        now: DateTime<Utc>,
        external_trigger_id: String,
        token: String,
    ) -> Result<(ExternalTriggerRecord, Option<ExternalTriggerRecord>, bool)> {
        self.db.transaction(|tx| {
            if let Some(descriptor) = active_default_for_agent_tx(tx, agent_id)? {
                if descriptor.token.is_some() {
                    return Ok((descriptor, None, false));
                }

                let mut revoked = descriptor;
                revoked.status = ExternalTriggerStatus::Revoked;
                revoked.revoked_at = Some(now);
                upsert_external_trigger_tx(tx, &revoked)?;

                let descriptor = default_external_trigger_record(
                    agent_id,
                    delivery_mode.clone(),
                    now,
                    external_trigger_id.clone(),
                    token.clone(),
                );
                upsert_external_trigger_tx(tx, &descriptor)?;
                return Ok((descriptor, Some(revoked), true));
            }

            let descriptor = default_external_trigger_record(
                agent_id,
                delivery_mode.clone(),
                now,
                external_trigger_id.clone(),
                token.clone(),
            );
            upsert_external_trigger_tx(tx, &descriptor)?;
            Ok((descriptor, None, true))
        })
    }

    pub fn reset_default_for_agent(
        &self,
        agent_id: &str,
        delivery_mode: CallbackDeliveryMode,
        now: DateTime<Utc>,
        external_trigger_id: String,
        token: String,
    ) -> Result<(ExternalTriggerRecord, Option<ExternalTriggerRecord>)> {
        self.db.transaction(|tx| {
            let revoked = if let Some(descriptor) = active_default_for_agent_tx(tx, agent_id)? {
                let mut revoked = descriptor;
                revoked.status = ExternalTriggerStatus::Revoked;
                revoked.revoked_at = Some(now);
                upsert_external_trigger_tx(tx, &revoked)?;
                Some(revoked)
            } else {
                None
            };

            let descriptor = default_external_trigger_record(
                agent_id,
                delivery_mode.clone(),
                now,
                external_trigger_id.clone(),
                token.clone(),
            );
            upsert_external_trigger_tx(tx, &descriptor)?;
            Ok((descriptor, revoked))
        })
    }

    pub fn latest(&self, external_trigger_id: &str) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM external_triggers WHERE external_trigger_id = ?1",
                [external_trigger_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM external_triggers
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, external_trigger_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_external_trigger_payload(&row?))
            .collect()
    }

    pub fn latest_for_agent_limit(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ExternalTriggerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM external_triggers
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, external_trigger_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_external_trigger_payload(&row?))
            .collect()
    }

    pub fn active_default_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM external_triggers
                 WHERE target_agent_id = ?1 AND status = 'active'
                 ORDER BY created_at DESC, external_trigger_id ASC
                 LIMIT 1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn active_by_token_hash(&self, token_hash: &str) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM external_triggers
                 WHERE token_hash = ?1 AND status = 'active'
                 LIMIT 1",
                [token_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM external_triggers
             ORDER BY created_at DESC, external_trigger_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_external_trigger_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl OperatorNotificationRepository<'_> {
    pub fn insert(&self, agent_id: &str, record: &OperatorNotificationRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_operator_notification_tx(tx, agent_id, record))
    }

    pub fn read_recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<OperatorNotificationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_notifications
             WHERE agent_id = ?1
             ORDER BY created_at DESC, notification_id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_operator_notification_payload(&row?))
            .collect()
    }
}

impl OperatorTransportBindingRepository<'_> {
    pub fn upsert(&self, agent_id: &str, record: &OperatorTransportBinding) -> Result<()> {
        self.db
            .transaction(|tx| upsert_operator_transport_binding_tx(tx, agent_id, record))
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<OperatorTransportBinding>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_transport_bindings
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, binding_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_operator_transport_binding_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn read_recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<OperatorTransportBinding>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_transport_bindings
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, binding_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_operator_transport_binding_payload(&row?))
            .collect()
    }

    pub fn latest_all(&self) -> Result<Vec<OperatorTransportBinding>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_transport_bindings
             ORDER BY created_at DESC, binding_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_operator_transport_binding_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl OperatorDeliveryRepository<'_> {
    pub fn upsert(&self, agent_id: &str, record: &OperatorDeliveryRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_operator_delivery_record_tx(tx, agent_id, record))
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<OperatorDeliveryRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_delivery_records
             WHERE agent_id = ?1
             ORDER BY created_at DESC, delivery_intent_id DESC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_operator_delivery_record_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn read_recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<OperatorDeliveryRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_delivery_records
             WHERE agent_id = ?1
             ORDER BY created_at DESC, delivery_intent_id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_operator_delivery_record_payload(&row?))
            .collect()
    }

    pub fn latest_all(&self) -> Result<Vec<OperatorDeliveryRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM operator_delivery_records
             ORDER BY created_at DESC, delivery_intent_id DESC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_operator_delivery_record_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl TaskRepository<'_> {
    pub fn import_legacy(&self, records: Vec<TaskRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("tasks", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("tasks", "jsonl", "db", |tx| {
                let latest = reduce_task_records(records);
                for record in latest.values() {
                    let _ = upsert_task_tx(tx, record)?;
                }
                let active_records = latest
                    .values()
                    .filter(|record| is_active_task_status(&record.status))
                    .count();
                Ok(serde_json::json!({
                    "imported_records": latest.len(),
                    "active_records": active_records,
                }))
            })
    }

    pub fn upsert(&self, record: &TaskRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_task_tx(tx, record).map(|_| ()))
    }

    pub fn upsert_with_index_changes(
        &self,
        record: &TaskRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            let _ = upsert_task_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn latest(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM tasks WHERE task_id = ?1",
                [task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_task_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<TaskRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM tasks
             ORDER BY updated_at DESC, created_at DESC, task_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_task_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn latest_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.query_for_agent(agent_id, "owner_agent_id = ?1", [agent_id], limit)
    }

    pub fn activity_watermark_for_agent(&self, agent_id: Option<&str>) -> Result<(u64, u128)> {
        let connection = self.db.connection()?;
        let mut hasher = Sha256::new();
        let mut count = 0u64;
        if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM tasks
                 WHERE owner_agent_id = ?1
                 ORDER BY task_id ASC",
            )?;
            let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
            for row in rows {
                let payload = row?;
                count += 1;
                hasher.update(payload.as_bytes());
                hasher.update(b"\n");
            }
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM tasks
                 ORDER BY task_id ASC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                let payload = row?;
                count += 1;
                hasher.update(payload.as_bytes());
                hasher.update(b"\n");
            }
        }

        let digest = hasher.finalize();
        let mut marker_bytes = [0u8; 16];
        marker_bytes.copy_from_slice(&digest[..16]);
        Ok((count, u128::from_be_bytes(marker_bytes)))
    }

    pub fn active_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.query_for_agent(
            agent_id,
            "owner_agent_id = ?1 AND status IN ('queued', 'running', 'cancelling')",
            [agent_id],
            limit,
        )
    }

    fn query_for_agent(
        &self,
        _agent_id: &str,
        where_clause: &str,
        params: [&str; 1],
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let sql = format!(
            "SELECT payload_json
             FROM tasks
             WHERE {where_clause}
             ORDER BY updated_at DESC, created_at DESC, task_id ASC
             LIMIT {limit}",
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_task_payload(&row?)).collect()
    }
}

impl WaitConditionRepository<'_> {
    pub fn import_legacy(&self, records: Vec<WaitConditionRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("wait_conditions", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("wait_conditions", "jsonl", "db", |tx| {
                let mut imported_ids = BTreeMap::<String, ()>::new();
                for record in records {
                    imported_ids.insert(record.id.clone(), ());
                    let _ = upsert_wait_condition_tx(tx, &record)?;
                }
                Ok(serde_json::json!({ "imported_records": imported_ids.len() }))
            })
    }

    pub fn upsert(&self, record: &WaitConditionRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_wait_condition_tx(tx, record).map(|_| ()))
    }

    pub fn latest_all(&self) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id,
                wake_sources_json, continuation_json
             FROM wait_conditions
             ORDER BY wait_condition_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<WaitConditionRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id,
                wake_sources_json, continuation_json
             FROM wait_conditions
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        let mut records: Vec<_> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WaitConditionRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        let mut records: Vec<_> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))?;
        records.reverse();
        Ok(records)
    }

    pub fn active_for_agent(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE agent_id = ?1 AND status = 'active'
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))
    }

    pub fn active_all(&self) -> Result<Vec<WaitConditionRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT wait_condition_id, agent_id, work_item_id, status, kind, source,
                subject_ref, waiting_for, created_at, updated_at, expires_at,
                resolved_at, cancelled_at, last_turn_id,
                wake_sources_json, continuation_json
             FROM wait_conditions
             WHERE status = 'active'
             ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            decode_wait_condition_row(row).map_err(wait_condition_decode_error)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("reading wait conditions: {e}"))
    }
}

impl QueueEntryRepository<'_> {
    pub fn import_legacy(&self, records: Vec<QueueEntryRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("queue_entries", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("queue_entries", "jsonl", "db", |tx| {
                let mut imported_ids = BTreeMap::<String, ()>::new();
                for record in records {
                    imported_ids.insert(record.message_id.clone(), ());
                    let _ = upsert_queue_entry_tx(tx, &record)?;
                }
                Ok(serde_json::json!({ "imported_records": imported_ids.len() }))
            })
    }

    pub fn upsert(&self, record: &QueueEntryRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_queue_entry_tx(tx, record).map(|_| ()))
    }

    pub fn try_claim_queued_message(&self, record: &QueueEntryRecord) -> Result<bool> {
        self.db
            .transaction(|tx| try_claim_queued_message_tx(tx, record))
    }

    pub fn latest_all(&self) -> Result<Vec<QueueEntryRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM queue_entries
             ORDER BY updated_at DESC, created_at DESC, message_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_queue_entry_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<QueueEntryRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM queue_entries
                 WHERE agent_id = ?1
                 ORDER BY updated_at DESC, created_at DESC, message_id ASC
                 LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_queue_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM queue_entries
                 ORDER BY updated_at DESC, created_at DESC, message_id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_queue_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        records.reverse();
        Ok(records)
    }

    /// Returns true if the given agent has any messages currently queued.
    pub fn has_queued_for_agent(&self, agent_id: &str) -> Result<bool> {
        let connection = self.db.connection()?;
        let status = enum_string(&crate::types::QueueEntryStatus::Queued)?;
        let interrupted_status = enum_string(&crate::types::QueueEntryStatus::Interrupted)?;
        let exists: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM queue_entries WHERE agent_id = ?1 AND status IN (?2, ?3) LIMIT 1",
                params![agent_id, status, interrupted_status],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    /// Returns only entries currently queued for a specific agent.
    pub fn queued_for_agent(&self, agent_id: &str) -> Result<Vec<QueueEntryRecord>> {
        let connection = self.db.connection()?;
        let queued_status = enum_string(&crate::types::QueueEntryStatus::Queued)?;
        let interrupted_status = enum_string(&crate::types::QueueEntryStatus::Interrupted)?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM queue_entries
             WHERE agent_id = ?1 AND status IN (?2, ?3)
             ORDER BY updated_at DESC, created_at DESC, message_id ASC",
        )?;
        let rows = statement.query_map(
            params![agent_id, queued_status, interrupted_status],
            |row| row.get::<_, String>(0),
        )?;
        let mut records: Vec<_> = rows
            .map(|row| decode_queue_entry_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl TimerRepository<'_> {
    pub fn import_legacy(&self, records: Vec<TimerRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("timers", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("timers", "jsonl", "db", |tx| {
                let latest = reduce_timer_records(records);
                for record in latest.values() {
                    upsert_timer_tx(tx, record)?;
                }
                let active_records = latest
                    .values()
                    .filter(|record| record.status == TimerStatus::Active)
                    .count();
                Ok(serde_json::json!({
                    "imported_records": latest.len(),
                    "active_records": active_records,
                }))
            })
    }

    pub fn upsert(&self, record: &TimerRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_timer_tx(tx, record))
    }

    pub fn latest(&self, timer_id: &str) -> Result<Option<TimerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM timers WHERE timer_id = ?1",
                [timer_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_timer_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<TimerRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_timer_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<TimerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_timer_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TimerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM timers
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, timer_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records: Vec<_> = rows
            .map(|row| decode_timer_payload(&row?))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }
}

impl TurnRecordRepository<'_> {
    pub fn import_legacy(
        &self,
        messages: Vec<serde_json::Value>,
        tool_executions: Vec<ToolExecutionRecord>,
        briefs: Vec<BriefRecord>,
        delivery_summaries: Vec<DeliverySummaryRecord>,
        wait_conditions: Vec<WaitConditionRecord>,
    ) -> Result<()> {
        if self.db.storage_domain_is_complete("turn_records", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("turn_records", "jsonl-derived", "db", |tx| {
                let records = derive_turn_records_from_legacy_evidence(
                    messages,
                    tool_executions,
                    briefs,
                    delivery_summaries,
                    wait_conditions,
                )?;
                for record in &records {
                    upsert_turn_record_tx(tx, record)?;
                }
                Ok(serde_json::json!({
                    "imported_records": records.len(),
                    "source": "legacy evidence jsonl",
                    "ignored": "turns.jsonl"
                }))
            })
    }

    pub fn upsert(&self, record: &TurnRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_turn_record_tx(tx, record))
    }

    pub fn recent_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TurnRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM turn_records
             WHERE agent_id = ?1
             ORDER BY turn_index DESC, created_at DESC, turn_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        let mut records = rows
            .map(|row| decode_turn_record_payload(&row?))
            .collect::<Result<Vec<_>>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM turn_records
             ORDER BY turn_index DESC, created_at DESC, turn_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let mut records = rows
            .map(|row| decode_turn_record_payload(&row?))
            .collect::<Result<Vec<_>>>()?;
        records.reverse();
        Ok(records)
    }
}

impl MessageRepository<'_> {
    pub fn import_legacy(&self, messages: Vec<serde_json::Value>) -> Result<()> {
        if self.db.storage_domain_is_complete("messages", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("messages", "jsonl", "db", |tx| {
                let mut imported_messages = 0_u64;
                let mut dropped_messages = 0_u64;
                for raw_message in messages {
                    match normalize_legacy_message_value(raw_message)? {
                        Some(message) => {
                            upsert_message_tx(tx, &message)?;
                            imported_messages += 1;
                        }
                        None => dropped_messages += 1,
                    }
                }
                Ok(serde_json::json!({
                    "imported_messages": imported_messages,
                    "dropped_messages": dropped_messages,
                }))
            })
    }

    pub fn upsert(&self, message: &MessageEnvelope) -> Result<()> {
        self.db.transaction(|tx| upsert_message_tx(tx, message))
    }

    pub fn append_with_index_changes(
        &self,
        message: &MessageEnvelope,
        changes: &[RuntimeIndexChange],
    ) -> Result<MessageEnvelope> {
        self.db.transaction(|tx| {
            let (appended, inserted) = append_message_tx(tx, message)?;
            if inserted {
                insert_runtime_index_changes_tx(tx, changes)?;
            }
            Ok(appended)
        })
    }

    pub fn upsert_with_index_changes(
        &self,
        message: &MessageEnvelope,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            upsert_message_tx(tx, message)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn upsert_many(&self, messages: &[MessageEnvelope]) -> Result<()> {
        self.db.transaction(|tx| {
            for message in messages {
                upsert_message_tx(tx, message)?;
            }
            Ok(())
        })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<MessageEnvelope>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                 ORDER BY message_seq IS NULL ASC, message_seq DESC, created_at DESC, message_id ASC
                 LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 ORDER BY message_seq IS NULL ASC, message_seq DESC, created_at DESC, message_id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        records.reverse();
        Ok(records)
    }

    pub fn from(
        &self,
        agent_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<MessageEnvelope>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                 ORDER BY message_seq IS NULL DESC, message_seq ASC, created_at ASC, message_id ASC
                 LIMIT -1 OFFSET ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, offset], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 ORDER BY message_seq IS NULL DESC, message_seq ASC, created_at ASC, message_id ASC
                 LIMIT -1 OFFSET ?1",
            )?;
            let rows = statement.query_map([offset], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        if records.len() > limit {
            records.drain(0..(records.len() - limit));
        }
        Ok(records)
    }

    pub fn all(&self, agent_id: Option<&str>) -> Result<Vec<MessageEnvelope>> {
        let connection = self.db.connection()?;
        if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 WHERE agent_id = ?1
                 ORDER BY message_seq IS NULL DESC, message_seq ASC, created_at ASC, message_id ASC",
            )?;
            let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?)).collect()
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM messages
                 ORDER BY message_seq IS NULL DESC, message_seq ASC, created_at ASC, message_id ASC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_message_payload(&row?)).collect()
        }
    }

    pub fn all_values(&self, agent_id: Option<&str>) -> Result<Vec<serde_json::Value>> {
        self.all(agent_id)?
            .into_iter()
            .map(|message| serde_json::to_value(message).map_err(Into::into))
            .collect()
    }

    pub fn by_id(
        &self,
        agent_id: Option<&str>,
        message_id: &str,
    ) -> Result<Option<MessageEnvelope>> {
        let connection = self.db.connection()?;
        let payload = if let Some(agent_id) = agent_id {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM messages
                     WHERE agent_id = ?1 AND message_id = ?2
                     LIMIT 1",
                    params![agent_id, message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        } else {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM messages
                     WHERE message_id = ?1
                     LIMIT 1",
                    [message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        };
        payload
            .map(|payload| decode_message_payload(&payload))
            .transpose()
    }

    pub fn count(&self, agent_id: Option<&str>) -> Result<usize> {
        let connection = self.db.connection()?;
        let count: i64 = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT COUNT(*) FROM messages WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?
        };
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    pub fn max_message_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        let connection = self.db.connection()?;
        let max_seq: Option<i64> = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(message_seq) FROM messages WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row("SELECT MAX(message_seq) FROM messages", [], |row| {
                row.get(0)
            })?
        };
        Ok(max_seq.unwrap_or_default().max(0) as u64)
    }
}

impl TranscriptRepository<'_> {
    pub fn import_legacy(&self, entries: Vec<TranscriptEntry>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("transcript_entries", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("transcript_entries", "jsonl", "db", |tx| {
                for entry in &entries {
                    upsert_transcript_entry_tx(tx, entry)?;
                }
                Ok(serde_json::json!({
                    "imported_transcript_entries": entries.len(),
                }))
            })
    }

    pub fn upsert(&self, entry: &TranscriptEntry) -> Result<()> {
        self.db
            .transaction(|tx| upsert_transcript_entry_tx(tx, entry))
    }

    pub fn append(&self, entry: &TranscriptEntry) -> Result<TranscriptEntry> {
        self.db
            .transaction(|tx| append_transcript_entry_tx(tx, entry).map(|(entry, _)| entry))
    }

    pub fn upsert_many(&self, entries: &[TranscriptEntry]) -> Result<()> {
        self.db.transaction(|tx| {
            for entry in entries {
                upsert_transcript_entry_tx(tx, entry)?;
            }
            Ok(())
        })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<TranscriptEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut records = if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 WHERE agent_id = ?1
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) DESC, created_at DESC, evidence_id ASC
                 LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) DESC, created_at DESC, evidence_id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect::<Result<Vec<_>>>()?
        };
        records.reverse();
        Ok(records)
    }

    pub fn all(&self, agent_id: Option<&str>) -> Result<Vec<TranscriptEntry>> {
        let connection = self.db.connection()?;
        if let Some(agent_id) = agent_id {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 WHERE agent_id = ?1
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) ASC, created_at ASC, evidence_id ASC",
            )?;
            let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect()
        } else {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM transcript_entries
                 ORDER BY COALESCE(transcript_seq, 9223372036854775807) ASC, created_at ASC, evidence_id ASC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| decode_transcript_entry_payload(&row?))
                .collect()
        }
    }

    pub fn by_id(&self, agent_id: Option<&str>, entry_id: &str) -> Result<Option<TranscriptEntry>> {
        let connection = self.db.connection()?;
        let payload: Option<String> = if let Some(agent_id) = agent_id {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM transcript_entries
                     WHERE agent_id = ?1 AND evidence_id = ?2
                     LIMIT 1",
                    params![agent_id, entry_id],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM transcript_entries
                     WHERE evidence_id = ?1
                     LIMIT 1",
                    [entry_id],
                    |row| row.get(0),
                )
                .optional()?
        };
        payload
            .map(|payload| decode_transcript_entry_payload(&payload))
            .transpose()
    }

    pub fn max_transcript_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        let connection = self.db.connection()?;
        let max_seq: Option<i64> = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(transcript_seq) FROM transcript_entries WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row(
                "SELECT MAX(transcript_seq) FROM transcript_entries",
                [],
                |row| row.get(0),
            )?
        };
        Ok(max_seq.unwrap_or_default().max(0) as u64)
    }
}

impl EvidenceRepository<'_> {
    pub fn import_legacy(
        &self,
        messages: Vec<serde_json::Value>,
        transcript_entries: Vec<TranscriptEntry>,
        tool_executions: Vec<ToolExecutionRecord>,
        briefs: Vec<BriefRecord>,
        delivery_summaries: Vec<DeliverySummaryRecord>,
    ) -> Result<()> {
        if self.db.storage_domain_is_complete("evidence", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("evidence", "jsonl", "db", |tx| {
                let mut imported_messages = 0_u64;
                let mut dropped_messages = 0_u64;
                for raw_message in messages {
                    match normalize_legacy_message_value(raw_message)? {
                        Some(message) => {
                            insert_message_evidence_tx(tx, &message)?;
                            imported_messages += 1;
                        }
                        None => dropped_messages += 1,
                    }
                }
                for entry in &transcript_entries {
                    insert_transcript_evidence_tx(tx, entry)?;
                }
                for record in &tool_executions {
                    insert_tool_evidence_tx(tx, record)?;
                }
                for brief in &briefs {
                    insert_brief_evidence_tx(tx, brief)?;
                }
                for summary in &delivery_summaries {
                    insert_delivery_summary_evidence_tx(tx, summary)?;
                }
                Ok(serde_json::json!({
                    "imported_messages": imported_messages,
                    "dropped_messages": dropped_messages,
                    "imported_transcript_entries": transcript_entries.len(),
                    "imported_tool_executions": tool_executions.len(),
                    "imported_briefs": briefs.len(),
                    "imported_delivery_summaries": delivery_summaries.len(),
                }))
            })
    }

    pub fn append_message(&self, message: &MessageEnvelope) -> Result<()> {
        self.db
            .transaction(|tx| insert_message_evidence_tx(tx, message))
    }

    pub fn append_transcript_entry(&self, entry: &TranscriptEntry) -> Result<()> {
        self.db
            .transaction(|tx| insert_transcript_evidence_tx(tx, entry))
    }

    pub fn append_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_tool_evidence_tx(tx, record))
    }

    pub fn append_tool_execution_with_index_changes(
        &self,
        record: &ToolExecutionRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            insert_tool_evidence_tx(tx, record)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn append_brief(&self, brief: &BriefRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_brief_evidence_tx(tx, brief))
    }

    pub fn append_brief_with_index_changes(
        &self,
        brief: &BriefRecord,
        changes: &[RuntimeIndexChange],
    ) -> Result<()> {
        self.db.transaction(|tx| {
            insert_brief_evidence_tx(tx, brief)?;
            insert_runtime_index_changes_tx(tx, changes)
        })
    }

    pub fn append_delivery_summary(&self, record: &DeliverySummaryRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_delivery_summary_evidence_tx(tx, record))
    }

    pub fn query(&self, kind: EvidenceKind, query: EvidenceQuery<'_>) -> Result<Vec<EvidenceRow>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let mut clauses = Vec::new();
        let mut params = Vec::<String>::new();
        push_optional_clause(&mut clauses, &mut params, "agent_id", query.agent_id);
        push_optional_clause(&mut clauses, &mut params, "turn_id", query.turn_id);
        push_optional_clause(&mut clauses, &mut params, "message_id", query.message_id);
        push_optional_clause(&mut clauses, &mut params, "task_id", query.task_id);
        push_optional_clause(
            &mut clauses,
            &mut params,
            "work_item_id",
            query.work_item_id,
        );
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT evidence_id, agent_id, turn_id, message_id, task_id, work_item_id, created_at, preview
             FROM {}{}
             ORDER BY created_at DESC, evidence_id ASC
             LIMIT {}",
            kind.table_name(),
            where_clause,
            limit
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(EvidenceRow {
                evidence_id: row.get(0)?,
                agent_id: row.get(1)?,
                turn_id: row.get(2)?,
                message_id: row.get(3)?,
                task_id: row.get(4)?,
                work_item_id: row.get(5)?,
                created_at: row.get(6)?,
                preview: row.get(7)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn recent_payloads(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<EvidencePayloadRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT payload_json
             FROM {}
             WHERE agent_id = ?1
             ORDER BY created_at DESC, evidence_id DESC
             LIMIT ?2",
            kind.table_name()
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![agent_id, limit], |row| {
            Ok(EvidencePayloadRow {
                payload_json: row.get(0)?,
            })
        })?;
        let mut records: Vec<_> = rows
            .map(|row| row.map_err(Into::into))
            .collect::<Result<_>>()?;
        records.reverse();
        Ok(records)
    }

    pub fn payload_by_id(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        evidence_id: &str,
    ) -> Result<Option<EvidencePayloadRow>> {
        let sql = format!(
            "SELECT payload_json
             FROM {}
             WHERE agent_id = ?1 AND evidence_id = ?2
             LIMIT 1",
            kind.table_name()
        );
        let connection = self.db.connection()?;
        connection
            .query_row(&sql, params![agent_id, evidence_id], |row| {
                Ok(EvidencePayloadRow {
                    payload_json: row.get(0)?,
                })
            })
            .optional()
            .map_err(Into::into)
    }

    pub fn payloads_by_ids(
        &self,
        kind: EvidenceKind,
        agent_id: &str,
        evidence_ids: &[String],
    ) -> Result<Vec<EvidencePayloadRow>> {
        if evidence_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = evidence_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT payload_json FROM {} WHERE agent_id = ?1 AND evidence_id IN ({})",
            kind.table_name(),
            placeholders
        );
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let mut all_params: Vec<Box<dyn ToSql>> = Vec::with_capacity(1 + evidence_ids.len());
        all_params.push(Box::new(agent_id.to_string()));
        for id in evidence_ids {
            all_params.push(Box::new(id.clone()));
        }
        let param_refs: Vec<&dyn ToSql> = all_params.iter().map(|p| p.as_ref()).collect();
        let rows = statement.query_map(param_refs.as_slice(), |row| {
            Ok(EvidencePayloadRow {
                payload_json: row.get(0)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn recent_briefs(&self, agent_id: &str, limit: usize) -> Result<Vec<BriefRecord>> {
        self.recent_payloads(EvidenceKind::Brief, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn brief_by_id(&self, agent_id: &str, brief_id: &str) -> Result<Option<BriefRecord>> {
        self.payload_by_id(EvidenceKind::Brief, agent_id, brief_id)?
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .transpose()
    }

    pub fn briefs_by_ids(&self, agent_id: &str, brief_ids: &[String]) -> Result<Vec<BriefRecord>> {
        self.payloads_by_ids(EvidenceKind::Brief, agent_id, brief_ids)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn recent_tool_executions(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ToolExecutionRecord>> {
        self.recent_payloads(EvidenceKind::ToolExecution, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn tool_execution_by_id(
        &self,
        agent_id: &str,
        tool_id: &str,
    ) -> Result<Option<ToolExecutionRecord>> {
        self.payload_by_id(EvidenceKind::ToolExecution, agent_id, tool_id)?
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .transpose()
    }

    pub fn recent_delivery_summaries(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        self.recent_payloads(EvidenceKind::DeliverySummary, agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    pub fn latest_delivery_summary(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<Option<DeliverySummaryRecord>> {
        let connection = self.db.connection()?;
        let payload = connection
            .query_row(
                "SELECT payload_json
                 FROM delivery_summaries
                 WHERE agent_id = ?1 AND work_item_id = ?2
                 ORDER BY created_at DESC, evidence_id DESC
                 LIMIT 1",
                params![agent_id, work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload
            .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
            .transpose()
    }

    pub fn count_briefs(&self, agent_id: &str) -> Result<usize> {
        let connection = self.db.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM briefs WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }
}

impl AuditEventSink<'_> {
    pub fn append(&self, agent_id: Option<&str>, event: &AuditEvent) -> Result<AuditEvent> {
        self.db.transaction_with_context(
            RuntimeDbWriteContext::sync("audit_events.append", "audit_events"),
            |tx| append_audit_event_tx(tx, agent_id, event).map(|(event, _)| event),
        )
    }

    /// Check whether an audit event with the given id already exists.
    pub fn has_event_by_id(&self, event_id: &str) -> Result<bool> {
        let connection = self.db.connection()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM audit_events WHERE audit_event_id = ?1",
                [event_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn append_many(
        &self,
        agent_id: Option<&str>,
        events: &[AuditEvent],
    ) -> Result<Vec<AuditEvent>> {
        self.db.transaction_with_context(
            RuntimeDbWriteContext::sync("audit_events.append_many", "audit_events"),
            |tx| {
                let mut appended = Vec::with_capacity(events.len());
                for event in events {
                    appended.push(append_audit_event_tx(tx, agent_id, event)?.0);
                }
                Ok(appended)
            },
        )
    }

    pub fn import_legacy(&self, agent_id: Option<&str>, events: Vec<AuditEvent>) -> Result<()> {
        if self.db.storage_domain_is_complete("audit_events", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("audit_events", "jsonl", "db", |tx| {
                for event in &events {
                    import_audit_event_tx(tx, agent_id, event)?;
                }
                Ok(serde_json::json!({ "imported_records": events.len() }))
            })
    }

    pub fn recent(&self, agent_id: Option<&str>, limit: usize) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events");
        if agent_id.is_some() {
            sql.push_str(" WHERE agent_id = ?1");
        }
        sql.push_str(" ORDER BY event_seq DESC, created_at DESC LIMIT ?");
        let mut statement = connection.prepare(&sql)?;
        let mut events = if let Some(agent_id) = agent_id {
            statement
                .query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?
                .map(|row| {
                    let payload = row?;
                    serde_json::from_str(&payload).map_err(Into::into)
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            statement
                .query_map(params![limit], |row| row.get::<_, String>(0))?
                .map(|row| {
                    let payload = row?;
                    serde_json::from_str(&payload).map_err(Into::into)
                })
                .collect::<Result<Vec<_>>>()?
        };
        events.reverse();
        Ok(events)
    }

    pub fn latest_event_seq(&self, agent_id: Option<&str>) -> Result<Option<u64>> {
        let connection = self.db.connection()?;
        let value = if let Some(agent_id) = agent_id {
            connection.query_row(
                "SELECT MAX(event_seq) FROM audit_events WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, Option<i64>>(0),
            )?
        } else {
            connection.query_row("SELECT MAX(event_seq) FROM audit_events", [], |row| {
                row.get::<_, Option<i64>>(0)
            })?
        };
        value
            .map(|seq| u64::try_from(seq).context("stored audit event sequence is negative"))
            .transpose()
    }

    pub fn max_event_seq(&self, agent_id: Option<&str>) -> Result<u64> {
        Ok(self.latest_event_seq(agent_id)?.unwrap_or(0))
    }

    pub fn range(
        &self,
        agent_id: Option<&str>,
        before_seq: Option<u64>,
        after_seq: Option<u64>,
        descending: bool,
        limit: usize,
    ) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let lower = i64::try_from(after_seq.unwrap_or(0))
            .context("audit event lower cursor exceeds SQLite integer range")?;
        let upper = before_seq
            .map(|seq| {
                i64::try_from(seq).context("audit event upper cursor exceeds SQLite integer range")
            })
            .transpose()?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events WHERE event_seq > ?1");
        if upper.is_some() {
            sql.push_str(" AND event_seq < ?2");
        }
        if agent_id.is_some() {
            let param_index = if upper.is_some() { 3 } else { 2 };
            sql.push_str(&format!(" AND agent_id = ?{param_index}"));
        }
        if descending {
            sql.push_str(" ORDER BY event_seq DESC, created_at DESC");
        } else {
            sql.push_str(" ORDER BY event_seq ASC, created_at ASC");
        }
        let limit_param_index = 2 + usize::from(upper.is_some()) + usize::from(agent_id.is_some());
        sql.push_str(&format!(" LIMIT ?{limit_param_index}"));
        let mut statement = connection.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(lower)];
        if let Some(upper) = upper {
            params.push(Box::new(upper));
        }
        if let Some(agent_id) = agent_id {
            params.push(Box::new(agent_id.to_owned()));
        }
        params.push(Box::new(limit));
        let events = statement
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect();
        events
    }

    pub fn page_after(
        &self,
        agent_id: Option<&str>,
        after_event_seq: u64,
        limit: usize,
    ) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql = String::from("SELECT data_json FROM audit_events WHERE event_seq > ?1");
        if agent_id.is_some() {
            sql.push_str(" AND agent_id = ?2");
        }
        let limit_param_index = if agent_id.is_some() { 3 } else { 2 };
        sql.push_str(&format!(
            " ORDER BY event_seq ASC, created_at ASC LIMIT ?{limit_param_index}"
        ));
        let mut statement = connection.prepare(&sql)?;
        if let Some(agent_id) = agent_id {
            let after_event_seq = i64::try_from(after_event_seq)
                .context("audit event cursor exceeds SQLite integer range")?;
            let rows = statement.query_map(params![after_event_seq, agent_id, limit], |row| {
                row.get::<_, String>(0)
            })?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect()
        } else {
            let after_event_seq = i64::try_from(after_event_seq)
                .context("audit event cursor exceeds SQLite integer range")?;
            let rows = statement.query_map(params![after_event_seq, limit], |row| {
                row.get::<_, String>(0)
            })?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect()
        }
    }
}

fn upsert_work_item_delegation_tx(
    tx: &Transaction<'_>,
    record: &WorkItemDelegationRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let state = enum_string(&record.state)?;
    tx.execute(
        "INSERT INTO work_item_delegations (
            delegation_id, parent_agent_id, parent_work_item_id, child_agent_id,
            child_work_item_id, state, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(delegation_id) DO UPDATE SET
            parent_agent_id = excluded.parent_agent_id,
            parent_work_item_id = excluded.parent_work_item_id,
            child_agent_id = excluded.child_agent_id,
            child_work_item_id = excluded.child_work_item_id,
            state = excluded.state,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= work_item_delegations.updated_at",
        params![
            record.delegation_id,
            record.parent_agent_id,
            record.parent_work_item_id,
            record.child_agent_id,
            record.child_work_item_id,
            state,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_work_item_continuation_tx(
    tx: &Transaction<'_>,
    record: &WorkItemContinuationFrame,
) -> Result<bool> {
    let payload_json = serde_json::to_string(record)?;
    let existing = tx
        .query_row(
            "SELECT payload_json FROM work_item_continuations WHERE continuation_id = ?1",
            [&record.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing.as_deref() == Some(payload_json.as_str()) {
        return Ok(false);
    }
    let return_policy = enum_string(&record.return_policy)?;
    let state = enum_string(&record.state)?;
    let changed = tx.execute(
        "INSERT INTO work_item_continuations (
            continuation_id, agent_id, suspended_work_item_id, active_work_item_id,
            return_policy, state, created_at, updated_at, resolved_at, cancelled_at,
            resolution_reason, last_turn_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(continuation_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            suspended_work_item_id = excluded.suspended_work_item_id,
            active_work_item_id = excluded.active_work_item_id,
            return_policy = excluded.return_policy,
            state = excluded.state,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            resolved_at = excluded.resolved_at,
            cancelled_at = excluded.cancelled_at,
            resolution_reason = excluded.resolution_reason,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= work_item_continuations.updated_at",
        params![
            record.id,
            record.agent_id,
            record.suspended_work_item_id,
            record.active_work_item_id,
            return_policy,
            state,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.resolved_at.map(timestamp),
            record.cancelled_at.map(timestamp),
            record.resolution_reason.as_deref(),
            record.turn_id.as_deref(),
            payload_json,
        ],
    )?;
    Ok(changed != 0)
}

fn upsert_context_episode_tx(tx: &Transaction<'_>, record: &ContextEpisodeRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let boundary_reason = enum_string(&record.boundary_reason)?;
    tx.execute(
        "INSERT INTO context_episode_anchors (
            episode_id, agent_id, workspace_id, work_item_id, boundary_reason,
            start_turn_index, end_turn_index, started_at, ended_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(episode_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            workspace_id = excluded.workspace_id,
            work_item_id = excluded.work_item_id,
            boundary_reason = excluded.boundary_reason,
            start_turn_index = excluded.start_turn_index,
            end_turn_index = excluded.end_turn_index,
            started_at = excluded.started_at,
            ended_at = excluded.ended_at,
            payload_json = excluded.payload_json
         WHERE excluded.ended_at >= context_episode_anchors.ended_at",
        params![
            record.id,
            record.agent_id,
            record.workspace_id,
            record.current_work_item_id,
            boundary_reason,
            record.start_turn_index as i64,
            record.end_turn_index as i64,
            timestamp(record.created_at),
            timestamp(record.finalized_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_external_trigger_tx(tx: &Transaction<'_>, record: &ExternalTriggerRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let revoked_at = record.revoked_at.map(timestamp);
    let last_delivered_at = record.last_delivered_at.map(timestamp);
    tx.execute(
        "INSERT INTO external_triggers (
            external_trigger_id, target_agent_id, trigger_url,
            token_hash, status, created_at, revoked_at, last_delivered_at,
           delivery_count, payload_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(external_trigger_id) DO UPDATE SET
            target_agent_id = excluded.target_agent_id,
            trigger_url = excluded.trigger_url,
            token_hash = excluded.token_hash,
            status = excluded.status,
            created_at = excluded.created_at,
            revoked_at = excluded.revoked_at,
            last_delivered_at = excluded.last_delivered_at,
            delivery_count = excluded.delivery_count,
            payload_json = excluded.payload_json
         WHERE excluded.delivery_count > external_triggers.delivery_count
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') > COALESCE(external_triggers.last_delivered_at, '')
            )
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') = COALESCE(external_triggers.last_delivered_at, '')
                AND COALESCE(excluded.revoked_at, '') > COALESCE(external_triggers.revoked_at, '')
            )
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') = COALESCE(external_triggers.last_delivered_at, '')
                AND COALESCE(excluded.revoked_at, '') = COALESCE(external_triggers.revoked_at, '')
                AND excluded.created_at >= external_triggers.created_at
            )",
        params![
            record.external_trigger_id,
            record.target_agent_id,
           record.token,
            record.token_hash,
            status,
            timestamp(record.created_at),
            revoked_at,
            last_delivered_at,
            record.delivery_count as i64,
            payload_json,
        ],
    )?;
    Ok(())
}

fn active_default_for_agent_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<Option<ExternalTriggerRecord>> {
    tx.query_row(
        "SELECT payload_json
         FROM external_triggers
         WHERE target_agent_id = ?1 AND status = 'active'
         ORDER BY created_at DESC, external_trigger_id ASC
         LIMIT 1",
        [agent_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| decode_external_trigger_payload(&payload))
    .transpose()
}

fn default_external_trigger_record(
    agent_id: &str,
    delivery_mode: CallbackDeliveryMode,
    now: DateTime<Utc>,
    external_trigger_id: String,
    token: String,
) -> ExternalTriggerRecord {
    ExternalTriggerRecord {
        external_trigger_id,
        target_agent_id: agent_id.to_string(),
        scope: ExternalTriggerScope::Agent,
        delivery_mode,
        token: Some(token.clone()),
        token_hash: crate::callbacks::hash_callback_token(&token),
        status: ExternalTriggerStatus::Active,
        created_at: now,
        revoked_at: None,
        last_delivered_at: None,
        delivery_count: 0,
    }
}

fn insert_operator_notification_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    record: &OperatorNotificationRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT OR IGNORE INTO operator_notifications (
            notification_id, agent_id, created_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            record.notification_id,
            agent_id,
            timestamp(record.created_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_operator_transport_binding_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    record: &OperatorTransportBinding,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    tx.execute(
        "INSERT INTO operator_transport_bindings (
            binding_id, target_agent_id, status, created_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(binding_id) DO UPDATE SET
            target_agent_id = excluded.target_agent_id,
            status = excluded.status,
            created_at = excluded.created_at,
            payload_json = excluded.payload_json",
        params![
            record.binding_id,
            agent_id,
            status,
            timestamp(record.created_at),
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_operator_delivery_record_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    record: &OperatorDeliveryRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT INTO operator_delivery_records (
            delivery_intent_id, agent_id, created_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(delivery_intent_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            created_at = excluded.created_at,
            payload_json = excluded.payload_json",
        params![
            record.delivery_intent_id,
            agent_id,
            timestamp(record.created_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_new_work_item_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
) -> Result<bool> {
    if record.revision != 1 {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_initial_revision",
            Some(1),
            Some(record.revision),
            false,
        )
        .into());
    }
    let actual_revision = tx
        .query_row(
            "SELECT revision FROM work_items WHERE work_item_id = ?1",
            [&record.id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|revision| u64::try_from(revision).context("stored work item revision is negative"))
        .transpose()?;
    if let Some(actual_revision) = actual_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "record_exists",
            None,
            Some(actual_revision),
            false,
        )
        .into());
    }
    import_work_item_tx(tx, record)?;
    Ok(true)
}

pub(crate) fn update_expected_work_item_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
    expected_revision: u64,
) -> Result<bool> {
    let next_revision = expected_revision.checked_add(1).ok_or_else(|| {
        RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_revision_transition",
            Some(expected_revision),
            Some(record.revision),
            false,
        )
    })?;
    if record.revision != next_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_revision_transition",
            Some(expected_revision),
            Some(record.revision),
            false,
        )
        .into());
    }

    let existing = tx
        .query_row(
            "SELECT revision, payload_json FROM work_items WHERE work_item_id = ?1",
            [&record.id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((actual_revision, existing_payload)) = existing else {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "not_found",
            Some(expected_revision),
            None,
            false,
        )
        .into());
    };
    let actual_revision =
        u64::try_from(actual_revision).context("stored work item revision is negative")?;
    let payload_json = serde_json::to_string(record)?;
    if actual_revision == next_revision {
        if existing_payload == payload_json {
            return Ok(false);
        }
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "same_revision_payload_conflict",
            Some(expected_revision),
            Some(actual_revision),
            false,
        )
        .into());
    }
    if actual_revision != expected_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "revision_conflict",
            Some(expected_revision),
            Some(actual_revision),
            true,
        )
        .into());
    }

    update_work_item_row_tx(tx, record, &payload_json, expected_revision)?;
    Ok(true)
}

fn update_work_item_row_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
    payload_json: &str,
    expected_revision: u64,
) -> Result<()> {
    let state = enum_string(&record.state)?;
    let plan_status = enum_string(&record.plan_status)?;
    let completed_at =
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at));
    let plan_artifact_path = record
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.path.display().to_string());
    let changed = tx.execute(
        "UPDATE work_items SET
            agent_id = ?1,
            state = ?2,
            objective = ?3,
            plan_status = ?4,
            revision = ?5,
            created_at = ?6,
            updated_at = ?7,
            completed_at = ?8,
            plan_artifact_path = ?9,
            last_turn_id = ?10,
            payload_json = ?11,
            blocked_by = ?12,
            recheck_at = ?13,
            recheck_consumed_at = ?14
         WHERE work_item_id = ?15 AND revision = ?16",
        params![
            record.agent_id,
            state,
            record.objective,
            plan_status,
            record.revision as i64,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            plan_artifact_path,
            record.turn_id,
            payload_json,
            record.blocked_by,
            record.recheck_at.map(timestamp),
            record.recheck_consumed_at.map(timestamp),
            record.id,
            expected_revision as i64,
        ],
    )?;
    if changed != 1 {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "revision_conflict",
            Some(expected_revision),
            None,
            true,
        )
        .into());
    }
    Ok(())
}

fn import_work_item_tx(tx: &Transaction<'_>, record: &WorkItemRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let state = enum_string(&record.state)?;
    let plan_status = enum_string(&record.plan_status)?;
    let completed_at =
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at));
    let plan_artifact_path = record
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.path.display().to_string());
    let blocked_by = record.blocked_by.clone();
    let recheck_at = record.recheck_at.map(|t| timestamp(t));
    let recheck_consumed_at = record.recheck_consumed_at.map(|t| timestamp(t));
    tx.execute(
        "INSERT INTO work_items (
            work_item_id, agent_id, state, objective, plan_status,
            revision, current_focus, created_at, updated_at, completed_at,
            plan_artifact_path, last_turn_id, payload_json,
            blocked_by, recheck_at, recheck_consumed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(work_item_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            state = excluded.state,
            objective = excluded.objective,
            plan_status = excluded.plan_status,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            plan_artifact_path = excluded.plan_artifact_path,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json,
            blocked_by = excluded.blocked_by,
            recheck_at = excluded.recheck_at,
            recheck_consumed_at = excluded.recheck_consumed_at
         WHERE excluded.revision >= work_items.revision",
        params![
            record.id,
            record.agent_id,
            state,
            record.objective,
            plan_status,
            record.revision as i64,
            0_i64,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            plan_artifact_path,
            record.turn_id,
            payload_json,
            blocked_by,
            recheck_at,
            recheck_consumed_at,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_task_tx(tx: &Transaction<'_>, record: &TaskRecord) -> Result<bool> {
    let existing = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&record.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_task_payload(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        match task_transition(existing, record)? {
            StateTransitionOutcome::Applied => {}
            StateTransitionOutcome::Idempotent => return Ok(false),
        }
    }

    let kind = record.kind.as_str();
    let status = enum_string(&record.status)?;
    let status_phase = i64::from(task_status_phase(&record.status));
    let child_agent_id = task_detail_string(&record.detail, "child_agent_id");
    let parent_agent_id = child_agent_id.as_ref().map(|_| record.agent_id.clone());
    let input_target = task_detail_string(&record.detail, "input_target");
    let wait_policy = enum_string(&record.wait_policy())?;
    let output_path = task_detail_string(&record.detail, "output_path");
    let result_summary = task_detail_string(&record.detail, "output_summary")
        .map(|summary| truncate_task_payload_string(&summary));
    let exit_status = task_detail_i64(&record.detail, "exit_status");
    let terminal_reentry = i64::from(record.terminal_reentry());
    let completed_at =
        is_terminal_task_status(&record.status).then(|| timestamp(record.updated_at));
    let payload_json = serde_json::to_string(&slim_task_record_for_payload(record))?;
    tx.execute(
        "INSERT INTO tasks (
            task_id, owner_agent_id, parent_agent_id, child_agent_id, kind, status,
            summary, input_target, wait_policy, output_path, result_summary,
            exit_status, terminal_reentry, revision, created_at, updated_at,
            completed_at, last_message_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(task_id) DO UPDATE SET
            owner_agent_id = excluded.owner_agent_id,
            parent_agent_id = excluded.parent_agent_id,
            child_agent_id = excluded.child_agent_id,
            kind = excluded.kind,
            status = excluded.status,
            summary = excluded.summary,
            input_target = excluded.input_target,
            wait_policy = excluded.wait_policy,
            output_path = excluded.output_path,
            result_summary = excluded.result_summary,
            exit_status = excluded.exit_status,
            terminal_reentry = excluded.terminal_reentry,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            last_message_id = excluded.last_message_id,
            payload_json = excluded.payload_json
         WHERE excluded.revision > tasks.revision
            OR (excluded.revision = tasks.revision AND ?20 >= CASE tasks.status
                WHEN 'queued' THEN 0
                WHEN 'running' THEN 1
                WHEN 'cancelling' THEN 2
                ELSE 3
            END)",
        params![
            record.id,
            record.agent_id,
            parent_agent_id,
            child_agent_id,
            kind,
            status,
            record.summary,
            input_target,
            wait_policy,
            output_path,
            result_summary,
            exit_status,
            terminal_reentry,
            task_revision(record),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            record.parent_message_id,
            payload_json,
            status_phase,
        ],
    )?;
    Ok(true)
}

pub(crate) fn task_status_phase(status: &TaskStatus) -> u8 {
    match status {
        TaskStatus::Queued => 0,
        TaskStatus::Running => 1,
        TaskStatus::Cancelling => 2,
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Interrupted => 3,
    }
}

pub(crate) fn upsert_wait_condition_tx(
    tx: &Transaction<'_>,
    record: &WaitConditionRecord,
) -> Result<bool> {
    let existing = tx
        .query_row(
            "SELECT payload_json FROM wait_conditions WHERE wait_condition_id = ?1",
            [&record.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_wait_condition_payload(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        match wait_condition_transition(existing, record)? {
            StateTransitionOutcome::Applied => {}
            StateTransitionOutcome::Idempotent => return Ok(false),
        }
    }

    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let kind = enum_string(&record.kind)?;
    let wake_sources_json = serde_json::to_string(&record.wake_sources)?;
    let continuation_json = record
        .continuation
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    tx.execute(
        "INSERT INTO wait_conditions (
            wait_condition_id, agent_id, work_item_id, status, kind, source,
            subject_ref, waiting_for, created_at, updated_at, expires_at,
            resolved_at, cancelled_at, last_turn_id,
            wake_sources_json, continuation_json, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(wait_condition_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            work_item_id = excluded.work_item_id,
            status = excluded.status,
            kind = excluded.kind,
            source = excluded.source,
            subject_ref = excluded.subject_ref,
            waiting_for = excluded.waiting_for,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            expires_at = excluded.expires_at,
            resolved_at = excluded.resolved_at,
            cancelled_at = excluded.cancelled_at,
            last_turn_id = excluded.last_turn_id,
            wake_sources_json = excluded.wake_sources_json,
            continuation_json = excluded.continuation_json,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= wait_conditions.updated_at",
        params![
            record.id,
            record.agent_id,
            record.work_item_id,
            status,
            kind,
            record.source,
            record.subject_ref,
            record.waiting_for,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.expires_at.map(timestamp),
            record.resolved_at.map(timestamp),
            record.cancelled_at.map(timestamp),
            record.turn_id,
            wake_sources_json,
            continuation_json,
            payload_json,
        ],
    )?;
    Ok(true)
}

pub(crate) fn upsert_queue_entry_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
) -> Result<bool> {
    let existing = tx
        .query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
            [&record.message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_queue_entry_payload(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        match queue_entry_transition(existing, record)? {
            StateTransitionOutcome::Applied => {}
            StateTransitionOutcome::Idempotent => return Ok(false),
        }
    }

    let payload_json = serde_json::to_string(record)?;
    let priority = enum_string(&record.priority)?;
    let status = enum_string(&record.status)?;
    tx.execute(
        "INSERT INTO queue_entries (
            message_id, agent_id, priority, status, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(message_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            priority = excluded.priority,
            status = excluded.status,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= queue_entries.updated_at",
        params![
            record.message_id,
            record.agent_id,
            priority,
            status,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(true)
}

pub(crate) fn compare_and_set_queue_entry_tx(
    tx: &Transaction<'_>,
    expected: &QueueEntryRecord,
    record: &QueueEntryRecord,
) -> Result<bool> {
    if expected.message_id != record.message_id || expected.agent_id != record.agent_id {
        return Err(anyhow!(
            "queue compare-and-set identity must remain unchanged"
        ));
    }
    queue_entry_transition(expected, record)?;

    let expected_payload_json = serde_json::to_string(expected)?;
    let payload_json = serde_json::to_string(record)?;
    let priority = enum_string(&record.priority)?;
    let status = enum_string(&record.status)?;
    let changed = tx.execute(
        "UPDATE queue_entries
         SET priority = ?3,
             status = ?4,
             created_at = ?5,
             updated_at = ?6,
             payload_json = ?7
         WHERE message_id = ?1
           AND agent_id = ?2
           AND payload_json = ?8",
        params![
            record.message_id,
            record.agent_id,
            priority,
            status,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
            expected_payload_json,
        ],
    )?;
    Ok(changed == 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateTransitionOutcome {
    Applied,
    Idempotent,
}

pub(crate) fn task_transition(
    existing: &TaskRecord,
    incoming: &TaskRecord,
) -> Result<StateTransitionOutcome> {
    if existing == incoming {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    if is_terminal_task_status(&existing.status) {
        if is_terminal_task_status(&incoming.status) && task_terminal_payload_eq(existing, incoming)
        {
            return Ok(StateTransitionOutcome::Idempotent);
        }
        return Err(RuntimeStateTransitionConflict::new(
            "task",
            &existing.id,
            enum_string(&existing.status)?,
            enum_string(&incoming.status)?,
        )
        .into());
    }
    if task_revision(incoming) < task_revision(existing)
        || (task_revision(incoming) == task_revision(existing)
            && task_status_phase(&incoming.status) < task_status_phase(&existing.status))
    {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    Ok(StateTransitionOutcome::Applied)
}

pub(crate) fn queue_entry_transition(
    existing: &QueueEntryRecord,
    incoming: &QueueEntryRecord,
) -> Result<StateTransitionOutcome> {
    if existing == incoming {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    if is_terminal_queue_entry_status(&existing.status) {
        if is_terminal_queue_entry_status(&incoming.status)
            && queue_entry_terminal_payload_eq(existing, incoming)
        {
            return Ok(StateTransitionOutcome::Idempotent);
        }
        return Err(RuntimeStateTransitionConflict::new(
            "queue entry",
            &existing.message_id,
            enum_string(&existing.status)?,
            enum_string(&incoming.status)?,
        )
        .into());
    }
    if incoming.updated_at < existing.updated_at {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    Ok(StateTransitionOutcome::Applied)
}

pub(crate) fn wait_condition_transition(
    existing: &WaitConditionRecord,
    incoming: &WaitConditionRecord,
) -> Result<StateTransitionOutcome> {
    if existing == incoming {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    if is_terminal_wait_condition_status(&existing.status) {
        if is_terminal_wait_condition_status(&incoming.status)
            && wait_condition_terminal_payload_eq(existing, incoming)
        {
            return Ok(StateTransitionOutcome::Idempotent);
        }
        return Err(RuntimeStateTransitionConflict::new(
            "wait condition",
            &existing.id,
            enum_string(&existing.status)?,
            enum_string(&incoming.status)?,
        )
        .into());
    }
    if incoming.updated_at < existing.updated_at {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    Ok(StateTransitionOutcome::Applied)
}

fn is_terminal_queue_entry_status(status: &QueueEntryStatus) -> bool {
    matches!(
        status,
        QueueEntryStatus::Processed | QueueEntryStatus::Aborted | QueueEntryStatus::Dropped
    )
}

fn is_terminal_wait_condition_status(status: &WaitConditionStatus) -> bool {
    matches!(
        status,
        WaitConditionStatus::Resolved
            | WaitConditionStatus::Cancelled
            | WaitConditionStatus::Expired
    )
}

fn queue_entry_terminal_payload_eq(
    existing: &QueueEntryRecord,
    incoming: &QueueEntryRecord,
) -> bool {
    let mut existing = existing.clone();
    existing.updated_at = incoming.updated_at;
    existing == *incoming
}

fn wait_condition_terminal_payload_eq(
    existing: &WaitConditionRecord,
    incoming: &WaitConditionRecord,
) -> bool {
    let mut existing = existing.clone();
    existing.updated_at = incoming.updated_at;
    existing == *incoming
}

fn task_terminal_payload_eq(existing: &TaskRecord, incoming: &TaskRecord) -> bool {
    let existing = canonical_task_terminal_payload(existing);
    let incoming = canonical_task_terminal_payload(incoming);
    existing.id == incoming.id
        && existing.agent_id == incoming.agent_id
        && existing.kind == incoming.kind
        && existing.status == incoming.status
        && existing.parent_message_id == incoming.parent_message_id
        && existing.work_item_id == incoming.work_item_id
        && existing.summary == incoming.summary
        && existing.detail == incoming.detail
        && existing.recovery == incoming.recovery
}

fn canonical_task_terminal_payload(record: &TaskRecord) -> TaskRecord {
    let mut canonical = record.clone();
    canonical.detail = canonical
        .detail
        .as_ref()
        .map(canonical_task_terminal_detail);
    canonical
}

fn canonical_task_terminal_detail(value: &serde_json::Value) -> serde_json::Value {
    // Terminal identity ignores derived observation fields even when storage retains them.
    match value {
        serde_json::Value::Object(map) => {
            let mut canonical = serde_json::Map::new();
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "initial_output"
                        | "output_summary"
                        | "output_preview"
                        | "terminal_snapshot_ready"
                ) {
                    continue;
                }
                canonical.insert(key.clone(), canonical_task_terminal_detail(value));
            }
            serde_json::Value::Object(canonical)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_task_terminal_detail).collect())
        }
        _ => value.clone(),
    }
}

pub(crate) fn try_claim_queued_message_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
) -> Result<bool> {
    try_transition_claimable_message_tx(tx, record, QueueEntryStatus::Dequeued, true)
}

pub(crate) fn queue_entry_is_claimable_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
    include_interrupted: bool,
) -> Result<bool> {
    let status = tx
        .query_row(
            "SELECT status
             FROM queue_entries
             WHERE message_id = ?1 AND agent_id = ?2",
            params![record.message_id, record.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(status.is_some_and(|status| {
        status == enum_string(&QueueEntryStatus::Queued).expect("queue status serializes")
            || (include_interrupted
                && status
                    == enum_string(&QueueEntryStatus::Interrupted)
                        .expect("queue status serializes"))
    }))
}

pub(crate) fn try_interject_queued_message_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
) -> Result<bool> {
    try_transition_claimable_message_tx(tx, record, QueueEntryStatus::Interjected, false)
}

fn try_transition_claimable_message_tx(
    tx: &Transaction<'_>,
    record: &QueueEntryRecord,
    target_status: QueueEntryStatus,
    include_interrupted: bool,
) -> Result<bool> {
    let queued_status = enum_string(&QueueEntryStatus::Queued)?;
    let secondary_status = enum_string(if include_interrupted {
        &QueueEntryStatus::Interrupted
    } else {
        &QueueEntryStatus::Queued
    })?;
    let mut claimed = record.clone();
    claimed.status = target_status;
    let payload_json = serde_json::to_string(&claimed)?;
    let priority = enum_string(&claimed.priority)?;
    let status = enum_string(&claimed.status)?;
    let changed = tx.execute(
        "UPDATE queue_entries
         SET priority = ?3,
             status = ?4,
             created_at = ?5,
             updated_at = ?6,
             payload_json = ?7
         WHERE message_id = ?1
           AND agent_id = ?2
           AND status IN (?8, ?9)",
        params![
            claimed.message_id,
            claimed.agent_id,
            priority,
            status,
            timestamp(claimed.created_at),
            timestamp(claimed.updated_at),
            payload_json,
            queued_status,
            secondary_status,
        ],
    )?;
    Ok(changed == 1)
}

fn upsert_timer_tx(tx: &Transaction<'_>, record: &TimerRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let updated_at = timer_updated_at(record);
    tx.execute(
        "INSERT INTO timers (
            timer_id, agent_id, status, summary, created_at, duration_ms,
            interval_ms, repeat, next_fire_at, last_fired_at, fire_count,
            updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(timer_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            status = excluded.status,
            summary = excluded.summary,
            created_at = excluded.created_at,
            duration_ms = excluded.duration_ms,
            interval_ms = excluded.interval_ms,
            repeat = excluded.repeat,
            next_fire_at = excluded.next_fire_at,
            last_fired_at = excluded.last_fired_at,
            fire_count = excluded.fire_count,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.fire_count > timers.fire_count
            OR (
                excluded.fire_count = timers.fire_count
                AND (
                    CASE excluded.status
                        WHEN 'active' THEN 0
                        WHEN 'cancelled' THEN 1
                        WHEN 'completed' THEN 2
                        ELSE 0
                    END
                    > CASE timers.status
                        WHEN 'active' THEN 0
                        WHEN 'cancelled' THEN 1
                        WHEN 'completed' THEN 2
                        ELSE 0
                    END
                    OR (
                        excluded.status = timers.status
                        AND excluded.updated_at >= timers.updated_at
                    )
                )
            )",
        params![
            record.id,
            record.agent_id,
            status,
            record.summary,
            timestamp(record.created_at),
            record.duration_ms as i64,
            record.interval_ms.map(|value| value as i64),
            i64::from(record.repeat),
            record.next_fire_at.map(timestamp),
            record.last_fired_at.map(timestamp),
            record.fire_count as i64,
            timestamp(updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_turn_record_tx(tx: &Transaction<'_>, record: &TurnRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let terminal_kind = record
        .terminal
        .as_ref()
        .map(|terminal| enum_string(&terminal.kind))
        .transpose()?;
    let completed_at = record
        .terminal
        .as_ref()
        .map(|terminal| timestamp(terminal.completed_at));
    tx.execute(
        "INSERT INTO turn_records (
            turn_id, turn_index, agent_id, run_id, current_work_item_id,
            trigger_message_id, terminal_kind, created_at, completed_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(turn_id) DO UPDATE SET
            turn_index = excluded.turn_index,
            agent_id = excluded.agent_id,
            run_id = excluded.run_id,
            current_work_item_id = excluded.current_work_item_id,
            trigger_message_id = excluded.trigger_message_id,
            terminal_kind = excluded.terminal_kind,
            created_at = excluded.created_at,
            completed_at = excluded.completed_at,
            payload_json = excluded.payload_json
         WHERE COALESCE(excluded.completed_at, excluded.created_at) >= COALESCE(turn_records.completed_at, turn_records.created_at)",
        params![
            record.turn_id,
            record.turn_index as i64,
            record.agent_id,
            record.run_id,
            record.current_work_item_id,
            record
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref()),
            terminal_kind,
            timestamp(record.created_at),
            completed_at,
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn derive_turn_records_from_legacy_evidence(
    messages: Vec<serde_json::Value>,
    tool_executions: Vec<ToolExecutionRecord>,
    briefs: Vec<BriefRecord>,
    delivery_summaries: Vec<DeliverySummaryRecord>,
    wait_conditions: Vec<WaitConditionRecord>,
) -> Result<Vec<TurnRecord>> {
    let mut records = BTreeMap::<String, TurnRecord>::new();
    for raw_message in messages {
        if let Some(message) = normalize_legacy_message_value(raw_message)? {
            let turn_key = turn_key_from_message(&message);
            let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
                TurnRecord::new(&message.agent_id, &turn_key.turn_id, turn_key.turn_index)
            });
            reinforce_turn_index(record, &turn_key);
            record.created_at = record.created_at.min(message.created_at);
            record.input_message_ids.push(message.id.clone());
            if record.trigger.is_none() {
                record.trigger = Some(crate::types::TurnTriggerSummary::from_message(&message));
            }
            if record.current_work_item_id.is_none() {
                record.current_work_item_id = message.work_item_id.clone();
            }
        }
    }
    for tool in tool_executions {
        let Some(turn_key) = turn_key_from_optional(tool.turn_id.as_deref(), tool.turn_index)
        else {
            continue;
        };
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&tool.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(tool.created_at);
        record.tool_execution_ids.push(tool.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = tool.work_item_id.clone();
        }
    }
    for brief in briefs {
        let Some(turn_key) = turn_key_from_optional(
            brief.turn_id.as_deref(),
            brief.turn_index.unwrap_or_default(),
        ) else {
            continue;
        };
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&brief.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(brief.created_at);
        record.produced_brief_ids.push(brief.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = brief.work_item_id.clone();
        }
    }
    for summary in delivery_summaries {
        let Some(turn_key) = turn_key_from_optional(
            summary.turn_id.as_deref(),
            summary.source_turn_index.unwrap_or_default(),
        ) else {
            continue;
        };
        let record = records.entry(turn_key.turn_id.clone()).or_insert_with(|| {
            TurnRecord::new(&summary.agent_id, &turn_key.turn_id, turn_key.turn_index)
        });
        reinforce_turn_index(record, &turn_key);
        record.created_at = record.created_at.min(summary.created_at);
        record.delivery_summary_ids.push(summary.id.clone());
        record
            .completed_work_item_ids
            .push(summary.work_item_id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = Some(summary.work_item_id.clone());
        }
    }
    for condition in wait_conditions {
        let Some(turn_id) = condition
            .turn_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let record = records
            .entry(turn_id.trim().to_string())
            .or_insert_with(|| TurnRecord::new(&condition.agent_id, turn_id.trim(), 0));
        record.created_at = record.created_at.min(condition.created_at);
        record.waiting_condition_ids.push(condition.id.clone());
        if record.current_work_item_id.is_none() {
            record.current_work_item_id = condition.work_item_id.clone();
        }
    }
    for record in records.values_mut() {
        record.input_message_ids.sort();
        record.input_message_ids.dedup();
        record.tool_execution_ids.sort();
        record.tool_execution_ids.dedup();
        record.produced_brief_ids.sort();
        record.produced_brief_ids.dedup();
        record.delivery_summary_ids.sort();
        record.delivery_summary_ids.dedup();
        record.completed_work_item_ids.sort();
        record.completed_work_item_ids.dedup();
        record.waiting_condition_ids.sort();
        record.waiting_condition_ids.dedup();
    }
    Ok(records.into_values().collect())
}

pub(crate) struct DerivedTurnKey {
    turn_id: String,
    turn_index: u64,
}

pub(crate) fn reinforce_turn_index(record: &mut TurnRecord, turn_key: &DerivedTurnKey) {
    if record.turn_index == 0 && turn_key.turn_index != 0 {
        record.turn_index = turn_key.turn_index;
    }
}

pub(crate) fn turn_key_from_message(message: &MessageEnvelope) -> DerivedTurnKey {
    if let Some(turn_id) = message
        .turn_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return DerivedTurnKey {
            turn_id: turn_id.trim().to_string(),
            turn_index: 0,
        };
    }
    let turn_index = message.message_seq.unwrap_or_default();
    DerivedTurnKey {
        turn_id: format!("legacy-turn-{turn_index}"),
        turn_index,
    }
}

pub(crate) fn turn_key_from_optional(
    turn_id: Option<&str>,
    turn_index: u64,
) -> Option<DerivedTurnKey> {
    if let Some(turn_id) = turn_id.filter(|value| !value.trim().is_empty()) {
        return Some(DerivedTurnKey {
            turn_id: turn_id.trim().to_string(),
            turn_index,
        });
    }
    (turn_index != 0).then(|| DerivedTurnKey {
        turn_id: format!("legacy-turn-{turn_index}"),
        turn_index,
    })
}

pub(crate) fn newer_work_item_record(
    candidate: &WorkItemRecord,
    existing: &WorkItemRecord,
) -> bool {
    candidate
        .revision
        .cmp(&existing.revision)
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

pub(crate) fn reduce_timer_records(records: Vec<TimerRecord>) -> BTreeMap<String, TimerRecord> {
    let mut latest = BTreeMap::<String, TimerRecord>::new();
    for record in records {
        if latest
            .get(&record.id)
            .is_none_or(|existing| newer_timer_record(&record, existing))
        {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

pub(crate) fn newer_timer_record(candidate: &TimerRecord, existing: &TimerRecord) -> bool {
    candidate
        .fire_count
        .cmp(&existing.fire_count)
        .then_with(|| {
            timer_status_rank(&candidate.status).cmp(&timer_status_rank(&existing.status))
        })
        .then_with(|| timer_updated_at(candidate).cmp(&timer_updated_at(existing)))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| candidate.id.cmp(&existing.id))
        .is_gt()
}

pub(crate) fn timer_status_rank(status: &TimerStatus) -> u8 {
    match status {
        TimerStatus::Active => 0,
        TimerStatus::Cancelled => 1,
        TimerStatus::Completed => 2,
    }
}

pub(crate) fn timer_updated_at(record: &TimerRecord) -> DateTime<Utc> {
    record
        .last_fired_at
        .or(record.next_fire_at)
        .unwrap_or(record.created_at)
}

pub(crate) fn reduce_external_trigger_records(
    records: Vec<ExternalTriggerRecord>,
) -> BTreeMap<String, ExternalTriggerRecord> {
    let mut latest_by_id = BTreeMap::<String, ExternalTriggerRecord>::new();
    for record in records {
        if latest_by_id
            .get(&record.external_trigger_id)
            .is_none_or(|existing| newer_external_trigger_record(&record, existing))
        {
            latest_by_id.insert(
                record.external_trigger_id.clone(),
                normalize_external_trigger_record(record),
            );
        }
    }

    let mut active_by_agent = BTreeMap::<String, String>::new();
    for record in latest_by_id.values() {
        if record.status != ExternalTriggerStatus::Active {
            continue;
        }
        let replace = active_by_agent
            .get(&record.target_agent_id)
            .and_then(|id| latest_by_id.get(id))
            .is_none_or(|existing| newer_external_trigger_record(record, existing));
        if replace {
            active_by_agent.insert(
                record.target_agent_id.clone(),
                record.external_trigger_id.clone(),
            );
        }
    }

    for record in latest_by_id.values_mut() {
        if record.status == ExternalTriggerStatus::Active
            && active_by_agent.get(&record.target_agent_id) != Some(&record.external_trigger_id)
        {
            record.status = ExternalTriggerStatus::Revoked;
            record.revoked_at.get_or_insert(record.created_at);
        }
    }
    latest_by_id
}

pub(crate) fn normalize_external_trigger_record(
    mut record: ExternalTriggerRecord,
) -> ExternalTriggerRecord {
    record.scope = ExternalTriggerScope::Agent;
    record.delivery_mode = CallbackDeliveryMode::WakeHint;
    record
}

pub(crate) fn newer_external_trigger_record(
    candidate: &ExternalTriggerRecord,
    existing: &ExternalTriggerRecord,
) -> bool {
    candidate
        .delivery_count
        .cmp(&existing.delivery_count)
        .then_with(|| candidate.last_delivered_at.cmp(&existing.last_delivered_at))
        .then_with(|| candidate.revoked_at.cmp(&existing.revoked_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| {
            candidate
                .external_trigger_id
                .cmp(&existing.external_trigger_id)
        })
        .is_gt()
}

pub(crate) fn reduce_task_records(records: Vec<TaskRecord>) -> BTreeMap<String, TaskRecord> {
    let mut latest = BTreeMap::<String, TaskRecord>::new();
    for record in records {
        if let Some(previous) = latest.get(&record.id) {
            let mut merged = record.clone();
            if merged.summary.is_none() {
                merged.summary = previous.summary.clone();
            }
            if merged.detail.is_none() {
                merged.detail = previous.detail.clone();
            }
            if merged.recovery.is_none() {
                merged.recovery = previous.recovery.clone();
            }
            if newer_task_record(&merged, previous) {
                latest.insert(record.id.clone(), merged);
            }
        } else {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_work_item_delegation_records(
    records: Vec<WorkItemDelegationRecord>,
) -> BTreeMap<String, WorkItemDelegationRecord> {
    let mut latest = BTreeMap::<String, WorkItemDelegationRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.delegation_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.delegation_id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_context_episode_records(
    records: Vec<ContextEpisodeRecord>,
) -> BTreeMap<String, ContextEpisodeRecord> {
    let mut latest = BTreeMap::<String, ContextEpisodeRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.id)
            .is_none_or(|existing| record.finalized_at >= existing.finalized_at);
        if should_replace {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_workspace_entry_records(
    records: Vec<WorkspaceEntry>,
) -> BTreeMap<String, WorkspaceEntry> {
    let mut latest = BTreeMap::<String, WorkspaceEntry>::new();
    for record in records {
        let should_replace = latest
            .get(&record.workspace_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.workspace_id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_workspace_occupancy_records(
    records: Vec<WorkspaceOccupancyRecord>,
) -> BTreeMap<String, WorkspaceOccupancyRecord> {
    let mut latest = BTreeMap::<String, WorkspaceOccupancyRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.occupancy_id)
            .is_none_or(|existing| record.released_at >= existing.released_at);
        if should_replace {
            latest.insert(record.occupancy_id.clone(), record);
        }
    }
    latest
}

pub(crate) fn reduce_agent_identity_records(
    records: Vec<AgentIdentityRecord>,
) -> BTreeMap<String, AgentIdentityRecord> {
    let mut latest = BTreeMap::<String, AgentIdentityRecord>::new();
    for record in records {
        let should_replace = latest
            .get(&record.agent_id)
            .is_none_or(|existing| record.updated_at >= existing.updated_at);
        if should_replace {
            latest.insert(record.agent_id.clone(), record);
        }
    }
    latest
}

pub(crate) fn slim_task_record_for_payload(record: &TaskRecord) -> TaskRecord {
    let mut slim = record.clone();
    slim.detail = slim.detail.as_ref().map(slim_task_detail_value);
    slim
}

pub(crate) fn slim_task_detail_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut slim = serde_json::Map::new();
            for (key, value) in map {
                if key == "initial_output" {
                    continue;
                }
                slim.insert(key.clone(), slim_task_detail_value(value));
            }
            serde_json::Value::Object(slim)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .take(TASK_PAYLOAD_ARRAY_LIMIT)
                .map(slim_task_detail_value)
                .collect(),
        ),
        serde_json::Value::String(value) => {
            serde_json::Value::String(truncate_task_payload_string(value))
        }
        _ => value.clone(),
    }
}

pub(crate) fn truncate_task_payload_string(value: &str) -> String {
    value.chars().take(TASK_PAYLOAD_STRING_LIMIT).collect()
}

pub(crate) fn newer_task_record(candidate: &TaskRecord, existing: &TaskRecord) -> bool {
    task_revision(candidate)
        .cmp(&task_revision(existing))
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

pub(crate) fn task_revision(record: &TaskRecord) -> i64 {
    record.updated_at.timestamp_millis()
}

pub(crate) fn decode_agent_state_payload(payload: &str) -> Result<AgentState> {
    serde_json::from_str(payload).context("decoding agent state payload from runtime db")
}

pub(crate) fn decode_workspace_entry_payload(payload: &str) -> Result<WorkspaceEntry> {
    serde_json::from_str(payload).context("decoding workspace entry payload from runtime db")
}

pub(crate) fn decode_workspace_occupancy_payload(
    payload: &str,
) -> Result<WorkspaceOccupancyRecord> {
    serde_json::from_str(payload).context("decoding workspace occupancy payload from runtime db")
}

pub(crate) fn decode_execution_root_entry_payload(payload: &str) -> Result<ExecutionRootEntry> {
    serde_json::from_str(payload).context("decoding execution root entry payload from runtime db")
}

pub(crate) fn decode_agent_identity_payload(payload: &str) -> Result<AgentIdentityRecord> {
    serde_json::from_str(payload).context("decoding agent identity payload from runtime db")
}

pub(crate) fn decode_work_item_payload(payload: &str) -> Result<WorkItemRecord> {
    serde_json::from_str(payload).context("decoding work item payload from runtime db")
}

pub(crate) fn decode_work_item_delegation_payload(
    payload: &str,
) -> Result<WorkItemDelegationRecord> {
    serde_json::from_str(payload).context("decoding work item delegation payload from runtime db")
}

pub(crate) fn decode_work_item_continuation_payload(
    payload: &str,
) -> Result<WorkItemContinuationFrame> {
    serde_json::from_str(payload).context("decoding work item continuation payload from runtime db")
}

pub(crate) fn decode_context_episode_payload(payload: &str) -> Result<ContextEpisodeRecord> {
    serde_json::from_str(payload).context("decoding context episode payload from runtime db")
}

pub(crate) fn decode_external_trigger_payload(payload: &str) -> Result<ExternalTriggerRecord> {
    serde_json::from_str(payload).context("decoding external trigger payload from runtime db")
}

pub(crate) fn decode_operator_notification_payload(
    payload: &str,
) -> Result<OperatorNotificationRecord> {
    serde_json::from_str(payload).context("decoding operator notification payload from runtime db")
}

pub(crate) fn decode_operator_transport_binding_payload(
    payload: &str,
) -> Result<OperatorTransportBinding> {
    serde_json::from_str(payload)
        .context("decoding operator transport binding payload from runtime db")
}

pub(crate) fn decode_operator_delivery_record_payload(
    payload: &str,
) -> Result<OperatorDeliveryRecord> {
    serde_json::from_str(payload)
        .context("decoding operator delivery record payload from runtime db")
}

pub(crate) fn decode_task_payload(payload: &str) -> Result<TaskRecord> {
    serde_json::from_str(payload).context("decoding task payload from runtime db")
}

pub(crate) fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("parsing timestamp: {value}"))
}

pub(crate) fn parse_optional_timestamp(value: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    value.map(parse_timestamp).transpose()
}

/// Convert an [`anyhow::Error`] from row decoding into a [`rusqlite::Error`]
/// that preserves the original message. Previously these errors were mapped to
/// [`rusqlite::Error::InvalidQuery`] whose Display text ("Query is not
/// read-only") is completely unrelated to the actual failure (e.g. a malformed
/// timestamp or invalid enum value), making diagnosis nearly impossible.
fn wait_condition_decode_error(e: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("{e:#}").into(),
    )
}

pub(crate) fn decode_wait_condition_row(row: &rusqlite::Row<'_>) -> Result<WaitConditionRecord> {
    let id: String = row.get(0)?;
    let agent_id: String = row.get(1)?;
    let work_item_id: Option<String> = row.get(2)?;
    let status_str: String = row.get(3)?;
    let kind_str: String = row.get(4)?;
    let source: Option<String> = row.get(5)?;
    let subject_ref: Option<String> = row.get(6)?;
    let waiting_for: String = row.get(7)?;
    let created_at_str: String = row.get(8)?;
    let updated_at_str: String = row.get(9)?;
    let expires_at_str: Option<String> = row.get(10)?;
    let resolved_at_str: Option<String> = row.get(11)?;
    let cancelled_at_str: Option<String> = row.get(12)?;
    let turn_id: Option<String> = row.get(13)?;
    let wake_sources_json: String = row.get(14)?;
    let continuation_json: Option<String> = row.get(15)?;
    let status: WaitConditionStatus = serde_json::from_value(serde_json::Value::String(status_str))
        .context("parsing wait condition status")?;
    let kind: WaitConditionKind = serde_json::from_value(serde_json::Value::String(kind_str))
        .context("parsing wait condition kind")?;
    let wake_sources: Vec<WakeSource> =
        serde_json::from_str(&wake_sources_json).context("parsing wake_sources_json")?;
    let continuation: Option<serde_json::Value> = continuation_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("parsing continuation_json")?;
    Ok(WaitConditionRecord {
        id,
        agent_id,
        work_item_id,
        status,
        kind,
        source,
        subject_ref,
        waiting_for,
        wake_sources,
        continuation,
        created_at: parse_timestamp(&created_at_str)?,
        updated_at: parse_timestamp(&updated_at_str)?,
        expires_at: parse_optional_timestamp(expires_at_str.as_deref())?,
        resolved_at: parse_optional_timestamp(resolved_at_str.as_deref())?,
        cancelled_at: parse_optional_timestamp(cancelled_at_str.as_deref())?,
        turn_id,
    })
}

pub(crate) fn decode_queue_entry_payload(payload: &str) -> Result<QueueEntryRecord> {
    serde_json::from_str(payload).context("decoding queue entry payload from runtime db")
}

pub(crate) fn decode_wait_condition_payload(payload: &str) -> Result<WaitConditionRecord> {
    serde_json::from_str(payload).context("decoding wait condition payload from runtime db")
}

pub(crate) fn decode_timer_payload(payload: &str) -> Result<TimerRecord> {
    serde_json::from_str(payload).context("decoding timer payload from runtime db")
}

pub(crate) fn decode_turn_record_payload(payload: &str) -> Result<TurnRecord> {
    serde_json::from_str(payload).context("decoding turn record payload from runtime db")
}

pub(crate) fn decode_message_payload(payload: &str) -> Result<MessageEnvelope> {
    serde_json::from_str(payload).context("decoding message payload from runtime db")
}

pub(crate) fn decode_transcript_entry_payload(payload: &str) -> Result<TranscriptEntry> {
    serde_json::from_str(payload).context("decoding transcript entry payload from runtime db")
}

pub(crate) fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("expected enum to serialize as string"))
}

pub(crate) fn task_detail_string(detail: &Option<serde_json::Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

pub(crate) fn task_detail_i64(detail: &Option<serde_json::Value>, key: &str) -> Option<i64> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_i64())
}

pub(crate) fn is_active_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    )
}

pub(crate) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

pub(crate) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
