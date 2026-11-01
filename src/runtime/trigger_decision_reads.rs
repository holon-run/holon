//! Internal Trigger decisions borrow complete facts from one publication.
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_scheduling::{CapturedScheduling, SchedulingCaptureRequest, SchedulingReadError},
    resident_work::CapturedWork,
    timer_mutation_reads::selected_work_material,
    RuntimeHandle,
};
use crate::types::{AgentState, ExternalTriggerRecord, ExternalTriggerScope};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

fn owned_trigger_material(
    selected: &CapturedScheduling,
    work: Option<&CapturedWork>,
    owned_bytes: u64,
    owned_records: usize,
    ledger: &InputEvidenceBudgetLedger,
    record_limit: usize,
    byte_limit: usize,
) -> Result<Arc<BudgetViewGuard>> {
    let (mut bytes, mut records) = selected.selected_material_estimate()?;
    if let Some(work) = work {
        ensure!(work.scope == selected.scope, "Trigger/Work scopes differ");
        let (other_bytes, other_records) = selected_work_material(work)?;
        bytes = bytes
            .checked_add(other_bytes)
            .context("Trigger selected bytes overflow")?;
        records = records
            .checked_add(other_records)
            .context("Trigger selected records overflow")?;
    }
    ensure!(
        bytes
            .checked_add(owned_bytes)
            .is_some_and(|n| n <= byte_limit.min(64 * 1024 * 1024) as u64)
            && records
                .checked_add(owned_records)
                .is_some_and(|n| n <= record_limit.min(65_536)),
        SchedulingReadError::ResourceExhausted("selected domains and Trigger decision copies")
    );
    Ok(ledger.register_owned_bytes(
        &selected.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: owned_bytes,
            metadata_estimate_bytes: 0,
        },
    )?)
}

pub(in crate::runtime) struct ActiveTriggerDecisionRead {
    pub records: Vec<ExternalTriggerRecord>,
    pub material: Arc<BudgetViewGuard>,
}
impl ActiveTriggerDecisionRead {
    pub(in crate::runtime) fn from_selected(
        scheduling: &CapturedScheduling,
        work: &CapturedWork,
        ledger: &InputEvidenceBudgetLedger,
        records: usize,
        bytes: usize,
    ) -> Result<Self> {
        let trigger = scheduling.active_default_trigger()?;
        let owned = serialized_bytes(&trigger)?
            .checked_add(512)
            .and_then(|n| n.checked_mul(32))
            .context("Trigger projection copies overflow")?;
        let material =
            owned_trigger_material(scheduling, Some(work), owned, 32, ledger, records, bytes)?;
        Ok(Self {
            records: trigger.cloned().into_iter().collect(),
            material,
        })
    }
}

pub(in crate::runtime) struct CommandTriggerRead {
    pub url: Option<String>,
    pub material: Option<Arc<BudgetViewGuard>>,
}
impl CommandTriggerRead {
    pub(in crate::runtime) fn from_selected(
        scheduling: &CapturedScheduling,
        base_url: &str,
        ledger: &InputEvidenceBudgetLedger,
        records: usize,
        bytes: usize,
    ) -> Result<Self> {
        let trigger = scheduling.active_default_trigger()?;
        let token = trigger
            .filter(|trigger| trigger.scope == ExternalTriggerScope::Agent)
            .and_then(|trigger| trigger.token.as_deref());
        let Some(token) = token else {
            return Ok(Self {
                url: None,
                material: None,
            });
        };
        let owned = u64::try_from(base_url.len())?
            .checked_add(u64::try_from(token.len())?)
            .and_then(|n| n.checked_add(512))
            .and_then(|n| n.checked_mul(32))
            .context("Command Trigger URL copies overflow")?;
        let material = owned_trigger_material(scheduling, None, owned, 32, ledger, records, bytes)?;
        Ok(Self {
            url: Some(crate::callbacks::build_callback_url(
                base_url,
                &trigger.unwrap().delivery_mode,
                token,
            )),
            material: Some(material),
        })
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) async fn capture_command_trigger_read(
        &self,
        agent_id: &str,
    ) -> Result<(Arc<AgentState>, CommandTriggerRead)> {
        let owner = self.begin_mutation().await?;
        let read = owner.capture_scheduling_read(&SchedulingCaptureRequest {
            include_live: true,
            history_limit: 0,
            required_keys: Default::default(),
        })?;
        ensure!(
            read.state.id == agent_id && read.scheduling.scope.agent_id == agent_id,
            "Command Trigger scope differs"
        );
        let (_, records, bytes) = owner.context_input_evidence_policy()?;
        let trigger = CommandTriggerRead::from_selected(
            &read.scheduling,
            &self.inner.callback_base_url,
            &self.inner.input_evidence_budget,
            records,
            bytes,
        )?;
        Ok((read.state, trigger))
    }
}

#[cfg(test)]
#[path = "trigger_decision_reads_tests.rs"]
mod tests;
