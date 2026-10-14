//! Accepted query acquisition and owned Scheduling wire material.
use super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
};
use super::resident_scheduling::{
    CanonicalSchedulingFact, CapturedScheduling, SchedulingCaptureRequest, SchedulingFamily,
    SchedulingKey, SchedulingReadError,
};
use super::*;
use crate::runtime_error::RuntimeError;

pub(crate) struct SchedulingReadMaterial {
    _selected: Option<CapturedScheduling>,
    _owned: Arc<BudgetViewGuard>,
    _selector: Option<Arc<BudgetViewGuard>>,
    _read: Vec<Arc<BudgetViewGuard>>,
}
pub(crate) struct SchedulingRecordsRead {
    pub token: super::mutation_owner::PublicationToken,
    pub state: Arc<AgentState>,
    pub timers: Vec<TimerRecord>,
    pub external_triggers: Vec<ExternalTriggerRecord>,
    material: SchedulingReadMaterial,
}
impl SchedulingRecordsRead {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<TimerRecord>,
        Vec<ExternalTriggerRecord>,
        SchedulingReadMaterial,
    ) {
        (self.timers, self.external_triggers, self.material)
    }
}
pub(crate) struct TimerRecordRead {
    selected: CapturedScheduling,
    key: SchedulingKey,
    _owned: Arc<BudgetViewGuard>,
    _selector: Arc<BudgetViewGuard>,
}
impl TimerRecordRead {
    pub(crate) fn record(&self) -> Result<Option<&TimerRecord>> {
        self.selected.timer(&self.key.id)
    }
}

pub(crate) fn project_selected_scheduling_records(
    selected: CapturedScheduling,
    timer_limit: usize,
    ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    byte_limit: usize,
    record_limit: usize,
    prior: (usize, usize),
    read_guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<(
    Vec<TimerRecord>,
    Vec<ExternalTriggerRecord>,
    SchedulingReadMaterial,
)> {
    project_scheduling_records(
        selected,
        timer_limit,
        ledger,
        byte_limit,
        record_limit,
        prior,
        read_guards,
        false,
    )
}

pub(crate) fn project_cold_scheduling_records(
    selected: CapturedScheduling,
    timer_limit: usize,
    ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    byte_limit: usize,
    record_limit: usize,
    prior: (usize, usize),
    read_guards: Vec<Arc<BudgetViewGuard>>,
) -> Result<(
    Vec<TimerRecord>,
    Vec<ExternalTriggerRecord>,
    SchedulingReadMaterial,
)> {
    project_scheduling_records(
        selected,
        timer_limit,
        ledger,
        byte_limit,
        record_limit,
        prior,
        read_guards,
        true,
    )
}

fn project_scheduling_records(
    selected: CapturedScheduling,
    timer_limit: usize,
    ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    byte_limit: usize,
    record_limit: usize,
    prior: (usize, usize),
    read_guards: Vec<Arc<BudgetViewGuard>>,
    cold: bool,
) -> Result<(
    Vec<TimerRecord>,
    Vec<ExternalTriggerRecord>,
    SchedulingReadMaterial,
)> {
    anyhow::ensure!(
        timer_limit <= super::resident_scheduling::SCHEDULING_HISTORY_WINDOW,
        SchedulingReadError::NeedsHistory
    );
    let mut count = 0usize;
    let mut bytes = 512u64;
    for record in &selected.records {
        match &record.fact {
            CanonicalSchedulingFact::Timer(timer) if timer_limit > 0 => {
                bytes = bytes
                    .checked_add(serialized_bytes(timer)?)
                    .context("Timer output bytes overflow")?;
                count += 1;
            }
            CanonicalSchedulingFact::ExternalTrigger(trigger) => {
                bytes = bytes
                    .checked_add(serialized_bytes(trigger)?)
                    .context("Trigger output bytes overflow")?;
                count += 1;
            }
            _ => {}
        }
    }
    let bytes = bytes
        .checked_add(
            u64::try_from(count)?
                .checked_mul(128)
                .context("Scheduling output slots overflow")?,
        )
        .and_then(|n| n.checked_mul(4))
        .context("Scheduling output copies overflow")?;
    let records = count
        .checked_add(1)
        .and_then(|n| n.checked_mul(4))
        .context("Scheduling output records overflow")?;
    let (retained, retained_records) = selected.selected_material_estimate()?;
    anyhow::ensure!(
        u64::try_from(prior.0)?
            .checked_add(retained)
            .and_then(|n| n.checked_add(bytes))
            .is_some_and(|n| n <= u64::try_from(byte_limit.min(64 * 1024 * 1024)).unwrap_or(0))
            && prior
                .1
                .checked_add(retained_records)
                .and_then(|n| n.checked_add(records))
                .is_some_and(|n| n <= record_limit.min(65536)),
        SchedulingReadError::ResourceExhausted(
            "cold read/selected Scheduling/output combined material"
        )
    );
    let owned = ledger.register_owned_bytes(
        &selected.scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: bytes,
            metadata_estimate_bytes: 0,
        },
    )?;
    let selected_history = |family: SchedulingFamily, key: &SchedulingKey| {
        selected
            .coverage
            .history
            .get(&family)
            .is_some_and(|history| {
                let keys = if cold {
                    &history.keys
                } else {
                    &history.loaded_keys
                };
                keys.contains(key)
            })
    };
    let mut timers = Vec::new();
    let mut external_triggers = Vec::new();
    for record in &selected.records {
        match &record.fact {
            CanonicalSchedulingFact::Timer(timer)
                if timer_limit > 0
                    && (selected
                        .coverage
                        .active_timers
                        .contains(&record.locator.key)
                        || selected_history(SchedulingFamily::Timer, &record.locator.key)) =>
            {
                timers.push(timer.clone())
            }
            CanonicalSchedulingFact::ExternalTrigger(trigger)
                if selected
                    .coverage
                    .active_triggers
                    .contains(&record.locator.key)
                    || selected_history(SchedulingFamily::ExternalTrigger, &record.locator.key) =>
            {
                external_triggers.push(trigger.clone())
            }
            _ => {}
        }
    }
    if cold {
        // Match the original repository's selected set, then its reverse step.
        timers.sort_by(|a, b| {
            crate::runtime_db::repositories::timestamp(
                crate::runtime_db::repositories::timer_updated_at(b),
            )
            .cmp(&crate::runtime_db::repositories::timestamp(
                crate::runtime_db::repositories::timer_updated_at(a),
            ))
            .then_with(|| {
                crate::runtime_db::repositories::timestamp(b.created_at)
                    .cmp(&crate::runtime_db::repositories::timestamp(a.created_at))
            })
            .then_with(|| a.id.cmp(&b.id))
        });
        timers.truncate(timer_limit);
        timers.reverse();
        external_triggers.sort_by(|a, b| {
            crate::runtime_db::repositories::timestamp(b.created_at)
                .cmp(&crate::runtime_db::repositories::timestamp(a.created_at))
                .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
        });
    } else {
        timers.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        timers.truncate(timer_limit);
        external_triggers.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
        });
    }
    Ok((
        timers,
        external_triggers,
        SchedulingReadMaterial {
            _selected: Some(selected),
            _owned: owned,
            _selector: None,
            _read: read_guards,
        },
    ))
}
impl SchedulingReadMaterial {
    pub(crate) fn active_timer_count(&self) -> Result<usize> {
        let Some(selected) = self._selected.as_ref() else {
            return Ok(0);
        };
        anyhow::ensure!(
            selected.coverage.active_timers_complete,
            SchedulingReadError::IncompleteLive
        );
        Ok(selected.records.iter().filter(|record|
            selected.coverage.active_timers.contains(&record.locator.key)
            && matches!(&record.fact, CanonicalSchedulingFact::Timer(timer) if timer.status==TimerStatus::Active)
        ).count())
    }

    pub(crate) fn proven_empty_cold(
        scope: &super::resident_input_evidence::InputEvidenceScope,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        byte_limit: usize,
        record_limit: usize,
        usage: (usize, usize),
        read_guards: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<Self> {
        anyhow::ensure!(
            usage
                .0
                .checked_add(2048)
                .is_some_and(|n| n <= byte_limit.min(64 * 1024 * 1024))
                && usage
                    .1
                    .checked_add(4)
                    .is_some_and(|n| n <= record_limit.min(65536)),
            SchedulingReadError::ResourceExhausted("empty cold Scheduling output material")
        );
        let owned = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 2048,
                metadata_estimate_bytes: 0,
            },
        )?;
        Ok(Self {
            _selected: None,
            _owned: owned,
            _selector: None,
            _read: read_guards,
        })
    }
}
impl RuntimeHandle {
    pub(super) fn scheduling_observation_material(
        &self,
        selected: CapturedScheduling,
    ) -> Result<SchedulingReadMaterial> {
        // This view is used only for scalar closure/posture observations.
        let owned = self.scheduling_output_budget(&selected, 512, 1, 0, 0)?;
        Ok(SchedulingReadMaterial {
            _selected: Some(selected),
            _owned: owned,
            _selector: None,
            _read: Vec::new(),
        })
    }

    pub(crate) fn scheduling_publication_token(
        &self,
    ) -> Result<super::mutation_owner::PublicationToken> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Scheduling root unpublished")?;
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "Scheduling publication token scope differs"
        );
        Ok(root.token)
    }
    fn scheduling_output_budget(
        &self,
        selected: &CapturedScheduling,
        bytes: u64,
        count: usize,
        selector_bytes: u64,
        selector_records: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        let (retained, retained_count) = selected.selected_material_estimate()?;
        let limits = self.inner.input_evidence_budget.limits();
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("Scheduling output limit overflow")?
            .min(64 * 1024 * 1024);
        let record_limit = usize::try_from(limits.record_limit)?.min(65536);
        anyhow::ensure!(
            retained
                .checked_add(bytes)
                .and_then(|n| n.checked_add(selector_bytes))
                .is_some_and(|n| n <= byte_limit)
                && retained_count
                    .checked_add(count)
                    .and_then(|n| n.checked_add(selector_records))
                    .is_some_and(|n| n <= record_limit),
            SchedulingReadError::ResourceExhausted(
                "selected Scheduling/selector/output combined material"
            )
        );
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            &selected.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?)
    }

    fn scheduling_records_from_capture(
        &self,
        read: super::scheduling_reads::SchedulingReadSnapshot,
        timer_limit: usize,
    ) -> Result<SchedulingRecordsRead> {
        let limits = self.inner.input_evidence_budget.limits();
        let byte_limit = usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("Scheduling output limit overflow")?,
        )?;
        let (timers, external_triggers, material) = project_selected_scheduling_records(
            read.scheduling,
            timer_limit,
            &self.inner.input_evidence_budget,
            byte_limit,
            usize::try_from(limits.record_limit)?,
            (0, 0),
            Vec::new(),
        )?;
        Ok(SchedulingRecordsRead {
            token: read.token,
            state: read.state,
            timers,
            external_triggers,
            material,
        })
    }

    /// Pure loaded read; an absent domain/key remains typed preparation debt.
    pub(crate) async fn capture_scheduling_records_read(
        &self,
        timer_limit: usize,
    ) -> Result<SchedulingRecordsRead> {
        let read = self
            .capture_scheduling_read(&SchedulingCaptureRequest::current())
            .await?;
        self.scheduling_records_from_capture(read, timer_limit)
    }
    /// Explicit accepted list/output query boundary, with no late canonical adoption.
    pub(crate) async fn acquire_scheduling_records_read(
        &self,
        timer_limit: usize,
    ) -> Result<SchedulingRecordsRead> {
        anyhow::ensure!(
            timer_limit <= super::resident_scheduling::SCHEDULING_HISTORY_WINDOW,
            SchedulingReadError::NeedsHistory
        );
        let mut owner = self.begin_mutation().await?;
        let request = SchedulingCaptureRequest::current();
        owner.prepare_owner_scheduling_request(&request).await?;
        self.scheduling_records_from_capture(owner.capture_scheduling_read(&request)?, timer_limit)
    }

    /// Accepted exact query may prepare only original current/fixed qualification.
    pub(crate) async fn acquire_timer_record_read(&self, id: &str) -> Result<TimerRecordRead> {
        let mut owner = self.begin_mutation().await?;
        let (_, record_limit, byte_limit) = owner.context_input_evidence_policy()?;
        let scope = owner.input_evidence_scope();
        let selector_bytes = serialized_bytes(&id)?
            .checked_add(512)
            .and_then(|n| n.checked_mul(8))
            .context("Timer selector material overflow")?;
        anyhow::ensure!(
            selector_bytes <= u64::try_from(byte_limit.min(64 * 1024 * 1024))?
                && record_limit.min(65536) >= 8,
            SchedulingReadError::ResourceExhausted("Timer selector material")
        );
        let selector = self.inner.input_evidence_budget.register_owned_bytes(
            &scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: selector_bytes,
            },
        )?;
        let key = SchedulingKey::new(SchedulingFamily::Timer, id);
        let mut request = SchedulingCaptureRequest::current();
        request.required_keys.insert(key.clone());
        match owner.capture_scheduling_read(&request) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.downcast_ref::<SchedulingReadError>(),
                    Some(
                        SchedulingReadError::NeedsPreparation(_)
                            | SchedulingReadError::IncompleteLive
                            | SchedulingReadError::NeedsHistory
                    )
                ) =>
            {
                if let Err(error) = owner.prepare_owner_scheduling_request(&request).await {
                    if matches!(error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
                        Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::ForeignRecord(actual)) if actual==&key)
                    {
                        return Err(RuntimeError::not_found(
                            "timer_not_found",
                            format!("timer {id} not found"),
                        )
                        .with_safe_context("timer_id", id)
                        .into());
                    }
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
        let read = owner.capture_scheduling_read(&request)?;
        let bytes = serialized_bytes(&read.scheduling.timer(id)?)?
            .checked_add(512)
            .and_then(|n| n.checked_mul(4))
            .context("Timer wire material overflow")?;
        let owned = self.scheduling_output_budget(&read.scheduling, bytes, 4, selector_bytes, 8)?;
        Ok(TimerRecordRead {
            selected: read.scheduling,
            key,
            _owned: owned,
            _selector: selector,
        })
    }
}

#[cfg(test)]
#[path = "scheduling_output_tests.rs"]
mod tests;
