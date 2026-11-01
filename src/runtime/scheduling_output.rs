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
use crate::types::ExternalTriggerStateSnapshot;

pub(crate) struct SchedulingReadMaterial {
    source: Arc<SchedulingReadSource>,
}
impl PartialEq for ExternalTriggerRecordLoan {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl PartialEq for TimerRecordLoan {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
pub(crate) struct SchedulingReadSource {
    _selected: Option<CapturedScheduling>,
    _owned: Arc<BudgetViewGuard>,
    _selector: Option<Arc<BudgetViewGuard>>,
    _read: Vec<Arc<BudgetViewGuard>>,
    _body: Option<Arc<BudgetViewGuard>>,
    _work_queue: std::sync::OnceLock<Arc<BudgetViewGuard>>,
    _scheduler_work: std::sync::OnceLock<Arc<BudgetViewGuard>>,
    _acquisition: std::sync::OnceLock<Arc<BudgetViewGuard>>,
}
impl std::ops::Deref for SchedulingReadMaterial {
    type Target = SchedulingReadSource;

    fn deref(&self) -> &Self::Target {
        &self.source
    }
}

/// Read-only Timer body backed by its complete original capture and material.
#[derive(Clone)]
pub struct TimerRecordLoan {
    source: Arc<SchedulingReadSource>,
    index: usize,
}
impl std::ops::Deref for TimerRecordLoan {
    type Target = TimerRecord;

    fn deref(&self) -> &Self::Target {
        timer_at(
            self.source._selected.as_ref().expect("Timer loan capture"),
            self.index,
        )
    }
}
impl std::fmt::Debug for TimerRecordLoan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, formatter)
    }
}
impl serde::Serialize for TimerRecordLoan {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&**self, serializer)
    }
}
impl PartialEq<TimerRecord> for TimerRecordLoan {
    fn eq(&self, other: &TimerRecord) -> bool {
        &**self == other
    }
}

/// Read-only Trigger body; cloning a loan retains the complete original source.
#[derive(Clone)]
pub struct ExternalTriggerRecordLoan {
    source: Arc<SchedulingReadSource>,
    index: usize,
}
impl std::ops::Deref for ExternalTriggerRecordLoan {
    type Target = ExternalTriggerRecord;

    fn deref(&self) -> &Self::Target {
        let selected = self
            .source
            ._selected
            .as_ref()
            .expect("Trigger loan capture");
        let CanonicalSchedulingFact::ExternalTrigger(record) = &selected.records[self.index].fact
        else {
            unreachable!("Trigger loan family");
        };
        record
    }
}
impl std::fmt::Debug for ExternalTriggerRecordLoan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, formatter)
    }
}
impl serde::Serialize for ExternalTriggerRecordLoan {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&**self, serializer)
    }
}
impl PartialEq<ExternalTriggerRecord> for ExternalTriggerRecordLoan {
    fn eq(&self, other: &ExternalTriggerRecord) -> bool {
        &**self == other
    }
}
impl std::borrow::Borrow<ExternalTriggerRecord> for ExternalTriggerRecordLoan {
    fn borrow(&self) -> &ExternalTriggerRecord {
        self
    }
}
impl ExternalTriggerRecordLoan {
    pub(crate) fn state_snapshot(&self) -> Result<ExternalTriggerStateSnapshot> {
        external_trigger_state_snapshot(self)
    }
}
pub(super) fn external_trigger_state_snapshot(
    record: &ExternalTriggerRecord,
) -> Result<ExternalTriggerStateSnapshot> {
    crate::diagnostics::resource_io::admit_cloned_value(&(
        &record.external_trigger_id,
        &record.target_agent_id,
        &record.scope,
        &record.delivery_mode,
        &record.status,
        record.delivery_count,
        record.created_at,
        record.revoked_at,
        record.last_delivered_at,
    ))?;
    Ok(ExternalTriggerStateSnapshot {
        external_trigger_id: record.external_trigger_id.clone(),
        target_agent_id: record.target_agent_id.clone(),
        scope: record.scope.clone(),
        delivery_mode: record.delivery_mode.clone(),
        status: record.status.clone(),
        delivery_count: record.delivery_count,
        created_at: record.created_at,
        revoked_at: record.revoked_at,
        last_delivered_at: record.last_delivered_at,
    })
}
/// A read-only Timer whose original read material lives through consumption.
/// Serialization preserves the plain record, without material or scope fields.
pub struct OwnedTimerRead {
    record: TimerRecord,
    _material: TimerRecordRead,
}
impl std::ops::Deref for OwnedTimerRead {
    type Target = TimerRecord;

    fn deref(&self) -> &Self::Target {
        &self.record
    }
}
impl std::fmt::Debug for OwnedTimerRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.record, formatter)
    }
}
impl serde::Serialize for OwnedTimerRead {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.record, serializer)
    }
}
/// A read-only Timer list whose original material lives through consumption.
/// Serialization preserves the plain JSON array; no qualified scope is exposed.
pub struct OwnedTimersRead {
    records: Vec<TimerRecordLoan>,
    _material: SchedulingReadMaterial,
}
impl std::ops::Deref for OwnedTimersRead {
    type Target = [TimerRecordLoan];

    fn deref(&self) -> &Self::Target {
        &self.records
    }
}
impl std::fmt::Debug for OwnedTimersRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.records, formatter)
    }
}
impl serde::Serialize for OwnedTimersRead {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.records, serializer)
    }
}
/// A read-only Trigger list whose original material lives through consumption.
/// Serialization preserves the plain JSON array; no qualified scope is exposed.
pub struct OwnedExternalTriggersRead {
    records: Vec<ExternalTriggerRecordLoan>,
    _material: SchedulingReadMaterial,
}
impl std::ops::Deref for OwnedExternalTriggersRead {
    type Target = [ExternalTriggerRecordLoan];

    fn deref(&self) -> &Self::Target {
        &self.records
    }
}
impl std::fmt::Debug for OwnedExternalTriggersRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.records, formatter)
    }
}
impl serde::Serialize for OwnedExternalTriggersRead {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.records, serializer)
    }
}
pub(crate) struct SchedulingRecordsRead {
    pub token: super::mutation_owner::PublicationToken,
    pub state: Arc<AgentState>,
    pub timers: Vec<TimerRecordLoan>,
    pub external_triggers: Vec<ExternalTriggerRecordLoan>,
    material: SchedulingReadMaterial,
}
impl SchedulingRecordsRead {
    pub(crate) fn into_timers(self) -> OwnedTimersRead {
        OwnedTimersRead {
            records: self.timers,
            _material: self.material,
        }
    }

    pub(crate) fn into_external_triggers(self) -> OwnedExternalTriggersRead {
        OwnedExternalTriggersRead {
            records: self.external_triggers,
            _material: self.material,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<TimerRecordLoan>,
        Vec<ExternalTriggerRecordLoan>,
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

    pub(crate) fn into_owned_record(self) -> Result<Option<OwnedTimerRead>> {
        let record = self.record()?.cloned();
        Ok(record.map(|record| OwnedTimerRead {
            record,
            _material: self,
        }))
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
    Vec<TimerRecordLoan>,
    Vec<ExternalTriggerRecordLoan>,
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
    Vec<TimerRecordLoan>,
    Vec<ExternalTriggerRecordLoan>,
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

// Match repository UTC millisecond text, including signed expanded years and
// leap seconds. A DateTime comparison would retain discarded submilliseconds.
struct ColdTimestamp {
    bytes: [u8; 27],
    len: usize,
}

impl ColdTimestamp {
    fn new(value: DateTime<Utc>) -> Self {
        let mut key = Self {
            bytes: [0; 27],
            len: 0,
        };
        // Naive UTC avoids DateTime's owned offset name; write_to avoids the
        // DelayedFormat Display implementation's intermediate String.
        value
            .naive_utc()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .write_to(&mut key)
            .expect("Chrono UTC millisecond text fits 27 stack bytes");
        key
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl std::fmt::Write for ColdTimestamp {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(std::fmt::Error)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(std::fmt::Error)?
            .copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

fn compare_cold_timestamps(a: DateTime<Utc>, b: DateTime<Utc>) -> std::cmp::Ordering {
    ColdTimestamp::new(a)
        .as_bytes()
        .cmp(ColdTimestamp::new(b).as_bytes())
}

fn output_history_contains(
    selected: &CapturedScheduling,
    family: SchedulingFamily,
    key: &SchedulingKey,
    cold: bool,
) -> bool {
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
}

fn timer_at(selected: &CapturedScheduling, index: usize) -> &TimerRecord {
    let CanonicalSchedulingFact::Timer(timer) = &selected.records[index].fact else {
        unreachable!("Timer selection family");
    };
    timer
}

// Select before owning any output body. The bounded scratch is stack-only;
// selected continues to own every qualified source until the final wire holder.
fn select_timer_output(
    selected: &CapturedScheduling,
    limit: usize,
    cold: bool,
) -> [Option<usize>; super::resident_scheduling::SCHEDULING_HISTORY_WINDOW] {
    let mut timers = [None; super::resident_scheduling::SCHEDULING_HISTORY_WINDOW];
    let mut len = 0usize;
    if limit == 0 {
        return timers;
    }
    for (index, record) in selected.records.iter().enumerate() {
        let CanonicalSchedulingFact::Timer(timer) = &record.fact else {
            continue;
        };
        if !selected
            .coverage
            .active_timers
            .contains(&record.locator.key)
            && !output_history_contains(
                selected,
                SchedulingFamily::Timer,
                &record.locator.key,
                cold,
            )
        {
            continue;
        }
        let position = timers[..len].partition_point(|candidate: &Option<usize>| {
            let candidate = timer_at(selected, candidate.expect("selected Timer slot"));
            let order = if cold {
                compare_cold_timestamps(
                    crate::runtime_db::repositories::timer_updated_at(candidate),
                    crate::runtime_db::repositories::timer_updated_at(timer),
                )
                .then_with(|| compare_cold_timestamps(candidate.created_at, timer.created_at))
            } else {
                candidate.created_at.cmp(&timer.created_at)
            };
            order
                .reverse()
                .then_with(|| candidate.id.cmp(&timer.id))
                .is_lt()
        });
        if position < limit {
            let next_len = (len + 1).min(limit);
            timers.copy_within(position..next_len - 1, position + 1);
            timers[position] = Some(index);
            len = next_len;
        }
    }
    timers
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
    Vec<TimerRecordLoan>,
    Vec<ExternalTriggerRecordLoan>,
    SchedulingReadMaterial,
)> {
    anyhow::ensure!(
        timer_limit <= super::resident_scheduling::SCHEDULING_HISTORY_WINDOW,
        SchedulingReadError::NeedsHistory
    );
    let selected_timers = select_timer_output(&selected, timer_limit, cold);
    let selected_trigger =
        |selected: &CapturedScheduling,
         record: &super::resident_scheduling::QualifiedSchedulingFact| {
            selected
                .coverage
                .active_triggers
                .contains(&record.locator.key)
                || output_history_contains(
                    selected,
                    SchedulingFamily::ExternalTrigger,
                    &record.locator.key,
                    cold,
                )
        };
    let timer_capacity = selected_timers.iter().flatten().count();
    let mut trigger_capacity = 0usize;
    let mut bytes = 512u64;
    for index in selected_timers.iter().flatten() {
        let timer = timer_at(&selected, *index);
        bytes = bytes
            .checked_add(serialized_bytes(timer)?)
            .context("Timer output bytes overflow")?;
    }
    for record in &selected.records {
        match &record.fact {
            CanonicalSchedulingFact::ExternalTrigger(trigger)
                if selected_trigger(&selected, record) =>
            {
                bytes = bytes
                    .checked_add(serialized_bytes(trigger)?)
                    .context("Trigger output bytes overflow")?;
                trigger_capacity += 1;
            }
            _ => {}
        }
    }
    let count = timer_capacity
        .checked_add(trigger_capacity)
        .context("Scheduling output count overflow")?;
    let slots = timer_capacity
        .checked_mul(std::mem::size_of::<TimerRecord>())
        .and_then(|bytes| {
            trigger_capacity
                .checked_mul(std::mem::size_of::<ExternalTriggerRecord>())
                .and_then(|triggers| bytes.checked_add(triggers))
        })
        .context("Scheduling output slots overflow")?;
    let bytes = bytes
        .checked_add(u64::try_from(slots)?)
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
    // Cold Host callers have no ambient phase. Keep this synchronous output
    // boundary explicit and retain the original allowance through consumption.
    let body_phase = cold
        .then(|| {
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(
                ledger,
                &selected.scope,
            )
        })
        .transpose()?;
    let _body_entry = body_phase.as_ref().map(|phase| phase.enter());
    let loan_slots = timer_capacity
        .checked_mul(std::mem::size_of::<TimerRecordLoan>())
        .and_then(|n| {
            trigger_capacity
                .checked_mul(std::mem::size_of::<ExternalTriggerRecordLoan>())
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_add(std::mem::size_of::<SchedulingReadSource>() + 128))
        .context("Scheduling loan slots overflow")?;
    crate::diagnostics::resource_io::charge_owned_metadata(
        u64::try_from(loan_slots)?,
        count
            .checked_add(1)
            .context("Scheduling loan count overflow")?,
    )?;
    // A detached handle retains all selected bodies and original read/Pins
    // guards, without constructing a second owning body.
    let source = Arc::new(SchedulingReadSource {
        _selected: Some(selected),
        _owned: owned,
        _selector: None,
        _read: read_guards,
        _body: body_phase.as_ref().map(|phase| phase.guard()).transpose()?,
        _work_queue: std::sync::OnceLock::new(),
        _scheduler_work: std::sync::OnceLock::new(),
        _acquisition: std::sync::OnceLock::new(),
    });
    let mut timers = Vec::new();
    let mut external_triggers = Vec::new();
    // Only final output handles enter these owning vectors.
    timers
        .try_reserve_exact(timer_capacity)
        .map_err(|_| SchedulingReadError::ResourceExhausted("Timer output allocation"))?;
    external_triggers
        .try_reserve_exact(trigger_capacity)
        .map_err(|_| SchedulingReadError::ResourceExhausted("Trigger output allocation"))?;
    for index in selected_timers.into_iter().flatten() {
        timers.push(TimerRecordLoan {
            source: source.clone(),
            index,
        });
    }
    let selected = source
        ._selected
        .as_ref()
        .expect("Scheduling output capture");
    for (index, record) in selected.records.iter().enumerate() {
        match &record.fact {
            CanonicalSchedulingFact::ExternalTrigger(_) if selected_trigger(selected, record) => {
                external_triggers.push(ExternalTriggerRecordLoan {
                    source: source.clone(),
                    index,
                })
            }
            _ => {}
        }
    }
    if cold {
        // Match the original repository's selected set, then its reverse step.
        // Capture selects unique family keys; the final ID makes ties total.
        timers.reverse();
        external_triggers.sort_unstable_by(|a, b| {
            compare_cold_timestamps(b.created_at, a.created_at)
                .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
        });
    } else {
        external_triggers.sort_unstable_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
        });
    }
    Ok((timers, external_triggers, SchedulingReadMaterial { source }))
}
impl SchedulingReadMaterial {
    pub(crate) fn with_acquisition_material(self, guard: Arc<BudgetViewGuard>) -> Result<Self> {
        self._acquisition
            .set(guard)
            .map_err(|_| anyhow!("Scheduling acquisition material already attached"))?;
        Ok(self)
    }

    pub(crate) fn with_scheduler_work_material(self, guard: Arc<BudgetViewGuard>) -> Result<Self> {
        self._scheduler_work
            .set(guard)
            .map_err(|_| anyhow!("Scheduling scheduler work material already attached"))?;
        Ok(self)
    }

    pub(crate) fn with_work_queue_material(self, guard: Arc<BudgetViewGuard>) -> Result<Self> {
        self._work_queue
            .set(guard)
            .map_err(|_| anyhow!("Scheduling Work queue material already attached"))?;
        Ok(self)
    }

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
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(std::mem::size_of::<SchedulingReadSource>() + 128)?,
            1,
        )?;
        Ok(Self {
            source: Arc::new(SchedulingReadSource {
                _selected: None,
                _owned: owned,
                _selector: None,
                _read: read_guards,
                _body: None,
                _work_queue: std::sync::OnceLock::new(),
                _scheduler_work: std::sync::OnceLock::new(),
                _acquisition: std::sync::OnceLock::new(),
            }),
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
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(std::mem::size_of::<SchedulingReadSource>() + 128)?,
            1,
        )?;
        Ok(SchedulingReadMaterial {
            source: Arc::new(SchedulingReadSource {
                _selected: Some(selected),
                _owned: owned,
                _selector: None,
                _read: Vec::new(),
                _body: None,
                _work_queue: std::sync::OnceLock::new(),
                _scheduler_work: std::sync::OnceLock::new(),
                _acquisition: std::sync::OnceLock::new(),
            }),
        })
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

    pub(in crate::runtime) fn scheduling_records_from_capture(
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

#[cfg(test)]
#[path = "timer_record_read_tests.rs"]
mod timer_record_read_tests;

#[cfg(test)]
#[path = "trigger_list_read_tests.rs"]
mod trigger_list_read_tests;

#[cfg(test)]
#[path = "timer_list_read_tests.rs"]
mod timer_list_read_tests;

#[cfg(test)]
#[path = "scheduling_output_material_tests.rs"]
mod material_tests;
