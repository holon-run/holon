//! Frozen safe-point attachments retain their original owner and whole receipt.
use super::*;
use crate::runtime_db::transitions::{LiveInputAttachmentCommand, PreparedInputTransition};

impl RuntimeMutation {
    pub(in crate::runtime) async fn commit_live_input_attachment(
        &mut self,
        command: LiveInputAttachmentCommand,
    ) -> Result<bool> {
        let prepared = PreparedInputTransition::live_input_attachment(
            self.input_evidence_scope(),
            crate::ids::audit_event_id(),
            self.runtime.now(),
            command,
        )?;
        let commit = self.commit_prepared_input_transition(prepared).await?;
        let applied = commit.applied;
        self.runtime.apply_transition_commit(commit).await;
        Ok(applied)
    }
}
