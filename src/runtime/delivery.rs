use super::*;

use std::collections::BTreeMap;

use crate::types::{
    BriefAttachment, GenerateImageResult, OperatorMessageRecord, OperatorMessageStatus,
    ToolExecutionStatus,
};

const OPERATOR_MESSAGE_SCAN_MIN: usize = 256;
const OPERATOR_MESSAGE_SCAN_HEADROOM: usize = 16;

impl RuntimeHandle {
    pub async fn recent_briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        self.inner.storage.read_recent_briefs(limit)
    }

    pub async fn brief_by_id(&self, brief_id: &str) -> Result<Option<BriefRecord>> {
        self.inner.storage.read_brief_by_id(brief_id)
    }

    pub async fn recent_operator_messages(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorMessageRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let state = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        let scan_limit = limit
            .saturating_mul(OPERATOR_MESSAGE_SCAN_HEADROOM)
            .max(OPERATOR_MESSAGE_SCAN_MIN);
        let messages_by_id = self
            .inner
            .storage
            .read_recent_messages(scan_limit)?
            .into_iter()
            .filter(|message| message.kind == MessageKind::OperatorPrompt)
            .map(|message| (message.id.clone(), message))
            .collect::<BTreeMap<_, _>>();

        let mut latest_queue_entries = BTreeMap::new();
        for entry in self.inner.storage.read_recent_queue_entries(scan_limit)? {
            latest_queue_entries.insert(entry.message_id.clone(), entry);
        }

        let mut records = latest_queue_entries
            .values()
            .filter_map(|entry| {
                let message = messages_by_id.get(&entry.message_id)?;
                Some(OperatorMessageRecord {
                    message_id: entry.message_id.clone(),
                    agent_id: entry.agent_id.clone(),
                    status: operator_message_status(&entry.status, &entry.priority, &state),
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                    body: message.body.clone(),
                    error: None,
                })
            })
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.message_id.cmp(&right.message_id))
        });
        if records.len() > limit {
            records.drain(0..records.len() - limit);
        }
        Ok(records)
    }

    pub(super) async fn persist_brief(&self, brief: &BriefRecord) -> Result<()> {
        let mut bound_brief = brief.clone();
        {
            let guard = self.inner.agent.lock().await;
            bound_brief.workspace_id = guard
                .state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_id.clone())
                .unwrap_or_else(|| crate::types::AGENT_HOME_WORKSPACE_ID.to_string());
            if bound_brief.work_item_id.is_none() {
                bound_brief.work_item_id = guard.state.current_turn_work_item_id.clone();
            }
            if bound_brief.turn_id.is_none() {
                bound_brief.turn_id = guard.state.current_turn_id.clone();
            }
        }
        self.attach_generated_image_brief_attachments(&mut bound_brief)?;
        let event_payload = BriefCreatedAuditEvent::from_brief(&bound_brief);
        let evidence_brief = bound_brief.clone();
        self.persist_brief_evidence(&evidence_brief)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "brief_created",
            to_json_value(&event_payload),
        ))?;
        let mut guard = self.inner.agent.lock().await;
        guard.state.last_brief_at = Some(bound_brief.created_at);
        guard.persist_state(&self.inner.storage)?;
        Ok(())
    }

    fn attach_generated_image_brief_attachments(&self, brief: &mut BriefRecord) -> Result<()> {
        let Some(turn_id) = brief.turn_id.as_deref() else {
            return Ok(());
        };
        let existing = brief.attachments.get_or_insert_with(Vec::new);
        let mut existing_uris = existing
            .iter()
            .filter_map(|attachment| attachment.uri.clone())
            .collect::<std::collections::HashSet<_>>();
        for record in self.inner.storage.read_recent_tool_executions(64)? {
            if record.status != ToolExecutionStatus::Success
                || record.turn_id.as_deref() != Some(turn_id)
                || record.tool_name != crate::tool::names::GENERATE_IMAGE
            {
                continue;
            }
            let Some(value) = record.output.get("result").cloned() else {
                continue;
            };
            let Ok(result) = serde_json::from_value::<GenerateImageResult>(value) else {
                continue;
            };
            for image in result.images {
                if existing_uris.contains(&image.uri) {
                    continue;
                }
                let uri = image.uri.clone();
                existing.push(BriefAttachment {
                    kind: "image".to_string(),
                    name: image
                        .path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or(image.id.as_str())
                        .to_string(),
                    uri: Some(uri.clone()),
                    value: Some(to_json_value(&image)),
                });
                existing_uris.insert(uri);
            }
        }
        if existing.is_empty() {
            brief.attachments = None;
        }
        Ok(())
    }
}

fn operator_message_status(
    status: &QueueEntryStatus,
    priority: &Priority,
    state: &AgentState,
) -> OperatorMessageStatus {
    match status {
        QueueEntryStatus::Queued
            if *priority == Priority::Interject && state.current_run_id.is_some() =>
        {
            OperatorMessageStatus::WaitingForSafePoint
        }
        QueueEntryStatus::Queued => OperatorMessageStatus::Queued,
        QueueEntryStatus::Dequeued | QueueEntryStatus::Interjected => {
            OperatorMessageStatus::Processing
        }
        QueueEntryStatus::Interrupted => OperatorMessageStatus::Queued,
        QueueEntryStatus::Processed => OperatorMessageStatus::Processed,
        QueueEntryStatus::Aborted => OperatorMessageStatus::Failed,
        QueueEntryStatus::Dropped => OperatorMessageStatus::Dropped,
    }
}
