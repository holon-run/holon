//! Bounded safe-point input selection; discovery never grants source authority.

use super::*;
use crate::diagnostics::resource_io::{self, ResourceReadMaterial};
use crate::domain::execution_protocol::ExecutionAttemptState;
use crate::runtime::input_consumption::InputScope;
use crate::types::{LiveInputBatchRecord, LiveInputSafePoint};
use std::collections::BTreeSet;
use std::fmt::Write as _;

impl RuntimeHandle {
    pub(super) async fn attach_live_inputs(
        &self,
        agent_id: &str,
        provider_round: usize,
        available: impl Into<super::input_consumption::projection::InputBudget>,
        results_only: bool,
    ) -> Result<Vec<String>> {
        let budget = available.into();
        let mut mutation = self.begin_mutation().await?;
        let material = ResourceReadMaterial::new(
            &self.inner.input_evidence_budget,
            &mutation.input_evidence_scope(),
        )?;
        let (expected_state, snapshot, cancelled) = mutation.idle_selection_snapshot(&material)?;
        if mutation
            .queue
            .peek_next_matching(scheduler::is_operator_interjection_message)
            .is_some()
        {
            return Ok(Vec::new());
        }
        let plan = self.with_idle_read(Some(&expected_state), &material, |read| {
        let Some(binding) = expected_state.current_execution_binding.as_ref() else {
            return Ok(None);
        };
        let Some(activation_id) = binding.activation_id.as_deref() else {
            return Ok(None);
        };
        if expected_state.id != agent_id
            || expected_state.status != AgentStatus::AwakeRunning
            || expected_state.current_turn_id.as_deref() != Some(binding.turn_id.as_str())
            || cancelled
        {
            return Ok(None);
        }
        let Some(execution) = read.protocol()?
        else {
            return Ok(None);
        };
        let Some(attempt) = execution.attempts.get(activation_id).filter(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && attempt.agent_id == agent_id
                && attempt.source_message_id.as_deref() == Some(binding.source_message_id.as_str())
        }) else {
            return Ok(None);
        };
        let Some(turn) = read.shared_turn(&binding.turn_id)? else {
            return Ok(None);
        };
        let safe_point = LiveInputSafePoint::new(activation_id, provider_round);
        if turn.terminal.is_some()
            || !turn.input_message_ids.contains(&binding.source_message_id)
            || turn.live_input_batches.len() >= 4
            || turn
                .live_input_batches
                .iter()
                .any(|batch| batch.safe_point == safe_point)
        {
            return Ok(None);
        }
        let used_items: usize = turn
            .live_input_batches
            .iter()
            .map(|batch| batch.result_identities.len() + batch.ordinary_message_ids.len())
            .sum();
        let used_bytes: usize = turn
            .live_input_batches
            .iter()
            .map(|batch| batch.rendered_bytes)
            .sum();
        let max_items = 8.min(32usize.saturating_sub(used_items));
        let max_bytes = budget
            .bytes
            .min(24 * 1024)
            .min((96 * 1024usize).saturating_sub(used_bytes));
        if max_items == 0 || max_bytes <= LiveInputBatchRecord::HEADER.len() {
            return Ok(None);
        }
        let cutoff = self.now();
        // Two fixed bounded source snapshots, at most 64 inspected candidates.
        let candidates = read.same_turn_candidate_ids(&turn.turn_id, cutoff)?;
        let snapshot = snapshot.iter().take(if results_only { 0 } else { 32 });
        let mut batch = LiveInputBatchRecord {
            activation_id: activation_id.to_owned(),
            safe_point,
            cutoff,
            max_items,
            max_bytes,
            rendered_bytes: LiveInputBatchRecord::HEADER.len(),
            result_identities: Vec::new(),
            ordinary_message_ids: Vec::new(),
            items: Vec::new(),
        };
        let mut messages = Vec::new();
        let mut queues = Vec::new();
        let mut selected = BTreeSet::new();
        for identity in candidates {
            if batch.result_identities.len() >= max_items {
                break;
            }
            let Some(record) = read.same_turn_candidate(&identity, &turn.turn_id, cutoff)? else {
                continue;
            };
            let Some(message) = read.shared_message(&record.message_id)? else {
                continue;
            };
            let metadata = format!(
                "- result_identity: {}\n  task_id: {}\n  creation_source: {}\n  rejoin_generation: {}\n  captured_owner: {}\n",
                record.result_identity, record.task_id, serde_json::to_string(&record.source)?,
                record.rejoin_generation, serde_json::to_string(&record.work_item_id)?,
            );
            let reply_id =
                crate::wake_contract::agent_message_reply_reference(&message).map(str::to_owned);
            let mut sources = vec![message];
            if let Some(reply_id) = reply_id.as_deref() {
                if !selected.contains(reply_id)
                    && !turn.input_message_ids.iter().any(|id| id == reply_id)
                {
                    let Some(reply) = read.shared_message(reply_id)? else {
                        continue;
                    };
                    sources.push(reply);
                }
            }
            let Some(mut items) = super::input_consumption::projection::qualified_group(
                read,
                &sources,
                |items| {
                    let mut text = batch.rendered_text();
                    text.push_str(&metadata);
                    for item in items {
                        write!(text, "{}", item.rendered)?;
                    }
                    Ok(text.len() <= max_bytes && budget.fits("current_live_input_batches", &text))
                },
            )?
            else {
                break;
            };
            items[0].rendered.insert_str(0, &metadata);
            let additions: Vec<_> = sources.into_iter().zip(items).collect();
            let mut source_queues = Vec::new();
            let mut valid = true;
            for (message, _) in &additions {
                if let Some(queue) = read.queue(&message.id)? {
                    match queue.status {
                        QueueEntryStatus::Queued => source_queues.push(QueueEntryRecord {
                            status: QueueEntryStatus::Interjected,
                            updated_at: cutoff,
                            ..(*queue).clone()
                        }),
                        _ if turn.input_message_ids.contains(&message.id) => {}
                        _ => {
                            valid = false;
                            break;
                        }
                    }
                } else {
                    valid = false;
                    break;
                }
            }
            if !valid {
                continue;
            }
            resource_io::admit_cloned_value(&record.result_identity)?;
            batch.result_identities.push(record.result_identity.clone());
            queues.extend(source_queues);
            for (message, item) in additions {
                if selected.insert(message.id.clone()) {
                    batch.rendered_bytes += item.rendered.len();
                    batch.items.push(item);
                    messages.push(message);
                }
            }
        }
        let sender_scope = if let Some(idle) = &turn.idle_input_batch {
            read.with_message(&idle.anchor_message_id, |anchor| read.idle_input_scope(anchor))?.flatten()
        } else { None };
        for candidate in snapshot {
            if batch.result_identities.len() + batch.ordinary_message_ids.len() >= max_items {
                break;
            }
            if candidate.created_at > cutoff
                || selected.contains(&candidate.id)
                || candidate.kind == MessageKind::TaskResult
                || candidate.priority == Priority::Interject
                || candidate.authority_class == AuthorityClass::OperatorInstruction
            {
                continue;
            }
            let Some(message) = read.shared_message(&candidate.id)? else {
                continue;
            };
            let Some(scope) = read.idle_input_scope(&message)? else {
                continue;
            };
            let eligible = match &scope {
                InputScope::ReceiverOwner(owner) => binding.work_item_id.as_ref() == Some(owner),
                InputScope::Sender(_) => sender_scope.as_ref() == Some(&scope),
                InputScope::AgentLifecycle(_) => false,
            };
            if !eligible {
                continue;
            }
            let Some(queue) = read.queue(&message.id)?
                .filter(|queue| queue.status == QueueEntryStatus::Queued)
            else {
                continue;
            };
            let remaining = max_bytes.saturating_sub(batch.rendered_bytes);
            let preceding = format!("[current_live_input_batches]\n{}", batch.rendered_text());
            let Some(item) = super::input_consumption::projection::qualified_item_after(
                read,
                &message,
                budget.tokens,
                remaining,
                &preceding,
            )?
            else {
                break;
            };
            batch.ordinary_message_ids.push(message.id.clone());
            batch.rendered_bytes += item.rendered.len();
            batch.items.push(item);
            selected.insert(message.id.clone());
            messages.push(message);
            queues.push(QueueEntryRecord {
                status: QueueEntryStatus::Interjected,
                updated_at: cutoff,
                ..(*queue).clone()
            });
        }
        if batch.result_identities.is_empty() && batch.ordinary_message_ids.is_empty() {
            return Ok(None);
        }
        let removed_from_memory = queues
            .iter()
            .filter(|queue| {
                mutation
                    .queue
                    .peek_next_matching(|message| message.id == queue.message_id)
                    .is_some()
            })
            .count();
        resource_io::admit_cloned_value(&*expected_state)?;
        let mut committed_state = (*expected_state).clone();
        committed_state.pending = mutation.queue.len().saturating_sub(removed_from_memory);
        let text = batch.rendered_text();
        if !budget.fits("current_live_input_batches", &text) {
            return Ok(None);
        }
        resource_io::admit_cloned_value(&*attempt)?;
        resource_io::admit_cloned_value(&*turn)?;
        resource_io::admit_cloned_value(&*expected_state)?;
        let messages = messages.iter().map(|message| {
            resource_io::admit_cloned_value(&**message)?;
            Ok((**message).clone())
        }).collect::<Result<Vec<_>>>()?;
        Ok(Some((crate::runtime_db::transitions::LiveInputAttachmentCommand {
            expected_attempt: attempt.clone(), expected_turn: (*turn).clone(), batch, messages,
            queues, agent_state: crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new((*expected_state).clone())), record: Box::new(committed_state),
            }, fault: self.take_transition_fault(),
            audit_event_id: crate::ids::audit_event_id(),
        }, text)))
        })?;
        let Some((command, text)) = plan else {
            return Ok(Vec::new());
        };
        if !mutation.commit_live_input_attachment(command).await? {
            return Ok(Vec::new());
        }
        Ok(vec![text])
    }
}
