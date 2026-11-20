use anyhow::{Context, Result};
use chrono::Utc;
use holon_storage::agent_registry::relations::AgentRelations;
use rusqlite::{Connection, Transaction};

use crate::runtime_db::types::AgentCanonicalRelationRepository;
use crate::types::{
    AgentCanonicalRecordSet, AgentCanonicalRelationsProjection, AgentCapabilityPolicyRecord,
    AgentDurabilityRecord, AgentIdentityRecord, AgentLifecycleAttachmentRecord,
    AgentLineageChildrenSnapshot, AgentLineageRecord, AgentMessagePolicyRecord,
    AgentRelationBackfillDiagnostic, AgentRelationBackfillReport, AgentSupervisionRecord,
    AgentSupervisionState,
};
pub(crate) use holon_storage::agent_registry::relations::{
    independent_creation_records, supervised_creation_records,
};

#[cfg(all(test, holon_lib_test_shard = "b"))]
use crate::types::{
    AgentCanonicalDurability, AgentCanonicalProjectionIssue, AgentCanonicalRelationAxis,
    AgentCanonicalResolution, AgentCanonicalValueSource, AgentCapabilityFamily,
    AgentCapabilityPolicyRule, AgentIdentityLifecycle, AgentKind, AgentLifecycleAttachment,
    AgentLifecycleFenceState, AgentLineageCreationCause, AgentMessagePolicyRule,
    AgentMessagePrincipalKind, AgentOwnership, AgentPolicyEffect, AgentProfilePreset,
    AgentRegistryStatus, AgentRelationBackfillOutcome, TaskKind, TaskRecord,
};
#[cfg(all(test, holon_lib_test_shard = "b"))]
use holon_storage::agent_registry::relations::{decode_enum, LegacySupervisionTaskEvidence};
#[cfg(all(test, holon_lib_test_shard = "b"))]
use holon_storage::codec::timestamp;
#[cfg(all(test, holon_lib_test_shard = "b"))]
use rusqlite::params;

const RELATIONS: AgentRelations = AgentRelations {
    material: super::AGENT_REGISTRY_MATERIAL,
};

/// The original caller's material view lives through report consumption.
#[derive(Debug, serde::Serialize)]
#[serde(transparent)]
pub struct AgentRelationBackfillSnapshot {
    report: AgentRelationBackfillReport,
    #[serde(skip)]
    _material: Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

impl std::ops::Deref for AgentRelationBackfillSnapshot {
    type Target = AgentRelationBackfillReport;

    fn deref(&self) -> &Self::Target {
        &self.report
    }
}

impl Clone for AgentRelationBackfillSnapshot {
    fn clone(&self) -> Self {
        let material = self._material.as_ref().map(|guard| guard.fork_owned());
        Self {
            report: self.report.clone(),
            _material: material,
        }
    }
}

impl AgentCanonicalRelationRepository<'_> {
    pub fn upsert_lineage(&self, record: &AgentLineageRecord) -> Result<()> {
        self.db
            .transaction(|tx| RELATIONS.upsert_lineage_tx(tx, record))
    }

    pub fn upsert_supervision(&self, record: &AgentSupervisionRecord) -> Result<()> {
        self.db
            .transaction(|tx| RELATIONS.upsert_supervision_tx(tx, record))
    }

    pub fn upsert_durability(&self, record: &AgentDurabilityRecord) -> Result<()> {
        self.db
            .transaction(|tx| RELATIONS.upsert_durability(tx, record))
    }

    pub fn upsert_lifecycle_attachment(
        &self,
        record: &AgentLifecycleAttachmentRecord,
    ) -> Result<()> {
        self.db
            .transaction(|tx| RELATIONS.upsert_lifecycle_attachment(tx, record))
    }

    pub fn upsert_capability_policy(&self, record: &AgentCapabilityPolicyRecord) -> Result<()> {
        self.db
            .transaction(|tx| RELATIONS.upsert_capability_policy_tx(tx, record))
    }

    pub fn upsert_message_policy(&self, record: &AgentMessagePolicyRecord) -> Result<()> {
        self.db
            .transaction(|tx| RELATIONS.upsert_message_policy_tx(tx, record))
    }

    pub fn latest(&self, agent_id: &str) -> Result<Option<AgentCanonicalRelationsProjection>> {
        self.db
            .with_read(|connection| canonical_relations_from_connection(connection, agent_id))
    }

    pub(crate) fn prepare_capability_policy(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Result<Option<AgentCapabilityPolicyRecord>> {
        self.db
            .read_for_preparation(|tx| RELATIONS.prepare_capability_policy(tx, identity))
    }

    pub fn lineage_children(&self, parent_agent_id: &str) -> Result<AgentLineageChildrenSnapshot> {
        self.db.with_read(|connection| {
            let records = RELATIONS.lineage_children(connection, parent_agent_id)?;
            Ok(AgentLineageChildrenSnapshot {
                records,
                _material: crate::runtime::agent_observation::active_observation_material()?,
            })
        })
    }

    pub fn backfill(
        &self,
        apply: bool,
        diagnostic_sample_limit: usize,
    ) -> Result<AgentRelationBackfillSnapshot> {
        self.backfill_with_backup(apply, diagnostic_sample_limit, None)
    }

    pub fn backfill_with_backup(
        &self,
        apply: bool,
        diagnostic_sample_limit: usize,
        backup_path: Option<String>,
    ) -> Result<AgentRelationBackfillSnapshot> {
        let requested_at = Utc::now();
        let run = if apply {
            Some(self.db.transaction(|tx| {
                RELATIONS
                    .begin_or_resume_backfill(tx, requested_at, || uuid::Uuid::new_v4().simple())
            })?)
        } else {
            None
        };
        let started_at = run
            .as_ref()
            .map(|(_, started_at)| *started_at)
            .unwrap_or(requested_at);

        let agent_ids = RELATIONS.backfill_agent_ids(&self.db.connection()?)?;
        let mut report = AgentRelationBackfillReport {
            run_id: run.map(|(run_id, _)| run_id),
            backup_path,
            apply,
            scanned_agents: 0,
            changed_agents: 0,
            unchanged_agents: 0,
            diagnostic_agents: 0,
            migrated_axes: 0,
            diagnostic_sample_limit,
            diagnostics: Vec::new(),
            started_at,
            completed_at: started_at,
        };

        for agent_id in agent_ids {
            let result = self.db.transaction(|tx| {
                RELATIONS.backfill_agent_relations_tx(
                    tx,
                    &agent_id,
                    apply,
                    report.run_id.as_deref(),
                )
            });
            let (migrated_axes, issues) = match result {
                Ok(result) => result,
                Err(error) => {
                    if let Some(run_id) = report.run_id.as_deref() {
                        let failed_at = Utc::now();
                        report.completed_at = failed_at;
                        self.db.transaction(|tx| {
                            RELATIONS.finish_backfill_run(tx, run_id, &report, false)
                        })?;
                    }
                    return Err(error).context(RELATIONS.relation_formatted_text(format_args!(
                        "backfilling canonical relations for agent {agent_id}"
                    ))?);
                }
            };
            report.scanned_agents += 1;
            report.migrated_axes += migrated_axes.len();
            if migrated_axes.is_empty() {
                report.unchanged_agents += 1;
            } else {
                report.changed_agents += 1;
            }
            if !issues.is_empty() {
                report.diagnostic_agents += 1;
                if report.diagnostics.len() < diagnostic_sample_limit {
                    RELATIONS.material.reserve_slot(&mut report.diagnostics)?;
                    report.diagnostics.push(AgentRelationBackfillDiagnostic {
                        agent_id,
                        migrated_axes,
                        issues,
                    });
                }
            }
        }

        report.completed_at = Utc::now();
        if let Some(run_id) = report.run_id.as_deref() {
            self.db
                .transaction(|tx| RELATIONS.finish_backfill_run(tx, run_id, &report, true))?;
        }
        Ok(AgentRelationBackfillSnapshot {
            report,
            _material: crate::diagnostics::resource_io::ResourceReadMaterial::active_guard()?,
        })
    }
}

pub(crate) fn upsert_canonical_record_set_tx(
    tx: &Transaction<'_>,
    records: &AgentCanonicalRecordSet,
) -> Result<()> {
    RELATIONS.upsert_canonical_record_set_tx(tx, records)
}

pub(crate) fn transition_supervision_state_tx(
    tx: &Transaction<'_>,
    child_agent_id: &str,
    state: AgentSupervisionState,
    revision: u64,
    updated_at: chrono::DateTime<Utc>,
) -> Result<()> {
    RELATIONS.transition_supervision_state_tx(tx, child_agent_id, state, revision, updated_at)
}

pub(crate) fn canonical_relations_from_connection(
    connection: &Connection,
    agent_id: &str,
) -> Result<Option<AgentCanonicalRelationsProjection>> {
    RELATIONS.canonical_relations_from_connection(connection, agent_id)
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod material_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod identity_task_material_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod enum_decode_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod diagnostic_material_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod backfill_material_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod legacy_relation_material_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod legacy_policy_material_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod lineage_children_material_tests;
