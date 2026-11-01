//! Idle selection holds one owner; each source keeps its original ACK phase.
use super::*;
use crate::{
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::input_consumption::{AttachmentPlan, INPUT_BATCH_SCAN},
    runtime_db::transitions::{
        PreparedQueueTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
    },
};

impl RuntimeMutation {
    fn idle_installed_turn(
        &self,
        material: &ResourceReadMaterial,
        id: &str,
    ) -> Result<crate::runtime::input_consumption::read::QualifiedTurnRead> {
        material.read(|| {
            let captured = self.capture_input_evidence()?;
            anyhow::ensure!(
                captured.scope == self.input_evidence_scope(),
                "idle installed ACK belongs to another scope"
            );
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of_val(&captured))?,
                1,
            )?;
            crate::runtime::input_consumption::read::QualifiedTurnRead::from_capture(
                Arc::new(captured),
                id,
            )
        })
    }

    pub(in crate::runtime) fn idle_selection_snapshot(
        &self,
        material: &ResourceReadMaterial,
    ) -> Result<(Arc<AgentState>, Vec<Arc<MessageEnvelope>>, bool)> {
        let root = self.capture_published_root()?;
        anyhow::ensure!(
            root.state.as_ref() == &self.state,
            "idle selection cannot read candidate State"
        );
        material.read(|| {
            let count = self.queue.len().min(INPUT_BATCH_SCAN);
            resource_io::charge_owned_metadata(
                u64::try_from(
                    std::mem::size_of::<Vec<Arc<MessageEnvelope>>>()
                        + count * std::mem::size_of::<Arc<MessageEnvelope>>(),
                )?,
                count + 1,
            )?;
            let messages = self
                .queue
                .shared_messages()
                .take(INPUT_BATCH_SCAN)
                .cloned()
                .collect();
            Ok((
                root.state.clone(),
                messages,
                self.current_run_abort
                    .as_ref()
                    .is_some_and(|abort| abort.token.is_cancelled()),
            ))
        })
    }

    pub(in crate::runtime) async fn commit_idle_attachment(
        &mut self,
        plan: AttachmentPlan,
        material: ResourceReadMaterial,
    ) -> Result<()> {
        let AttachmentPlan {
            prior,
            turn,
            candidates,
        } = plan;
        let turn_id = material.read(|| {
            resource_io::admit_cloned_value(&prior.turn_id)?;
            Ok(prior.turn_id.clone())
        })?;
        let prepared = material.read(|| {
            crate::runtime_db::prepared_input_evidence::PreparedInputEvidence::turn_idle_batch(
                self.input_evidence_scope(),
                crate::ids::audit_event_id(),
                self.runtime.now(),
                &prior,
                turn,
                &self.runtime.inner.input_evidence_budget,
            )
        })?;
        self.commit_prepared_evidence(prepared, None).await?;
        let original_binding = self.state.current_execution_binding.clone();
        let candidate_count = candidates.len();
        if candidate_count == 0 {
            return Ok(());
        }
        let mut turn = self.idle_installed_turn(&material, &turn_id)?;
        for (index, (message, item)) in candidates.into_iter().enumerate() {
            if self.state.current_execution_binding != original_binding
                || self.state.status != AgentStatus::AwakeRunning
                || self
                    .current_run_abort
                    .as_ref()
                    .is_some_and(|abort| abort.token.is_cancelled())
                || self.queue.peek().is_some_and(|message| {
                    message.priority == Priority::Interject
                        || message.authority_class == AuthorityClass::OperatorInstruction
                })
            {
                break;
            }
            let (queue, attached_turn_id) = material.read(|| {
                resource_io::admit_cloned_value(&self.state)?;
                let expected = self.state.clone();
                resource_io::admit_cloned_value(&expected)?;
                let mut state = expected.clone();
                state.pending = self.queue.len().saturating_sub(1);
                resource_io::admit_cloned_value(&turn.turn_id)?;
                let turn_id = turn.turn_id.clone();
                resource_io::admit_cloned_value(&*turn)?;
                let mut attached = (*turn).clone();
                resource_io::admit_cloned_value(&message.id)?;
                attached.input_message_ids.push(message.id.clone());
                let batch = attached.idle_input_batch.as_mut().context("idle attachment omitted original batch")?;
                batch.rendered_bytes += item.rendered.len();
                batch.items.push(item);
                let cutoff = batch.cutoff;
                let rendered_bytes = batch.rendered_bytes;
                resource_io::admit_cloned_value(&message.routing_context)?;
                let transcript = TranscriptEntry::new(
                    &message.agent_id, TranscriptEntryKind::IncomingMessage, Some(0), Some(message.id.clone()),
                    serde_json::json!({ "turn_id": turn_id.as_str(), "boundary": "idle_input_batch",
                        "source_ref": format!("message:{}", message.id), "authority_class": message.authority_class,
                        "routing_context": message.routing_context }),
                );
                let command = QueueTransitionCommand {
                    agent_id: message.agent_id.clone(), operation: QueueOperation::Interject,
                    mutation: QueueMutation::Consume(QueueEntryRecord {
                        message_id: message.id.clone(), agent_id: message.agent_id.clone(), priority: message.priority.clone(),
                        status: QueueEntryStatus::Interjected, created_at: message.created_at, updated_at: cutoff,
                    }),
                    scheduler_claim_work_item: None,
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected)), record: Box::new(state),
                    }),
                    message_evidence: Vec::new(), transcript_entries: vec![transcript], turn_record: Some(attached),
                    audit_events: vec![AuditEvent::legacy("idle_input_attached", serde_json::json!({
                        "agent_id": message.agent_id, "turn_id": turn_id.as_str(), "message_id": message.id,
                        "cutoff": cutoff, "rendered_bytes": rendered_bytes,
                    }))],
                    notify_scheduler: false, fault: self.runtime.take_transition_fault(), brief_evidence: Vec::new(),
                };
                Ok((PreparedQueueTransition::new(command), turn_id))
            })?;
            let commit = self
                .commit_prepared_queue(crate::ids::audit_event_id(), queue)
                .await?;
            if !commit.applied {
                break;
            }
            // Installation strips duplicate ACK bodies. Reuse its qualified
            // Input root, never a Current pointer or a reopened SQL read.
            let next_turn = if index + 1 < candidate_count {
                Some(self.idle_installed_turn(&material, &attached_turn_id)?)
            } else {
                None
            };
            self.runtime.apply_transition_commit(commit).await;
            let Some(next_turn) = next_turn else { break };
            turn = next_turn;
        }
        Ok(())
    }
}
