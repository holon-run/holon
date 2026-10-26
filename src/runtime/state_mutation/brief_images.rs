//! Exact recent-64 membership and full Tool loans share the original owner/TX.
use super::*;
use crate::{
    diagnostics::resource_io::{admit_cloned_value, charge_owned_metadata, ResourceReadMaterial},
    runtime::resident_input_evidence::{
        CapturedInputEvidence, InputEvidenceFamily as Family, InputEvidenceKey as Key,
        InputQueryRead, PreparedInputEvidence, ToolContextProjection,
    },
    runtime_db::{
        context_inputs::ContextDurableRead,
        input_evidence_snapshot::{
            prepare_input_evidence_with_scope_read_tx, qualified_tool_body_on,
            InputEvidenceBootstrapSnapshot, InputEvidenceLoadRequest,
        },
    },
    types::{ToolExecutionRecord, ToolExecutionStatus},
};

impl RuntimeMutation {
    pub(in crate::runtime) fn prepare_brief_image_inputs(
        &self,
        turn_id: &str,
        material: &ResourceReadMaterial,
    ) -> Result<(Vec<ToolExecutionRecord>, CapturedInputEvidence)> {
        let root = self.capture_published_root()?;
        anyhow::ensure!(
            root.state.as_ref() == &self.candidate.state,
            "image attachments cannot consume a private candidate"
        );
        let scope = self.input_evidence_scope();
        let budget = &self.runtime.inner.input_evidence_budget;
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        material.read(|| {
            self.runtime.inner.runtime_db.transaction(|tx| {
                let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
                anyhow::ensure!(
                    read.facts().state.as_deref() == Some(root.state.as_ref()),
                    "image attachments differ from the held State"
                );
                // LIMIT precedes the Turn/status/name filter, as in the original reader.
                let (count, bytes): (usize, u64) = tx.query_row(
                    "SELECT COUNT(*),COALESCE(SUM(length(CAST(evidence_id AS BLOB))),0)
                     FROM (SELECT evidence_id FROM tool_executions WHERE agent_id=?1
                           ORDER BY created_at DESC,evidence_id DESC LIMIT 64)",
                    [&scope.agent_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                anyhow::ensure!(count <= record_limit, "image Tool membership exceeds policy");
                let slots = count
                    .checked_mul(std::mem::size_of::<String>())
                    .context("image Tool membership overflow")?;
                charge_owned_metadata(
                    bytes.checked_add(u64::try_from(slots)?).context("image Tool bytes overflow")?,
                    count,
                )?;
                let mut statement = tx.prepare(
                    "SELECT evidence_id FROM tool_executions WHERE agent_id=?1
                     ORDER BY created_at DESC,evidence_id DESC LIMIT 64",
                )?;
                let ids = statement
                    .query_map([&scope.agent_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut request = InputEvidenceLoadRequest {
                    query: Some(InputQueryRead {
                        selection: Default::default(),
                        ledger: budget.clone(),
                    }),
                    windows: Vec::new(),
                    mandatory_roots: Default::default(),
                    interrupted_sources: Default::default(),
                    selected_completion_brief_ids: Default::default(),
                    record_limit,
                    byte_limit,
                };
                for id in &ids {
                    admit_cloned_value(&(Family::ToolExecution, id))?;
                    request.query.as_mut().unwrap().selection.required_keys
                        .insert(Key::new(Family::ToolExecution, id));
                }
                let original_cleanup = read.facts().identity.as_ref().is_some_and(|identity| {
                    identity.status == crate::types::AgentRegistryStatus::Deleting
                });
                let snapshot = prepare_input_evidence_with_scope_read_tx(
                    tx, &scope, request.borrowed(), original_cleanup, read,
                )?;
                let InputEvidenceBootstrapSnapshot::Current(snapshot) = snapshot else {
                    anyhow::bail!("image attachment scope retired during preparation");
                };
                let inputs = snapshot.resident.with_budget(budget.clone())?
                    .capture_for_request(&request)?;
                let selected = |record: &ToolContextProjection| {
                    record.status == ToolExecutionStatus::Success
                        && record.turn_id.as_deref() == Some(turn_id)
                        && record.tool_name == crate::tool::names::GENERATE_IMAGE
                };
                let count = inputs.records.iter().filter(|accepted| {
                    matches!(&accepted.record, PreparedInputEvidence::ToolExecution(tool) if selected(tool))
                }).count();
                charge_owned_metadata(
                    u64::try_from(count.checked_mul(std::mem::size_of::<ToolExecutionRecord>())
                        .context("image Tool vector overflow")?)?,
                    count,
                )?;
                let mut tools = Vec::with_capacity(count);
                for id in ids {
                    let accepted = inputs.records.iter()
                        .find(|accepted| accepted.locator.key.family == Family::ToolExecution
                            && accepted.locator.key.id == id)
                        .context("image Tool body was not qualified")?;
                    if let PreparedInputEvidence::ToolExecution(tool) = &accepted.record {
                        if selected(tool) {
                            tools.push(qualified_tool_body_on(tx, accepted)?);
                        }
                    }
                }
                // This finite read neither installs nor replaces any published domain.
                Ok((tools, inputs))
            })
        })
    }
}
