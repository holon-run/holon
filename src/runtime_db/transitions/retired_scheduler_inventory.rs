use anyhow::{Context, Result};

use super::RuntimeTransitionRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetiredSchedulerRolloutMetadata {
    pub retirement_marked: bool,
    pub protocol_mode: String,
    pub config_revision: u64,
    pub preflight_count: u64,
    pub manifest_count: u64,
    pub scenario_count: u64,
    pub authoritative_scenario_count: u64,
    pub stale_authoritative_scenario_count: u64,
    pub hard_blocker_count: u64,
    pub command_result_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulerAuthorityInventoryRecord {
    pub storage: &'static str,
    pub role: &'static str,
    pub canonical_reader: bool,
    pub target: &'static str,
    pub row_count: u64,
}

impl RuntimeTransitionRepository<'_> {
    pub(crate) fn retired_scheduler_partition_exists(&self, agent_id: &str) -> Result<bool> {
        let connection = self.db.connection()?;
        for table in [
            "scheduler_agent_slots",
            "scheduler_agent_dispatch",
            "scheduler_agent_focus",
            "scheduler_work_demands",
        ] {
            let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE agent_id = ?1)");
            if connection.query_row(&sql, [agent_id], |row| row.get::<_, bool>(0))? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn inspect_retired_scheduler_rollout_metadata(
        &self,
    ) -> Result<RetiredSchedulerRolloutMetadata> {
        let connection = self.db.connection()?;
        let retirement_marked = connection.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM scheduler_rollout_retirement WHERE retirement_id = 1
             )",
            [],
            |row| row.get(0),
        )?;
        let (protocol_mode, config_revision): (String, i64) = connection.query_row(
            "SELECT protocol_mode, config_revision
             FROM scheduler_protocol_config
             WHERE config_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let count = |sql: &str| -> Result<u64> {
            let value: i64 = connection.query_row(sql, [], |row| row.get(0))?;
            to_u64(value, "retired scheduler rollout metadata count")
        };
        Ok(RetiredSchedulerRolloutMetadata {
            retirement_marked,
            protocol_mode,
            config_revision: to_u64(config_revision, "rollout config revision")?,
            preflight_count: count("SELECT COUNT(*) FROM scheduler_rollout_preflights")?,
            manifest_count: count("SELECT COUNT(*) FROM scheduler_rollout_manifests")?,
            scenario_count: count("SELECT COUNT(*) FROM scheduler_scenario_authorities")?,
            authoritative_scenario_count: count(
                "SELECT COUNT(*) FROM scheduler_scenario_authorities
                 WHERE mode = 'authoritative'",
            )?,
            stale_authoritative_scenario_count: count(
                "SELECT COUNT(*)
                 FROM scheduler_scenario_authorities AS authority
                 LEFT JOIN scheduler_rollout_manifests AS manifest
                   ON manifest.manifest_revision = authority.manifest_revision
                 LEFT JOIN scheduler_rollout_preflights AS preflight
                   ON preflight.preflight_revision = authority.preflight_revision
                 WHERE authority.mode = 'authoritative'
                   AND (
                     authority.manifest_revision IS NULL
                     OR authority.preflight_revision IS NULL
                     OR manifest.manifest_revision IS NULL
                     OR preflight.preflight_revision IS NULL
                   )",
            )?,
            hard_blocker_count: count("SELECT COUNT(*) FROM scheduler_scenario_hard_blockers")?,
            command_result_count: count("SELECT COUNT(*) FROM scheduler_rollout_command_results")?,
        })
    }

    pub(crate) fn inspect_scheduler_authority_inventory(
        &self,
    ) -> Result<Vec<SchedulerAuthorityInventoryRecord>> {
        const INVENTORY: &[(&str, &str, bool, &str)] = &[
            (
                "queue_entries",
                "authority",
                true,
                "retain as queue source authority",
            ),
            (
                "work_items",
                "authority",
                true,
                "retain as WorkItem lifecycle authority",
            ),
            (
                "wait_conditions",
                "authority",
                true,
                "retain as the only wait authority",
            ),
            (
                "tasks",
                "authority",
                true,
                "retain as task lifecycle authority",
            ),
            (
                "scheduler_activations",
                "retired_authority_and_evidence",
                false,
                "remove after execution-attempt migration validation",
            ),
            (
                "scheduler_activation_settlements",
                "retired_evidence",
                false,
                "remove after execution-outcome migration validation",
            ),
            (
                "scheduler_agent_slots",
                "retired_authority",
                false,
                "remove",
            ),
            (
                "scheduler_agent_dispatch",
                "retired_authority",
                false,
                "remove",
            ),
            (
                "scheduler_agent_focus",
                "retired_projection",
                false,
                "remove",
            ),
            (
                "scheduler_work_demands",
                "retired_authority",
                false,
                "remove",
            ),
            ("scheduler_waits", "retired_authority", false, "remove"),
            (
                "scheduler_wait_generations",
                "retired_authority",
                false,
                "remove",
            ),
            (
                "scheduler_missing_settlements",
                "retired_recovery_authority",
                false,
                "remove",
            ),
            (
                "scheduler_activation_authorities",
                "retired_compatibility",
                false,
                "remove",
            ),
            (
                "scheduler_activation_sources",
                "retired_evidence",
                false,
                "remove after execution-source migration validation",
            ),
            (
                "scheduler_activation_inputs",
                "retired_evidence",
                false,
                "remove after execution-source migration validation",
            ),
            (
                "scheduler_continuation_admissions",
                "retired_evidence",
                false,
                "remove after continuation migration validation",
            ),
            (
                "scheduler_yield_continuations",
                "retired_authority",
                false,
                "remove",
            ),
            (
                "scheduler_protocol_command_results",
                "retired_evidence",
                false,
                "remove after audit-retention export",
            ),
            (
                "scheduler_protocol_command_conflict_attempts",
                "retired_evidence",
                false,
                "remove after audit-retention export",
            ),
            (
                "scheduler_protocol_migrations",
                "retired_compatibility",
                false,
                "remove",
            ),
            (
                "scheduler_protocol_config",
                "retired_compatibility",
                false,
                "remove",
            ),
            (
                "scheduler_rollout_preflights",
                "retired_rollout",
                false,
                "remove",
            ),
            (
                "scheduler_rollout_manifests",
                "retired_rollout",
                false,
                "remove",
            ),
            (
                "scheduler_scenario_authorities",
                "retired_rollout",
                false,
                "remove",
            ),
            (
                "scheduler_scenario_hard_blockers",
                "retired_rollout",
                false,
                "remove",
            ),
            (
                "scheduler_shadow_comparisons",
                "retired_rollout",
                false,
                "remove",
            ),
            (
                "scheduler_semantic_shadow_decisions",
                "retired_rollout",
                false,
                "remove",
            ),
        ];
        let connection = self.db.connection()?;
        INVENTORY
            .iter()
            .map(|(storage, role, canonical_reader, target)| {
                let count: i64 = connection.query_row(
                    &format!("SELECT COUNT(*) FROM {storage}"),
                    [],
                    |row| row.get(0),
                )?;
                Ok(SchedulerAuthorityInventoryRecord {
                    storage,
                    role,
                    canonical_reader: *canonical_reader,
                    target,
                    row_count: to_u64(count, "scheduler authority inventory row count")?,
                })
            })
            .collect()
    }
}

fn to_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("{field} must be non-negative"))
}
