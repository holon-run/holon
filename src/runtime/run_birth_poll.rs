//! Finite poll facts from the original born report; no history-sized baseline.
use super::RunBirthReportProjection;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::io::Write;

/// Publication generations also change for resource observation. Only actual
/// original run activity may reset the existing quiescence window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunPollActivity([u8; 32]);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunPollFacts {
    pub(crate) activity: RunPollActivity,
    pub(crate) turn_terminal_observed: bool,
    pub(crate) operator_visible_assistant_text_observed: bool,
    pub(crate) runtime_error: bool,
}
struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl RunBirthReportProjection {
    pub(crate) fn poll_facts(&self) -> Result<RunPollFacts> {
        let state = &self.source.state;
        let mut hash = Sha256::new();
        serde_json::to_writer(
            HashWriter(&mut hash),
            &(
                &state.status,
                state.current_run_id.as_deref(),
                state.pending,
                state.turn_index,
                state
                    .last_turn_terminal
                    .as_ref()
                    .map(|record| record.turn_index),
                self.source.tasks.coverage.revision,
                self.source.events.selected.birth_cursor,
            ),
        )?;
        // Actual qualified aliases detect changes to a run's retained Input.
        // Borrow in canonical report order; hash without a JSON Value or Vec.
        for record in self.born_inputs() {
            hash.update([0]);
            serde_json::to_writer(
                HashWriter(&mut hash),
                &(
                    &record.locator,
                    self.source
                        .inputs
                        .query
                        .born_key_seqs
                        .get(&record.locator.key),
                ),
            )?;
        }
        Ok(RunPollFacts {
            activity: RunPollActivity(hash.finalize().into()),
            turn_terminal_observed: state
                .last_turn_terminal
                .as_ref()
                .is_some_and(|record| record.turn_index > self.source.baseline.turn_index),
            operator_visible_assistant_text_observed: self.born_events().any(|event| {
                event.kind == "assistant_round_recorded"
                    && event.data["visibility"] == "operator_visible"
                    && event.data["has_text"].as_bool() == Some(true)
            }),
            runtime_error: self
                .born_events()
                .any(|event| event.kind == "runtime_error"),
        })
    }
}

#[cfg(test)]
#[path = "run_birth_poll_tests.rs"]
mod tests;
