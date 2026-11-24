//! Per-attempt timings for selected synchronous SQLite write callbacks.

use std::{
    cell::Cell,
    fmt,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
#[repr(usize)]
pub enum WriteCallbackPhase {
    QueuePreflight,
    QueueBusiness,
    QueueFinalize,
    QueueReceiptPersist,
    RecoveryPreflight,
    RecoveryReceiptLookup,
    RecoveryPrepare,
}

const PHASE_NAMES: [&str; WriteCallbackPhase::RecoveryPrepare as usize + 1] = [
    "queue_preflight",
    "queue_business",
    "queue_finalize",
    "queue_receipt_persist",
    "recovery_preflight",
    "recovery_receipt_lookup",
    "recovery_prepare",
];

/// Supply the same recorder to the synchronous writer and callback. Do not
/// share it between logical writes. Timings may nest and are not additive.
#[derive(Default)]
pub struct WriteCallbackPhases([Cell<Option<Duration>>; PHASE_NAMES.len()]);

impl WriteCallbackPhase {
    #[must_use]
    pub fn start(
        self,
        phases: Option<&WriteCallbackPhases>,
    ) -> Option<WriteCallbackPhaseTimer<'_>> {
        phases.map(|phases| WriteCallbackPhaseTimer {
            slot: &phases.0[self as usize],
            started: Instant::now(),
        })
    }
}

pub struct WriteCallbackPhaseTimer<'a> {
    slot: &'a Cell<Option<Duration>>,
    started: Instant,
}

impl Drop for WriteCallbackPhaseTimer<'_> {
    fn drop(&mut self) {
        let previous = self.slot.get().unwrap_or_default();
        self.slot
            .set(Some(previous.saturating_add(self.started.elapsed())));
    }
}

impl WriteCallbackPhases {
    pub(crate) fn reset(&self) {
        for slot in &self.0 {
            slot.set(None);
        }
    }

    pub(crate) fn log_value(&self) -> Option<impl tracing::Value + '_> {
        self.0
            .iter()
            .any(|slot| slot.get().is_some())
            .then(|| tracing::field::display(self))
    }
}

impl fmt::Display for WriteCallbackPhases {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut separator = "";
        for (name, slot) in PHASE_NAMES.iter().zip(&self.0) {
            if let Some(elapsed) = slot.get() {
                write!(f, "{separator}{name}:{}", elapsed.as_micros())?;
                separator = ",";
            }
        }
        Ok(())
    }
}
