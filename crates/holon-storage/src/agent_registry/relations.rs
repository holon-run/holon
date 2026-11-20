//! Canonical Agent relations persisted inside the caller's transaction.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use super::AgentRegistryMaterial;

use holon_domain::{
    AgentCanonicalDurability, AgentCanonicalProjectionIssue, AgentCanonicalProjectionSources,
    AgentCanonicalRecordSet, AgentCanonicalRelationAxis, AgentCanonicalRelationsProjection,
    AgentCanonicalResolution, AgentCanonicalValueSource, AgentCapabilityFamily,
    AgentCapabilityPolicyRecord, AgentCapabilityPolicyRule, AgentDurabilityRecord,
    AgentIdentityLifecycle, AgentIdentityRecord, AgentKind, AgentLifecycleAttachment,
    AgentLifecycleAttachmentRecord, AgentLifecycleFenceState, AgentLineageCreationCause,
    AgentLineageRecord, AgentMessagePolicyRecord, AgentMessagePolicyRule,
    AgentMessagePrincipalKind, AgentOwnership, AgentPolicyEffect, AgentProfilePreset,
    AgentRegistryStatus, AgentRelationBackfillOutcome, AgentRelationBackfillReport,
    AgentSupervisionRecord, AgentSupervisionState, TaskKind, TaskRecord,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySupervisionTaskEvidence {
    pub task_id: String,
    pub owner_agent_id: String,
    pub child_agent_id: Option<String>,
    pub is_child_agent_task: bool,
    pub delegated_from_work_item_id: Option<String>,
}

pub struct AgentRelations {
    pub material: AgentRegistryMaterial,
}

impl AgentRelations {
    pub fn backfill_agent_relations_tx(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
        apply: bool,
        run_id: Option<&str>,
    ) -> Result<(
        Vec<AgentCanonicalRelationAxis>,
        Vec<AgentCanonicalProjectionIssue>,
    )> {
        if let Some(run_id) = run_id.filter(|_| apply) {
            let checkpoint = tx
                .query_row(
                    "SELECT migrated_axes_json, issues_json
                     FROM agent_relation_backfill_outcomes
                     WHERE run_id = ?1 AND agent_id = ?2",
                    params![run_id, agent_id],
                    |row| Ok((self.relation_text(row, 0)?, self.relation_text(row, 1)?)),
                )
                .optional()?;
            if let Some((migrated_axes, issues)) = checkpoint {
                (self.material.admit_json_payload)(&migrated_axes)?;
                (self.material.admit_json_payload)(&issues)?;
                return Ok((
                    serde_json::from_str(&migrated_axes)
                        .context("decoding relation backfill checkpoint axes")?,
                    serde_json::from_str(&issues)
                        .context("decoding relation backfill checkpoint issues")?,
                ));
            }
        }
        let payload = tx.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [agent_id],
            |row| self.relation_text(row, 0),
        )?;
        (self.material.admit_json_payload)(&payload)?;
        let identity: AgentIdentityRecord = serde_json::from_str(&payload)
            .context("decoding agent identity for relation backfill")?;
        let canonical = AgentCanonicalRecordSet {
            lineage: self.latest_lineage(tx, agent_id)?,
            supervision: self.latest_supervision(tx, agent_id)?,
            durability: self.latest_durability(tx, agent_id)?,
            lifecycle_attachment: self.latest_lifecycle_attachment(tx, agent_id)?,
            capability_policy: self.latest_capability_policy(tx, agent_id)?,
            message_policy: self.latest_message_policy(tx, agent_id)?,
        };
        let task = identity
            .delegated_from_task_id
            .as_deref()
            .map(|task_id| self.legacy_task_evidence(tx, task_id))
            .transpose()?
            .flatten();
        let projection =
            self.project_agent_canonical_relations(&identity, canonical, task.as_ref())?;
        let (records, migrated_axes, issues) = self.legacy_axes_without_issues(projection)?;
        if apply {
            self.upsert_canonical_record_set_tx(tx, &records)?;
            let outcome = if issues.is_empty() {
                if migrated_axes.is_empty() {
                    AgentRelationBackfillOutcome::Unchanged
                } else {
                    AgentRelationBackfillOutcome::Applied
                }
            } else {
                AgentRelationBackfillOutcome::Diagnostic
            };
            tx.execute(
                "INSERT INTO agent_relation_backfill_outcomes (
                   run_id, agent_id, identity_revision, outcome, migrated_axes_json,
                   issues_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(run_id, agent_id) DO UPDATE SET
                   identity_revision = excluded.identity_revision,
                   outcome = excluded.outcome,
                   migrated_axes_json = excluded.migrated_axes_json,
                   issues_json = excluded.issues_json,
                   updated_at = excluded.updated_at",
                params![
                    run_id.context("apply backfill requires a run id")?,
                    agent_id,
                    sqlite_revision(identity.revision)?,
                    self.enum_string(&outcome)?,
                    self.relation_json(&migrated_axes)?,
                    self.relation_json(&issues)?,
                    self.relation_timestamp(Utc::now())?,
                ],
            )?;
        }
        Ok((migrated_axes, issues))
    }

    #[doc(hidden)]
    pub fn legacy_axes_without_issues(
        &self,
        mut projection: AgentCanonicalRelationsProjection,
    ) -> Result<(
        AgentCanonicalRecordSet,
        Vec<AgentCanonicalRelationAxis>,
        Vec<AgentCanonicalProjectionIssue>,
    )> {
        let can_migrate = |axis| !projection.issues.iter().any(|issue| issue.axis == axis);
        let mut records = AgentCanonicalRecordSet::default();
        let mut axes = Vec::new();
        if projection.sources.lineage == Some(AgentCanonicalValueSource::Legacy)
            && can_migrate(AgentCanonicalRelationAxis::Lineage)
        {
            self.material.reserve_slot(&mut axes)?;
            records.lineage = projection.lineage.take();
            axes.push(AgentCanonicalRelationAxis::Lineage);
        }
        if projection.sources.supervision == Some(AgentCanonicalValueSource::Legacy)
            && can_migrate(AgentCanonicalRelationAxis::Supervision)
        {
            self.material.reserve_slot(&mut axes)?;
            records.supervision = projection.supervision.take();
            axes.push(AgentCanonicalRelationAxis::Supervision);
        }
        if projection.sources.durability == Some(AgentCanonicalValueSource::Legacy)
            && can_migrate(AgentCanonicalRelationAxis::Durability)
        {
            self.material.reserve_slot(&mut axes)?;
            records.durability = projection.durability.take();
            axes.push(AgentCanonicalRelationAxis::Durability);
        }
        if projection.sources.lifecycle_attachment == Some(AgentCanonicalValueSource::Legacy)
            && can_migrate(AgentCanonicalRelationAxis::LifecycleAttachment)
        {
            self.material.reserve_slot(&mut axes)?;
            records.lifecycle_attachment = projection.lifecycle_attachment.take();
            axes.push(AgentCanonicalRelationAxis::LifecycleAttachment);
        }
        if projection.sources.capability_policy == Some(AgentCanonicalValueSource::Legacy)
            && can_migrate(AgentCanonicalRelationAxis::CapabilityPolicy)
        {
            self.material.reserve_slot(&mut axes)?;
            records.capability_policy = projection.capability_policy.take();
            axes.push(AgentCanonicalRelationAxis::CapabilityPolicy);
        }
        if projection.sources.message_policy == Some(AgentCanonicalValueSource::Legacy)
            && can_migrate(AgentCanonicalRelationAxis::MessagePolicy)
            && can_migrate(AgentCanonicalRelationAxis::Supervision)
        {
            self.material.reserve_slot(&mut axes)?;
            records.message_policy = projection.message_policy.take();
            axes.push(AgentCanonicalRelationAxis::MessagePolicy);
        }
        Ok((records, axes, projection.issues))
    }

    pub fn upsert_canonical_record_set_tx(
        &self,
        tx: &Transaction<'_>,
        records: &AgentCanonicalRecordSet,
    ) -> Result<()> {
        if let Some(record) = records.lineage.as_ref() {
            self.upsert_lineage_tx(tx, record)?;
        }
        if let Some(record) = records.supervision.as_ref() {
            self.upsert_supervision_tx(tx, record)?;
        }
        if let Some(record) = records.durability.as_ref() {
            self.upsert_durability(tx, record)?;
        }
        if let Some(record) = records.lifecycle_attachment.as_ref() {
            self.upsert_lifecycle_attachment(tx, record)?;
        }
        if let Some(record) = records.capability_policy.as_ref() {
            self.upsert_capability_policy_tx(tx, record)?;
        }
        if let Some(record) = records.message_policy.as_ref() {
            self.upsert_message_policy_tx(tx, record)?;
        }
        Ok(())
    }

    pub fn transition_supervision_state_tx(
        &self,
        tx: &Transaction<'_>,
        child_agent_id: &str,
        state: AgentSupervisionState,
        revision: u64,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let Some(mut supervision) = self.latest_supervision(tx, child_agent_id)? else {
            return Ok(());
        };
        if supervision.state == state && supervision.revision >= revision {
            return Ok(());
        }
        supervision.state = state;
        supervision.revision = std::cmp::max(revision, supervision.revision.saturating_add(1));
        supervision.updated_at = updated_at;
        self.upsert_supervision_tx(tx, &supervision)
    }

    pub fn canonical_relations_from_connection(
        &self,
        connection: &rusqlite::Connection,
        agent_id: &str,
    ) -> Result<Option<AgentCanonicalRelationsProjection>> {
        let Some(identity) = self.projection_identity(connection, agent_id)? else {
            return Ok(None);
        };

        let records = AgentCanonicalRecordSet {
            lineage: self.latest_lineage(connection, agent_id)?,
            supervision: self.latest_supervision(connection, agent_id)?,
            durability: self.latest_durability(connection, agent_id)?,
            lifecycle_attachment: self.latest_lifecycle_attachment(connection, agent_id)?,
            capability_policy: self.latest_capability_policy(connection, agent_id)?,
            message_policy: self.latest_message_policy(connection, agent_id)?,
        };
        let task = identity
            .delegated_from_task_id
            .as_deref()
            .map(|task_id| self.legacy_task_evidence(connection, task_id))
            .transpose()?
            .flatten();
        Ok(Some(self.project_agent_canonical_relations(
            &identity,
            records,
            task.as_ref(),
        )?))
    }

    #[doc(hidden)]
    pub fn projection_identity(
        &self,
        connection: &rusqlite::Connection,
        agent_id: &str,
    ) -> Result<Option<AgentIdentityRecord>> {
        connection
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                [agent_id],
                |row| self.relation_text(row, 0),
            )
            .optional()?
            .map(|payload| {
                (self.material.admit_json_payload)(&payload)?;
                serde_json::from_str::<AgentIdentityRecord>(&payload)
                    .context("decoding agent identity for canonical projection")
            })
            .transpose()
    }

    pub fn upsert_lineage_tx(
        &self,
        tx: &Transaction<'_>,
        record: &AgentLineageRecord,
    ) -> Result<()> {
        let payload_json = self.relation_json(record)?;
        let affected = tx.execute(
            "INSERT INTO agent_lineages (
               child_agent_id, parent_agent_id, creation_cause, revision, created_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(child_agent_id) DO UPDATE SET
               parent_agent_id = excluded.parent_agent_id,
               creation_cause = excluded.creation_cause,
               revision = excluded.revision,
               created_at = excluded.created_at,
               payload_json = excluded.payload_json
             WHERE excluded.revision >= agent_lineages.revision
               AND excluded.parent_agent_id = agent_lineages.parent_agent_id",
            params![
                record.child_agent_id,
                record.parent_agent_id,
                self.enum_string(&record.creation_cause)?,
                sqlite_revision(record.revision)?,
                self.relation_timestamp(record.created_at)?,
                payload_json,
            ],
        )?;
        anyhow::ensure!(
            affected == 1,
            "canonical lineage conflict for child {}",
            record.child_agent_id
        );
        Ok(())
    }

    pub fn upsert_supervision_tx(
        &self,
        tx: &Transaction<'_>,
        record: &AgentSupervisionRecord,
    ) -> Result<()> {
        if record.state == AgentSupervisionState::Active {
            crate::reclamation::ensure_work_admission_tx(tx, &record.supervisor_agent_id)?;
            crate::reclamation::ensure_work_admission_tx(tx, &record.child_agent_id)?;
        }
        let payload_json = self.relation_json(record)?;
        let affected = tx.execute(
            "INSERT INTO agent_supervisions (
               supervision_id, supervisor_agent_id, child_agent_id,
               delegated_from_work_item_id, delegated_from_task_id, state, revision,
               created_at, updated_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(supervision_id) DO UPDATE SET
               supervisor_agent_id = excluded.supervisor_agent_id,
               child_agent_id = excluded.child_agent_id,
               delegated_from_work_item_id = excluded.delegated_from_work_item_id,
               delegated_from_task_id = excluded.delegated_from_task_id,
               state = excluded.state,
               revision = excluded.revision,
               created_at = excluded.created_at,
               updated_at = excluded.updated_at,
               payload_json = excluded.payload_json
             WHERE excluded.revision >= agent_supervisions.revision",
            params![
                record.supervision_id,
                record.supervisor_agent_id,
                record.child_agent_id,
                record.delegated_from_work_item_id,
                record.delegated_from_task_id,
                self.enum_string(&record.state)?,
                sqlite_revision(record.revision)?,
                self.relation_timestamp(record.created_at)?,
                self.relation_timestamp(record.updated_at)?,
                payload_json,
            ],
        )?;
        anyhow::ensure!(
            affected == 1,
            "canonical supervision conflict for {}",
            record.supervision_id
        );
        Ok(())
    }

    fn insert_versioned_record_tx<T: serde::Serialize>(
        &self,
        tx: &Transaction<'_>,
        table: &str,
        agent_id: &str,
        revision: u64,
        created_at: chrono::DateTime<chrono::Utc>,
        record: &T,
        value: Option<(&str, String)>,
    ) -> Result<()> {
        let revision = sqlite_revision(revision)?;
        let payload_json = self.relation_json(record)?;
        let (sql, value) = if let Some((column, value)) = value {
            (
                self.relation_formatted_text(format_args!(
                    "INSERT INTO {table} (agent_id, revision, {column}, created_at, payload_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(agent_id, revision) DO UPDATE SET
                       {column} = excluded.{column},
                       created_at = excluded.created_at,
                       payload_json = excluded.payload_json"
                ))?,
                Some(value),
            )
        } else {
            (
                self.relation_formatted_text(format_args!(
                    "INSERT INTO {table} (agent_id, revision, created_at, payload_json)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(agent_id, revision) DO UPDATE SET
                       created_at = excluded.created_at,
                       payload_json = excluded.payload_json"
                ))?,
                None,
            )
        };
        match value {
            Some(value) => {
                tx.execute(
                    &sql,
                    params![
                        agent_id,
                        revision,
                        value,
                        self.relation_timestamp(created_at)?,
                        payload_json
                    ],
                )?;
            }
            None => {
                tx.execute(
                    &sql,
                    params![
                        agent_id,
                        revision,
                        self.relation_timestamp(created_at)?,
                        payload_json
                    ],
                )?;
            }
        }
        Ok(())
    }

    pub fn upsert_capability_policy_tx(
        &self,
        tx: &Transaction<'_>,
        record: &AgentCapabilityPolicyRecord,
    ) -> Result<()> {
        crate::write_queue::begin_capability_policy_write();
        self.insert_versioned_record_tx(
            tx,
            "agent_capability_policy_records",
            &record.agent_id,
            record.revision,
            record.created_at,
            record,
            None,
        )?;
        tx.execute(
            "DELETE FROM agent_capability_policy_rules WHERE agent_id = ?1 AND revision = ?2",
            params![record.agent_id, sqlite_revision(record.revision)?],
        )?;
        for (rule_index, rule) in record.rules.iter().enumerate() {
            tx.execute(
                "INSERT INTO agent_capability_policy_rules (
                   agent_id, revision, rule_index, capability_family, effect
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    record.agent_id,
                    sqlite_revision(record.revision)?,
                    i64::try_from(rule_index).context("capability policy rule index overflow")?,
                    self.enum_string(&rule.family)?,
                    self.enum_string(&rule.effect)?,
                ],
            )?;
        }
        Ok(())
    }

    pub fn upsert_message_policy_tx(
        &self,
        tx: &Transaction<'_>,
        record: &AgentMessagePolicyRecord,
    ) -> Result<()> {
        let revision = sqlite_revision(record.revision)?;
        tx.execute(
            "INSERT INTO agent_message_policy_records (
               agent_id, revision, default_effect, created_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(agent_id, revision) DO UPDATE SET
               default_effect = excluded.default_effect,
               created_at = excluded.created_at,
               payload_json = excluded.payload_json",
            params![
                record.agent_id,
                revision,
                self.enum_string(&record.default_effect)?,
                self.relation_timestamp(record.created_at)?,
                self.relation_json(record)?,
            ],
        )?;
        tx.execute(
            "DELETE FROM agent_message_policy_rules WHERE agent_id = ?1 AND revision = ?2",
            params![record.agent_id, revision],
        )?;
        for (rule_index, rule) in record.rules.iter().enumerate() {
            tx.execute(
                "INSERT INTO agent_message_policy_rules (
                   agent_id, revision, rule_index, principal_kind, principal_id, route, effect
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    record.agent_id,
                    revision,
                    i64::try_from(rule_index).context("message policy rule index overflow")?,
                    self.enum_string(&rule.principal_kind)?,
                    rule.principal_id,
                    rule.route,
                    self.enum_string(&rule.effect)?,
                ],
            )?;
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn enum_string<T: serde::Serialize>(&self, value: &T) -> Result<String> {
        self.material.admit_serialized_value(value)?;
        match serde_json::to_value(value)? {
            serde_json::Value::String(value) => Ok(value),
            _ => anyhow::bail!("enum did not serialize as a string"),
        }
    }

    #[doc(hidden)]
    pub fn relation_timestamp(&self, value: chrono::DateTime<Utc>) -> Result<String> {
        self.material.timestamp(value)
    }

    #[doc(hidden)]
    pub fn relation_json(&self, value: &impl serde::Serialize) -> Result<String> {
        self.material.admit_serialized_value(value)?;
        Ok(serde_json::to_string(value)?)
    }

    #[doc(hidden)]
    pub fn relation_formatted_text(&self, arguments: std::fmt::Arguments<'_>) -> Result<String> {
        struct Length(usize);
        impl std::fmt::Write for Length {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.0 = self.0.checked_add(text.len()).ok_or(std::fmt::Error)?;
                Ok(())
            }
        }
        let mut length = Length(0);
        std::fmt::write(&mut length, arguments).context("relation text length overflow")?;
        (self.material.charge_owned_metadata)(
            length.0.saturating_add(std::mem::size_of::<String>()) as u64,
            1,
        )?;
        let mut text = String::with_capacity(length.0);
        std::fmt::write(&mut text, arguments).context("formatting relation text")?;
        Ok(text)
    }

    fn relation_owned_text(&self, text: &str) -> Result<String> {
        (self.material.charge_owned_metadata)(
            text.len().saturating_add(std::mem::size_of::<String>()) as u64,
            1,
        )?;
        Ok(text.to_owned())
    }

    fn relation_text(&self, row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<String> {
        let value = row.get_ref(column)?;
        let text = value.as_str().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(error))
        })?;
        self.relation_owned_text(text).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), error.into())
        })
    }

    fn relation_optional_text(
        &self,
        row: &rusqlite::Row<'_>,
        column: usize,
    ) -> rusqlite::Result<Option<String>> {
        if row.get_ref(column)? == rusqlite::types::ValueRef::Null {
            Ok(None)
        } else {
            self.relation_text(row, column).map(Some)
        }
    }

    #[doc(hidden)]
    pub fn latest_lineage(
        &self,
        connection: &rusqlite::Connection,
        agent_id: &str,
    ) -> Result<Option<AgentLineageRecord>> {
        connection
            .query_row(
                "SELECT parent_agent_id, creation_cause, revision, created_at
                 FROM agent_lineages WHERE child_agent_id = ?1",
                [agent_id],
                |row| {
                    Ok((
                        self.relation_text(row, 0)?,
                        self.relation_text(row, 1)?,
                        row.get::<_, i64>(2)?,
                        self.relation_text(row, 3)?,
                    ))
                },
            )
            .optional()?
            .map(|(parent_agent_id, creation_cause, revision, created_at)| {
                Ok(AgentLineageRecord {
                    child_agent_id: self.relation_owned_text(agent_id)?,
                    parent_agent_id,
                    creation_cause: decode_enum(&creation_cause, "lineage creation cause")?,
                    revision: u64::try_from(revision).context("negative lineage revision")?,
                    created_at: parse_timestamp(&created_at, "lineage")?,
                })
            })
            .transpose()
    }

    #[doc(hidden)]
    pub fn latest_supervision(
        &self,
        connection: &rusqlite::Connection,
        agent_id: &str,
    ) -> Result<Option<AgentSupervisionRecord>> {
        connection
            .query_row(
                "SELECT supervision_id, supervisor_agent_id, delegated_from_work_item_id,
                        delegated_from_task_id, state, revision, created_at, updated_at
                 FROM agent_supervisions
                 WHERE child_agent_id = ?1
                 ORDER BY revision DESC, updated_at DESC, supervision_id DESC
                 LIMIT 1",
                [agent_id],
                |row| {
                    Ok((
                        self.relation_text(row, 0)?,
                        self.relation_text(row, 1)?,
                        self.relation_optional_text(row, 2)?,
                        self.relation_optional_text(row, 3)?,
                        self.relation_text(row, 4)?,
                        row.get::<_, i64>(5)?,
                        self.relation_text(row, 6)?,
                        self.relation_text(row, 7)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(
                    supervision_id,
                    supervisor_agent_id,
                    delegated_from_work_item_id,
                    delegated_from_task_id,
                    state,
                    revision,
                    created_at,
                    updated_at,
                )| {
                    Ok(AgentSupervisionRecord {
                        supervision_id,
                        supervisor_agent_id,
                        child_agent_id: self.relation_owned_text(agent_id)?,
                        delegated_from_work_item_id,
                        delegated_from_task_id,
                        state: decode_enum(&state, "supervision state")?,
                        revision: u64::try_from(revision)
                            .context("negative supervision revision")?,
                        created_at: parse_timestamp(&created_at, "supervision created")?,
                        updated_at: parse_timestamp(&updated_at, "supervision updated")?,
                    })
                },
            )
            .transpose()
    }

    #[doc(hidden)]
    pub fn latest_durability(
        &self,
        connection: &rusqlite::Connection,
        agent_id: &str,
    ) -> Result<Option<AgentDurabilityRecord>> {
        connection
            .query_row(
                "SELECT durability, revision, created_at FROM agent_durability_records
                 WHERE agent_id = ?1 ORDER BY revision DESC LIMIT 1",
                [agent_id],
                |row| {
                    Ok((
                        self.relation_text(row, 0)?,
                        row.get::<_, i64>(1)?,
                        self.relation_text(row, 2)?,
                    ))
                },
            )
            .optional()?
            .map(|(durability, revision, created_at)| {
                Ok(AgentDurabilityRecord {
                    agent_id: self.relation_owned_text(agent_id)?,
                    durability: decode_enum(&durability, "agent durability")?,
                    revision: u64::try_from(revision).context("negative durability revision")?,
                    created_at: parse_timestamp(&created_at, "durability")?,
                })
            })
            .transpose()
    }

    #[doc(hidden)]
    pub fn latest_lifecycle_attachment(
        &self,
        connection: &rusqlite::Connection,
        agent_id: &str,
    ) -> Result<Option<AgentLifecycleAttachmentRecord>> {
        connection
            .query_row(
                "SELECT attachment, revision, created_at FROM agent_lifecycle_attachment_records
                 WHERE agent_id = ?1 ORDER BY revision DESC LIMIT 1",
                [agent_id],
                |row| {
                    Ok((
                        self.relation_text(row, 0)?,
                        row.get::<_, i64>(1)?,
                        self.relation_text(row, 2)?,
                    ))
                },
            )
            .optional()?
            .map(|(attachment, revision, created_at)| {
                Ok(AgentLifecycleAttachmentRecord {
                    agent_id: self.relation_owned_text(agent_id)?,
                    attachment: decode_enum(&attachment, "lifecycle attachment")?,
                    revision: u64::try_from(revision)
                        .context("negative lifecycle attachment revision")?,
                    created_at: parse_timestamp(&created_at, "lifecycle attachment")?,
                })
            })
            .transpose()
    }

    #[doc(hidden)]
    pub fn latest_capability_policy(
        &self,
        connection: &rusqlite::Connection,
        agent_id: &str,
    ) -> Result<Option<AgentCapabilityPolicyRecord>> {
        let header = connection
            .query_row(
                "SELECT revision, created_at FROM agent_capability_policy_records
                 WHERE agent_id = ?1 ORDER BY revision DESC LIMIT 1",
                [agent_id],
                |row| Ok((row.get::<_, i64>(0)?, self.relation_text(row, 1)?)),
            )
            .optional()?;
        let Some((revision, created_at)) = header else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT capability_family, effect FROM agent_capability_policy_rules
             WHERE agent_id = ?1 AND revision = ?2 ORDER BY rule_index",
        )?;
        let mut rules = Vec::new();
        let mut rows = statement.query(params![agent_id, revision])?;
        while let Some(row) = rows.next()? {
            let family = self.relation_text(row, 0)?;
            let effect = self.relation_text(row, 1)?;
            let rule = AgentCapabilityPolicyRule {
                family: decode_enum(&family, "capability family")?,
                effect: decode_enum(&effect, "capability policy effect")?,
            };
            (self.material.charge_owned_metadata)(
                std::mem::size_of::<AgentCapabilityPolicyRule>() as u64,
                1,
            )?;
            rules.push(rule);
        }
        Ok(Some(AgentCapabilityPolicyRecord {
            agent_id: self.relation_owned_text(agent_id)?,
            revision: u64::try_from(revision).context("negative capability policy revision")?,
            rules,
            created_at: parse_timestamp(&created_at, "capability policy")?,
        }))
    }

    #[doc(hidden)]
    pub fn latest_message_policy(
        &self,
        connection: &rusqlite::Connection,
        agent_id: &str,
    ) -> Result<Option<AgentMessagePolicyRecord>> {
        let header = connection
            .query_row(
                "SELECT revision, default_effect, created_at FROM agent_message_policy_records
                 WHERE agent_id = ?1 ORDER BY revision DESC LIMIT 1",
                [agent_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        self.relation_text(row, 1)?,
                        self.relation_text(row, 2)?,
                    ))
                },
            )
            .optional()?;
        let Some((revision, default_effect, created_at)) = header else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT principal_kind, principal_id, route, effect
             FROM agent_message_policy_rules
             WHERE agent_id = ?1 AND revision = ?2 ORDER BY rule_index",
        )?;
        let mut rules = Vec::new();
        let mut rows = statement.query(params![agent_id, revision])?;
        while let Some(row) = rows.next()? {
            let principal_kind = self.relation_text(row, 0)?;
            let principal_id = self.relation_optional_text(row, 1)?;
            let route = self.relation_optional_text(row, 2)?;
            let effect = self.relation_text(row, 3)?;
            let rule = AgentMessagePolicyRule {
                principal_kind: decode_enum(&principal_kind, "message principal kind")?,
                principal_id,
                route,
                effect: decode_enum(&effect, "message policy effect")?,
            };
            (self.material.charge_owned_metadata)(
                std::mem::size_of::<AgentMessagePolicyRule>() as u64,
                1,
            )?;
            rules.push(rule);
        }
        Ok(Some(AgentMessagePolicyRecord {
            agent_id: self.relation_owned_text(agent_id)?,
            revision: u64::try_from(revision).context("negative message policy revision")?,
            default_effect: decode_enum(&default_effect, "message policy default effect")?,
            rules,
            created_at: parse_timestamp(&created_at, "message policy")?,
        }))
    }

    #[doc(hidden)]
    pub fn legacy_task_evidence(
        &self,
        connection: &rusqlite::Connection,
        task_id: &str,
    ) -> Result<Option<LegacySupervisionTaskEvidence>> {
        connection
            .query_row(
                "SELECT owner_agent_id, child_agent_id, kind, payload_json
                 FROM tasks WHERE task_id = ?1",
                [task_id],
                |row| {
                    Ok((
                        self.relation_text(row, 0)?,
                        self.relation_optional_text(row, 1)?,
                        self.relation_text(row, 2)?,
                        self.relation_text(row, 3)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(owner_agent_id, child_agent_id, kind, payload)| -> Result<_> {
                    let kind: TaskKind = serde_json::from_value(serde_json::Value::String(kind))
                        .context("decoding legacy supervision task kind")?;
                    (self.material.admit_json_payload)(&payload)?;
                    let task: TaskRecord = serde_json::from_str(&payload)
                        .context("decoding legacy supervision task")?;
                    Ok(LegacySupervisionTaskEvidence {
                        task_id: self.relation_owned_text(task_id)?,
                        owner_agent_id,
                        is_child_agent_task: kind.is_child_agent()
                            || (kind == TaskKind::ActorInvocation
                                && child_agent_id.is_some()
                                && task
                                    .detail
                                    .as_ref()
                                    .and_then(|detail| detail.get("created_new_subagent"))
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false)),
                        child_agent_id,
                        delegated_from_work_item_id: task
                            .effective_work_item_id()
                            .map(|text| self.relation_owned_text(text))
                            .transpose()?,
                    })
                },
            )
            .transpose()
    }

    #[doc(hidden)]
    pub fn project_agent_canonical_relations(
        &self,
        identity: &AgentIdentityRecord,
        canonical: AgentCanonicalRecordSet,
        task: Option<&LegacySupervisionTaskEvidence>,
    ) -> Result<AgentCanonicalRelationsProjection> {
        let mut issues = Vec::new();
        let mut sources = AgentCanonicalProjectionSources::default();
        let canonical_lineage = canonical.lineage.is_some();
        let canonical_supervision = canonical.supervision.is_some();
        let canonical_durability = canonical.durability.is_some();
        let canonical_lifecycle_attachment = canonical.lifecycle_attachment.is_some();
        let identity_lifecycle = match identity.status {
            AgentRegistryStatus::Active => AgentIdentityLifecycle::Active,
            AgentRegistryStatus::Deleting => AgentIdentityLifecycle::Deleting,
            AgentRegistryStatus::Deleted => AgentIdentityLifecycle::Deleted,
        };
        let lifecycle_fence = match identity.status {
            AgentRegistryStatus::Active => AgentLifecycleFenceState::Open,
            AgentRegistryStatus::Deleting => AgentLifecycleFenceState::DeletionFenced,
            AgentRegistryStatus::Deleted => AgentLifecycleFenceState::Tombstoned,
        };

        let lineage = match canonical.lineage {
            Some(record) => {
                sources.lineage = Some(AgentCanonicalValueSource::Canonical);
                Some(record)
            }
            None => {
                let record = self.legacy_lineage(identity, &mut issues)?;
                if record.is_some() {
                    sources.lineage = Some(AgentCanonicalValueSource::Legacy);
                }
                record
            }
        };

        let supervision = match canonical.supervision {
            Some(record) => {
                sources.supervision = Some(AgentCanonicalValueSource::Canonical);
                Some(record)
            }
            None => {
                let record =
                    self.legacy_supervision(identity, lineage.as_ref(), task, &mut issues)?;
                if record.is_some() {
                    sources.supervision = Some(AgentCanonicalValueSource::Legacy);
                }
                record
            }
        };

        let durability = match canonical.durability {
            Some(record) => {
                sources.durability = Some(AgentCanonicalValueSource::Canonical);
                Some(record)
            }
            None => {
                let record = self.legacy_durability(identity, supervision.as_ref(), &mut issues)?;
                if record.is_some() {
                    sources.durability = Some(AgentCanonicalValueSource::Legacy);
                }
                record
            }
        };

        let lifecycle_attachment = match canonical.lifecycle_attachment {
            Some(record) => {
                sources.lifecycle_attachment = Some(AgentCanonicalValueSource::Canonical);
                Some(record)
            }
            None => {
                let record =
                    self.legacy_lifecycle_attachment(identity, supervision.as_ref(), &mut issues)?;
                if record.is_some() {
                    sources.lifecycle_attachment = Some(AgentCanonicalValueSource::Legacy);
                }
                record
            }
        };

        let capability_policy = match canonical.capability_policy {
            Some(record) => {
                sources.capability_policy = Some(AgentCanonicalValueSource::Canonical);
                Some(record)
            }
            None => {
                let record = self.legacy_capability_policy(identity, &mut issues)?;
                if record.is_some() {
                    sources.capability_policy = Some(AgentCanonicalValueSource::Legacy);
                }
                record
            }
        };

        let message_policy = match canonical.message_policy {
            Some(record) => {
                sources.message_policy = Some(AgentCanonicalValueSource::Canonical);
                Some(record)
            }
            None => {
                sources.message_policy = Some(AgentCanonicalValueSource::Legacy);
                Some(self.legacy_message_policy(identity, supervision.as_ref())?)
            }
        };

        self.validate_legacy_profile(identity, &mut issues)?;
        self.validate_canonical_legacy_drift(
            identity,
            task,
            canonical_lineage.then_some(lineage.as_ref()).flatten(),
            canonical_supervision
                .then_some(supervision.as_ref())
                .flatten(),
            canonical_durability
                .then_some(durability.as_ref())
                .flatten(),
            canonical_lifecycle_attachment
                .then_some(lifecycle_attachment.as_ref())
                .flatten(),
            &mut issues,
        )?;
        self.validate_effective_relations(
            identity,
            supervision.as_ref(),
            durability.as_ref(),
            lifecycle_attachment.as_ref(),
            &mut issues,
        )?;
        let resolution = projection_resolution(&issues);
        Ok(AgentCanonicalRelationsProjection {
            agent_id: self.relation_owned_text(&identity.agent_id)?,
            identity_lifecycle,
            lifecycle_fence,
            lineage,
            supervision,
            durability,
            lifecycle_attachment,
            capability_policy,
            message_policy,
            sources,
            resolution,
            issues,
        })
    }

    #[doc(hidden)]
    pub fn legacy_lineage(
        &self,
        identity: &AgentIdentityRecord,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
    ) -> Result<Option<AgentLineageRecord>> {
        let parent = match (
            identity.lineage_parent_agent_id.as_deref(),
            identity.parent_agent_id.as_deref(),
        ) {
            (Some(lineage_parent), Some(parent)) if lineage_parent != parent => {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::Lineage,
                    AgentCanonicalResolution::Contradictory,
                    format_args!(
                        "legacy lineage parent {lineage_parent} conflicts with parent {parent}"
                    ),
                )?;
                return Ok(None);
            }
            (Some(parent), _) | (_, Some(parent)) => Some(parent),
            (None, None) => None,
        };
        let Some(parent) = parent else {
            if identity.kind == AgentKind::Child {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::Lineage,
                    AgentCanonicalResolution::MissingEvidence,
                    format_args!("legacy child identity has no parent evidence"),
                )?;
            }
            return Ok(None);
        };
        Ok(Some(AgentLineageRecord {
            child_agent_id: self.relation_owned_text(&identity.agent_id)?,
            parent_agent_id: self.relation_owned_text(parent)?,
            creation_cause: AgentLineageCreationCause::LegacySpawn,
            revision: 0,
            created_at: identity.created_at,
        }))
    }

    #[doc(hidden)]
    pub fn legacy_supervision(
        &self,
        identity: &AgentIdentityRecord,
        lineage: Option<&AgentLineageRecord>,
        task: Option<&LegacySupervisionTaskEvidence>,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
    ) -> Result<Option<AgentSupervisionRecord>> {
        if identity.ownership() == AgentOwnership::SelfOwned {
            if identity.delegated_from_task_id.is_some() {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::Supervision,
                    AgentCanonicalResolution::Contradictory,
                    format_args!(
                        "self-owned legacy identity still carries delegated supervision task evidence"
                    ),
                )?;
            }
            return Ok(None);
        }
        let Some(supervisor_agent_id) = lineage.map(|record| record.parent_agent_id.as_str())
        else {
            self.issue(
                issues,
                AgentCanonicalRelationAxis::Supervision,
                AgentCanonicalResolution::MissingEvidence,
                format_args!("parent-supervised legacy identity has no unambiguous parent"),
            )?;
            return Ok(None);
        };
        let Some(task_id) = identity.delegated_from_task_id.as_deref() else {
            self.issue(
                issues,
                AgentCanonicalRelationAxis::Supervision,
                AgentCanonicalResolution::MissingEvidence,
                format_args!("parent-supervised legacy identity has no delegated supervision task"),
            )?;
            return Ok(None);
        };
        let Some(task) = task else {
            self.issue(
                issues,
                AgentCanonicalRelationAxis::Supervision,
                AgentCanonicalResolution::MissingEvidence,
                format_args!("delegated supervision task {task_id} is missing"),
            )?;
            return Ok(None);
        };
        if task.task_id != task_id
            || task.owner_agent_id != supervisor_agent_id
            || task.child_agent_id.as_deref() != Some(identity.agent_id.as_str())
            || !task.is_child_agent_task
        {
            self.issue(
                issues,
                AgentCanonicalRelationAxis::Supervision,
                AgentCanonicalResolution::Contradictory,
                format_args!(
                    "delegated task {task_id} does not prove the expected supervision relation"
                ),
            )?;
            return Ok(None);
        }
        (self.material.charge_owned_metadata)(
            task_id
                .len()
                .saturating_add("legacy:".len())
                .saturating_add(std::mem::size_of::<String>()) as u64,
            1,
        )?;
        Ok(Some(AgentSupervisionRecord {
            supervision_id: format!("legacy:{task_id}"),
            supervisor_agent_id: self.relation_owned_text(supervisor_agent_id)?,
            child_agent_id: self.relation_owned_text(&identity.agent_id)?,
            delegated_from_work_item_id: task
                .delegated_from_work_item_id
                .as_deref()
                .map(|text| self.relation_owned_text(text))
                .transpose()?,
            delegated_from_task_id: Some(self.relation_owned_text(task_id)?),
            state: match identity.status {
                AgentRegistryStatus::Active => AgentSupervisionState::Active,
                AgentRegistryStatus::Deleting => AgentSupervisionState::CleanupRequired,
                AgentRegistryStatus::Deleted => AgentSupervisionState::Closed,
            },
            revision: 0,
            created_at: identity.created_at,
            updated_at: identity.updated_at,
        }))
    }

    #[doc(hidden)]
    pub fn legacy_durability(
        &self,
        identity: &AgentIdentityRecord,
        supervision: Option<&AgentSupervisionRecord>,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
    ) -> Result<Option<AgentDurabilityRecord>> {
        let durability = match identity.durability {
            Some(holon_domain::AgentDurability::Persistent) => AgentCanonicalDurability::Persistent,
            Some(holon_domain::AgentDurability::Ephemeral) => AgentCanonicalDurability::Ephemeral,
            None => match identity.ownership() {
                AgentOwnership::SelfOwned => AgentCanonicalDurability::Persistent,
                AgentOwnership::ParentSupervised if supervision.is_some() => {
                    AgentCanonicalDurability::Ephemeral
                }
                AgentOwnership::ParentSupervised => {
                    self.issue(
                        issues,
                        AgentCanonicalRelationAxis::Durability,
                        AgentCanonicalResolution::MissingEvidence,
                        format_args!("parent-supervised durability cannot be resolved without supervision evidence"),
                    )?;
                    return Ok(None);
                }
            },
        };
        Ok(Some(AgentDurabilityRecord {
            agent_id: self.relation_owned_text(&identity.agent_id)?,
            durability,
            revision: 0,
            created_at: identity.created_at,
        }))
    }

    #[doc(hidden)]
    pub fn legacy_lifecycle_attachment(
        &self,
        identity: &AgentIdentityRecord,
        supervision: Option<&AgentSupervisionRecord>,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
    ) -> Result<Option<AgentLifecycleAttachmentRecord>> {
        let attachment = match identity.ownership() {
            AgentOwnership::SelfOwned => AgentLifecycleAttachment::Independent,
            AgentOwnership::ParentSupervised if supervision.is_some() => {
                AgentLifecycleAttachment::SupervisionAttached
            }
            AgentOwnership::ParentSupervised => {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::LifecycleAttachment,
                    AgentCanonicalResolution::MissingEvidence,
                    format_args!("parent-supervised lifecycle attachment cannot be resolved without supervision evidence"),
                )?;
                return Ok(None);
            }
        };
        Ok(Some(AgentLifecycleAttachmentRecord {
            agent_id: self.relation_owned_text(&identity.agent_id)?,
            attachment,
            revision: 0,
            created_at: identity.created_at,
        }))
    }

    #[doc(hidden)]
    pub fn legacy_capability_policy(
        &self,
        identity: &AgentIdentityRecord,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
    ) -> Result<Option<AgentCapabilityPolicyRecord>> {
        let preset =
            identity
                .profile_preset
                .or_else(|| match (identity.ownership(), identity.visibility) {
                    (AgentOwnership::SelfOwned, holon_domain::AgentVisibility::Public) => {
                        Some(AgentProfilePreset::PublicNamed)
                    }
                    (AgentOwnership::ParentSupervised, _) => Some(AgentProfilePreset::PrivateChild),
                    _ => None,
                });
        let Some(preset) = preset else {
            self.issue(
                issues,
                AgentCanonicalRelationAxis::CapabilityPolicy,
                AgentCanonicalResolution::MissingEvidence,
                format_args!("legacy capability package cannot be resolved"),
            )?;
            return Ok(None);
        };
        let families = [
            AgentCapabilityFamily::CoreAgent,
            AgentCapabilityFamily::LocalEnvironment,
            AgentCapabilityFamily::Web,
            AgentCapabilityFamily::AgentCreation,
            AgentCapabilityFamily::AuthorityExpanding,
            AgentCapabilityFamily::ExternalTrigger,
        ];
        (self.material.charge_owned_metadata)(
            std::mem::size_of::<AgentCapabilityPolicyRule>().saturating_mul(families.len()) as u64,
            families.len(),
        )?;
        let rules = families
            .into_iter()
            .map(|family| {
                let allowed = match preset {
                    AgentProfilePreset::PublicNamed => true,
                    AgentProfilePreset::PrivateChild => !matches!(
                        family,
                        AgentCapabilityFamily::AgentCreation
                            | AgentCapabilityFamily::AuthorityExpanding
                    ),
                };
                AgentCapabilityPolicyRule {
                    family,
                    effect: if allowed {
                        AgentPolicyEffect::Allow
                    } else {
                        AgentPolicyEffect::Deny
                    },
                }
            })
            .collect();
        Ok(Some(AgentCapabilityPolicyRecord {
            agent_id: self.relation_owned_text(&identity.agent_id)?,
            revision: 0,
            rules,
            created_at: identity.created_at,
        }))
    }

    #[doc(hidden)]
    pub fn legacy_message_policy(
        &self,
        identity: &AgentIdentityRecord,
        supervision: Option<&AgentSupervisionRecord>,
    ) -> Result<AgentMessagePolicyRecord> {
        let supervision = supervision.filter(|record| {
            matches!(
                record.state,
                AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
            )
        });
        let rule_count = 2 + usize::from(supervision.is_some());
        (self.material.charge_owned_metadata)(
            std::mem::size_of::<AgentMessagePolicyRule>().saturating_mul(rule_count) as u64,
            rule_count,
        )?;
        let mut rules = Vec::with_capacity(rule_count);
        rules.push(AgentMessagePolicyRule {
            principal_kind: AgentMessagePrincipalKind::Operator,
            principal_id: None,
            route: Some(self.relation_owned_text("operator_control")?),
            effect: AgentPolicyEffect::Allow,
        });
        rules.push(AgentMessagePolicyRule {
            principal_kind: AgentMessagePrincipalKind::RuntimeCapability,
            principal_id: Some(self.relation_owned_text("runtime:agent-invocation")?),
            route: Some(self.relation_owned_text("agent_invocation")?),
            effect: AgentPolicyEffect::Allow,
        });
        if let Some(supervision) = supervision {
            rules.push(AgentMessagePolicyRule {
                principal_kind: AgentMessagePrincipalKind::SupervisingParent,
                principal_id: Some(self.relation_owned_text(&supervision.supervisor_agent_id)?),
                route: Some(self.relation_owned_text("supervision_follow_up")?),
                effect: AgentPolicyEffect::Allow,
            });
        }
        Ok(AgentMessagePolicyRecord {
            agent_id: self.relation_owned_text(&identity.agent_id)?,
            revision: 0,
            default_effect: AgentPolicyEffect::Deny,
            rules,
            created_at: identity.created_at,
        })
    }

    fn validate_legacy_profile(
        &self,
        identity: &AgentIdentityRecord,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
    ) -> Result<()> {
        match identity.profile_preset {
            Some(AgentProfilePreset::PublicNamed)
                if identity.ownership() != AgentOwnership::SelfOwned
                    || identity.delegated_from_task_id.is_some() =>
            {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::LifecycleAttachment,
                    AgentCanonicalResolution::Contradictory,
                    format_args!("public_named legacy preset conflicts with supervision evidence"),
                )?;
            }
            Some(AgentProfilePreset::PrivateChild)
                if identity.ownership() == AgentOwnership::SelfOwned
                    && identity.parent_agent_id.is_none()
                    && identity.lineage_parent_agent_id.is_none() =>
            {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::LifecycleAttachment,
                    AgentCanonicalResolution::Ambiguous,
                    format_args!("private_child legacy preset has no lifecycle ownership evidence"),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_canonical_legacy_drift(
        &self,
        identity: &AgentIdentityRecord,
        task: Option<&LegacySupervisionTaskEvidence>,
        lineage: Option<&AgentLineageRecord>,
        supervision: Option<&AgentSupervisionRecord>,
        durability: Option<&AgentDurabilityRecord>,
        attachment: Option<&AgentLifecycleAttachmentRecord>,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
    ) -> Result<()> {
        if let Some(lineage) = lineage {
            let legacy_parent = identity
                .lineage_parent_agent_id
                .as_deref()
                .or(identity.parent_agent_id.as_deref());
            if legacy_parent.is_some_and(|parent| parent != lineage.parent_agent_id) {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::Lineage,
                    AgentCanonicalResolution::Contradictory,
                    format_args!("canonical lineage differs from legacy parent evidence"),
                )?;
            }
        }
        if let Some(supervision) = supervision {
            if identity.ownership() == AgentOwnership::SelfOwned
                || identity
                    .parent_agent_id
                    .as_deref()
                    .is_some_and(|parent| parent != supervision.supervisor_agent_id)
                || identity
                    .delegated_from_task_id
                    .as_deref()
                    .is_some_and(|task_id| {
                        supervision.delegated_from_task_id.as_deref() != Some(task_id)
                    })
                || task.is_some_and(|task| {
                    task.owner_agent_id != supervision.supervisor_agent_id
                        || task.child_agent_id.as_deref() != Some(identity.agent_id.as_str())
                })
            {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::Supervision,
                    AgentCanonicalResolution::Contradictory,
                    format_args!(
                        "canonical supervision differs from legacy ownership, parent, or task evidence"
                    ),
                )?;
            }
        }
        if let (Some(durability), Some(legacy)) = (durability, identity.durability) {
            let legacy = match legacy {
                holon_domain::AgentDurability::Persistent => AgentCanonicalDurability::Persistent,
                holon_domain::AgentDurability::Ephemeral => AgentCanonicalDurability::Ephemeral,
            };
            if durability.durability != legacy {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::Durability,
                    AgentCanonicalResolution::Contradictory,
                    format_args!("canonical durability differs from explicit legacy durability"),
                )?;
            }
        }
        if let Some(attachment) = attachment {
            let legacy = match identity.ownership() {
                AgentOwnership::SelfOwned => AgentLifecycleAttachment::Independent,
                AgentOwnership::ParentSupervised => AgentLifecycleAttachment::SupervisionAttached,
            };
            if attachment.attachment != legacy {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::LifecycleAttachment,
                    AgentCanonicalResolution::Contradictory,
                    format_args!(
                        "canonical lifecycle attachment differs from legacy ownership evidence"
                    ),
                )?;
            }
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn validate_effective_relations(
        &self,
        identity: &AgentIdentityRecord,
        supervision: Option<&AgentSupervisionRecord>,
        durability: Option<&AgentDurabilityRecord>,
        attachment: Option<&AgentLifecycleAttachmentRecord>,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
    ) -> Result<()> {
        if let Some(supervision) = supervision {
            let expected_state = match identity.status {
                AgentRegistryStatus::Active => AgentSupervisionState::Active,
                AgentRegistryStatus::Deleting => AgentSupervisionState::CleanupRequired,
                AgentRegistryStatus::Deleted => AgentSupervisionState::Closed,
            };
            // Active identities can retain unresolved cleanup responsibility after
            // their supervisor disappears. This posture grants no deletion authority.
            let retained_cleanup_debt = identity.status == AgentRegistryStatus::Active
                && supervision.state == AgentSupervisionState::CleanupRequired;
            if supervision.state != expected_state && !retained_cleanup_debt {
                self.issue(
                    issues,
                    AgentCanonicalRelationAxis::Supervision,
                    AgentCanonicalResolution::Contradictory,
                    format_args!(
                        "supervision state {:?} conflicts with identity lifecycle {:?}",
                        supervision.state, identity.status
                    ),
                )?;
            }
        }
        let supervision_active = supervision.is_some_and(|record| {
            matches!(
                record.state,
                AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
            )
        });
        match attachment.map(|record| record.attachment) {
            Some(AgentLifecycleAttachment::Independent) if supervision_active => self.issue(
                issues,
                AgentCanonicalRelationAxis::LifecycleAttachment,
                AgentCanonicalResolution::Contradictory,
                format_args!("independent lifecycle conflicts with active supervision"),
            )?,
            Some(AgentLifecycleAttachment::SupervisionAttached) if supervision.is_none() => self
                .issue(
                    issues,
                    AgentCanonicalRelationAxis::Supervision,
                    AgentCanonicalResolution::MissingEvidence,
                    format_args!("supervision-attached lifecycle has no supervision record"),
                )?,
            _ => {}
        }
        if supervision_active
            && durability
                .is_some_and(|record| record.durability == AgentCanonicalDurability::Persistent)
        {
            self.issue(
                issues,
                AgentCanonicalRelationAxis::Durability,
                AgentCanonicalResolution::Contradictory,
                format_args!(
                    "persistent durability conflicts with first-release active supervision"
                ),
            )?;
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn issue(
        &self,
        issues: &mut Vec<AgentCanonicalProjectionIssue>,
        axis: AgentCanonicalRelationAxis,
        resolution: AgentCanonicalResolution,
        detail: std::fmt::Arguments<'_>,
    ) -> Result<()> {
        struct Length(usize);
        impl std::fmt::Write for Length {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.0 = self.0.checked_add(text.len()).ok_or(std::fmt::Error)?;
                Ok(())
            }
        }
        let mut length = Length(0);
        std::fmt::write(&mut length, detail).context("relation diagnostic length overflow")?;
        (self.material.charge_owned_metadata)(
            length
                .0
                .saturating_add(std::mem::size_of::<String>())
                .saturating_add(std::mem::size_of::<AgentCanonicalProjectionIssue>())
                as u64,
            2,
        )?;
        // Reserve only the admitted slot, not Vec's amortized growth capacity.
        issues.try_reserve_exact(1)?;
        let mut text = String::with_capacity(length.0);
        std::fmt::write(&mut text, detail).context("formatting relation diagnostic")?;
        issues.push(AgentCanonicalProjectionIssue {
            axis,
            resolution,
            detail: text,
        });
        Ok(())
    }

    pub fn upsert_durability(
        &self,
        tx: &Transaction<'_>,
        record: &AgentDurabilityRecord,
    ) -> Result<()> {
        self.insert_versioned_record_tx(
            tx,
            "agent_durability_records",
            &record.agent_id,
            record.revision,
            record.created_at,
            record,
            Some(("durability", self.enum_string(&record.durability)?)),
        )
    }

    pub fn upsert_lifecycle_attachment(
        &self,
        tx: &Transaction<'_>,
        record: &AgentLifecycleAttachmentRecord,
    ) -> Result<()> {
        self.insert_versioned_record_tx(
            tx,
            "agent_lifecycle_attachment_records",
            &record.agent_id,
            record.revision,
            record.created_at,
            record,
            Some(("attachment", self.enum_string(&record.attachment)?)),
        )
    }

    pub fn prepare_capability_policy(
        &self,
        tx: &Transaction<'_>,
        identity: &AgentIdentityRecord,
    ) -> Result<Option<AgentCapabilityPolicyRecord>> {
        anyhow::ensure!(
            self.projection_identity(tx, &identity.agent_id)?.as_ref() == Some(identity),
            "capability policy identity differs from the Agent publication"
        );
        match self.latest_capability_policy(tx, &identity.agent_id)? {
            Some(policy) => Ok(Some(policy)),
            None => self.legacy_capability_policy(identity, &mut Vec::new()),
        }
    }

    pub fn lineage_children(
        &self,
        connection: &Connection,
        parent_agent_id: &str,
    ) -> Result<Vec<AgentLineageRecord>> {
        let mut records: Vec<(String, usize, AgentLineageRecord)> = Vec::new();
        let mut statement = connection.prepare(
            "SELECT child_agent_id, payload_json
         FROM agent_lineages
         WHERE parent_agent_id = ?1
         ORDER BY child_agent_id ASC",
        )?;
        let mut rows = statement.query([parent_agent_id])?;
        while let Some(row) = rows.next()? {
            let child_agent_id = lineage_sql_text(row, 0)?;
            let payload_json = lineage_sql_text(row, 1)?;
            (self.material.admit_json_payload)(payload_json)?;
            let lineage = serde_json::from_str(payload_json)
                .context("decoding canonical lineage child record")?;
            self.material.reserve_slot(&mut records)?;
            records.push((
                self.relation_owned_text(child_agent_id)?,
                records.len(),
                lineage,
            ));
        }
        let canonical_count = records.len();
        let mut identity_statement = connection.prepare(
            "SELECT payload_json
         FROM agent_identities
         ORDER BY agent_id ASC",
        )?;
        let mut identities = identity_statement.query([])?;
        while let Some(row) = identities.next()? {
            let payload_json = lineage_sql_text(row, 0)?;
            (self.material.admit_json_payload)(payload_json)?;
            let identity: AgentIdentityRecord = serde_json::from_str(payload_json)
                .context("decoding agent identity for legacy lineage children")?;
            let Some(lineage) = self.legacy_lineage(&identity, &mut Vec::new())? else {
                continue;
            };
            if lineage.parent_agent_id == parent_agent_id {
                if records[..canonical_count]
                    .binary_search_by(|(key, _, _)| key.cmp(&identity.agent_id))
                    .is_err()
                {
                    self.material.reserve_slot(&mut records)?;
                    records.push((identity.agent_id, records.len(), lineage));
                }
            }
        }
        // Input ordinal preserves first-wins without allocating stable-sort scratch.
        records.sort_unstable_by(|left, right| {
            left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1))
        });
        records.dedup_by(|left, right| left.0 == right.0);
        (self.material.charge_owned_metadata)(
            records
                .len()
                .saturating_mul(std::mem::size_of::<AgentLineageRecord>()) as u64,
            records.len(),
        )?;
        let mut children = Vec::new();
        children.try_reserve_exact(records.len())?;
        children.extend(records.into_iter().map(|(_, _, lineage)| lineage));
        children.sort_unstable_by(|left, right| left.child_agent_id.cmp(&right.child_agent_id));
        Ok(children)
    }

    pub fn begin_or_resume_backfill<D: std::fmt::Display>(
        &self,
        tx: &Transaction<'_>,
        requested_at: chrono::DateTime<Utc>,
        new_run_id: impl FnOnce() -> D,
    ) -> Result<(String, chrono::DateTime<Utc>)> {
        let existing = tx
            .query_row(
                "SELECT run_id, started_at FROM agent_relation_backfill_runs
                 WHERE status = 'running'
                 ORDER BY started_at DESC LIMIT 1",
                [],
                |row| Ok((self.relation_text(row, 0)?, self.relation_text(row, 1)?)),
            )
            .optional()?;
        if let Some((run_id, started_at)) = existing {
            return Ok((
                run_id,
                parse_timestamp(&started_at, "relation backfill run")?,
            ));
        }
        let run_id =
            self.relation_formatted_text(format_args!("agent-relations-{}", new_run_id()))?;
        tx.execute(
            "INSERT INTO agent_relation_backfill_runs (
               run_id, status, started_at, updated_at
             ) VALUES (?1, 'running', ?2, ?2)",
            params![run_id, self.relation_timestamp(requested_at)?],
        )?;
        Ok((run_id, requested_at))
    }

    pub fn finish_backfill_run(
        &self,
        tx: &Transaction<'_>,
        run_id: &str,
        report: &AgentRelationBackfillReport,
        succeeded: bool,
    ) -> Result<()> {
        let status = if succeeded { "completed" } else { "failed" };
        tx.execute(
            "UPDATE agent_relation_backfill_runs
             SET status = ?4, updated_at = ?1, completed_at = ?1,
                 report_json = ?2
             WHERE run_id = ?3",
            params![
                self.relation_timestamp(report.completed_at)?,
                self.relation_json(report)?,
                run_id,
                status,
            ],
        )?;
        Ok(())
    }

    pub fn backfill_agent_ids(&self, connection: &Connection) -> Result<Vec<String>> {
        let mut statement =
            connection.prepare("SELECT agent_id FROM agent_identities ORDER BY agent_id ASC")?;
        let mut rows = statement.query([])?;
        let mut agent_ids = Vec::new();
        while let Some(row) = rows.next()? {
            self.material.reserve_slot(&mut agent_ids)?;
            agent_ids.push(self.relation_text(row, 0)?);
        }
        Ok(agent_ids)
    }
}

pub fn independent_creation_records(
    identity: &AgentIdentityRecord,
    lineage_parent_agent_id: Option<&str>,
    durability: AgentCanonicalDurability,
) -> AgentCanonicalRecordSet {
    AgentCanonicalRecordSet {
        lineage: lineage_parent_agent_id.map(|parent_agent_id| AgentLineageRecord {
            child_agent_id: identity.agent_id.clone(),
            parent_agent_id: parent_agent_id.to_string(),
            creation_cause: AgentLineageCreationCause::CreateAgent,
            revision: identity.revision,
            created_at: identity.created_at,
        }),
        supervision: None,
        durability: Some(AgentDurabilityRecord {
            agent_id: identity.agent_id.clone(),
            durability,
            revision: identity.revision,
            created_at: identity.created_at,
        }),
        lifecycle_attachment: Some(AgentLifecycleAttachmentRecord {
            agent_id: identity.agent_id.clone(),
            attachment: AgentLifecycleAttachment::Independent,
            revision: identity.revision,
            created_at: identity.created_at,
        }),
        capability_policy: Some(capability_policy_for_preset(
            identity,
            AgentProfilePreset::PublicNamed,
        )),
        message_policy: Some(message_policy_for_supervisor(identity, None)),
    }
}

pub fn supervised_creation_records(
    identity: &AgentIdentityRecord,
    supervisor_agent_id: &str,
    delegated_from_task_id: &str,
    delegated_from_work_item_id: Option<&str>,
) -> AgentCanonicalRecordSet {
    AgentCanonicalRecordSet {
        lineage: Some(AgentLineageRecord {
            child_agent_id: identity.agent_id.clone(),
            parent_agent_id: supervisor_agent_id.to_string(),
            creation_cause: AgentLineageCreationCause::CreateAgent,
            revision: identity.revision,
            created_at: identity.created_at,
        }),
        supervision: Some(AgentSupervisionRecord {
            supervision_id: format!("supervision:{delegated_from_task_id}"),
            supervisor_agent_id: supervisor_agent_id.to_string(),
            child_agent_id: identity.agent_id.clone(),
            delegated_from_work_item_id: delegated_from_work_item_id.map(ToString::to_string),
            delegated_from_task_id: Some(delegated_from_task_id.to_string()),
            state: AgentSupervisionState::Active,
            revision: identity.revision,
            created_at: identity.created_at,
            updated_at: identity.updated_at,
        }),
        durability: Some(AgentDurabilityRecord {
            agent_id: identity.agent_id.clone(),
            durability: AgentCanonicalDurability::Ephemeral,
            revision: identity.revision,
            created_at: identity.created_at,
        }),
        lifecycle_attachment: Some(AgentLifecycleAttachmentRecord {
            agent_id: identity.agent_id.clone(),
            attachment: AgentLifecycleAttachment::SupervisionAttached,
            revision: identity.revision,
            created_at: identity.created_at,
        }),
        capability_policy: Some(capability_policy_for_preset(
            identity,
            AgentProfilePreset::PrivateChild,
        )),
        message_policy: Some(message_policy_for_supervisor(
            identity,
            Some(supervisor_agent_id),
        )),
    }
}

fn sqlite_revision(revision: u64) -> Result<i64> {
    i64::try_from(revision).context("canonical record revision exceeds SQLite range")
}

fn lineage_sql_text<'row>(
    row: &'row rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<&'row str> {
    let value = row.get_ref(column)?;
    let name = row.as_ref().column_name(column)?;
    value.as_str().map_err(|error| match error {
        rusqlite::types::FromSqlError::InvalidType => {
            rusqlite::Error::InvalidColumnType(column, name.to_owned(), value.data_type())
        }
        error => {
            rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(error))
        }
    })
}

#[doc(hidden)]
pub fn decode_enum<T: serde::de::DeserializeOwned + Copy>(value: &str, label: &str) -> Result<T> {
    T::deserialize(serde::de::value::BorrowedStrDeserializer::<serde_json::Error>::new(value))
        .with_context(|| format!("decoding {label}"))
}

fn parse_timestamp(value: &str, label: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .with_context(|| format!("decoding {label} timestamp"))
}

fn capability_policy_for_preset(
    identity: &AgentIdentityRecord,
    preset: AgentProfilePreset,
) -> AgentCapabilityPolicyRecord {
    let families = [
        AgentCapabilityFamily::CoreAgent,
        AgentCapabilityFamily::LocalEnvironment,
        AgentCapabilityFamily::Web,
        AgentCapabilityFamily::AgentCreation,
        AgentCapabilityFamily::AuthorityExpanding,
        AgentCapabilityFamily::ExternalTrigger,
    ];
    let rules = families
        .into_iter()
        .map(|family| {
            let allowed = match preset {
                AgentProfilePreset::PublicNamed => true,
                AgentProfilePreset::PrivateChild => !matches!(
                    family,
                    AgentCapabilityFamily::AgentCreation
                        | AgentCapabilityFamily::AuthorityExpanding
                ),
            };
            AgentCapabilityPolicyRule {
                family,
                effect: if allowed {
                    AgentPolicyEffect::Allow
                } else {
                    AgentPolicyEffect::Deny
                },
            }
        })
        .collect();
    AgentCapabilityPolicyRecord {
        agent_id: identity.agent_id.clone(),
        revision: identity.revision,
        rules,
        created_at: identity.created_at,
    }
}

fn message_policy_for_supervisor(
    identity: &AgentIdentityRecord,
    supervisor_agent_id: Option<&str>,
) -> AgentMessagePolicyRecord {
    let mut rules = vec![
        AgentMessagePolicyRule {
            principal_kind: AgentMessagePrincipalKind::Operator,
            principal_id: None,
            route: Some("operator_control".into()),
            effect: AgentPolicyEffect::Allow,
        },
        AgentMessagePolicyRule {
            principal_kind: AgentMessagePrincipalKind::RuntimeCapability,
            principal_id: Some("runtime:agent-invocation".into()),
            route: Some("agent_invocation".into()),
            effect: AgentPolicyEffect::Allow,
        },
    ];
    if let Some(supervisor_agent_id) = supervisor_agent_id {
        rules.push(AgentMessagePolicyRule {
            principal_kind: AgentMessagePrincipalKind::SupervisingParent,
            principal_id: Some(supervisor_agent_id.to_string()),
            route: Some("supervision_follow_up".into()),
            effect: AgentPolicyEffect::Allow,
        });
    }
    AgentMessagePolicyRecord {
        agent_id: identity.agent_id.clone(),
        revision: identity.revision,
        default_effect: AgentPolicyEffect::Deny,
        rules,
        created_at: identity.created_at,
    }
}

fn projection_resolution(issues: &[AgentCanonicalProjectionIssue]) -> AgentCanonicalResolution {
    if issues
        .iter()
        .any(|issue| issue.resolution == AgentCanonicalResolution::Contradictory)
    {
        AgentCanonicalResolution::Contradictory
    } else if issues
        .iter()
        .any(|issue| issue.resolution == AgentCanonicalResolution::Ambiguous)
    {
        AgentCanonicalResolution::Ambiguous
    } else if issues
        .iter()
        .any(|issue| issue.resolution == AgentCanonicalResolution::MissingEvidence)
    {
        AgentCanonicalResolution::MissingEvidence
    } else {
        AgentCanonicalResolution::Resolved
    }
}
