//! Finite current execution inputs for original Work/Wait command planning.
use super::*;
use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, WorkItemExecutionRecord,
    },
    runtime::mutation_owner::PublicationToken,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExecutionCommandReadError {
    #[error("ResourceExhausted: execution command material exceeds its shared read allowance")]
    ResourceExhausted,
    #[error("registered Work execution has been pruned: {0}")]
    PrunedWork(String),
}

/// This short borrow ends with the held owner; it is never stored in a plan.
/// The selected result contains no resident-map, Root or runtime back-reference.
pub(crate) struct ExecutionCommandReadSource<'a> {
    inputs: &'a ResidentInputEvidence,
    token: PublicationToken,
    ledger: &'a InputEvidenceBudgetLedger,
    records: usize,
    bytes: usize,
}
#[derive(Debug)]
pub(crate) struct ExecutionCommandRead {
    scope: InputEvidenceScope,
    token: PublicationToken,
    initialized: bool,
    open: Option<Arc<QualifiedInputEvidence>>,
    work: Option<(
        String,
        ExecutionWorkPresence,
        Option<Arc<QualifiedInputEvidence>>,
    )>,
    _owned: [Arc<BudgetViewGuard>; 2],
    _pins: Arc<BudgetViewGuard>,
}
impl ResidentInputEvidence {
    pub(crate) fn execution_command_source<'a>(
        &'a self,
        token: PublicationToken,
        ledger: &'a InputEvidenceBudgetLedger,
        records: usize,
        bytes: usize,
    ) -> ExecutionCommandReadSource<'a> {
        ExecutionCommandReadSource {
            inputs: self,
            token,
            ledger,
            records: records.min(65_536),
            bytes: bytes.min(64 * 1024 * 1024),
        }
    }
}
impl ExecutionCommandReadSource<'_> {
    /// A missing observation requests explicit owner preparation, never absence.
    pub(crate) fn requires_work_preparation(&self, id: &str) -> bool {
        self.inputs.coverage.execution.initialized
            && !self.inputs.coverage.execution.work_items.contains_key(id)
    }

    pub(crate) fn work_preparation_selector(
        &self,
        id: &str,
    ) -> Result<(InputEvidenceKey, Arc<BudgetViewGuard>)> {
        ensure!(!id.is_empty(), "execution command Work ID is empty");
        let bytes = serialized_bytes(&(self.inputs.scope(), id))?
            .checked_add(512)
            .and_then(|size| size.checked_mul(8))
            .context("execution preparation selector overflow")?;
        self.bound(bytes, 16)?;
        let guard = self.ledger.register_owned_bytes(
            self.inputs.scope(),
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        Ok((
            InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, id),
            guard,
        ))
    }

    pub(crate) fn capture(
        &self,
        work_id: Option<&str>,
        command_addresses: &[&str],
    ) -> Result<Arc<ExecutionCommandRead>> {
        let inputs = self.inputs;
        ensure!(
            self.token.identity_incarnation == inputs.scope.identity_incarnation,
            "execution command publication incarnation differs"
        );
        let execution = &inputs.coverage.execution;
        ensure!(
            execution.open_attempt_ids.len() <= 1,
            "multiple current execution attempts"
        );
        let selector_bytes = serialized_bytes(&(
            &inputs.scope,
            work_id,
            command_addresses,
            execution.open_attempt_ids.iter().next(),
        ))?
        .checked_add(512)
        .and_then(|bytes| bytes.checked_mul(8))
        .context("execution command selector material overflow")?;
        let selector_records = command_addresses
            .len()
            .checked_add(2)
            .and_then(|count| count.checked_mul(8))
            .context("execution command selector count overflow")?;
        self.bound(selector_bytes, selector_records)?;
        let selector_guard = self.ledger.register_owned_bytes(
            &inputs.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: selector_bytes,
            },
        )?;
        if !execution.initialized {
            ensure!(
                !inputs.records.values().any(|evidence| matches!(
                    &evidence.record,
                    PreparedInputEvidence::Attempt(_)
                        | PreparedInputEvidence::Outcome(_)
                        | PreparedInputEvidence::WorkExecution { .. }
                )),
                "uninitialized execution partition contains qualified records"
            );
            ensure!(
                execution.open_attempt_ids.is_empty()
                    && execution.nonterminal_work_item_ids.is_empty()
                    && execution.sources.values().all(
                        |source| source.allocation_highwater == 0 && source.attempts.is_empty()
                    )
                    && execution
                        .work_items
                        .values()
                        .all(|work| work.presence == ExecutionWorkPresence::Absent),
                "uninitialized execution partition contains current metadata"
            );
        }
        // Borrow existing keys until all selected/owned material has an admission.
        let find = |family, id: &str| {
            inputs
                .records
                .iter()
                .find(|(key, _)| key.family == family && key.id == id)
                .map(|(_, record)| record)
        };
        let open = execution
            .open_attempt_ids
            .iter()
            .next()
            .map(|id| {
                let record = find(InputEvidenceFamily::Attempt, id).ok_or_else(|| {
                    InputEvidenceReadError::NeedsPreparation(InputEvidenceKey::new(
                        InputEvidenceFamily::Attempt,
                        id,
                    ))
                })?;
                ensure!(
                    record.locator.scope == inputs.scope
                        && record.locator.key.family == InputEvidenceFamily::Attempt
                        && record.locator.key.id == *id,
                    "current execution locator differs"
                );
                ensure!(
                    matches!(&record.record, PreparedInputEvidence::Attempt(attempt)
                if attempt.state == ExecutionAttemptState::Open && attempt.attempt_id == *id
                && attempt.agent_id == inputs.scope.agent_id),
                    "current execution frontier differs from its qualified body"
                );
                Ok::<_, anyhow::Error>(record)
            })
            .transpose()?;
        ensure!(
            !inputs
                .records
                .values()
                .any(|evidence| matches!(&evidence.record,
            PreparedInputEvidence::Attempt(attempt) if attempt.state == ExecutionAttemptState::Open
            && !execution.open_attempt_ids.contains(&attempt.attempt_id))),
            "qualified open execution is absent from the current frontier"
        );
        let work = work_id.map(|id| -> Result<_> {
            ensure!(!id.is_empty(), "execution command Work ID is empty");
            let presence = if execution.initialized {
                execution.work_items.get(id).ok_or_else(|| {
                    InputEvidenceReadError::NeedsPreparation(InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, id))
                })?.presence
            } else { ExecutionWorkPresence::Absent };
            let body = find(InputEvidenceFamily::WorkExecution, id);
            match presence {
                ExecutionWorkPresence::Present => {
                    let evidence = body.ok_or_else(|| InputEvidenceReadError::NeedsPreparation(
                        InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, id)))?;
                    ensure!(evidence.locator.scope == inputs.scope
                        && evidence.locator.key.id == id
                        && matches!(&evidence.record, PreparedInputEvidence::WorkExecution {work_item_id,..} if work_item_id == id),
                        "execution command Work locator differs");
                }
                ExecutionWorkPresence::Absent | ExecutionWorkPresence::Pruned => {
                    ensure!(body.is_none(), "negative execution Work observation has a body");
                }
            }
            Ok((id, presence, body))
        }).transpose()?;
        let selected_count = usize::from(open.is_some())
            + usize::from(work.as_ref().is_some_and(|(_, _, body)| body.is_some()));
        let record_bytes = open
            .into_iter()
            .chain(work.as_ref().and_then(|(_, _, body)| *body))
            .try_fold(0u64, |sum, evidence| -> Result<u64> {
                Ok(sum
                    .checked_add(u64::try_from(evidence.resident_bytes)?)
                    .context("execution command selected bytes overflow")?)
            })?;
        // Eight copies cover this selected read and its finite original command,
        // caller clone, reducer/readback and returned plan until the original ACK.
        let copy_bytes = record_bytes
            .checked_mul(8)
            .context("execution command copies overflow")?;
        let records = selector_records
            .checked_add(
                selected_count
                    .checked_mul(8)
                    .context("execution command body count overflow")?,
            )
            .context("execution command material count overflow")?;
        self.bound(
            selector_bytes
                .checked_add(copy_bytes)
                .context("execution command combined bytes overflow")?,
            records,
        )?;
        let command_guard = self.ledger.register_owned_bytes(
            &inputs.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: copy_bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let selected = open
            .into_iter()
            .chain(work.as_ref().and_then(|(_, _, body)| *body))
            .cloned()
            .collect::<Vec<_>>();
        let pins =
            self.ledger
                .register_view(&inputs.scope, InputMemoryCategory::Pins, &selected)?;
        Ok(Arc::new(ExecutionCommandRead {
            scope: inputs.scope.clone(),
            token: self.token,
            initialized: execution.initialized,
            open: open.cloned(),
            work: work.map(|(id, presence, body)| (id.to_string(), presence, body.cloned())),
            _owned: [selector_guard, command_guard],
            _pins: pins,
        }))
    }
    fn bound(&self, bytes: u64, records: usize) -> Result<()> {
        ensure!(
            bytes <= self.bytes as u64 && records <= self.records,
            ExecutionCommandReadError::ResourceExhausted
        );
        Ok(())
    }
}
impl ExecutionCommandRead {
    pub(crate) fn is_initialized(&self) -> bool {
        self.initialized
    }
    pub(crate) fn validate_scope(&self, agent: &str) -> Result<()> {
        ensure!(
            self.scope.agent_id == agent
                && self.scope.identity_incarnation == self.token.identity_incarnation,
            "execution command scope differs"
        );
        Ok(())
    }
    pub(crate) fn open_attempt(&self) -> Option<&ExecutionAttempt> {
        self.open
            .as_ref()
            .and_then(|evidence| match &evidence.record {
                PreparedInputEvidence::Attempt(attempt) => Some(attempt),
                _ => None,
            })
    }
    pub(crate) fn work_execution(&self, id: &str) -> Result<Option<&WorkItemExecutionRecord>> {
        let (_, presence, body) = self
            .work
            .as_ref()
            .filter(|(selected, _, _)| selected == id)
            .ok_or_else(|| {
                InputEvidenceReadError::NeedsPreparation(InputEvidenceKey::new(
                    InputEvidenceFamily::WorkExecution,
                    id,
                ))
            })?;
        match presence {
            ExecutionWorkPresence::Absent => Ok(None),
            ExecutionWorkPresence::Pruned => {
                anyhow::bail!(ExecutionCommandReadError::PrunedWork(id.to_owned()))
            }
            ExecutionWorkPresence::Present => match body.as_ref().map(|body| &body.record) {
                Some(PreparedInputEvidence::WorkExecution { record, .. }) => Ok(Some(record)),
                _ => anyhow::bail!("selected execution Work has no qualified body"),
            },
        }
    }
}

#[cfg(test)]
#[path = "execution_command_reads_tests.rs"]
mod tests;
