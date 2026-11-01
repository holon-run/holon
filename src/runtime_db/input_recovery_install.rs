//! Focused acknowledged selectors; canonical bodies belong to resident domains.
//! Receipts retain IDs only; recovery reads current rows in its fresh view.
use std::collections::BTreeSet;

use anyhow::{ensure, Context, Result};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::{repositories, transitions::PostCommitEffects};
use crate::runtime::resident_input_evidence::{InputEvidenceKey, InputEvidenceScope};

const MAX_INSTALL_KEYS: usize = 65_536;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputMutationInstallKeys {
    pub work_items: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    /// Original owning selectors, not durable body/absence authority. Fresh
    /// recovery derives dependencies again from the current Work records.
    pub input_roots: BTreeSet<InputEvidenceKey>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub completion_brief_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub wait_conditions: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub continuations: BTreeSet<String>,
    pub tasks: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub task_result_settlements: BTreeSet<String>,
    pub timers: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub scheduling: BTreeSet<crate::runtime::resident_scheduling::SchedulingKey>,
    pub external_triggers: BTreeSet<String>,
}
impl InputMutationInstallKeys {
    pub(crate) fn validate(&self) -> Result<()> {
        let mut count = 0usize;
        for keys in [
            &self.work_items,
            &self.wait_conditions,
            &self.continuations,
            &self.completion_brief_ids,
            &self.tasks,
            &self.task_result_settlements,
            &self.timers,
            &self.external_triggers,
        ] {
            count = count
                .checked_add(keys.len())
                .context("input cache target count overflow")?;
            ensure!(
                keys.iter().all(|id| !id.trim().is_empty()),
                "input cache target has an empty ID"
            );
        }
        count = count
            .checked_add(self.scheduling.len())
            .context("Scheduling install count overflow")?;
        ensure!(
            self.scheduling.iter().all(|key| !key.id.is_empty()),
            "empty Scheduling install key"
        );
        ensure!(
            self.input_roots.iter().all(|key| !key.id.trim().is_empty()),
            "input owning target has an empty ID"
        );
        count = count
            .checked_add(self.input_roots.len())
            .context("input owning target count overflow")?;
        ensure!(
            count <= MAX_INSTALL_KEYS,
            "input cache targets exceed preparation limit"
        );
        Ok(())
    }

    pub(crate) fn from_effects(
        scope: &InputEvidenceScope,
        effects: &PostCommitEffects,
    ) -> Result<Self> {
        let mut keys = Self::default();
        for record in &effects.work_items {
            ensure!(
                record.agent_id == scope.agent_id,
                "WorkItem cache target differs from input scope"
            );
            keys.work_items.insert(record.id.clone());
        }
        for record in &effects.tasks {
            ensure!(
                record.agent_id == scope.agent_id,
                "Task cache target differs from input scope"
            );
            keys.tasks.insert(record.id.clone());
        }
        // Domain install records include unrelated active/history bodies. They
        // are never an actual-write inventory or an original command selector.
        for key in effects
            .task_write_keys
            .iter()
            .chain(&effects.task_observation_keys)
        {
            match key.family {
                crate::runtime::resident_task::TaskFamily::Task => {
                    keys.tasks.insert(key.id.clone());
                }
                crate::runtime::resident_task::TaskFamily::ResultSettlement => {
                    keys.task_result_settlements.insert(key.id.clone());
                }
            }
        }
        if let Some(delta) = &effects.task_delta {
            ensure!(&delta.scope == scope, "Task delta install scope differs");
        }
        for record in &effects.timers {
            ensure!(
                record.agent_id == scope.agent_id,
                "Timer cache target differs from input scope"
            );
            keys.timers.insert(record.id.clone());
        }
        for record in &effects.external_triggers {
            ensure!(
                record.target_agent_id == scope.agent_id,
                "ExternalTrigger cache target differs from input scope"
            );
            keys.external_triggers
                .insert(record.external_trigger_id.clone());
        }
        keys.scheduling
            .extend(effects.scheduling_write_keys.iter().cloned());
        keys.validate()?;
        Ok(keys)
    }
}

/// Missing current rows deliberately remain in `keys` as finite selectors.
/// Every body belongs only to its fresh qualified domain. This metadata is
/// neither a historical body authority nor a second installation cache.
#[derive(Debug, Clone)]
pub(crate) struct InputRecoveryInstallFacts {
    pub scope: InputEvidenceScope,
    pub keys: InputMutationInstallKeys,
}

pub(crate) fn load_current_install_facts_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &InputMutationInstallKeys,
) -> Result<InputRecoveryInstallFacts> {
    ensure!(
        !scope.agent_id.trim().is_empty() && scope.identity_incarnation > 0,
        "invalid input cache scope"
    );
    keys.validate()?;
    for (table, id_column, owner_column, ids) in [
        ("timers", "timer_id", "agent_id", &keys.timers),
        (
            "external_triggers",
            "external_trigger_id",
            "target_agent_id",
            &keys.external_triggers,
        ),
    ] {
        for id in ids {
            // No body or address String is loaded. A missing current row keeps
            // its original selector; an existing foreign normalized row rejects.
            let exact: Option<bool> = tx.query_row(
                &format!("SELECT {id_column}=?1 AND {owner_column}=?2 FROM {table} WHERE {id_column}=?1"),
                rusqlite::params![id,scope.agent_id],|row|row.get(0)).optional()?;
            ensure!(
                exact != Some(false),
                "input install target normalized ID/agent differs from original scope"
            );
        }
    }
    let facts = InputRecoveryInstallFacts {
        scope: scope.clone(),
        keys: keys.clone(),
    };
    Ok(facts)
}

/// Called only for initial receipt insertion, while every target must exist in
/// the original SQL view. Replay must not revalidate rows pruned since that ACK.
pub(crate) fn validate_original_install_keys_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &InputMutationInstallKeys,
) -> Result<()> {
    ensure!(
        !scope.agent_id.trim().is_empty() && scope.identity_incarnation > 0,
        "invalid original install scope"
    );
    keys.validate()?;
    // Initial ACK still proves full typed bodies and exact normalized scope.
    // Fresh recovery never reuses these bodies; its qualified domain is separate.
    for id in &keys.timers {
        let payload = current_payload_tx(tx, scope, "timers", "timer_id", "agent_id", id)?
            .context("Timer installation target is missing from original SQL ACK")?;
        let record = repositories::decode_timer_payload(&payload)?;
        ensure!(
            record.id == *id && record.agent_id == scope.agent_id,
            "Timer original payload differs from normalized scope/ID"
        );
    }
    for id in &keys.external_triggers {
        let payload = current_payload_tx(
            tx,
            scope,
            "external_triggers",
            "external_trigger_id",
            "target_agent_id",
            id,
        )?
        .context("ExternalTrigger installation target is missing from original SQL ACK")?;
        let record = repositories::decode_external_trigger_payload(&payload)?;
        ensure!(
            record.external_trigger_id == *id && record.target_agent_id == scope.agent_id,
            "ExternalTrigger original payload differs from normalized scope/ID"
        );
    }
    // Qualification and canonical body/fence checks remain in the original
    // Task finalizer. The receipt proves these exact targets before its ACK,
    // without retaining a second Task body for later cache installation.
    for id in &keys.tasks {
        let exact: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1 AND owner_agent_id=?2 AND CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.id')=task_id AND json_extract(payload_json,'$.agent_id')=owner_agent_id ELSE 0 END)",
            rusqlite::params![id,scope.agent_id], |row| row.get(0),
        )?;
        ensure!(
            exact,
            "original Task installation target normalized/payload scope differs or is missing"
        );
    }
    for id in &keys.task_result_settlements {
        let row: Option<(String,String,Option<String>,Option<String>)> = tx.query_row(
            "SELECT result_identity,agent_id,json_extract(payload_json,'$.result_identity'),json_extract(payload_json,'$.agent_id') FROM task_result_settlements WHERE result_identity=?1",
            [id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional()?;
        let (actual_id, agent, payload_id, payload_agent) =
            row.context("original Task result installation target is missing from SQL ACK")?;
        ensure!(
            actual_id == *id
                && agent == scope.agent_id
                && payload_id.as_deref() == Some(id.as_str())
                && payload_agent.as_deref() == Some(scope.agent_id.as_str()),
            "original Task result installation target normalized/payload scope differs"
        );
    }
    for key in &keys.scheduling {
        let qualified:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM scheduling_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 AND availability='present')",
            rusqlite::params![scope.agent_id,scope.identity_incarnation,match key.family {
                crate::runtime::resident_scheduling::SchedulingFamily::Timer=>"timer",
                crate::runtime::resident_scheduling::SchedulingFamily::TimerWake=>"timer_wake",
                crate::runtime::resident_scheduling::SchedulingFamily::ExternalTrigger=>"external_trigger",
            },key.id],|r|r.get(0))?;
        ensure!(
            qualified,
            "original Scheduling install target lacks actual qualification ACK"
        );
    }
    // Work bodies belong only to the qualified Work domain. The original
    // receipt still proves every exact normalized target before SQL ACK.
    for (table, id_column, keys) in [
        ("work_items", "work_item_id", &keys.work_items),
        (
            "wait_conditions",
            "wait_condition_id",
            &keys.wait_conditions,
        ),
        (
            "work_item_continuations",
            "continuation_id",
            &keys.continuations,
        ),
    ] {
        for id in keys {
            let row: Option<(String, String, Option<String>, Option<String>)> = tx.query_row(
                &format!("SELECT {id_column},agent_id,json_extract(payload_json,'$.id'),json_extract(payload_json,'$.agent_id') FROM {table} WHERE {id_column}=?1"),
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).optional()?;
            let (actual_id, agent_id, payload_id, payload_agent) =
                row.context("original Work installation target is missing from SQL ACK")?;
            ensure!(
                actual_id == *id
                    && agent_id == scope.agent_id
                    && payload_id.as_deref() == Some(id.as_str())
                    && payload_agent.as_deref() == Some(scope.agent_id.as_str()),
                "original Work installation target normalized/payload scope differs"
            );
        }
    }
    Ok(())
}

fn current_payload_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    table: &'static str,
    id_column: &'static str,
    agent_column: &'static str,
    id: &str,
) -> Result<Option<String>> {
    let sql =
        format!("SELECT {id_column},{agent_column},payload_json FROM {table} WHERE {id_column}=?1");
    let row: Option<(String, String, String)> = tx
        .query_row(&sql, [id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let Some((actual_id, agent_id, payload)) = row else {
        return Ok(None);
    };
    ensure!(
        actual_id == id && agent_id == scope.agent_id,
        "input cache target normalized ID/agent differs from its original scope"
    );
    Ok(Some(payload))
}

#[cfg(test)]
#[path = "input_recovery_install_tests.rs"]
mod tests;
