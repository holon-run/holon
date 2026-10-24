//! Bounded safe-point input selection; discovery never grants source authority.

use super::*;
use crate::domain::execution_protocol::ExecutionAttemptState;
use crate::runtime::input_consumption::InputScope;
use crate::types::{LiveInputBatchRecord, LiveInputSafePoint};
use std::collections::BTreeSet;

impl RuntimeHandle {
    pub(super) async fn attach_live_inputs(
        &self,
        agent_id: &str,
        provider_round: usize,
        available: impl Into<super::input_consumption::projection::InputBudget>,
        results_only: bool,
    ) -> Result<Vec<String>> {
        let budget = available.into();
        let mut guard = self.inner.agent.lock().await;
        let expected_state = guard.state.clone();
        let Some(binding) = expected_state.current_execution_binding.as_ref() else {
            return Ok(Vec::new());
        };
        let Some(activation_id) = binding.activation_id.as_deref() else {
            return Ok(Vec::new());
        };
        if expected_state.id != agent_id
            || expected_state.status != AgentStatus::AwakeRunning
            || expected_state.current_turn_id.as_deref() != Some(binding.turn_id.as_str())
            || guard
                .current_run_abort
                .as_ref()
                .is_some_and(|abort| abort.token.is_cancelled())
            || guard
                .queue
                .peek_next_matching(scheduler::is_operator_interjection_message)
                .is_some()
        {
            return Ok(Vec::new());
        }
        let Some(execution) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(agent_id)?
        else {
            return Ok(Vec::new());
        };
        let Some(attempt) = execution.attempts.get(activation_id).filter(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && attempt.agent_id == agent_id
                && attempt.source_message_id.as_deref() == Some(binding.source_message_id.as_str())
        }) else {
            return Ok(Vec::new());
        };
        let Some(turn) = self.inner.storage.read_turn_by_id(&binding.turn_id)? else {
            return Ok(Vec::new());
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
            return Ok(Vec::new());
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
            return Ok(Vec::new());
        }
        let cutoff = self.now();
        // Two fixed bounded source snapshots, at most 64 inspected candidates.
        let records = self
            .inner
            .runtime_db
            .task_result_settlements()
            .same_turn_candidates(agent_id, &turn.turn_id, cutoff, 32)?;
        let snapshot = if results_only {
            Vec::new()
        } else {
            guard.queue.input_snapshot(32)
        };
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
        for record in records {
            if batch.result_identities.len() >= max_items {
                break;
            }
            let Some(message) = self.inner.storage.read_message_by_id(&record.message_id)? else {
                continue;
            };
            let metadata = format!(
                "- result_identity: {}\n  task_id: {}\n  parent_turn_id: {}\n  rejoin_generation: {}\n  captured_owner: {}\n",
                record.result_identity, record.task_id, record.parent_turn_id,
                record.rejoin_generation, serde_json::to_string(&record.work_item_id)?,
            );
            let reply_id =
                crate::wake_contract::agent_message_reply_reference(&message).map(str::to_owned);
            let mut sources = vec![message];
            if let Some(reply_id) = reply_id.as_deref() {
                if !selected.contains(reply_id)
                    && !turn.input_message_ids.iter().any(|id| id == reply_id)
                {
                    let Some(reply) = self.inner.storage.read_message_by_id(reply_id)? else {
                        continue;
                    };
                    sources.push(reply);
                }
            }
            let Some(mut items) = super::input_consumption::projection::persisted_group(
                &self.inner.storage,
                &sources,
                |items| {
                    let mut text = batch.rendered_text();
                    text.push_str(&metadata);
                    for item in items {
                        text.push_str(&item.rendered);
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
                if let Some(queue) = self.inner.runtime_db.queue_entries().latest(&message.id)? {
                    match queue.status {
                        QueueEntryStatus::Queued => source_queues.push(QueueEntryRecord {
                            status: QueueEntryStatus::Interjected,
                            updated_at: cutoff,
                            ..queue
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
            batch.result_identities.push(record.result_identity);
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
            self.inner
                .storage
                .read_message_by_id(&idle.anchor_message_id)?
                .map(|anchor| self.idle_input_scope(&anchor))
                .transpose()?
                .flatten()
        } else {
            None
        };
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
            let Some(message) = self.inner.storage.read_message_by_id(&candidate.id)? else {
                continue;
            };
            let Some(scope) = self.idle_input_scope(&message)? else {
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
            let Some(queue) = self
                .inner
                .runtime_db
                .queue_entries()
                .latest(&message.id)?
                .filter(|queue| queue.status == QueueEntryStatus::Queued)
            else {
                continue;
            };
            let remaining = max_bytes.saturating_sub(batch.rendered_bytes);
            let preceding = format!("[current_live_input_batches]\n{}", batch.rendered_text());
            let Some(item) = super::input_consumption::projection::persisted_item_after(
                &self.inner.storage,
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
                ..queue
            });
        }
        if batch.result_identities.is_empty() && batch.ordinary_message_ids.is_empty() {
            return Ok(Vec::new());
        }
        let removed_from_memory = queues
            .iter()
            .filter(|queue| {
                guard
                    .queue
                    .peek_next_matching(|message| message.id == queue.message_id)
                    .is_some()
            })
            .count();
        let mut committed_state = expected_state.clone();
        committed_state.pending = guard.queue.len().saturating_sub(removed_from_memory);
        let text = batch.rendered_text();
        if !budget.fits("current_live_input_batches", &text) {
            return Ok(Vec::new());
        }
        let mut commit = self
            .inner
            .runtime_db
            .transitions()
            .commit_live_input_attachment(
                &crate::runtime_db::transitions::LiveInputAttachmentCommand {
                    expected_attempt: attempt.clone(),
                    expected_turn: turn,
                    batch,
                    messages,
                    queues: queues.clone(),
                    agent_state: crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_state)),
                        record: Box::new(committed_state.clone()),
                    },
                    fault: self.take_transition_fault(),
                },
            )?;
        if !commit.applied {
            return Ok(Vec::new());
        }
        for queue in queues {
            guard
                .queue
                .pop_next_matching(|message| message.id == queue.message_id);
        }
        guard.state = committed_state.clone();
        guard.last_persisted_state = committed_state;
        commit.effects.agent_state = None;
        drop(guard);
        self.apply_transition_commit(commit).await;
        Ok(vec![text])
    }
}
