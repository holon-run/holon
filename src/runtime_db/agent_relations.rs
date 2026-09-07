use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::runtime_db::types::AgentCanonicalRelationRepository;
use crate::types::{
    AgentCanonicalDurability, AgentCanonicalProjectionIssue, AgentCanonicalProjectionSources,
    AgentCanonicalRecordSet, AgentCanonicalRelationAxis, AgentCanonicalRelationsProjection,
    AgentCanonicalResolution, AgentCanonicalValueSource, AgentCapabilityFamily,
    AgentCapabilityPolicyRecord, AgentCapabilityPolicyRule, AgentDurabilityRecord,
    AgentIdentityLifecycle, AgentIdentityRecord, AgentKind, AgentLifecycleAttachment,
    AgentLifecycleAttachmentRecord, AgentLifecycleFenceState, AgentLineageCreationCause,
    AgentLineageRecord, AgentMessagePolicyRecord, AgentMessagePolicyRule,
    AgentMessagePrincipalKind, AgentOwnership, AgentPolicyEffect, AgentProfilePreset,
    AgentRegistryStatus, AgentSupervisionRecord, AgentSupervisionState, TaskKind, TaskRecord,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacySupervisionTaskEvidence {
    pub task_id: String,
    pub owner_agent_id: String,
    pub child_agent_id: Option<String>,
    pub is_child_agent_task: bool,
    pub delegated_from_work_item_id: Option<String>,
}

impl AgentCanonicalRelationRepository<'_> {
    pub fn upsert_lineage(&self, record: &AgentLineageRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_lineage_tx(tx, record))
    }

    pub fn upsert_supervision(&self, record: &AgentSupervisionRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_supervision_tx(tx, record))
    }

    pub fn upsert_durability(&self, record: &AgentDurabilityRecord) -> Result<()> {
        self.db.transaction(|tx| {
            insert_versioned_record_tx(
                tx,
                "agent_durability_records",
                &record.agent_id,
                record.revision,
                record.created_at,
                record,
                Some(("durability", enum_string(&record.durability)?)),
            )
        })
    }

    pub fn upsert_lifecycle_attachment(
        &self,
        record: &AgentLifecycleAttachmentRecord,
    ) -> Result<()> {
        self.db.transaction(|tx| {
            insert_versioned_record_tx(
                tx,
                "agent_lifecycle_attachment_records",
                &record.agent_id,
                record.revision,
                record.created_at,
                record,
                Some(("attachment", enum_string(&record.attachment)?)),
            )
        })
    }

    pub fn upsert_capability_policy(&self, record: &AgentCapabilityPolicyRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_capability_policy_tx(tx, record))
    }

    pub fn upsert_message_policy(&self, record: &AgentMessagePolicyRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_message_policy_tx(tx, record))
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentCanonicalRelationsProjection>> {
        let connection = self.db.connection()?;
        canonical_relations_from_connection(&connection, agent_id)
    }

    pub fn lineage_children(&self, parent_agent_id: &str) -> Result<Vec<AgentLineageRecord>> {
        let connection = self.db.connection()?;
        let mut records = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT child_agent_id, payload_json
             FROM agent_lineages
             WHERE parent_agent_id = ?1
             ORDER BY child_agent_id ASC",
        )?;
        let rows = statement.query_map([parent_agent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (child_agent_id, payload_json) = row?;
            records.insert(
                child_agent_id,
                serde_json::from_str(&payload_json)
                    .context("decoding canonical lineage child record")?,
            );
        }
        let mut identity_statement = connection.prepare(
            "SELECT payload_json
             FROM agent_identities
             ORDER BY agent_id ASC",
        )?;
        let identities = identity_statement.query_map([], |row| row.get::<_, String>(0))?;
        for payload_json in identities {
            let identity: AgentIdentityRecord = serde_json::from_str(&payload_json?)
                .context("decoding agent identity for legacy lineage children")?;
            let Some(lineage) = legacy_lineage(&identity, &mut Vec::new()) else {
                continue;
            };
            if lineage.parent_agent_id == parent_agent_id {
                records.entry(identity.agent_id).or_insert(lineage);
            }
        }
        let mut records = records.into_values().collect::<Vec<_>>();
        records.sort_by(|left, right| left.child_agent_id.cmp(&right.child_agent_id));
        Ok(records)
    }
}

pub(crate) fn canonical_relations_from_connection(
    connection: &rusqlite::Connection,
    agent_id: &str,
) -> Result<Option<AgentCanonicalRelationsProjection>> {
    let identity = connection
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| {
            serde_json::from_str::<AgentIdentityRecord>(&payload)
                .context("decoding agent identity for canonical projection")
        })
        .transpose()?;
    let Some(identity) = identity else {
        return Ok(None);
    };

    let records = AgentCanonicalRecordSet {
        lineage: latest_lineage(connection, agent_id)?,
        supervision: latest_supervision(connection, agent_id)?,
        durability: latest_durability(connection, agent_id)?,
        lifecycle_attachment: latest_lifecycle_attachment(connection, agent_id)?,
        capability_policy: latest_capability_policy(connection, agent_id)?,
        message_policy: latest_message_policy(connection, agent_id)?,
    };
    let task = identity
        .delegated_from_task_id
        .as_deref()
        .map(|task_id| legacy_task_evidence(connection, task_id))
        .transpose()?
        .flatten();
    Ok(Some(project_agent_canonical_relations(
        &identity,
        records,
        task.as_ref(),
    )))
}

fn upsert_lineage_tx(tx: &Transaction<'_>, record: &AgentLineageRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
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
            enum_string(&record.creation_cause)?,
            sqlite_revision(record.revision)?,
            timestamp(record.created_at),
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

fn upsert_supervision_tx(tx: &Transaction<'_>, record: &AgentSupervisionRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
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
            enum_string(&record.state)?,
            sqlite_revision(record.revision)?,
            timestamp(record.created_at),
            timestamp(record.updated_at),
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
    tx: &Transaction<'_>,
    table: &str,
    agent_id: &str,
    revision: u64,
    created_at: chrono::DateTime<chrono::Utc>,
    record: &T,
    value: Option<(&str, String)>,
) -> Result<()> {
    let revision = sqlite_revision(revision)?;
    let payload_json = serde_json::to_string(record)?;
    let (sql, value) = if let Some((column, value)) = value {
        (
            format!(
                "INSERT INTO {table} (agent_id, revision, {column}, created_at, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(agent_id, revision) DO UPDATE SET
                   {column} = excluded.{column},
                   created_at = excluded.created_at,
                   payload_json = excluded.payload_json"
            ),
            Some(value),
        )
    } else {
        (
            format!(
                "INSERT INTO {table} (agent_id, revision, created_at, payload_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(agent_id, revision) DO UPDATE SET
                   created_at = excluded.created_at,
                   payload_json = excluded.payload_json"
            ),
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
                    timestamp(created_at),
                    payload_json
                ],
            )?;
        }
        None => {
            tx.execute(
                &sql,
                params![agent_id, revision, timestamp(created_at), payload_json],
            )?;
        }
    }
    Ok(())
}

fn upsert_capability_policy_tx(
    tx: &Transaction<'_>,
    record: &AgentCapabilityPolicyRecord,
) -> Result<()> {
    insert_versioned_record_tx(
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
                enum_string(&rule.family)?,
                enum_string(&rule.effect)?,
            ],
        )?;
    }
    Ok(())
}

fn upsert_message_policy_tx(tx: &Transaction<'_>, record: &AgentMessagePolicyRecord) -> Result<()> {
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
            enum_string(&record.default_effect)?,
            timestamp(record.created_at),
            serde_json::to_string(record)?,
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
                enum_string(&rule.principal_kind)?,
                rule.principal_id,
                rule.route,
                enum_string(&rule.effect)?,
            ],
        )?;
    }
    Ok(())
}

fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        _ => anyhow::bail!("enum did not serialize as a string"),
    }
}

fn sqlite_revision(revision: u64) -> Result<i64> {
    i64::try_from(revision).context("canonical record revision exceeds SQLite range")
}

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn latest_lineage(
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
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .map(|(parent_agent_id, creation_cause, revision, created_at)| {
            Ok(AgentLineageRecord {
                child_agent_id: agent_id.to_string(),
                parent_agent_id,
                creation_cause: decode_enum(&creation_cause, "lineage creation cause")?,
                revision: u64::try_from(revision).context("negative lineage revision")?,
                created_at: parse_timestamp(&created_at, "lineage")?,
            })
        })
        .transpose()
}

fn latest_supervision(
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
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
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
                    child_agent_id: agent_id.to_string(),
                    delegated_from_work_item_id,
                    delegated_from_task_id,
                    state: decode_enum(&state, "supervision state")?,
                    revision: u64::try_from(revision).context("negative supervision revision")?,
                    created_at: parse_timestamp(&created_at, "supervision created")?,
                    updated_at: parse_timestamp(&updated_at, "supervision updated")?,
                })
            },
        )
        .transpose()
}

fn latest_durability(
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
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .map(|(durability, revision, created_at)| {
            Ok(AgentDurabilityRecord {
                agent_id: agent_id.to_string(),
                durability: decode_enum(&durability, "agent durability")?,
                revision: u64::try_from(revision).context("negative durability revision")?,
                created_at: parse_timestamp(&created_at, "durability")?,
            })
        })
        .transpose()
}

fn latest_lifecycle_attachment(
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
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .map(|(attachment, revision, created_at)| {
            Ok(AgentLifecycleAttachmentRecord {
                agent_id: agent_id.to_string(),
                attachment: decode_enum(&attachment, "lifecycle attachment")?,
                revision: u64::try_from(revision)
                    .context("negative lifecycle attachment revision")?,
                created_at: parse_timestamp(&created_at, "lifecycle attachment")?,
            })
        })
        .transpose()
}

fn latest_capability_policy(
    connection: &rusqlite::Connection,
    agent_id: &str,
) -> Result<Option<AgentCapabilityPolicyRecord>> {
    let header = connection
        .query_row(
            "SELECT revision, created_at FROM agent_capability_policy_records
             WHERE agent_id = ?1 ORDER BY revision DESC LIMIT 1",
            [agent_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((revision, created_at)) = header else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT capability_family, effect FROM agent_capability_policy_rules
         WHERE agent_id = ?1 AND revision = ?2 ORDER BY rule_index",
    )?;
    let rules = statement
        .query_map(params![agent_id, revision], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| -> Result<_> {
            let (family, effect) = row?;
            Ok(AgentCapabilityPolicyRule {
                family: decode_enum(&family, "capability family")?,
                effect: decode_enum(&effect, "capability policy effect")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(AgentCapabilityPolicyRecord {
        agent_id: agent_id.to_string(),
        revision: u64::try_from(revision).context("negative capability policy revision")?,
        rules,
        created_at: parse_timestamp(&created_at, "capability policy")?,
    }))
}

fn latest_message_policy(
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
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
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
    let rules = statement
        .query_map(params![agent_id, revision], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .map(|row| -> Result<_> {
            let (principal_kind, principal_id, route, effect) = row?;
            Ok(AgentMessagePolicyRule {
                principal_kind: decode_enum(&principal_kind, "message principal kind")?,
                principal_id,
                route,
                effect: decode_enum(&effect, "message policy effect")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(AgentMessagePolicyRecord {
        agent_id: agent_id.to_string(),
        revision: u64::try_from(revision).context("negative message policy revision")?,
        default_effect: decode_enum(&default_effect, "message policy default effect")?,
        rules,
        created_at: parse_timestamp(&created_at, "message policy")?,
    }))
}

fn decode_enum<T: serde::de::DeserializeOwned>(value: &str, label: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .with_context(|| format!("decoding {label}"))
}

fn parse_timestamp(value: &str, label: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .with_context(|| format!("decoding {label} timestamp"))
}

fn legacy_task_evidence(
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
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .map(
            |(owner_agent_id, child_agent_id, kind, payload)| -> Result<_> {
                let kind: TaskKind = serde_json::from_value(serde_json::Value::String(kind))
                    .context("decoding legacy supervision task kind")?;
                let task: TaskRecord =
                    serde_json::from_str(&payload).context("decoding legacy supervision task")?;
                Ok(LegacySupervisionTaskEvidence {
                    task_id: task_id.to_string(),
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
                    delegated_from_work_item_id: task.effective_work_item_id().map(str::to_string),
                })
            },
        )
        .transpose()
}

pub(crate) fn project_agent_canonical_relations(
    identity: &AgentIdentityRecord,
    canonical: AgentCanonicalRecordSet,
    task: Option<&LegacySupervisionTaskEvidence>,
) -> AgentCanonicalRelationsProjection {
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
            let record = legacy_lineage(identity, &mut issues);
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
            let record = legacy_supervision(identity, lineage.as_ref(), task, &mut issues);
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
            let record = legacy_durability(identity, supervision.as_ref(), &mut issues);
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
            let record = legacy_lifecycle_attachment(identity, supervision.as_ref(), &mut issues);
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
            let record = legacy_capability_policy(identity, &mut issues);
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
            Some(legacy_message_policy(identity, supervision.as_ref()))
        }
    };

    validate_legacy_profile(identity, &mut issues);
    validate_canonical_legacy_drift(
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
    );
    validate_effective_relations(
        supervision.as_ref(),
        durability.as_ref(),
        lifecycle_attachment.as_ref(),
        &mut issues,
    );
    let resolution = projection_resolution(&issues);
    AgentCanonicalRelationsProjection {
        agent_id: identity.agent_id.clone(),
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
    }
}

fn legacy_lineage(
    identity: &AgentIdentityRecord,
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
) -> Option<AgentLineageRecord> {
    let parent = match (
        identity.lineage_parent_agent_id.as_deref(),
        identity.parent_agent_id.as_deref(),
    ) {
        (Some(lineage_parent), Some(parent)) if lineage_parent != parent => {
            issue(
                issues,
                AgentCanonicalRelationAxis::Lineage,
                AgentCanonicalResolution::Contradictory,
                format!("legacy lineage parent {lineage_parent} conflicts with parent {parent}"),
            );
            return None;
        }
        (Some(parent), _) | (_, Some(parent)) => Some(parent),
        (None, None) => None,
    };
    let Some(parent) = parent else {
        if identity.kind == AgentKind::Child {
            issue(
                issues,
                AgentCanonicalRelationAxis::Lineage,
                AgentCanonicalResolution::MissingEvidence,
                "legacy child identity has no parent evidence",
            );
        }
        return None;
    };
    Some(AgentLineageRecord {
        child_agent_id: identity.agent_id.clone(),
        parent_agent_id: parent.to_string(),
        creation_cause: AgentLineageCreationCause::LegacySpawn,
        revision: 0,
        created_at: identity.created_at,
    })
}

fn legacy_supervision(
    identity: &AgentIdentityRecord,
    lineage: Option<&AgentLineageRecord>,
    task: Option<&LegacySupervisionTaskEvidence>,
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
) -> Option<AgentSupervisionRecord> {
    if identity.ownership() == AgentOwnership::SelfOwned {
        if identity.delegated_from_task_id.is_some() {
            issue(
                issues,
                AgentCanonicalRelationAxis::Supervision,
                AgentCanonicalResolution::Contradictory,
                "self-owned legacy identity still carries delegated supervision task evidence",
            );
        }
        return None;
    }
    let Some(supervisor_agent_id) = lineage.map(|record| record.parent_agent_id.clone()) else {
        issue(
            issues,
            AgentCanonicalRelationAxis::Supervision,
            AgentCanonicalResolution::MissingEvidence,
            "parent-supervised legacy identity has no unambiguous parent",
        );
        return None;
    };
    let Some(task_id) = identity.delegated_from_task_id.as_deref() else {
        issue(
            issues,
            AgentCanonicalRelationAxis::Supervision,
            AgentCanonicalResolution::MissingEvidence,
            "parent-supervised legacy identity has no delegated supervision task",
        );
        return None;
    };
    let Some(task) = task else {
        issue(
            issues,
            AgentCanonicalRelationAxis::Supervision,
            AgentCanonicalResolution::MissingEvidence,
            format!("delegated supervision task {task_id} is missing"),
        );
        return None;
    };
    if task.task_id != task_id
        || task.owner_agent_id != supervisor_agent_id
        || task.child_agent_id.as_deref() != Some(identity.agent_id.as_str())
        || !task.is_child_agent_task
    {
        issue(
            issues,
            AgentCanonicalRelationAxis::Supervision,
            AgentCanonicalResolution::Contradictory,
            format!("delegated task {task_id} does not prove the expected supervision relation"),
        );
        return None;
    }
    Some(AgentSupervisionRecord {
        supervision_id: format!("legacy:{task_id}"),
        supervisor_agent_id,
        child_agent_id: identity.agent_id.clone(),
        delegated_from_work_item_id: task.delegated_from_work_item_id.clone(),
        delegated_from_task_id: Some(task_id.to_string()),
        state: AgentSupervisionState::Active,
        revision: 0,
        created_at: identity.created_at,
        updated_at: identity.updated_at,
    })
}

fn legacy_durability(
    identity: &AgentIdentityRecord,
    supervision: Option<&AgentSupervisionRecord>,
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
) -> Option<AgentDurabilityRecord> {
    let durability = match identity.durability {
        Some(crate::types::AgentDurability::Persistent) => AgentCanonicalDurability::Persistent,
        Some(crate::types::AgentDurability::Ephemeral) => AgentCanonicalDurability::Ephemeral,
        None => match identity.ownership() {
            AgentOwnership::SelfOwned => AgentCanonicalDurability::Persistent,
            AgentOwnership::ParentSupervised if supervision.is_some() => {
                AgentCanonicalDurability::Ephemeral
            }
            AgentOwnership::ParentSupervised => {
                issue(
                    issues,
                    AgentCanonicalRelationAxis::Durability,
                    AgentCanonicalResolution::MissingEvidence,
                    "parent-supervised durability cannot be resolved without supervision evidence",
                );
                return None;
            }
        },
    };
    Some(AgentDurabilityRecord {
        agent_id: identity.agent_id.clone(),
        durability,
        revision: 0,
        created_at: identity.created_at,
    })
}

fn legacy_lifecycle_attachment(
    identity: &AgentIdentityRecord,
    supervision: Option<&AgentSupervisionRecord>,
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
) -> Option<AgentLifecycleAttachmentRecord> {
    let attachment = match identity.ownership() {
        AgentOwnership::SelfOwned => AgentLifecycleAttachment::Independent,
        AgentOwnership::ParentSupervised if supervision.is_some() => {
            AgentLifecycleAttachment::SupervisionAttached
        }
        AgentOwnership::ParentSupervised => {
            issue(
                issues,
                AgentCanonicalRelationAxis::LifecycleAttachment,
                AgentCanonicalResolution::MissingEvidence,
                "parent-supervised lifecycle attachment cannot be resolved without supervision evidence",
            );
            return None;
        }
    };
    Some(AgentLifecycleAttachmentRecord {
        agent_id: identity.agent_id.clone(),
        attachment,
        revision: 0,
        created_at: identity.created_at,
    })
}

fn legacy_capability_policy(
    identity: &AgentIdentityRecord,
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
) -> Option<AgentCapabilityPolicyRecord> {
    let preset =
        identity
            .profile_preset
            .or_else(|| match (identity.ownership(), identity.visibility) {
                (AgentOwnership::SelfOwned, crate::types::AgentVisibility::Public) => {
                    Some(AgentProfilePreset::PublicNamed)
                }
                (AgentOwnership::ParentSupervised, _) => Some(AgentProfilePreset::PrivateChild),
                _ => None,
            });
    let Some(preset) = preset else {
        issue(
            issues,
            AgentCanonicalRelationAxis::CapabilityPolicy,
            AgentCanonicalResolution::MissingEvidence,
            "legacy capability package cannot be resolved",
        );
        return None;
    };
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
    Some(AgentCapabilityPolicyRecord {
        agent_id: identity.agent_id.clone(),
        revision: 0,
        rules,
        created_at: identity.created_at,
    })
}

fn legacy_message_policy(
    identity: &AgentIdentityRecord,
    supervision: Option<&AgentSupervisionRecord>,
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
    if let Some(supervision) = supervision.filter(|record| {
        matches!(
            record.state,
            AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
        )
    }) {
        rules.push(AgentMessagePolicyRule {
            principal_kind: AgentMessagePrincipalKind::SupervisingParent,
            principal_id: Some(supervision.supervisor_agent_id.clone()),
            route: Some("supervision_follow_up".into()),
            effect: AgentPolicyEffect::Allow,
        });
    }
    AgentMessagePolicyRecord {
        agent_id: identity.agent_id.clone(),
        revision: 0,
        default_effect: AgentPolicyEffect::Deny,
        rules,
        created_at: identity.created_at,
    }
}

fn validate_legacy_profile(
    identity: &AgentIdentityRecord,
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
) {
    match identity.profile_preset {
        Some(AgentProfilePreset::PublicNamed)
            if identity.ownership() != AgentOwnership::SelfOwned
                || identity.delegated_from_task_id.is_some() =>
        {
            issue(
                issues,
                AgentCanonicalRelationAxis::LifecycleAttachment,
                AgentCanonicalResolution::Contradictory,
                "public_named legacy preset conflicts with supervision evidence",
            );
        }
        Some(AgentProfilePreset::PrivateChild)
            if identity.ownership() == AgentOwnership::SelfOwned
                && identity.parent_agent_id.is_none()
                && identity.lineage_parent_agent_id.is_none() =>
        {
            issue(
                issues,
                AgentCanonicalRelationAxis::LifecycleAttachment,
                AgentCanonicalResolution::Ambiguous,
                "private_child legacy preset has no lifecycle ownership evidence",
            );
        }
        _ => {}
    }
}

fn validate_canonical_legacy_drift(
    identity: &AgentIdentityRecord,
    task: Option<&LegacySupervisionTaskEvidence>,
    lineage: Option<&AgentLineageRecord>,
    supervision: Option<&AgentSupervisionRecord>,
    durability: Option<&AgentDurabilityRecord>,
    attachment: Option<&AgentLifecycleAttachmentRecord>,
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
) {
    if let Some(lineage) = lineage {
        let legacy_parent = identity
            .lineage_parent_agent_id
            .as_deref()
            .or(identity.parent_agent_id.as_deref());
        if legacy_parent.is_some_and(|parent| parent != lineage.parent_agent_id) {
            issue(
                issues,
                AgentCanonicalRelationAxis::Lineage,
                AgentCanonicalResolution::Contradictory,
                "canonical lineage differs from legacy parent evidence",
            );
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
            issue(
                issues,
                AgentCanonicalRelationAxis::Supervision,
                AgentCanonicalResolution::Contradictory,
                "canonical supervision differs from legacy ownership, parent, or task evidence",
            );
        }
    }
    if let (Some(durability), Some(legacy)) = (durability, identity.durability) {
        let legacy = match legacy {
            crate::types::AgentDurability::Persistent => AgentCanonicalDurability::Persistent,
            crate::types::AgentDurability::Ephemeral => AgentCanonicalDurability::Ephemeral,
        };
        if durability.durability != legacy {
            issue(
                issues,
                AgentCanonicalRelationAxis::Durability,
                AgentCanonicalResolution::Contradictory,
                "canonical durability differs from explicit legacy durability",
            );
        }
    }
    if let Some(attachment) = attachment {
        let legacy = match identity.ownership() {
            AgentOwnership::SelfOwned => AgentLifecycleAttachment::Independent,
            AgentOwnership::ParentSupervised => AgentLifecycleAttachment::SupervisionAttached,
        };
        if attachment.attachment != legacy {
            issue(
                issues,
                AgentCanonicalRelationAxis::LifecycleAttachment,
                AgentCanonicalResolution::Contradictory,
                "canonical lifecycle attachment differs from legacy ownership evidence",
            );
        }
    }
}

fn validate_effective_relations(
    supervision: Option<&AgentSupervisionRecord>,
    durability: Option<&AgentDurabilityRecord>,
    attachment: Option<&AgentLifecycleAttachmentRecord>,
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
) {
    let supervision_active = supervision.is_some_and(|record| {
        matches!(
            record.state,
            AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
        )
    });
    match attachment.map(|record| record.attachment) {
        Some(AgentLifecycleAttachment::Independent) if supervision_active => issue(
            issues,
            AgentCanonicalRelationAxis::LifecycleAttachment,
            AgentCanonicalResolution::Contradictory,
            "independent lifecycle conflicts with active supervision",
        ),
        Some(AgentLifecycleAttachment::SupervisionAttached) if !supervision_active => issue(
            issues,
            AgentCanonicalRelationAxis::Supervision,
            AgentCanonicalResolution::MissingEvidence,
            "supervision-attached lifecycle has no active supervision record",
        ),
        _ => {}
    }
    if supervision_active
        && durability
            .is_some_and(|record| record.durability == AgentCanonicalDurability::Persistent)
    {
        issue(
            issues,
            AgentCanonicalRelationAxis::Durability,
            AgentCanonicalResolution::Contradictory,
            "persistent durability conflicts with first-release active supervision",
        );
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

fn issue(
    issues: &mut Vec<AgentCanonicalProjectionIssue>,
    axis: AgentCanonicalRelationAxis,
    resolution: AgentCanonicalResolution,
    detail: impl Into<String>,
) {
    issues.push(AgentCanonicalProjectionIssue {
        axis,
        resolution,
        detail: detail.into(),
    });
}

#[cfg(test)]
mod tests;
