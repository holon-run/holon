//! Owner preparation and a storage-free terminal assembler's owning input cut.
#[cfg(test)]
#[path = "inputs_amendment_tests.rs"]
mod amendment_tests;
#[cfg(test)]
#[path = "inputs_tests.rs"]
mod tests;

use crate::{
    diagnostics::resource_io::{admit_cloned_value, admit_owned_metadata, ResourceReadMaterial},
    runtime::{
        resident_input_evidence::{
            CapturedInputEvidence, InputEvidenceFamily as Family, InputQueryRead,
            PreparedInputEvidence, ToolContextProjection,
        },
        resident_work::WorkCaptureRequest,
        work_reads::WorkReadSnapshot,
        RuntimeHandle,
    },
    runtime_db::{
        input_evidence_snapshot::InputEvidenceLoadRequest,
        input_preparation::turn_inputs::{TurnInputMembers, TurnInputSelector},
    },
    types::{BriefRecord, MessageEnvelope, TurnRecord},
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

pub(super) struct TurnRecordInputs {
    pub work: WorkReadSnapshot,
    inputs: CapturedInputEvidence,
    members: TurnInputMembers,
    pub material: ResourceReadMaterial,
}

/// Original Input, Work and acquisition remain owned through settlement.
pub(in crate::runtime) struct BuiltTurnRecord {
    record: TurnRecord,
    material: Option<ResourceReadMaterial>,
    _source: Option<Arc<TurnRecordInputs>>,
}

impl std::fmt::Debug for BuiltTurnRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.record.fmt(f)
    }
}
impl std::ops::Deref for BuiltTurnRecord {
    type Target = TurnRecord;
    fn deref(&self) -> &Self::Target {
        &self.record
    }
}
#[cfg(test)]
impl std::ops::DerefMut for BuiltTurnRecord {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.record
    }
}
impl BuiltTurnRecord {
    fn edit<T>(&mut self, f: impl FnOnce(&mut TurnRecord) -> Result<T>) -> Result<T> {
        // Re-enter the exact original acquisition before any amendment allocates.
        let material = self.material.clone();
        if let Some(material) = material {
            return material.read(|| f(&mut self.record));
        }
        #[cfg(test)]
        {
            f(&mut self.record)
        }
        #[cfg(not(test))]
        {
            anyhow::bail!("terminal acquisition absent")
        }
    }
    fn include_id(
        &mut self,
        id: &str,
        field: fn(&mut TurnRecord) -> &mut Vec<String>,
    ) -> Result<()> {
        self.edit(|record| {
            let ids = field(record);
            if !ids.iter().any(|old| old == id) {
                admit_cloned_value(&id)?;
                ids.push(id.to_owned());
            }
            Ok(())
        })
    }
    pub(in crate::runtime) fn include_brief(&mut self, id: &str) -> Result<()> {
        self.include_id(id, |record| &mut record.produced_brief_ids)
    }
    pub(in crate::runtime) fn include_completed_work_item(&mut self, id: &str) -> Result<()> {
        self.include_id(id, |record| &mut record.completed_work_item_ids)
    }
    pub(in crate::runtime) fn include_tool_execution(&mut self, id: &str) -> Result<()> {
        self.include_id(id, |record| &mut record.tool_execution_ids)
    }
    pub(in crate::runtime) fn include_wait_condition(&mut self, id: &str) -> Result<()> {
        self.include_id(id, |record| &mut record.waiting_condition_ids)
    }
    pub(in crate::runtime) fn clone_brief_ids(&self) -> Result<Vec<String>> {
        self.with_material(|| {
            admit_cloned_value(&self.record.produced_brief_ids)?;
            Ok(self.record.produced_brief_ids.clone())
        })
    }
    pub(in crate::runtime) fn set_terminal(
        &mut self,
        terminal: &crate::types::TurnTerminalRecord,
    ) -> Result<()> {
        self.edit(|record| {
            admit_cloned_value(terminal)?;
            record.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(terminal));
            Ok(())
        })
    }
    pub(in crate::runtime) fn set_runtime_failure(
        &mut self,
        failure: &crate::runtime_error::RuntimeErrorDescriptor,
    ) -> Result<()> {
        self.edit(|record| {
            admit_cloned_value(failure)?;
            record.runtime_failure = Some(failure.clone());
            Ok(())
        })
    }
    pub(in crate::runtime) fn with_material<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if let Some(material) = &self.material {
            return material.read(f);
        }
        // Only synthetic unit-test commands can have no acquisition.
        #[cfg(test)]
        {
            f()
        }
        #[cfg(not(test))]
        {
            anyhow::bail!("terminal acquisition absent")
        }
    }
    pub(in crate::runtime) fn clone_record(&self) -> Result<TurnRecord> {
        self.with_material(|| {
            admit_cloned_value(&self.record)?;
            Ok(self.record.clone())
        })
    }
    pub(in crate::runtime) fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            record: self.clone_record()?,
            material: self.material.clone(),
            _source: self._source.clone(),
        })
    }
    #[cfg(test)]
    pub(in crate::runtime) fn fixture(record: TurnRecord) -> Self {
        Self {
            record,
            material: None,
            _source: None,
        }
    }
}

impl TurnRecordInputs {
    fn body(&self, family: Family, id: &str) -> Result<&PreparedInputEvidence> {
        self.inputs
            .records
            .iter()
            .find(|record| record.locator.key.family == family && record.locator.key.id == id)
            .map(|record| &record.record)
            .with_context(|| format!("terminal Input body not qualified: {family:?}/{id}"))
    }
    fn admit_borrowed_vector<T>(&self, count: usize) -> Result<()> {
        let bytes = count
            .checked_mul(std::mem::size_of::<&T>())
            .context("terminal borrowed vector overflow")?;
        Ok(admit_owned_metadata(u64::try_from(bytes)?, count)?)
    }
    pub(super) fn messages(&self) -> Result<Vec<&MessageEnvelope>> {
        self.admit_borrowed_vector::<MessageEnvelope>(self.members.messages.len())?;
        self.members
            .messages
            .iter()
            .map(|id| match self.body(Family::Message, id)? {
                PreparedInputEvidence::Message(message) => Ok(message),
                _ => anyhow::bail!("terminal Message family differs"),
            })
            .collect()
    }
    pub(super) fn briefs(&self) -> Result<Vec<&BriefRecord>> {
        self.admit_borrowed_vector::<BriefRecord>(self.members.briefs.len())?;
        self.members
            .briefs
            .iter()
            .map(|id| match self.body(Family::Brief, id)? {
                PreparedInputEvidence::Brief(brief) => Ok(brief),
                _ => anyhow::bail!("terminal Brief family differs"),
            })
            .collect()
    }
    pub(super) fn tools(&self) -> Result<Vec<&ToolContextProjection>> {
        self.admit_borrowed_vector::<ToolContextProjection>(self.members.tools.len())?;
        self.members
            .tools
            .iter()
            .map(|id| match self.body(Family::ToolExecution, id)? {
                PreparedInputEvidence::ToolExecution(tool) => Ok(tool),
                _ => anyhow::bail!("terminal Tool family differs"),
            })
            .collect()
    }
    pub(super) fn turn(&self, id: &str) -> Result<Option<&TurnRecord>> {
        match self
            .inputs
            .records
            .iter()
            .find(|record| record.locator.key.family == Family::Turn && record.locator.key.id == id)
        {
            Some(record) => match &record.record {
                PreparedInputEvidence::Turn(turn) => Ok(Some(turn)),
                _ => anyhow::bail!("terminal Turn family differs"),
            },
            None if self.members.absent_turns.iter().any(|absent| absent == id)
                || self
                    .inputs
                    .unavailable
                    .iter()
                    .any(|key| key.family == Family::Turn && key.id == id) =>
            {
                Ok(None)
            }
            None => anyhow::bail!("terminal Turn was not prepared: {id}"),
        }
    }
    pub(super) fn own(self, record: TurnRecord) -> Result<BuiltTurnRecord> {
        self.material.read(|| {
            Ok(admit_owned_metadata(
                u64::try_from(std::mem::size_of::<Self>() + 32)?,
                1,
            )?)
        })?;
        Ok(BuiltTurnRecord {
            record,
            material: Some(self.material.clone()),
            _source: Some(Arc::new(self)),
        })
    }
}

impl RuntimeHandle {
    // This owns a producer's command value, not qualified Input membership.
    // The acceptance harness still follows its explicit fixture writer path.
    pub(in crate::runtime) async fn own_produced_turn_record(
        &self,
        record: TurnRecord,
    ) -> Result<BuiltTurnRecord> {
        let owner = self.begin_mutation().await?;
        let material = ResourceReadMaterial::current_or_new(
            &self.inner.input_evidence_budget,
            &owner.input_evidence_scope(),
        )?;
        material.read(|| admit_cloned_value(&record))?;
        Ok(BuiltTurnRecord {
            record,
            material: Some(material),
            _source: None,
        })
    }

    pub(super) async fn prepare_turn_record_read(&self, turn_id: &str) -> Result<TurnRecordInputs> {
        let owner = self.begin_mutation().await?;
        let scope = owner.input_evidence_scope();
        let material =
            ResourceReadMaterial::current_or_new(&self.inner.input_evidence_budget, &scope)?;
        let source_message_id = material.read(|| {
            let source = owner
                .state
                .current_execution_binding
                .as_ref()
                .map(|binding| &binding.source_message_id);
            admit_cloned_value(&source)?;
            Ok(source.cloned())
        })?;
        let (_, record_limit, byte_limit) = owner.context_input_evidence_policy()?;
        let request = InputEvidenceLoadRequest {
            query: Some(InputQueryRead {
                selection: Default::default(),
                ledger: self.inner.input_evidence_budget.clone(),
            }),
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        // A missing original all-status Wait relation must not cause Work SQL.
        let work_request = material.read(|| {
            admit_cloned_value(&turn_id)?;
            let mut request = WorkCaptureRequest::current();
            request.include_live = false;
            request.nonopen_limit = 0;
            request.required_turn_ids.insert(turn_id.to_owned());
            Ok(request)
        })?;
        let work = material.read(|| owner.capture_work_read(&work_request))?;
        let (members, inputs) = owner.prepare_terminal_inputs(
            &request,
            &TurnInputSelector {
                turn_id,
                source_message_id: source_message_id.as_deref(),
            },
            &material,
        )?;
        material.read(|| {
            ensure!(
                inputs.scope == scope,
                "terminal prepared Input scope differs"
            );
            Ok(TurnRecordInputs {
                work,
                inputs,
                members,
                material: material.clone(),
            })
        })
    }
}
