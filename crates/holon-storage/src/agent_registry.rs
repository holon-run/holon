//! SQLite identity and relation operations within a caller-owned read or write view.

use std::io::Write;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use holon_domain::{AgentIdentityRecord, AgentKind, AgentRegistryStatus};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::codec::timestamp;

pub mod list;
mod list_codecs;
pub use list_codecs::{decode_external_trigger_payload, decode_work_item_continuation_payload};
pub mod relations;

type SerializeMaterial<'a> = dyn Fn(&mut dyn Write) -> serde_json::Result<()> + 'a;

/// Accounting callbacks borrow the original input and run before its
/// allocation. The caller owns the accounting scope and any resulting guard.
#[derive(Clone, Copy)]
pub struct AgentRegistryMaterial {
    pub admit_json_payload: fn(&str) -> Result<()>,
    pub charge_owned_metadata: fn(u64, usize) -> std::io::Result<()>,
    pub admit_serialized: fn(&SerializeMaterial<'_>) -> Result<()>,
}

impl AgentRegistryMaterial {
    fn admit_serialized_value(&self, value: &impl serde::Serialize) -> Result<()> {
        (self.admit_serialized)(&|writer| serde_json::to_writer(writer, value))
    }

    /// Reserve capacity for exactly one item after charging its owned metadata.
    pub fn reserve_slot<T>(&self, values: &mut Vec<T>) -> Result<()> {
        (self.charge_owned_metadata)(std::mem::size_of::<T>() as u64, 1)?;
        values.try_reserve_exact(1)?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn timestamp(&self, value: DateTime<Utc>) -> Result<String> {
        let year = chrono::Datelike::year(&value);
        let year_bytes = if (0..=9999).contains(&year) {
            4
        } else {
            (year.unsigned_abs().ilog10() as usize + 1).max(4) + 1
        };
        // UTC RFC3339 milliseconds: year plus "-MM-DDTHH:MM:SS.sssZ".
        (self.charge_owned_metadata)((year_bytes + 20 + std::mem::size_of::<String>()) as u64, 1)?;
        Ok(timestamp(value))
    }

    /// Enforces the durable Agent identity reservation invariant inside every
    /// registry write: an id retired in this runtime event-log epoch can never
    /// regain availability. Deletion only moves the reservation to `retired`;
    /// the reservation row itself is never removed.
    pub fn reserve_agent_identity_tx(
        &self,
        tx: &Transaction<'_>,
        record: &AgentIdentityRecord,
    ) -> Result<()> {
        let retired: Option<bool> = tx
            .query_row(
                "SELECT reservation_state = 'retired'
             FROM agent_identity_reservations
             WHERE agent_id = ?1",
                [&record.agent_id],
                |row| row.get(0),
            )
            .optional()?;
        let now = Utc::now();
        if retired == Some(true) {
            if record.status != AgentRegistryStatus::Deleted {
                return Err(anyhow!(
                    "agent id {} was retired in this runtime event-log epoch; \
                 retired agent ids are never reused",
                    record.agent_id
                ));
            }
            // Re-asserting the tombstone keeps the reservation retired; the
            // registry write below remains an idempotent stale-safe no-op.
            return Ok(());
        }
        let now = self.timestamp(now)?;
        if record.status == AgentRegistryStatus::Deleted {
            tx.execute(
            "INSERT INTO agent_identity_reservations (agent_id, reservation_state, reserved_at, retired_at, source)
             VALUES (?1, 'retired', ?2, ?2, 'agent_registry')
             ON CONFLICT(agent_id) DO UPDATE SET
               reservation_state = 'retired',
               retired_at = excluded.retired_at",
            params![record.agent_id, now],
        )?;
            return Ok(());
        }
        tx.execute(
        "INSERT OR IGNORE INTO agent_identity_reservations (agent_id, reservation_state, reserved_at, retired_at, source)
         VALUES (?1, 'active', ?2, NULL, 'agent_registry')",
        params![record.agent_id, now],
    )?;
        Ok(())
    }

    /// Re-activates a retired identity reservation. This is the only sanctioned
    /// `retired -> active` transition: it runs inside the reincarnation
    /// transaction, after the latest deletion job for the agent id has fully
    /// completed, and in the same transaction as the fresh Active registry
    /// write. Every other registry write path keeps the standing invariant that
    /// a retired id never regains availability implicitly.
    pub fn release_agent_identity_reservation_tx(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
    ) -> Result<()> {
        let now = self.timestamp(Utc::now())?;
        let released = tx.execute(
            "UPDATE agent_identity_reservations
         SET reservation_state = 'active',
             reserved_at = ?2,
             retired_at = NULL,
             source = 'reincarnation'
         WHERE agent_id = ?1 AND reservation_state = 'retired'",
            params![agent_id, now],
        )?;
        anyhow::ensure!(
            released == 1,
            "agent_reincarnation_rejected: agent {agent_id} identity reservation is not retired"
        );
        Ok(())
    }

    /// Returns whether this write installs a different incarnation in the transaction.
    pub fn upsert_agent_identity_tx(
        &self,
        tx: &Transaction<'_>,
        record: &AgentIdentityRecord,
        name_key: fn(&str) -> Result<String>,
    ) -> Result<bool> {
        let previous_incarnation = {
            let mut statement =
                tx.prepare("SELECT payload_json FROM agent_identities WHERE agent_id = ?1")?;
            let mut rows = statement.query([&record.agent_id])?;
            if let Some(row) = rows.next()? {
                let payload = row.get_ref(0)?.as_str()?;
                (self.admit_json_payload)(payload)?;
                Some(serde_json::from_str::<AgentIdentityRecord>(payload)?.incarnation)
            } else {
                None
            }
        };
        // The reservation guard runs before the registry write so a retired id
        // can never regain availability, not even through a stale replay.
        self.reserve_agent_identity_tx(tx, record)?;
        let payload_json = serde_json::to_string(record)?;
        let name_key = record.name.as_deref().map(name_key).transpose()?;
        let kind = match record.kind {
            AgentKind::Default => "default",
            AgentKind::Named => "named",
            AgentKind::Child => "child",
        };
        let visibility = record.visibility.label();
        let ownership = record.ownership.map(|value| value.label());
        let profile_preset = record.profile_preset.map(|value| value.label());
        let status = match record.status {
            AgentRegistryStatus::Active => "active",
            AgentRegistryStatus::Deleting => "deleting",
            AgentRegistryStatus::Deleted => "deleted",
        };
        tx.execute(
            "INSERT INTO agent_identities (
            agent_id, name, name_key, kind, visibility, ownership, profile_preset, status,
            parent_agent_id, lineage_parent_agent_id, delegated_from_task_id,
            created_at, updated_at, archived_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(agent_id) DO UPDATE SET
            name = excluded.name,
            name_key = excluded.name_key,
            kind = excluded.kind,
            visibility = excluded.visibility,
            ownership = excluded.ownership,
            profile_preset = excluded.profile_preset,
            status = excluded.status,
            parent_agent_id = excluded.parent_agent_id,
            lineage_parent_agent_id = excluded.lineage_parent_agent_id,
            delegated_from_task_id = excluded.delegated_from_task_id,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            archived_at = excluded.archived_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= agent_identities.updated_at",
            params![
                record.agent_id,
                record.name,
                name_key,
                kind,
                visibility,
                ownership,
                profile_preset,
                status,
                record.parent_agent_id,
                record.lineage_parent_agent_id,
                record.delegated_from_task_id,
                self.timestamp(record.created_at)?,
                self.timestamp(record.updated_at)?,
                record
                    .deleted_at
                    .map(|value| self.timestamp(value))
                    .transpose()?,
                payload_json,
            ],
        )?;
        if previous_incarnation != Some(record.incarnation) {
            return {
                let mut statement =
                    tx.prepare("SELECT payload_json FROM agent_identities WHERE agent_id = ?1")?;
                let mut rows = statement.query([&record.agent_id])?;
                let row = rows.next()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                let payload = row.get_ref(0)?.as_str()?;
                (self.admit_json_payload)(payload)?;
                Ok(serde_json::from_str::<AgentIdentityRecord>(payload)? == *record)
            };
        }
        Ok(false)
    }
}

pub fn decode_agent_identity_payload(payload: &str) -> Result<AgentIdentityRecord> {
    serde_json::from_str(payload).context("decoding agent identity payload from runtime db")
}

pub fn agent_identities_on(connection: &Connection) -> Result<Vec<AgentIdentityRecord>> {
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

pub fn agent_identity_on(
    connection: &Connection,
    agent_id: &str,
) -> Result<Option<AgentIdentityRecord>> {
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

#[cfg(test)]
mod tests;
