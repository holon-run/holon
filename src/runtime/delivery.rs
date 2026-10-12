use super::*;

use std::collections::BTreeMap;

use crate::types::{
    BriefAttachment, BriefKind, CompletionReportState, GenerateImageResult, OperatorMessageRecord,
    OperatorMessageStatus, ToolExecutionStatus, WorkItemRecord,
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

    pub async fn briefs_by_ids(&self, brief_ids: &[String]) -> Result<Vec<BriefRecord>> {
        self.inner.storage.read_briefs_by_ids(brief_ids)
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
        let runtime = self.clone();
        let brief = brief.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let result = runtime.persist_brief_owned(brief).await;
            let _ = send.send(result);
        })?;
        receive
            .await
            .map_err(|_| anyhow!("owned Brief publication worker stopped"))?
    }

    async fn persist_brief_owned(&self, mut bound_brief: BriefRecord) -> Result<()> {
        let (workspace_id, turn_id, default_turn_work_item_id) = self
            .read_committed_agent(|agent| {
                (
                    agent
                        .state
                        .active_workspace_entry
                        .as_ref()
                        .map(|entry| entry.workspace_id.clone())
                        .unwrap_or_else(|| crate::types::AGENT_HOME_WORKSPACE_ID.to_string()),
                    agent.state.current_turn_id.clone(),
                    agent.state.current_turn_work_item_id.clone(),
                )
            })
            .await?;
        bound_brief.workspace_id = workspace_id;
        if bound_brief.turn_id.is_none() {
            bound_brief.turn_id = turn_id;
        }
        if bound_brief.work_item_id.is_none() && bound_brief.kind == BriefKind::Result {
            if let Some(work_item) = self.pending_completion_intent_for_brief(&bound_brief)? {
                bound_brief.work_item_id = Some(work_item.id.clone());
                if self
                    .persist_completion_brief_binding(work_item, &bound_brief)
                    .await?
                {
                    return Ok(());
                }
            }
        }
        if bound_brief.work_item_id.is_none() {
            bound_brief.work_item_id = default_turn_work_item_id;
        }
        self.attach_generated_image_brief_attachments(&mut bound_brief)?;
        // Single runtime DB transition: the Brief record, the audit event
        // sequence allocation, and the unique `brief_created` event commit
        // atomically. The deterministic event identity makes a retry of the
        // same Brief reuse the committed event instead of appending a
        // duplicate.
        let created_event = brief_created_event_for(&bound_brief)?;
        let fault = self.take_transition_fault();
        let mut mutation = self.begin_mutation().await?;
        let prepared = self.inner.storage.prepare_input_evidence(
            mutation.input_evidence_scope(),
            created_event.id.clone(),
            bound_brief.created_at,
            crate::runtime_db::prepared_input_evidence::InputEvidenceWrite::BriefCreated {
                brief: bound_brief,
                event: created_event,
            },
        )?;
        let original = mutation.commit_prepared_evidence(prepared, fault).await?;
        let crate::runtime_db::input_mutation_receipts::InputMutationResult::Evidence {
            created_at: Some(created_at),
            ..
        } = original
        else {
            bail!("Brief acknowledgement omitted its original timestamp");
        };
        mutation.state.last_brief_at = Some(created_at);
        mutation.persist_state(&self.inner.storage).await?;
        Ok(())
    }

    fn pending_completion_intent_for_brief(
        &self,
        brief: &BriefRecord,
    ) -> Result<Option<WorkItemRecord>> {
        let Some(turn_id) = brief.turn_id.as_deref() else {
            return Ok(None);
        };
        let candidates = self
            .inner
            .runtime_db
            .work_items()
            .latest_for_agent(&brief.agent_id, usize::MAX)?
            .into_iter()
            .filter(|work_item| {
                work_item.result_brief_id.is_none()
                    && work_item.completion_intent.as_ref().is_some_and(|intent| {
                        intent.report_state == CompletionReportState::Pending
                            && intent.source_turn_id.as_deref() == Some(turn_id)
                            && intent.source_message_id.as_deref()
                                == brief.related_message_id.as_deref()
                    })
            })
            .collect::<Vec<_>>();
        Ok((candidates.len() == 1).then(|| candidates[0].clone()))
    }

    async fn persist_completion_brief_binding(
        &self,
        existing: WorkItemRecord,
        brief: &BriefRecord,
    ) -> Result<bool> {
        let event_payload = BriefCreatedAuditEvent::from_brief(brief);
        let finalized = self
            .finalize_work_item_completion_with_brief(
                &existing.id,
                brief,
                AuditEvent::legacy(
                    "work_item_completion_report_bound_from_final",
                    serde_json::json!({
                        "agent_id": existing.agent_id,
                        "work_item_id": existing.id,
                        "brief_id": brief.id,
                        "turn_id": brief.turn_id,
                        "source_message_id": brief.related_message_id,
                        "brief_event": event_payload,
                    }),
                ),
            )
            .await?;
        Ok(finalized.is_some())
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
        QueueEntryStatus::Quarantined => OperatorMessageStatus::Quarantined,
    }
}

#[cfg(test)]
mod input_evidence_tests {
    use super::*;
    use crate::runtime::tests::support::{context_config, StubProvider};

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, RuntimeHandle) {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_path_buf(),
            workspace.path().to_path_buf(),
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        runtime
            .runtime_db()
            .agent_identities()
            .upsert(&crate::types::AgentIdentityRecord::new(
                "default",
                crate::types::AgentKind::Default,
                crate::types::AgentVisibility::Public,
                crate::types::AgentOwnership::SelfOwned,
                crate::types::AgentProfilePreset::PublicNamed,
                None,
                None,
            ))
            .unwrap();
        (home, workspace, runtime)
    }

    #[tokio::test]
    async fn brief_known_publication_fault_restores_exact_event_once() {
        let (_home, _workspace, runtime) = fixture();
        runtime
            .storage()
            .enable_event_bus(crate::storage::EventBus::new(16))
            .unwrap();
        let mut receiver = runtime.storage().subscribe_events().unwrap().unwrap();
        let brief = BriefRecord::new("default", BriefKind::Ack, "exact Brief ACK", None, None);
        runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
        runtime.persist_brief(&brief).await.unwrap();

        let actual = runtime
            .storage()
            .read_brief_by_id(&brief.id)
            .unwrap()
            .unwrap();
        let events = runtime.storage().read_recent_events(32).unwrap();
        let created = events
            .iter()
            .filter(|event| event.kind == "brief_created")
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 1);
        assert_eq!(actual.created_event_seq, Some(created[0].event_seq));
        assert_eq!(
            runtime.agent_state().await.unwrap().last_brief_at,
            Some(actual.created_at)
        );
        assert!(runtime.inner.mutation_owner.capture().is_ok());
        let mut published = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            if event.event.kind == "brief_created" {
                published.push(event.event);
            }
        }
        assert_eq!(published, vec![(*created[0]).clone()]);

        runtime.persist_brief(&brief).await.unwrap();
        assert_eq!(
            runtime.storage().read_brief_by_id(&brief.id).unwrap(),
            Some(actual)
        );
        assert_eq!(
            runtime
                .storage()
                .read_recent_events(32)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "brief_created")
                .count(),
            1
        );
        while let Ok(event) = receiver.try_recv() {
            assert_ne!(
                event.event.kind, "brief_created",
                "replay cannot republish its audit event"
            );
        }
    }

    #[tokio::test]
    async fn accepted_brief_worker_survives_cancelled_api_caller() {
        let (_home, _workspace, runtime) = fixture();
        let reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
        let brief = BriefRecord::new("default", BriefKind::Ack, "owned publication", None, None);
        let caller_runtime = runtime.clone();
        let caller_brief = brief.clone();
        let caller = tokio::spawn(async move { caller_runtime.persist_brief(&caller_brief).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if !runtime.inner.mutation_tasks.lock().unwrap().all_finished() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(reservation);
        runtime.join_owned_mutations().await.unwrap();

        let actual = runtime
            .storage()
            .read_brief_by_id(&brief.id)
            .unwrap()
            .unwrap();
        assert!(actual.created_event_seq.is_some());
        assert_eq!(
            runtime.agent_state().await.unwrap().last_brief_at,
            Some(actual.created_at)
        );
        assert_eq!(
            runtime
                .storage()
                .read_recent_events(32)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "brief_created")
                .count(),
            1
        );
        assert!(runtime.inner.mutation_owner.capture().is_ok());
    }
}
