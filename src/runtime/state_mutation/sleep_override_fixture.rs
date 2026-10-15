//! Test-only actual-COMMIT loss bridge, inside the original mutation boundary.
use super::*;
use crate::runtime_db::transitions::{InputMutationExecution, PreparedInputTransition};
impl RuntimeMutation {
    pub(in crate::runtime) async fn lose_original_sleep_pair_ack_for_test(
        &mut self,
        prepared: &PreparedInputTransition,
        execute: impl FnOnce() -> Result<InputMutationExecution<TransitionCommit>>,
    ) -> Result<()> {
        anyhow::ensure!(
            prepared
                .descriptor()
                .command_nonce()
                .starts_with("runtime-sleep-override:"),
            "Sleep ACK fixture requires its original pair"
        );
        let witness = super::super::mutation_witness::MutationWitness::input_mutation(
            prepared.descriptor().command_nonce().into(),
            crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
                prepared.descriptor().clone(),
            ),
            Vec::new(),
        )?
        .with_input_runtime(vec![Arc::new(self.facts(Default::default())?)])?;
        self.commit_transition_inner(
            prepared.descriptor().command_nonce().into(),
            Some(witness),
            || {
                anyhow::ensure!(
                    matches!(execute()?, InputMutationExecution::Committed { .. }),
                    "Sleep fixture did not reach a real COMMIT"
                );
                anyhow::bail!("actual Sleep pair ACK lost after COMMIT")
            },
        )
        .await
        .map(drop)
    }
}
