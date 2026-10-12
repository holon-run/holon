//! Focused legacy cache installation targets of an acknowledged input command.
//! Receipts retain IDs only; recovery reads current rows in its fresh view.
use std::collections::BTreeSet;

use anyhow::{ensure, Context, Result};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::{repositories, transitions::PostCommitEffects};
use crate::{
    runtime::resident_input_evidence::InputEvidenceScope,
    types::{ExternalTriggerRecord, TaskRecord, TimerRecord, WorkItemRecord},
};

const MAX_INSTALL_KEYS: usize = 65_536;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputMutationInstallKeys {
    pub work_items: BTreeSet<String>,
    pub tasks: BTreeSet<String>,
    pub timers: BTreeSet<String>,
    pub external_triggers: BTreeSet<String>,
}
impl InputMutationInstallKeys {
    pub(crate) fn validate(&self) -> Result<()> {
        let mut count = 0usize;
        for keys in [
            &self.work_items,
            &self.tasks,
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
        keys.validate()?;
        Ok(keys)
    }
}

/// Missing current rows deliberately remain in `keys`: installation removes
/// exactly those old cached IDs, without clearing unrelated cache entries.
#[derive(Debug, Clone)]
pub(crate) struct InputRecoveryInstallFacts {
    pub scope: InputEvidenceScope,
    pub keys: InputMutationInstallKeys,
    pub work_items: Vec<WorkItemRecord>,
    pub tasks: Vec<TaskRecord>,
    pub timers: Vec<TimerRecord>,
    pub external_triggers: Vec<ExternalTriggerRecord>,
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
    let mut facts = InputRecoveryInstallFacts {
        scope: scope.clone(),
        keys: keys.clone(),
        work_items: Vec::new(),
        tasks: Vec::new(),
        timers: Vec::new(),
        external_triggers: Vec::new(),
    };
    for id in &keys.work_items {
        if let Some(payload) =
            current_payload_tx(tx, scope, "work_items", "work_item_id", "agent_id", id)?
        {
            let record = repositories::decode_work_item_payload(&payload)?;
            ensure!(
                record.id == *id && record.agent_id == scope.agent_id,
                "WorkItem cache payload differs from normalized scope/ID"
            );
            facts.work_items.push(record);
        }
    }
    for id in &keys.tasks {
        if let Some(payload) =
            current_payload_tx(tx, scope, "tasks", "task_id", "owner_agent_id", id)?
        {
            let record = repositories::decode_task_payload(&payload)?;
            ensure!(
                record.id == *id && record.agent_id == scope.agent_id,
                "Task cache payload differs from normalized scope/ID"
            );
            facts.tasks.push(record);
        }
    }
    for id in &keys.timers {
        if let Some(payload) = current_payload_tx(tx, scope, "timers", "timer_id", "agent_id", id)?
        {
            let record = repositories::decode_timer_payload(&payload)?;
            ensure!(
                record.id == *id && record.agent_id == scope.agent_id,
                "Timer cache payload differs from normalized scope/ID"
            );
            facts.timers.push(record);
        }
    }
    for id in &keys.external_triggers {
        if let Some(payload) = current_payload_tx(
            tx,
            scope,
            "external_triggers",
            "external_trigger_id",
            "target_agent_id",
            id,
        )? {
            let record = repositories::decode_external_trigger_payload(&payload)?;
            ensure!(
                record.external_trigger_id == *id && record.target_agent_id == scope.agent_id,
                "ExternalTrigger cache payload differs from normalized scope/ID"
            );
            facts.external_triggers.push(record);
        }
    }
    Ok(facts)
}

/// Called only for initial receipt insertion, while every target must exist in
/// the original SQL view. Replay must not revalidate rows pruned since that ACK.
pub(crate) fn validate_original_install_keys_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &InputMutationInstallKeys,
) -> Result<()> {
    let facts = load_current_install_facts_tx(tx, scope, keys)?;
    ensure!(
        facts.work_items.len() == keys.work_items.len()
            && facts.tasks.len() == keys.tasks.len()
            && facts.timers.len() == keys.timers.len()
            && facts.external_triggers.len() == keys.external_triggers.len(),
        "input cache installation target is missing from original SQL ACK"
    );
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
