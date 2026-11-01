//! Named accepted Timer-writer preparation; routing afterwards is pure.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_scheduling::{
        SchedulingCaptureRequest, SchedulingFamily, SchedulingKey, SchedulingReadError,
    },
    timer_mutation_reads::TimerMutationRead,
};
struct TimerReadSelector {
    request: SchedulingCaptureRequest,
    bytes: usize,
    record_limit: usize,
    byte_limit: usize,
    _material: Arc<BudgetViewGuard>,
}
impl RuntimeMutation {
    fn timer_read_selector(&self, timer_id: &str) -> Result<TimerReadSelector> {
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let record_limit = record_limit.min(65_536);
        let byte_limit = byte_limit.min(64 * 1024 * 1024);
        let bytes = usize::try_from(serialized_bytes(&timer_id)?)?
            .checked_add(512)
            .and_then(|n| n.checked_mul(8))
            .context("Timer accepted selector overflow")?;
        anyhow::ensure!(
            bytes < byte_limit && 8 < record_limit,
            SchedulingReadError::ResourceExhausted("accepted Timer selector")
        );
        let material = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &self.input_evidence_scope(),
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: u64::try_from(bytes)?,
                },
            )?;
        Ok(TimerReadSelector {
            request: SchedulingCaptureRequest {
                include_live: true,
                history_limit: 0,
                required_keys: std::collections::BTreeSet::from([SchedulingKey::new(
                    SchedulingFamily::Timer,
                    timer_id,
                )]),
            },
            bytes,
            record_limit,
            byte_limit,
            _material: material,
        })
    }
    fn capture_timer_route_from_selector(
        &self,
        selector: &TimerReadSelector,
        timer_id: &str,
        previous: Option<&crate::types::TimerRecord>,
        include_work: bool,
    ) -> Result<Option<TimerMutationRead>> {
        let root = self.capture_published_root()?;
        let selected = root
            .scheduling
            .as_ref()
            .ok_or(SchedulingReadError::IncompleteLive)?
            .capture(&selector.request)?;
        let work = if include_work {
            Some(
                root.work
                    .as_ref()
                    .ok_or(super::super::resident_work::WorkReadError::IncompleteLive)?
                    .capture(&super::super::work_reads::live_work_request())?,
            )
        } else {
            None
        };
        TimerMutationRead::from_selected(
            &selected,
            work.as_ref(),
            timer_id,
            previous,
            &self.runtime.inner.input_evidence_budget,
            selector.record_limit - 8,
            selector.byte_limit - selector.bytes,
        )
    }
    /// Original ACK/no-op continuation reads only the now-published view.
    pub(in crate::runtime) fn capture_timer_mutation_read(
        &self,
        timer_id: &str,
        previous: Option<&crate::types::TimerRecord>,
        include_work: bool,
    ) -> Result<Option<TimerMutationRead>> {
        let selector = self.timer_read_selector(timer_id)?;
        self.capture_timer_route_from_selector(&selector, timer_id, previous, include_work)
    }
    #[inline(never)]
    pub(in crate::runtime) fn prepare_timer_mutation_read<'a>(
        &'a mut self,
        timer_id: &'a str,
        previous: Option<&'a crate::types::TimerRecord>,
        include_work: bool,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<Option<TimerMutationRead>>> + Send + 'a>,
    > {
        Box::pin(self.prepare_timer_mutation_read_inner(timer_id, previous, include_work))
    }
    async fn prepare_timer_mutation_read_inner(
        &mut self,
        timer_id: &str,
        previous: Option<&crate::types::TimerRecord>,
        include_work: bool,
    ) -> Result<Option<TimerMutationRead>> {
        let selector = self.timer_read_selector(timer_id)?;
        // This is the accepted original mutation boundary, never an ordinary
        // lookup fallback; fixed/current qualification remains mandatory.
        match self.capture_scheduling_read(&selector.request) {
            Ok(_) => {}
            Err(error) if error.downcast_ref::<SchedulingReadError>().is_some() => {
                self.prepare_owner_scheduling_request(&selector.request)
                    .await?;
            }
            Err(error) => return Err(error),
        }
        if include_work {
            self.prepare_owner_work_request(&super::super::work_reads::live_work_request())
                .await?;
        }
        self.capture_timer_route_from_selector(&selector, timer_id, previous, include_work)
    }
}
