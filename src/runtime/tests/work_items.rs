use super::super::*;
use super::support::*;
use crate::types::{
    WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WaitingIntentScope, WakeSource,
    WorkItemPlanStatus, WorkItemReadiness, WorkItemSchedulingState, AGENT_HOME_WORKSPACE_ID,
};
use std::{fs::OpenOptions, io::Write};

fn legacy_blocking_payload_task_for_work_item(
    task_id: &str,
    work_item_id: Option<&str>,
) -> TaskRecord {
    TaskRecord {
        id: task_id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: work_item_id.map(ToString::to_string),
        summary: Some("legacy blocking command".into()),
        detail: Some(serde_json::json!({
            "wait_policy": "blocking",
            "work_item_id": work_item_id,
        })),
        recovery: None,
    }
}

fn continuation_context_config() -> ContextConfig {
    ContextConfig {
        prompt_budget_estimated_tokens: 16384,
        compaction_keep_recent_estimated_tokens: 2048,
        ..context_config()
    }
}

struct CompleteWorkItemReportProvider {
    work_item_id: String,
    report_text: Option<String>,
    calls: Mutex<usize>,
}

#[async_trait]
impl AgentProvider for CompleteWorkItemReportProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let blocks = if *calls == 1 {
            let mut blocks = Vec::new();
            if let Some(report_text) = &self.report_text {
                blocks.push(ModelBlock::Text {
                    text: report_text.clone(),
                });
            }
            blocks.push(ModelBlock::ToolUse {
                id: "complete-work".into(),
                name: "CompleteWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": self.work_item_id.clone(),
                }),
            });
            blocks
        } else {
            vec![ModelBlock::Text {
                text: "done".into(),
            }]
        };
        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

struct CompleteThenExecProvider {
    work_item_id: String,
    calls: Mutex<usize>,
}

#[async_trait]
impl AgentProvider for CompleteThenExecProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let blocks = if *calls == 1 {
            vec![
                ModelBlock::Text {
                    text: "Finished the tracked work.".into(),
                },
                ModelBlock::ToolUse {
                    id: "complete-work".into(),
                    name: "CompleteWorkItem".into(),
                    input: serde_json::json!({
                        "work_item_id": self.work_item_id.clone(),
                    }),
                },
                ModelBlock::ToolUse {
                    id: "verify".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'verified'",
                        "shell": "sh",
                    }),
                },
            ]
        } else {
            vec![ModelBlock::Text {
                text: "Verified after completion.".into(),
            }]
        };
        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

struct StaleTextThenCompleteProvider {
    work_item_id: String,
    calls: Mutex<usize>,
}

#[async_trait]
impl AgentProvider for StaleTextThenCompleteProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let blocks = if *calls == 1 {
            vec![
                ModelBlock::Text {
                    text: "This text belongs to the ExecCommand tool call.".into(),
                },
                ModelBlock::ToolUse {
                    id: "inspect".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'inspected'",
                        "shell": "sh",
                    }),
                },
                ModelBlock::ToolUse {
                    id: "complete-work".into(),
                    name: "CompleteWorkItem".into(),
                    input: serde_json::json!({
                        "work_item_id": self.work_item_id.clone(),
                    }),
                },
            ]
        } else {
            vec![ModelBlock::Text {
                text: "No completion report was provided.".into(),
            }]
        };
        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

struct MultiCompleteWorkItemReportProvider {
    work_item_ids: Vec<String>,
    report_texts: Vec<String>,
    calls: Mutex<usize>,
}

#[async_trait]
impl AgentProvider for MultiCompleteWorkItemReportProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let blocks = if *calls == 1 {
            let mut blocks = Vec::new();
            for (index, work_item_id) in self.work_item_ids.iter().enumerate() {
                if let Some(report_text) = self.report_texts.get(index) {
                    blocks.push(ModelBlock::Text {
                        text: report_text.clone(),
                    });
                }
                blocks.push(ModelBlock::ToolUse {
                    id: format!("complete-work-{index}"),
                    name: "CompleteWorkItem".into(),
                    input: serde_json::json!({
                        "work_item_id": work_item_id,
                    }),
                });
            }
            blocks
        } else {
            vec![ModelBlock::Text {
                text: "done".into(),
            }]
        };
        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[test]
fn work_item_record_revision_defaults_for_old_records() {
    let value = serde_json::json!({
        "id": "work-old",
        "agent_id": "default",
        "workspace_id": "agent_home",
        "objective": "old record",
        "state": "open",
        "plan_status": "draft",
        "created_at": Utc::now(),
        "updated_at": Utc::now(),
    });
    let record: WorkItemRecord = serde_json::from_value(value).unwrap();
    assert_eq!(record.revision, 1);
    assert!(record.plan_artifact.is_none());
    assert!(record.recheck_at.is_none());
    assert!(record.recheck_consumed_at.is_none());
}

#[tokio::test]
async fn update_work_item_sets_and_preserves_blocked_recheck_deadline() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("wait with fallback".into(), None, None, Vec::new())
        .await
        .unwrap();
    let before = Utc::now();
    let blocked = runtime
        .update_work_item_fields_with_recheck(
            work.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("waiting for CI".into())),
            Some(25),
        )
        .await
        .unwrap();
    let recheck_at = blocked.recheck_at.expect("blocked item has recheck_at");
    assert!(recheck_at >= before + chrono::Duration::milliseconds(25));
    assert!(blocked.recheck_consumed_at.is_none());

    let updated = runtime
        .update_work_item_fields(
            work.id.clone(),
            Some("wait with unchanged fallback".into()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(updated.recheck_at, Some(recheck_at));

    let cleared = runtime
        .update_work_item_fields(work.id.clone(), None, None, None, None, Some(None))
        .await
        .unwrap();
    assert!(cleared.blocked_by.is_none());
    assert!(cleared.recheck_at.is_none());
    assert!(cleared.recheck_consumed_at.is_none());
}

#[tokio::test]
async fn runtime_wakes_itself_for_blocked_work_item_recheck_deadline() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut blocked = WorkItemRecord::new(
        "default",
        "blocked work with fallback recheck",
        WorkItemState::Open,
    );
    blocked.blocked_by = Some("waiting for external wake".into());
    blocked.recheck_at = Some(Utc::now() + chrono::Duration::milliseconds(50));
    storage.append_work_item(&blocked).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("recheck observed")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let events = runtime.storage().read_recent_events(usize::MAX).unwrap();
    assert!(
        events.iter().any(|event| {
            event.kind == "system_tick_emitted"
                && event.data.get("subsystem").and_then(|value| value.as_str())
                    == Some("work_item_recheck")
        }),
        "runtime should emit a work_item_recheck tick without external input"
    );
    let latest = runtime
        .storage()
        .latest_work_item(&blocked.id)
        .unwrap()
        .expect("blocked work item exists");
    assert!(
        latest
            .recheck_consumed_at
            .zip(latest.recheck_at)
            .is_some_and(|(consumed_at, recheck_at)| consumed_at >= recheck_at),
        "deadline wake should consume the due blocked recheck"
    );
    runtime_task.abort();
}

#[tokio::test]
async fn work_queue_projection_derives_scheduling_state_per_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let runnable = runtime
        .create_work_item("runnable".into(), None, None, Vec::new())
        .await
        .unwrap();
    let external = runtime
        .create_work_item("external wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    let task_wait = runtime
        .create_work_item("task wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    let now = Utc::now();
    runtime
        .storage()
        .append_waiting_intent(&WaitingIntentRecord {
            id: "wait-external".into(),
            agent_id: "default".into(),
            scope: WaitingIntentScope::WorkItem,
            work_item_id: Some(external.id.clone()),
            description: "external callback".into(),
            source: "github".into(),
            resource: Some("pull_request:1".into()),
            condition: Some("checks".into()),
            delivery_mode: CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: "trigger-external".into(),
            created_at: now,
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        })
        .unwrap();
    runtime
        .storage()
        .append_task(&legacy_blocking_payload_task_for_work_item(
            "task-wait",
            Some(&task_wait.id),
        ))
        .unwrap();

    let projection = runtime.storage().work_queue_prompt_projection().unwrap();
    let state_for = |id: &str| {
        projection
            .readiness
            .iter()
            .find(|item| item.work_item.id == id)
            .map(|item| item.scheduling_state)
            .unwrap()
    };
    assert_eq!(state_for(&runnable.id), WorkItemSchedulingState::Runnable);
    assert_eq!(
        state_for(&external.id),
        WorkItemSchedulingState::WaitingExternal
    );
    assert_eq!(state_for(&task_wait.id), WorkItemSchedulingState::Runnable);
    assert!(projection
        .queued_runnable
        .iter()
        .any(|item| item.work_item.id == runnable.id));
    assert!(!projection
        .queued_runnable
        .iter()
        .any(|item| item.work_item.id == external.id));
}

#[tokio::test]
async fn work_item_query_tools_return_current_open_done_views() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let active = runtime
        .create_work_item("finish active delivery".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(active.id.clone()).await.unwrap();
    std::fs::write(
        crate::work_item_plan::plan_path(runtime.agent_home().as_path(), &active.id),
        "Inspect query surface behavior.",
    )
    .unwrap();
    runtime
        .update_work_item_fields(
            active.id.clone(),
            None,
            None,
            None,
            Some(vec![crate::types::TodoItem {
                text: "inspect query surface".into(),
                state: crate::types::TodoItemState::InProgress,
            }]),
            None,
        )
        .await
        .unwrap();
    let queued = runtime
        .create_work_item("queued delivery".into(), None, None, Vec::new())
        .await
        .unwrap();
    let completed = runtime
        .create_work_item("completed delivery".into(), None, None, Vec::new())
        .await
        .unwrap();
    let completed = runtime
        .complete_work_item(completed.id.clone(), Vec::new())
        .await
        .unwrap();
    let completed = runtime
        .promote_work_item_completion_report(
            completed.id.clone(),
            "Completed delivery report.".into(),
            Some(7),
            Some(0),
            Vec::new(),
        )
        .await
        .unwrap();
    bind_turn_to_work_item(&runtime, &active.id).await;

    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (active_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "active".into(),
                name: "ListWorkItems".into(),
                input: serde_json::json!({"filter": "current", "include_todo_list": true}),
            },
        )
        .await
        .unwrap();
    let active_payload = active_result.envelope.result.unwrap();
    let active_item = &active_payload["work_items"][0];
    assert_eq!(
        active_payload["context"]["current_work_item_id"].as_str(),
        Some(active.id.as_str())
    );
    assert_eq!(active_item["state"].as_str(), Some("open"));
    assert_eq!(active_item["focus"].as_str(), Some("current"));
    assert_eq!(active_item["readiness"].as_str(), Some("runnable"));
    assert_eq!(active_item["is_current"].as_bool(), Some(true));
    assert_eq!(active_item["is_runnable"].as_bool(), Some(true));
    assert_eq!(
        active_item["plan_artifact"]["preview"].as_str(),
        Some("Inspect query surface behavior.")
    );
    assert_eq!(
        active_item["plan_artifact"]["preview_complete"].as_bool(),
        Some(true)
    );
    assert_eq!(active_item["todo_list"].as_array().unwrap().len(), 1);
    assert_eq!(
        active_item["todo_list"][0]["state"].as_str(),
        Some("in_progress")
    );

    let now = Utc::now();
    runtime
        .storage()
        .append_wait_condition(&WaitConditionRecord {
            id: "weak-external-wait".into(),
            agent_id: "default".into(),
            work_item_id: Some(active.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("github".into()),
            subject_ref: Some("pull_request:1313".into()),
            waiting_for: "PR merged".into(),
            wake_sources: vec![WakeSource::ExternalIngress {
                external_trigger_id: Some("trigger-weak".into()),
            }],
            continuation: Some(serde_json::json!({
                "provider": "github",
                "subscription_id": "sub_1"
            })),
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
        })
        .unwrap();

    let (active_wait_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "active-wait".into(),
                name: "ListWorkItems".into(),
                input: serde_json::json!({"filter": "current"}),
            },
        )
        .await
        .unwrap();
    let active_wait_payload = active_wait_result.envelope.result.unwrap();
    let wait = &active_wait_payload["work_items"][0]["active_wait_conditions"][0];
    assert_eq!(wait["id"].as_str(), Some("weak-external-wait"));
    assert_eq!(wait["external_recoverability"].as_str(), Some("weak"));
    assert_eq!(
        wait["continuation"]["subscription_id"].as_str(),
        Some("sub_1")
    );

    let (agent_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "agent-get".into(),
                name: "AgentGet".into(),
                input: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    let agent_payload = agent_result.envelope.result.unwrap();
    let agent_wait = &agent_payload["agent"]["active_wait_conditions"][0];
    assert_eq!(agent_wait["id"].as_str(), Some("weak-external-wait"));
    assert_eq!(agent_wait["external_recoverability"].as_str(), Some("weak"));

    let (list_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "list".into(),
                name: "ListWorkItems".into(),
                input: serde_json::json!({"filter": "open", "limit": 10}),
            },
        )
        .await
        .unwrap();
    let list_payload = list_result.envelope.result.unwrap();
    let items = list_payload["work_items"].as_array().unwrap();
    assert_eq!(list_payload["total_matching"].as_u64(), Some(2));
    assert!(items
        .iter()
        .any(|item| item["id"].as_str() == Some(active.id.as_str())));
    assert!(items
        .iter()
        .any(|item| item["id"].as_str() == Some(queued.id.as_str())));
    assert!(!items
        .iter()
        .any(|item| item["id"].as_str() == Some(completed.id.as_str())));

    let (completed_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "completed".into(),
                name: "GetWorkItem".into(),
                input: serde_json::json!({"work_item_id": completed.id}),
            },
        )
        .await
        .unwrap();
    let completed_payload = completed_result.envelope.result.unwrap();
    assert_eq!(
        completed_payload["work_item"]["state"].as_str(),
        Some("completed")
    );
    assert_eq!(
        completed_payload["work_item"]["focus"].as_str(),
        Some("completed")
    );
    assert_eq!(
        completed_payload["work_item"]["readiness"].as_str(),
        Some("completed")
    );
    assert_eq!(
        completed_payload["work_item"]["completion_report"]["text"].as_str(),
        Some("Completed delivery report.")
    );
    assert_eq!(
        completed_payload["work_item"]["completion_report"]["source"].as_str(),
        Some("work_item_result_summary")
    );
    assert_eq!(
        completed_payload["work_item"]["completion_report"]["source_turn_index"].as_u64(),
        Some(7)
    );

    bind_turn_to_work_item(&runtime, completed.id.as_str()).await;
    let (fallback_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "fallback-active".into(),
                name: "ListWorkItems".into(),
                input: serde_json::json!({"filter": "current"}),
            },
        )
        .await
        .unwrap();
    let fallback_payload = fallback_result.envelope.result.unwrap();
    assert_eq!(
        fallback_payload["context"]["current_work_item_id"].as_str(),
        Some(active.id.as_str())
    );
    assert_eq!(
        fallback_payload["work_items"][0]["id"].as_str(),
        Some(active.id.as_str())
    );
}

#[tokio::test]
async fn work_item_query_tools_fall_back_to_delivery_summary_report() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item(
            "legacy delivery summary fallback".into(),
            None,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let completed = runtime
        .complete_work_item(work.id.clone(), Vec::new())
        .await
        .unwrap();
    let summary = DeliverySummaryRecord::new(
        "default",
        completed.id.clone(),
        "Legacy delivery summary report.",
        Some(11),
        None,
    );
    let summary_id = summary.id.clone();
    runtime.storage().append_delivery_summary(&summary).unwrap();

    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "completed".into(),
                name: "GetWorkItem".into(),
                input: serde_json::json!({"work_item_id": completed.id}),
            },
        )
        .await
        .unwrap();
    let payload = result.envelope.result.unwrap();
    let report = &payload["work_item"]["completion_report"];
    assert_eq!(
        report["text"].as_str(),
        Some("Legacy delivery summary report.")
    );
    assert_eq!(report["source"].as_str(), Some("delivery_summary"));
    assert_eq!(
        report["delivery_summary_id"].as_str(),
        Some(summary_id.as_str())
    );
    assert_eq!(report["source_turn_index"].as_u64(), Some(11));
}

#[tokio::test]
async fn work_item_revision_increments_on_updates_and_completion() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let created = runtime
        .create_work_item("revision contract".into(), None, None, Vec::new())
        .await
        .unwrap();
    assert_eq!(created.revision, 1);

    let updated = runtime
        .update_work_item_fields(
            created.id.clone(),
            None,
            Some(WorkItemPlanStatus::Ready),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(updated.revision, 2);

    let completed = runtime
        .complete_work_item(updated.id.clone(), Vec::new())
        .await
        .unwrap();
    assert_eq!(completed.revision, 3);
}

#[tokio::test]
async fn work_item_completion_ignores_running_tasks_and_clears_explicit_waits() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let target = runtime
        .create_work_item("target".into(), None, None, Vec::new())
        .await
        .unwrap();
    let other = runtime
        .create_work_item("other".into(), None, None, Vec::new())
        .await
        .unwrap();
    let related_task = legacy_blocking_payload_task_for_work_item("task-target", Some(&target.id));
    runtime.storage().append_task(&related_task).unwrap();
    let unrelated_task = legacy_blocking_payload_task_for_work_item("task-other", Some(&other.id));
    runtime.storage().append_task(&unrelated_task).unwrap();
    let unscoped_task = legacy_blocking_payload_task_for_work_item("task-unscoped", None);
    runtime.storage().append_task(&unscoped_task).unwrap();

    let completed = runtime
        .complete_work_item(target.id.clone(), Vec::new())
        .await
        .unwrap();
    assert_eq!(completed.id, target.id);
    assert_eq!(completed.result_summary, None);

    let explicit_wait = runtime
        .create_work_item("explicit wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .update_work_item_fields(
            explicit_wait.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("review the external result".into())),
        )
        .await
        .unwrap();

    let completed_wait = runtime
        .complete_work_item(explicit_wait.id.clone(), Vec::new())
        .await
        .unwrap();
    assert_eq!(completed_wait.state, WorkItemState::Completed);
    assert!(completed_wait.blocked_by.is_none());
}

#[tokio::test]
async fn work_item_query_tools_return_readiness_views() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let runnable = runtime
        .create_work_item("runnable work".into(), None, None, Vec::new())
        .await
        .unwrap();
    let waiting = runtime
        .create_work_item(
            "waiting work".into(),
            Some(WorkItemPlanStatus::NeedsInput),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let blocked = runtime
        .create_work_item("blocked work".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .update_work_item_fields(
            blocked.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("waiting for CI".into())),
        )
        .await
        .unwrap();
    let wait_condition_blocked = runtime
        .create_work_item("wait condition blocked".into(), None, None, Vec::new())
        .await
        .unwrap();
    let now = Utc::now();
    runtime
        .storage()
        .append_wait_condition(&WaitConditionRecord {
            id: "wait-external-only".into(),
            agent_id: "default".into(),
            work_item_id: Some(wait_condition_blocked.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("test".into()),
            subject_ref: Some("github:holon-run/holon#1459".into()),
            waiting_for: "waiting for follow-up review".into(),
            wake_sources: vec![WakeSource::ExternalIngress {
                external_trigger_id: None,
            }],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
        })
        .unwrap();

    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let list = |filter: &'static str| {
        let registry = &registry;
        let runtime = &runtime;
        async move {
            let (result, _) = registry
                .execute(
                    runtime,
                    "default",
                    &AuthorityClass::OperatorInstruction,
                    &crate::tool::ToolCall {
                        id: format!("list-{filter}"),
                        name: "ListWorkItems".into(),
                        input: serde_json::json!({"filter": filter}),
                    },
                )
                .await
                .unwrap();
            result.envelope.result.unwrap()
        }
    };

    let runnable_payload = list("runnable").await;
    assert_eq!(runnable_payload["total_matching"].as_u64(), Some(1));
    assert_eq!(
        runnable_payload["work_items"][0]["id"].as_str(),
        Some(runnable.id.as_str())
    );
    assert_eq!(
        runnable_payload["work_items"][0]["readiness"].as_str(),
        Some("runnable")
    );

    let waiting_payload = list("waiting_for_operator").await;
    assert_eq!(waiting_payload["total_matching"].as_u64(), Some(1));
    assert_eq!(
        waiting_payload["work_items"][0]["id"].as_str(),
        Some(waiting.id.as_str())
    );
    assert_eq!(
        waiting_payload["work_items"][0]["readiness"].as_str(),
        Some("waiting_for_operator")
    );
    assert_eq!(
        waiting_payload["work_items"][0]["is_runnable"].as_bool(),
        Some(false)
    );

    let queued_payload = list("queued").await;
    assert_eq!(queued_payload["total_matching"].as_u64(), Some(1));
    let queued_ids = queued_payload["work_items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert!(queued_ids.contains(&runnable.id.as_str()));
    assert!(!queued_ids.contains(&waiting.id.as_str()));
    assert!(!queued_ids.contains(&wait_condition_blocked.id.as_str()));

    let blocked_payload = list("blocked").await;
    assert_eq!(blocked_payload["total_matching"].as_u64(), Some(2));
    let blocked_items = blocked_payload["work_items"].as_array().unwrap();
    let blocked_ids = blocked_items
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert!(blocked_ids.contains(&blocked.id.as_str()));
    assert!(blocked_ids.contains(&wait_condition_blocked.id.as_str()));
    let wait_condition_view = blocked_items
        .iter()
        .find(|item| item["id"].as_str() == Some(wait_condition_blocked.id.as_str()))
        .expect("wait condition blocked item should be listed");
    assert_eq!(wait_condition_view["readiness"].as_str(), Some("blocked"));
}

#[tokio::test]
async fn work_item_tools_use_objective_plan_and_todo_list_shape() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());

    let (create_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "create".into(),
                name: "CreateWorkItem".into(),
                input: serde_json::json!({
                    "objective": "ship work item objective contract",
                    "plan_status": "ready",
                    "plan": "1. Inspect current contract\n2. Update tool shape\n3. Verify regression",
                    "todo_list": [
                        { "text": "inspect current contract", "state": "completed" },
                        { "text": "update tool shape", "state": "in_progress" },
                        { "text": "verify regression", "state": "pending" }
                    ]
                }),
            },
        )
        .await
        .unwrap();
    let create_payload = create_result.envelope.result.unwrap();
    let work_item_id = create_payload["work_item"]["id"].as_str().unwrap();
    assert_eq!(
        create_payload["work_item"]["plan_status"].as_str(),
        Some("ready")
    );
    assert_eq!(
        create_payload["work_item"]["plan_artifact"]["preview"].as_str(),
        Some("1. Inspect current contract\n2. Update tool shape\n3. Verify regression")
    );
    let plan_path = create_payload["work_item"]["plan_artifact"]["path"]
        .as_str()
        .unwrap();
    assert!(std::path::Path::new(plan_path).is_file());
    assert_eq!(
        create_payload["work_item"]["plan_artifact"]["owner_agent_id"].as_str(),
        Some("default")
    );
    assert_eq!(
        create_payload["work_item"]["plan_artifact"]["workspace_id"].as_str(),
        Some(crate::types::agent_home_workspace_id("default").as_str())
    );
    assert_eq!(
        create_payload["work_item"]["plan_artifact"]["workspace_alias"].as_str(),
        Some(AGENT_HOME_WORKSPACE_ID)
    );
    assert_eq!(
        create_payload["work_item"]["plan_artifact"]["relative_path"].as_str(),
        Some(format!("work-items/{work_item_id}/plan.md").as_str())
    );
    assert_eq!(
        create_payload["work_item"]["todo_list"][0]["state"].as_str(),
        Some("completed")
    );
    assert_eq!(
        create_payload["work_item"]["todo_list"][1]["state"].as_str(),
        Some("in_progress")
    );
    assert_eq!(
        create_payload["work_item"]["todo_list"][2]["state"].as_str(),
        Some("pending")
    );
    assert!(create_payload["work_item"]["todo_list"][0]["status"].is_null());

    let (get_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "get".into(),
                name: "GetWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item_id,
                    "include_todo_list": true
                }),
            },
        )
        .await
        .unwrap();
    let get_payload = get_result.envelope.result.unwrap();
    assert_eq!(
        get_payload["work_item"]["todo_list"][1]["state"].as_str(),
        Some("in_progress")
    );
    assert!(get_payload["work_item"]["todo_list"][1]["status"].is_null());
    assert!(get_payload["work_item"]["plan"].is_null());
    assert_eq!(
        get_payload["work_item"]["plan_artifact"]["path"].as_str(),
        Some(plan_path)
    );

    let returned_items = get_payload["work_item"]["todo_list"].clone();
    let (update_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "update".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item_id,
                    "todo_list": returned_items
                }),
            },
        )
        .await
        .unwrap();
    let update_payload = update_result.envelope.result.unwrap();
    assert_eq!(
        update_payload["work_item"]["todo_list"][1]["state"].as_str(),
        Some("in_progress")
    );
    assert!(update_payload["work_item"]["todo_list"][1]["status"].is_null());
}

#[tokio::test]
async fn work_item_plan_artifact_refreshes_after_direct_file_edit() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let work_item = runtime
        .create_work_item(
            "Refresh plan descriptor".into(),
            Some(WorkItemPlanStatus::Ready),
            Some("short plan".into()),
            Vec::new(),
        )
        .await
        .unwrap();

    let (first_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "first".into(),
                name: "GetWorkItem".into(),
                input: serde_json::json!({"work_item_id": work_item.id.clone()}),
            },
        )
        .await
        .unwrap();
    let first_payload = first_result.envelope.result.unwrap();
    let artifact = &first_payload["work_item"]["plan_artifact"];
    let plan_path = artifact["path"].as_str().unwrap();
    let first_hash = artifact["hash"].as_str().unwrap().to_string();
    assert_eq!(artifact["preview"].as_str(), Some("short plan"));
    assert_eq!(artifact["preview_complete"].as_bool(), Some(true));

    std::fs::write(
        plan_path,
        format!("{}\nend", "expanded plan line\n".repeat(200)),
    )
    .unwrap();

    let (second_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "second".into(),
                name: "GetWorkItem".into(),
                input: serde_json::json!({"work_item_id": work_item.id.clone()}),
            },
        )
        .await
        .unwrap();
    let second_payload = second_result.envelope.result.unwrap();
    let refreshed = &second_payload["work_item"]["plan_artifact"];
    assert_ne!(refreshed["hash"].as_str(), Some(first_hash.as_str()));
    assert!(refreshed["bytes"].as_u64().unwrap() > artifact["bytes"].as_u64().unwrap());
    assert_eq!(refreshed["preview_complete"].as_bool(), Some(false));
    assert!(refreshed["preview"]
        .as_str()
        .unwrap()
        .contains("expanded plan line"));
}

#[tokio::test]
async fn turn_end_refreshes_changed_work_item_plan_artifact_snapshot() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work = runtime
        .create_work_item(
            "Refresh artifact at turn end".into(),
            Some(WorkItemPlanStatus::Ready),
            Some("initial plan".into()),
            Vec::new(),
        )
        .await
        .unwrap();
    let original_artifact = work.plan_artifact.clone().unwrap();
    let plan_path = original_artifact.path.clone();
    std::fs::write(&plan_path, "changed plan body").unwrap();
    bind_turn_to_work_item(&runtime, &work.id).await;

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work.id);
    assert_eq!(committed.revision, work.revision + 1);
    assert_eq!(committed.plan_status, WorkItemPlanStatus::Ready);
    assert_ne!(
        committed.plan_artifact.as_ref().unwrap().hash,
        original_artifact.hash
    );
    assert_eq!(
        committed.plan_artifact.as_ref().unwrap().preview,
        "changed plan body"
    );
    let latest = runtime.latest_work_item(&work.id).await.unwrap().unwrap();
    assert_eq!(
        latest.plan_artifact.as_ref().unwrap().hash,
        committed.plan_artifact.as_ref().unwrap().hash
    );
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "work_item_plan_artifact_refreshed"
            && event.data["work_item_id"].as_str() == Some(work.id.as_str())));
}

#[tokio::test]
async fn turn_end_work_item_plan_artifact_refresh_is_noop_when_unchanged() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work = runtime
        .create_work_item(
            "Keep unchanged artifact stable".into(),
            Some(WorkItemPlanStatus::NeedsInput),
            Some("unchanged plan".into()),
            Vec::new(),
        )
        .await
        .unwrap();
    bind_turn_to_work_item(&runtime, &work.id).await;

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.revision, work.revision);
    assert_eq!(committed.plan_status, WorkItemPlanStatus::NeedsInput);
    assert_eq!(committed.plan_artifact, work.plan_artifact);
    let latest = runtime.latest_work_item(&work.id).await.unwrap().unwrap();
    assert_eq!(latest.revision, work.revision);
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "work_item_plan_artifact_refreshed"));
}

#[tokio::test]
async fn turn_end_work_item_plan_artifact_refresh_rejects_missing_existing_artifact() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work = runtime
        .create_work_item(
            "Reject missing artifact".into(),
            Some(WorkItemPlanStatus::Ready),
            Some("plan that should not disappear".into()),
            Vec::new(),
        )
        .await
        .unwrap();
    let plan_path = work.plan_artifact.as_ref().unwrap().path.clone();
    std::fs::remove_file(&plan_path).unwrap();
    bind_turn_to_work_item(&runtime, &work.id).await;

    let error = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap_err();

    assert!(error.to_string().contains("missing plan artifact"));
    assert!(!plan_path.exists());
    let latest = runtime.latest_work_item(&work.id).await.unwrap().unwrap();
    assert_eq!(latest.revision, work.revision);
}

#[tokio::test]
async fn complete_work_item_refreshes_latest_plan_artifact_snapshot() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work = runtime
        .create_work_item(
            "Complete with latest artifact".into(),
            Some(WorkItemPlanStatus::Ready),
            Some("initial completion plan".into()),
            Vec::new(),
        )
        .await
        .unwrap();
    let plan_path = work.plan_artifact.as_ref().unwrap().path.clone();
    std::fs::write(&plan_path, "completion plan after direct edit").unwrap();
    let expected =
        crate::work_item_plan::describe_plan_artifact(&plan_path, &work.agent_id, &work.id)
            .unwrap();

    let completed = runtime
        .complete_work_item(work.id.clone(), Vec::new())
        .await
        .unwrap();

    assert_eq!(completed.state, WorkItemState::Completed);
    assert_eq!(completed.plan_status, WorkItemPlanStatus::Ready);
    assert_eq!(completed.plan_artifact.as_ref(), Some(&expected));
    let latest = runtime.latest_work_item(&work.id).await.unwrap().unwrap();
    assert_eq!(latest.plan_artifact.as_ref(), Some(&expected));
}

#[tokio::test]
async fn work_item_read_tools_reject_legacy_include_plan_argument() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let work_item = runtime
        .create_work_item("Reject old read args".into(), None, None, Vec::new())
        .await
        .unwrap();

    for (tool_name, input) in [
        (
            "GetWorkItem",
            serde_json::json!({"work_item_id": work_item.id.clone(), "include_plan": true}),
        ),
        (
            "ListWorkItems",
            serde_json::json!({"filter": "current", "include_plan": true}),
        ),
    ] {
        let error = registry
            .execute(
                &runtime,
                "default",
                &AuthorityClass::OperatorInstruction,
                &crate::tool::ToolCall {
                    id: format!("{tool_name}-legacy-include-plan"),
                    name: tool_name.into(),
                    input,
                },
            )
            .await
            .unwrap_err();
        let tool_error = crate::tool::ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "invalid_tool_input");
        let parse_error = tool_error
            .details
            .as_ref()
            .and_then(|details| details.get("parse_error"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(parse_error.contains("unknown field `include_plan`"));
    }
}

#[tokio::test]
async fn update_work_item_can_refine_objective() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let work_item = runtime
        .create_work_item("Fix issue #869".into(), None, None, Vec::new())
        .await
        .unwrap();

    let (update_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "refine-target".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item.id.clone(),
                    "objective": "Fix issue #869 by allowing objective refinement"
                }),
            },
        )
        .await
        .unwrap();
    let payload = update_result.envelope.result.unwrap();
    assert_eq!(
        payload["work_item"]["objective"].as_str(),
        Some("Fix issue #869 by allowing objective refinement")
    );

    let latest = runtime
        .latest_work_item(&work_item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        latest.objective,
        "Fix issue #869 by allowing objective refinement"
    );
}

#[tokio::test]
async fn update_work_item_materializes_and_clears_legacy_inline_plan() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let mut legacy = WorkItemRecord::new("default", "Migrate inline plan", WorkItemState::Open);
    legacy.id = "legacy-plan-item".into();
    legacy.legacy_inline_plan = Some("Keep this legacy plan body in the artifact.".into());
    let mut legacy_json = serde_json::to_value(&legacy).unwrap();
    legacy_json["plan"] = serde_json::json!("Keep this legacy plan body in the artifact.");
    let work_items_path = dir
        .path()
        .join(".holon")
        .join("ledger")
        .join("work_items.jsonl");
    let mut work_items_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&work_items_path)
        .unwrap();
    writeln!(work_items_file, "{}", legacy_json).unwrap();

    let updated = runtime
        .update_work_item_fields(
            legacy.id.clone(),
            Some("Migrate inline plan without copying it".into()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(updated.revision, legacy.revision + 1);
    assert!(updated.legacy_inline_plan.is_none());
    let latest = runtime.latest_work_item(&legacy.id).await.unwrap().unwrap();
    assert!(latest.legacy_inline_plan.is_none());
    assert_eq!(latest.objective, "Migrate inline plan without copying it");

    let plan_path = crate::work_item_plan::plan_path(runtime.agent_home().as_path(), &legacy.id);
    assert_eq!(
        std::fs::read_to_string(&plan_path).unwrap(),
        "Keep this legacy plan body in the artifact."
    );
    let artifact =
        crate::work_item_plan::describe_plan_artifact(&plan_path, &legacy.agent_id, &legacy.id)
            .unwrap();
    assert_eq!(
        artifact.preview,
        "Keep this legacy plan body in the artifact."
    );
    assert!(artifact.preview_complete);

    let mut legacy_with_artifact =
        WorkItemRecord::new("default", "Keep existing artifact", WorkItemState::Open);
    legacy_with_artifact.id = "legacy-plan-item-with-artifact".into();
    legacy_with_artifact.legacy_inline_plan = Some("Stale inline plan body.".into());
    let mut legacy_with_artifact_json = serde_json::to_value(&legacy_with_artifact).unwrap();
    legacy_with_artifact_json["plan"] = serde_json::json!("Stale inline plan body.");
    writeln!(work_items_file, "{}", legacy_with_artifact_json).unwrap();
    let existing_plan_path =
        crate::work_item_plan::plan_path(runtime.agent_home().as_path(), &legacy_with_artifact.id);
    std::fs::create_dir_all(existing_plan_path.parent().unwrap()).unwrap();
    std::fs::write(&existing_plan_path, "Existing artifact body.").unwrap();

    let updated_with_artifact = runtime
        .update_work_item_fields(
            legacy_with_artifact.id.clone(),
            Some("Keep artifact while clearing inline plan".into()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

    assert!(updated_with_artifact.legacy_inline_plan.is_none());
    assert_eq!(
        std::fs::read_to_string(&existing_plan_path).unwrap(),
        "Existing artifact body."
    );
}

#[tokio::test]
async fn update_work_item_can_refine_objective_and_todo_list_together() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let work_item = runtime
        .create_work_item("Fix issue #869".into(), None, None, Vec::new())
        .await
        .unwrap();

    let (update_result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "refine-target-and-plan".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item.id.clone(),
                    "objective": "Fix issue #869 by allowing objective refinement",
                    "todo_list": [
                        { "text": "extend UpdateWorkItem schema", "state": "completed" },
                        { "text": "verify target update persistence", "state": "in_progress" }
                    ]
                }),
            },
        )
        .await
        .unwrap();
    let payload = update_result.envelope.result.unwrap();
    assert_eq!(
        payload["work_item"]["objective"].as_str(),
        Some("Fix issue #869 by allowing objective refinement")
    );
    assert_eq!(
        payload["work_item"]["todo_list"][0]["state"].as_str(),
        Some("completed")
    );
    assert_eq!(
        payload["work_item"]["todo_list"][1]["state"].as_str(),
        Some("in_progress")
    );
}

#[tokio::test]
async fn update_work_item_rejects_empty_objective() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let work_item = runtime
        .create_work_item("Fix issue #869".into(), None, None, Vec::new())
        .await
        .unwrap();

    let error = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "empty-target".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item.id.clone(),
                    "objective": "   "
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert!(tool_error.message.contains("objective"));
}

#[tokio::test]
async fn update_work_item_old_plan_shape_returns_state_example_hint() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());

    let error = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "bad-update".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": "item-1",
                    "todo_list": [
                        { "text": "inspect current handler", "status": "completed" }
                    ]
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
    let parse_error = tool_error
        .details
        .as_ref()
        .and_then(|details| details.get("parse_error"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(parse_error.contains("unknown field `status`"));
    assert!(parse_error.contains("state"));
    let recovery_hint = tool_error.recovery_hint.as_deref().unwrap_or_default();
    assert!(recovery_hint.contains("work_item_id"));
    assert!(recovery_hint.contains("\"state\":\"completed\""));
    assert!(recovery_hint.contains("pending, in_progress, or completed"));
}

#[tokio::test]
async fn update_work_item_missing_id_returns_top_level_field_hint() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());

    let error = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "missing-id".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "todo_list": [
                        { "text": "inspect current handler", "state": "completed" }
                    ]
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
    let recovery_hint = tool_error.recovery_hint.as_deref().unwrap_or_default();
    assert!(recovery_hint.contains("UpdateWorkItem schema"));
    assert!(recovery_hint.contains("work_item_id"));
    assert!(recovery_hint.contains("\"state\":\"completed\""));
}

#[tokio::test]
async fn persist_brief_binds_current_turn_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;

    runtime
        .persist_brief(&BriefRecord::new(
            "default",
            BriefKind::Result,
            "bound brief",
            None,
            None,
        ))
        .await
        .unwrap();

    let briefs = runtime.recent_briefs(10).await.unwrap();
    assert_eq!(briefs.len(), 1);
    assert_eq!(
        briefs[0].work_item_id.as_deref(),
        Some(work_item_id.as_str())
    );
}

#[tokio::test]
async fn create_callback_returns_default_ingress_with_current_turn_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;

    runtime
        .create_callback(
            "wait for review".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:302".into()),
            CallbackDeliveryMode::WakeHint,
        )
        .await
        .unwrap();

    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    let descriptors = runtime.latest_external_triggers().await.unwrap();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].scope, ExternalTriggerScope::Agent);
    assert!(descriptors[0].waiting_intent_id.is_none());
}

#[tokio::test]
async fn interactive_tool_execution_binds_current_turn_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(OneToolThenTextProvider {
            calls: Mutex::new(0),
        }),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens: 16384,
            compaction_keep_recent_estimated_tokens: 2048,
            ..context_config()
        },
    )
    .unwrap();

    let work_item = runtime
        .create_work_item("verify binding".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work_item.id.clone()).await.unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "run one verification command".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let tools = runtime.storage().read_recent_tool_executions(10).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_name, "ExecCommand");
    assert_eq!(
        tools[0].work_item_id.as_deref(),
        Some(work_item.id.as_str())
    );
}

#[tokio::test]
async fn complete_work_item_promotes_same_round_report_and_binds_evidence() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let seed_runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work_item = seed_runtime
        .create_work_item("ship completion report".into(), None, None, Vec::new())
        .await
        .unwrap();
    seed_runtime
        .pick_work_item(work_item.id.clone())
        .await
        .unwrap();

    let report_text = "Implemented completion report promotion and verified focused tests.";
    let provider = Arc::new(CompleteWorkItemReportProvider {
        work_item_id: work_item.id.clone(),
        report_text: Some(report_text.into()),
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        continuation_context_config(),
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "finish the tracked work".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        *provider.calls.lock().await,
        2,
        "promoted completion report should not hard-stop the provider loop"
    );

    let completed = runtime
        .latest_work_item(&work_item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, WorkItemState::Completed);
    assert_eq!(completed.result_summary.as_deref(), Some(report_text));
    let summary = runtime
        .storage()
        .latest_delivery_summary(&work_item.id)
        .unwrap()
        .expect("completion report should persist delivery summary");
    assert_eq!(summary.text, report_text);
    let briefs = runtime.recent_briefs(10).await.unwrap();
    let result_briefs = briefs
        .iter()
        .filter(|brief| brief.kind == BriefKind::Result && brief.text == report_text)
        .collect::<Vec<_>>();
    assert_eq!(
        result_briefs.len(),
        1,
        "completion report should produce exactly one user-facing result brief"
    );
    assert_eq!(
        result_briefs[0].work_item_id.as_deref(),
        Some(work_item.id.as_str())
    );
    assert_eq!(result_briefs[0].turn_index, Some(1));
    assert!(
        !briefs.iter().any(|brief| {
            brief.kind == BriefKind::Result
                && brief.related_message_id.as_deref() == Some(message.id.as_str())
        }),
        "normal terminal result brief should be suppressed after completion promotion"
    );
    let tools = runtime.storage().read_recent_tool_executions(10).unwrap();
    let complete_tool = tools
        .iter()
        .find(|tool| tool.tool_name == "CompleteWorkItem")
        .expect("completion tool should be recorded");
    assert_eq!(
        complete_tool.work_item_id.as_deref(),
        Some(work_item.id.as_str())
    );
    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let assistant_round = transcript
        .iter()
        .find(|entry| entry.kind == crate::types::TranscriptEntryKind::AssistantRound)
        .expect("assistant round should be recorded");
    assert_eq!(
        assistant_round.data["work_item_id"].as_str(),
        Some(work_item.id.as_str())
    );
    let tool_results = transcript
        .iter()
        .find(|entry| entry.kind == crate::types::TranscriptEntryKind::ToolResults)
        .expect("tool results should be recorded");
    let tool_result: serde_json::Value =
        serde_json::from_str(tool_results.data["results"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        tool_result["result"]["completion_report_promoted"].as_bool(),
        Some(true)
    );
    let events = runtime.storage().read_recent_events(usize::MAX).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "turn_terminal_brief_suppressed"
            && event.data["reason"].as_str() == Some("work_item_completion_report_promoted")
            && event.data["work_item_id"].as_str() == Some(work_item.id.as_str())
    }));
}

#[tokio::test]
async fn complete_work_item_followed_by_same_round_tool_keeps_terminal_brief() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let seed_runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work_item = seed_runtime
        .create_work_item("complete then verify".into(), None, None, Vec::new())
        .await
        .unwrap();
    seed_runtime
        .pick_work_item(work_item.id.clone())
        .await
        .unwrap();

    let provider = Arc::new(CompleteThenExecProvider {
        work_item_id: work_item.id.clone(),
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        continuation_context_config(),
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "complete and verify".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(*provider.calls.lock().await, 2);
    let briefs = runtime.recent_briefs(10).await.unwrap();
    assert!(briefs.iter().any(|brief| {
        brief.kind == BriefKind::Result && brief.text == "Finished the tracked work."
    }));
    assert!(briefs.iter().any(|brief| {
        brief.kind == BriefKind::Result && brief.text == "Verified after completion."
    }));
    let events = runtime.storage().read_recent_events(usize::MAX).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "turn_terminal_brief_suppressed"));
}

#[tokio::test]
async fn complete_work_item_does_not_promote_text_before_other_tool() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let seed_runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work_item = seed_runtime
        .create_work_item("complete after another tool".into(), None, None, Vec::new())
        .await
        .unwrap();
    seed_runtime
        .pick_work_item(work_item.id.clone())
        .await
        .unwrap();

    let provider = Arc::new(StaleTextThenCompleteProvider {
        work_item_id: work_item.id.clone(),
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider,
        "default".into(),
        continuation_context_config(),
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "inspect then complete".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let completed = runtime
        .latest_work_item(&work_item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, WorkItemState::Completed);
    assert_eq!(completed.result_summary, None);
    assert!(runtime
        .storage()
        .latest_delivery_summary(&work_item.id)
        .unwrap()
        .is_none());
    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let tool_results = transcript
        .iter()
        .find(|entry| entry.kind == crate::types::TranscriptEntryKind::ToolResults)
        .expect("tool results should be recorded");
    let completion_result = tool_results.data["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["tool_use_id"].as_str() == Some("complete-work"))
        .expect("completion result should be recorded");
    let content: serde_json::Value =
        serde_json::from_str(completion_result["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        content["result"]["completion_report_promoted"].as_bool(),
        None
    );
}

#[tokio::test]
async fn promoted_completion_report_resumes_next_queued_work_item_via_system_tick() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let seed_runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let active = seed_runtime
        .create_work_item("finish active work".into(), None, None, Vec::new())
        .await
        .unwrap();
    seed_runtime
        .pick_work_item(active.id.clone())
        .await
        .unwrap();
    let queued = seed_runtime
        .create_work_item("resume queued work".into(), None, None, Vec::new())
        .await
        .unwrap();

    let provider = Arc::new(CompleteWorkItemReportProvider {
        work_item_id: active.id.clone(),
        report_text: Some("Active work is complete.".into()),
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider,
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "finish active work".into(),
        },
    );

    runtime
        .process_message(message, closure_decision(ClosureOutcome::Completed, None))
        .await
        .unwrap();

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    let tick = messages
        .iter()
        .find(|message| {
            matches!(
                (&message.kind, &message.origin),
                (MessageKind::SystemTick, MessageOrigin::System { subsystem }) if subsystem == "work_queue"
            )
        })
        .expect("queued runnable work item should be resumed by work queue tick");
    assert_eq!(tick.work_item_id.as_deref(), Some(queued.id.as_str()));
    assert_eq!(
        tick.metadata
            .as_ref()
            .and_then(|metadata| metadata.get("work_queue"))
            .and_then(|metadata| metadata.get("reason"))
            .and_then(|value| value.as_str()),
        Some("queued_available")
    );
}

#[tokio::test]
async fn complete_work_item_without_same_round_report_warns_without_summary() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let seed_runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work_item = seed_runtime
        .create_work_item("complete without report".into(), None, None, Vec::new())
        .await
        .unwrap();
    seed_runtime
        .pick_work_item(work_item.id.clone())
        .await
        .unwrap();

    let provider = Arc::new(CompleteWorkItemReportProvider {
        work_item_id: work_item.id.clone(),
        report_text: None,
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "finish the tracked work".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();
    let completed = runtime
        .latest_work_item(&work_item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, WorkItemState::Completed);
    assert_eq!(completed.result_summary, None);
    assert!(runtime
        .storage()
        .latest_delivery_summary(&work_item.id)
        .unwrap()
        .is_none());
    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let tool_results = transcript
        .iter()
        .find(|entry| entry.kind == crate::types::TranscriptEntryKind::ToolResults)
        .expect("tool results should be recorded");
    let tool_result: serde_json::Value =
        serde_json::from_str(tool_results.data["results"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        tool_result["result"]["warnings"][0]["kind"].as_str(),
        Some("missing_completion_report")
    );
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "work_item_completion_warning"
            && event.data["kind"].as_str() == Some("missing_completion_report")
            && event.data["work_item_id"].as_str() == Some(work_item.id.as_str())
    }));
    assert!(!events
        .iter()
        .any(|event| event.kind == "turn_terminal_brief_suppressed"));
}

#[tokio::test]
async fn multiple_complete_work_items_in_one_round_do_not_promote_or_short_circuit() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let seed_runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let first = seed_runtime
        .create_work_item("first completion".into(), None, None, Vec::new())
        .await
        .unwrap();
    let second = seed_runtime
        .create_work_item("second completion".into(), None, None, Vec::new())
        .await
        .unwrap();

    let provider = Arc::new(MultiCompleteWorkItemReportProvider {
        work_item_ids: vec![first.id.clone(), second.id.clone()],
        report_texts: vec![
            "Finished the first work item.".into(),
            "Finished the second work item.".into(),
        ],
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        continuation_context_config(),
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "finish both tracked items".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    for (work_item_id, report_text) in [
        (&first.id, "Finished the first work item."),
        (&second.id, "Finished the second work item."),
    ] {
        let completed = runtime
            .latest_work_item(work_item_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, WorkItemState::Completed);
        assert_eq!(completed.result_summary.as_deref(), Some(report_text));
        assert_eq!(
            runtime
                .storage()
                .latest_delivery_summary(work_item_id)
                .unwrap()
                .expect("completion report should persist delivery summary")
                .text,
            report_text
        );
    }
    assert_eq!(
        *provider.calls.lock().await,
        2,
        "multiple completion reports should not hard-stop the provider loop"
    );
    let briefs = runtime.recent_briefs(10).await.unwrap();
    assert_eq!(
        briefs
            .iter()
            .filter(|brief| brief.kind == BriefKind::Result
                && brief.work_item_id.as_deref() == Some(first.id.as_str()))
            .count(),
        1
    );
    assert_eq!(
        briefs
            .iter()
            .filter(|brief| brief.kind == BriefKind::Result
                && brief.work_item_id.as_deref() == Some(second.id.as_str()))
            .count(),
        1
    );
    let events = runtime.storage().read_recent_events(usize::MAX).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.kind == "work_item_completion_warning"
                    && event.data["kind"].as_str()
                        == Some("completion_report_not_promoted_multiple_completions")
            })
            .count(),
        0
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "turn_terminal_brief_suppressed")
            .count(),
        2
    );
}

#[tokio::test]
async fn repeated_complete_work_item_does_not_overwrite_existing_report() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let seed_runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work_item = seed_runtime
        .create_work_item("already completed".into(), None, None, Vec::new())
        .await
        .unwrap();
    let completed = seed_runtime
        .complete_work_item(work_item.id.clone(), Vec::new())
        .await
        .unwrap();
    seed_runtime
        .promote_work_item_completion_report(
            completed.id.clone(),
            "Original completion report".into(),
            None,
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(CompleteWorkItemReportProvider {
            work_item_id: work_item.id.clone(),
            report_text: Some("Replacement report should not be promoted".into()),
            calls: Mutex::new(0),
        }),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "repeat completion".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let completed = runtime
        .latest_work_item(&work_item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        completed.result_summary.as_deref(),
        Some("Original completion report")
    );
    let summary = runtime
        .storage()
        .latest_delivery_summary(&work_item.id)
        .unwrap()
        .expect("original delivery summary should remain");
    assert_eq!(summary.text, "Original completion report");
    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let tool_results = transcript
        .iter()
        .find(|entry| entry.kind == crate::types::TranscriptEntryKind::ToolResults)
        .expect("tool results should be recorded");
    let tool_result: serde_json::Value =
        serde_json::from_str(tool_results.data["results"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        tool_result["result"]["completed_transition"].as_bool(),
        Some(false)
    );
    assert_ne!(
        tool_result["result"]["completion_report_promoted"].as_bool(),
        Some(true)
    );
}

#[tokio::test]
async fn complete_work_item_with_unfinished_todos_returns_structured_warning() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let work_item = runtime
        .create_work_item("finish todos".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .update_work_item_fields(
            work_item.id.clone(),
            None,
            None,
            None,
            Some(vec![
                TodoItem {
                    text: "still pending".into(),
                    state: TodoItemState::Pending,
                },
                TodoItem {
                    text: "actively checking".into(),
                    state: TodoItemState::InProgress,
                },
            ]),
            None,
        )
        .await
        .unwrap();

    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "complete".into(),
                name: "CompleteWorkItem".into(),
                input: serde_json::json!({"work_item_id": work_item.id}),
            },
        )
        .await
        .unwrap();
    let payload = result.envelope.result.unwrap();
    assert_eq!(
        payload["warnings"][0]["kind"].as_str(),
        Some("unfinished_todos")
    );
    assert_eq!(payload["warnings"][0]["pending_count"].as_u64(), Some(1));
    assert_eq!(
        payload["warnings"][0]["in_progress_count"].as_u64(),
        Some(1)
    );
    let events = runtime.storage().read_recent_events(20).unwrap();
    let completed_event = events
        .iter()
        .find(|event| {
            event.kind == "work_item_written" && event.data["action"].as_str() == Some("completed")
        })
        .expect("completion event should be recorded");
    assert_eq!(completed_event.data["warning_count"].as_u64(), Some(1));
    assert_eq!(
        completed_event.data["warnings"][0]["kind"].as_str(),
        Some("unfinished_todos")
    );
}

#[tokio::test]
async fn runtime_sleeps_after_processing() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "hello".into(),
        },
    );
    runtime.enqueue(message).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.status, AgentStatus::Asleep);
    runtime_task.abort();
}

#[tokio::test]
async fn turn_end_work_item_commit_defaults_completed_turn_to_active() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;
    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert!(committed.blocked_by.is_none());
    assert!(runtime
        .agent_state()
        .await
        .unwrap()
        .current_turn_work_item_id
        .is_none());
}

#[tokio::test]
async fn turn_end_work_item_commit_ignores_unfinished_turn_binding() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("no terminal yet".into(), None, None, Vec::new())
        .await
        .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.turn_index = 7;
        guard.state.current_turn_work_item_id = Some(work.id.clone());
        guard.state.last_turn_terminal = None;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap();
    assert!(committed.is_none());
    assert_eq!(
        runtime
            .agent_state()
            .await
            .unwrap()
            .current_turn_work_item_id
            .as_deref(),
        Some(work.id.as_str())
    );
    assert_eq!(
        runtime
            .latest_work_item(&work.id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn turn_end_work_item_commit_keeps_failed_turn_open_without_blocker() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.last_turn_terminal = Some(TurnTerminalRecord {
            turn_index: guard.state.turn_index,
            kind: TurnTerminalKind::Aborted,
            reason: None,
            last_assistant_message: Some("provider context_length_exceeded".into()),
            checkpoint: None,
            completed_at: Utc::now(),
            duration_ms: 42,
        });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert!(committed.blocked_by.is_none());
}

#[tokio::test]
async fn turn_end_work_item_commit_preserves_existing_blocker_on_failed_turn() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work_item_id = seed_bound_work_item(
        &runtime,
        WorkItemState::Open,
        None,
        Some("Waiting for reviewer response."),
    )
    .await;
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.last_turn_terminal = Some(TurnTerminalRecord {
            turn_index: guard.state.turn_index,
            kind: TurnTerminalKind::Aborted,
            reason: None,
            last_assistant_message: Some("provider timeout".into()),
            checkpoint: None,
            completed_at: Utc::now(),
            duration_ms: 42,
        });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert_eq!(
        committed.blocked_by.as_deref(),
        Some("Waiting for reviewer response.")
    );
}

#[tokio::test]
async fn turn_end_work_item_commit_does_not_block_bound_item_for_active_task_presence() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;
    mark_blocking_task(&runtime, "blocking-wait").await;

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert_eq!(committed.blocked_by.as_deref(), None);
}

#[tokio::test]
async fn turn_end_work_item_commit_preserves_explicit_completed_claim_without_waiting_facts() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work_item_id = seed_bound_work_item(
        &runtime,
        WorkItemState::Completed,
        Some("finished"),
        Some("all requested changes are done"),
    )
    .await;
    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Completed);
    assert!(committed.blocked_by.is_none());
}

#[tokio::test]
async fn turn_end_work_item_commit_rejects_completed_claim_when_runtime_is_still_waiting() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work_item_id = seed_bound_work_item(
        &runtime,
        WorkItemState::Completed,
        Some("finished"),
        Some("marked complete too early"),
    )
    .await;
    mark_blocking_task(&runtime, "blocking-after-complete").await;

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Completed);
    assert!(committed.blocked_by.is_none());
}

#[tokio::test]
async fn turn_end_work_item_commit_preserves_explicit_queued_claim() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work_item_id = seed_bound_work_item(
        &runtime,
        WorkItemState::Open,
        Some("yield the active slot"),
        Some("requeue after this turn"),
    )
    .await;
    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert!(committed.blocked_by.is_none());
}

#[tokio::test]
async fn external_trigger_creation_returns_default_ingress_without_waiting_intent() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let capability = runtime
        .create_external_trigger(
            "wait for external review".into(),
            "github".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(capability.delivery_mode, CallbackDeliveryMode::WakeHint);
    assert_eq!(capability.status, ExternalTriggerStatus::Active);
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
}

#[tokio::test]
async fn agent_scoped_external_trigger_survives_missing_work_item_cleanup() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let capability = runtime
        .create_external_trigger(
            "Check durable inbox for unread entries".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "test".into(),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "tick".into(),
        },
    );
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    assert!(runtime
        .latest_external_triggers()
        .await
        .unwrap()
        .iter()
        .any(
            |record| record.external_trigger_id == capability.external_trigger_id
                && record.status == ExternalTriggerStatus::Active
        ));
    let closure = runtime.current_closure_decision().await.unwrap();
    assert_ne!(
        closure.waiting_reason,
        Some(WaitingReason::AwaitingExternalChange)
    );
    let events = runtime.storage().read_recent_events(16).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "missing_current_work_item_before_wait"));
}

#[tokio::test]
async fn agent_scoped_wake_hint_preserves_external_trigger_provenance() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::Asleep;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let capability = runtime
        .create_external_trigger(
            "Check AgentInbox for unread items".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();

    let result = runtime
        .deliver_callback(
            &capability.external_trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Json {
                    value: serde_json::json!({
                        "latest_entry_id": "ent_123",
                        "preview": "new inbox item"
                    }),
                }),
                content_type: Some("application/json".into()),
                correlation_id: Some("corr-inbox".into()),
                causation_id: Some("cause-webhook".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.disposition, CallbackIngressDisposition::Triggered);
    assert_eq!(result.scope, ExternalTriggerScope::Agent);

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    let tick = messages
        .iter()
        .find(|message| message.kind == MessageKind::SystemTick)
        .expect("wake hint should emit a system tick");
    let wake_hint = tick
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("wake_hint"))
        .expect("wake hint metadata should exist");
    assert_eq!(wake_hint["source"].as_str(), None);
    assert_eq!(wake_hint["scope"].as_str(), Some("agent"));
    assert_eq!(wake_hint["waiting_intent_id"].as_str(), None);
    assert_eq!(
        wake_hint["external_trigger_id"].as_str(),
        Some(capability.external_trigger_id.as_str())
    );
    assert_eq!(wake_hint["description"].as_str(), None);
    assert_eq!(wake_hint["correlation_id"].as_str(), Some("corr-inbox"));
    assert_eq!(wake_hint["causation_id"].as_str(), Some("cause-webhook"));
    assert_eq!(
        wake_hint["body"]["value"]["latest_entry_id"].as_str(),
        Some("ent_123")
    );
}

#[tokio::test]
async fn legacy_descriptor_preserves_provenance_after_wait_cancel() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let now = Utc::now();
    let waiting_id = "legacy-waiting".to_string();
    let trigger_id = "legacy-trigger".to_string();
    runtime
        .storage()
        .append_waiting_intent(&WaitingIntentRecord {
            id: waiting_id.clone(),
            agent_id: "default".into(),
            scope: WaitingIntentScope::Agent,
            work_item_id: None,
            description: "legacy review wait".into(),
            source: "github".into(),
            resource: Some("pull_request:1215".into()),
            condition: Some("review submitted".into()),
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            status: WaitingIntentStatus::Active,
            external_trigger_id: trigger_id.clone(),
            created_at: now,
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: Some("legacy-corr".into()),
            causation_id: Some("legacy-cause".into()),
        })
        .unwrap();
    runtime
        .storage()
        .append_external_trigger(&ExternalTriggerRecord {
            external_trigger_id: trigger_id.clone(),
            target_agent_id: "default".into(),
            waiting_intent_id: Some(waiting_id.clone()),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            trigger_url: Some("http://127.0.0.1:7878/callbacks/enqueue/legacy".into()),
            token_hash: "legacy-token-hash".into(),
            status: ExternalTriggerStatus::Active,
            created_at: now,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        })
        .unwrap();

    runtime.cancel_waiting(&waiting_id).await.unwrap();
    let result = runtime
        .deliver_callback(
            &trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Text {
                    text: "legacy payload".into(),
                }),
                content_type: Some("text/plain".into()),
                correlation_id: None,
                causation_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        result.waiting_intent_id.as_deref(),
        Some(waiting_id.as_str())
    );
    assert_eq!(result.delivery_mode, CallbackDeliveryMode::WakeHint);
    assert_ne!(result.disposition, CallbackIngressDisposition::Enqueued);
    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(messages
        .iter()
        .all(|message| message.kind != MessageKind::CallbackEvent));
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("wake_hint"))
                .and_then(|wake_hint| wake_hint.get("external_trigger_id"))
                .and_then(serde_json::Value::as_str)
                == Some(trigger_id.as_str())
    }));
    let waiting = runtime.latest_waiting_intents().await.unwrap();
    let cancelled = waiting
        .iter()
        .find(|record| record.id == waiting_id)
        .expect("legacy waiting intent should remain auditable");
    assert_eq!(cancelled.status, WaitingIntentStatus::Cancelled);
    assert_eq!(cancelled.trigger_count, 0);
}

#[tokio::test]
async fn default_external_ingress_wakes_without_owning_work_item_wait_state() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("wait for CI".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .update_work_item_fields(
            work.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("awaiting CI success".into())),
        )
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let capability = runtime
        .create_external_trigger(
            "wait for CI".into(),
            "github".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            Some("CI run completed".into()),
            Some("holon-run/holon#1079".into()),
        )
        .await
        .unwrap();

    let result = runtime
        .deliver_callback(
            &capability.external_trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Json {
                    value: serde_json::json!({"check": "Rust", "conclusion": "success"}),
                }),
                content_type: Some("application/json".into()),
                correlation_id: Some("corr-ci".into()),
                causation_id: Some("run-123".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.disposition, CallbackIngressDisposition::Triggered);

    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    let messages = runtime.storage().read_recent_messages(10).unwrap();
    let tick = messages
        .iter()
        .find(|message| message.kind == MessageKind::SystemTick)
        .expect("wake hint should emit a system tick");
    assert_eq!(tick.work_item_id.as_deref(), None);
    let wake_hint = tick
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("wake_hint"))
        .expect("wake hint metadata should exist");
    assert_eq!(wake_hint["scope"].as_str(), Some("agent"));
    assert_eq!(wake_hint["work_item_id"].as_str(), None);
    assert_eq!(wake_hint["waiting_intent_id"].as_str(), None);

    let repeated = runtime
        .deliver_callback(
            &capability.external_trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Json {
                    value: serde_json::json!({"check": "Rust", "conclusion": "success", "attempt": 2}),
                }),
                content_type: Some("application/json".into()),
                correlation_id: Some("corr-ci".into()),
                causation_id: Some("run-124".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(repeated.disposition, CallbackIngressDisposition::Coalesced);
    let latest = runtime
        .storage()
        .latest_work_item(&work.id)
        .unwrap()
        .expect("work item should exist");
    assert_eq!(latest.blocked_by.as_deref(), Some("awaiting CI success"));

    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "callback_delivered"
            && event.data["waiting_intent_id"].is_null()
            && event.data["work_item_id"].is_null()
            && event.data["external_trigger_id"].as_str()
                == Some(capability.external_trigger_id.as_str())
    }));

    let completed = runtime
        .complete_work_item(work.id.clone(), Vec::new())
        .await
        .unwrap();
    assert_eq!(completed.state, WorkItemState::Completed);
    assert!(completed.blocked_by.is_none());
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
}

#[tokio::test]
async fn external_wake_records_wait_reconciliation_without_resolving_wait() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("reconciled")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let now = Utc::now();
    let work = runtime
        .create_work_item("wait for CI".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let waiting_id = "wait-ci".to_string();
    let trigger_id = "trigger-ci".to_string();
    runtime
        .storage()
        .append_waiting_intent(&WaitingIntentRecord {
            id: waiting_id.clone(),
            agent_id: "default".into(),
            scope: WaitingIntentScope::WorkItem,
            work_item_id: Some(work.id.clone()),
            description: "wait for CI".into(),
            source: "github".into(),
            resource: Some("holon-run/holon#1292".into()),
            condition: Some("checks complete".into()),
            delivery_mode: CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: trigger_id.clone(),
            created_at: now,
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        })
        .unwrap();
    runtime
        .storage()
        .append_external_trigger(&ExternalTriggerRecord {
            external_trigger_id: trigger_id.clone(),
            target_agent_id: "default".into(),
            waiting_intent_id: Some(waiting_id.clone()),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            trigger_url: Some("http://127.0.0.1:7878/callbacks/wake/ci".into()),
            token_hash: "token-hash".into(),
            status: ExternalTriggerStatus::Active,
            created_at: now,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        })
        .unwrap();

    runtime
        .deliver_callback(
            &trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Json {
                    value: serde_json::json!({"check": "Rust", "conclusion": "success"}),
                }),
                content_type: Some("application/json".into()),
                correlation_id: Some("corr-ci".into()),
                causation_id: Some("run-123".into()),
            },
        )
        .await
        .unwrap();
    let tick = runtime
        .storage()
        .read_recent_messages(10)
        .unwrap()
        .into_iter()
        .find(|message| message.kind == MessageKind::SystemTick)
        .expect("wake hint should enqueue a system tick");

    runtime
        .process_message(
            tick,
            closure_decision(
                ClosureOutcome::Waiting,
                Some(WaitingReason::AwaitingExternalChange),
            ),
        )
        .await
        .unwrap();

    let events = runtime.storage().read_recent_events(100).unwrap();
    let signal = events
        .iter()
        .find(|event| {
            event.kind == "wait_reconciliation_requested"
                && event.data["wait_condition_id"] == format!("waiting_intent:{waiting_id}")
        })
        .expect("external wake should request wait reconciliation");
    assert_eq!(
        signal.data["wake_source"].as_str(),
        Some("external_ingress")
    );
    assert_eq!(signal.data["work_item_id"].as_str(), Some(work.id.as_str()));
    assert_eq!(
        signal.data["subject_ref"].as_str(),
        Some(trigger_id.as_str())
    );
    assert_eq!(signal.data["waiting_for"].as_str(), Some("checks complete"));

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    let active = waiting
        .iter()
        .find(|record| record.id == waiting_id)
        .expect("waiting intent should remain visible after wake firing");
    assert_eq!(active.status, WaitingIntentStatus::Active);
    assert_eq!(active.trigger_count, 1);
}

#[tokio::test]
async fn completing_work_item_does_not_revoke_agent_external_trigger() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("wait for external review".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let capability = runtime
        .create_external_trigger(
            "wait for review".into(),
            "github".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();

    runtime
        .complete_work_item(work.id.clone(), Vec::new())
        .await
        .unwrap();

    let descriptor = runtime
        .latest_external_triggers()
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.external_trigger_id == capability.external_trigger_id)
        .expect("external trigger descriptor should exist");
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    assert_eq!(descriptor.status, ExternalTriggerStatus::Active);

    let result = runtime
        .deliver_callback(
            &capability.external_trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Text {
                    text: "late callback".into(),
                }),
                content_type: Some("text/plain".into()),
                correlation_id: None,
                causation_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.disposition, CallbackIngressDisposition::Triggered);
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "work_item_waiting_intents_cancelled"));
}

#[tokio::test]
async fn creating_agent_trigger_is_idempotent_for_source_and_delivery_mode() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("replace external condition".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let old_capability = runtime
        .create_external_trigger(
            "wait for CI".into(),
            "github".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            Some("ci pending".into()),
            Some("pull/1217".into()),
        )
        .await
        .unwrap();
    let new_capability = runtime
        .create_external_trigger(
            "wait for review approval".into(),
            "github".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            Some("review approved".into()),
            Some("pull/1217#review".into()),
        )
        .await
        .unwrap();

    assert_eq!(
        old_capability.external_trigger_id,
        new_capability.external_trigger_id
    );
    assert_eq!(new_capability.status, ExternalTriggerStatus::Active);

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    let descriptors = runtime.latest_external_triggers().await.unwrap();
    assert!(waiting.is_empty());
    assert!(descriptors.iter().any(|record| {
        record.external_trigger_id == new_capability.external_trigger_id
            && record.status == ExternalTriggerStatus::Active
            && record.scope == ExternalTriggerScope::Agent
    }));
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "agent_waiting_intents_cancelled"));
}

#[tokio::test]
async fn default_external_ingress_ignores_legacy_active_trigger_without_url() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let now = Utc::now();
    let legacy_trigger_id = "legacy-missing-url-trigger".to_string();
    runtime
        .storage()
        .append_external_trigger(&ExternalTriggerRecord {
            external_trigger_id: legacy_trigger_id.clone(),
            target_agent_id: "default".into(),
            waiting_intent_id: None,
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            trigger_url: None,
            token_hash: "legacy-token-hash".into(),
            status: ExternalTriggerStatus::Active,
            created_at: now,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        })
        .unwrap();

    let capability = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await
        .unwrap();

    assert_ne!(capability.external_trigger_id, legacy_trigger_id);
    assert_eq!(capability.status, ExternalTriggerStatus::Active);
    assert!(capability.trigger_url.contains("/callbacks/wake/"));
    let descriptors = runtime.latest_external_triggers().await.unwrap();
    assert!(descriptors.iter().any(|record| {
        record.external_trigger_id == legacy_trigger_id
            && record.status == ExternalTriggerStatus::Active
            && record.trigger_url.is_none()
    }));
    assert!(descriptors.iter().any(|record| {
        record.external_trigger_id == capability.external_trigger_id
            && record.status == ExternalTriggerStatus::Active
            && record.trigger_url.as_deref() == Some(capability.trigger_url.as_str())
    }));
}

#[tokio::test]
async fn picking_new_work_item_does_not_cancel_agent_trigger() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let old_work = runtime
        .create_work_item("old waiting condition".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(old_work.id.clone()).await.unwrap();
    let old_work_trigger = runtime
        .create_external_trigger(
            "wait for old work event".into(),
            "github".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    let agent_trigger = runtime
        .create_external_trigger(
            "watch durable inbox".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    let new_work = runtime
        .create_work_item("new active condition".into(), None, None, Vec::new())
        .await
        .unwrap();

    runtime.pick_work_item(new_work.id.clone()).await.unwrap();

    assert_eq!(
        old_work_trigger.external_trigger_id,
        agent_trigger.external_trigger_id
    );
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    assert!(runtime
        .latest_external_triggers()
        .await
        .unwrap()
        .iter()
        .any(
            |record| record.external_trigger_id == agent_trigger.external_trigger_id
                && record.status == ExternalTriggerStatus::Active
        ));
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "work_item_waiting_intents_cancelled"));
}

#[tokio::test]
async fn reconcile_waiting_contract_preserves_agent_callback_after_active_work_switch() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let old_work = runtime
        .create_work_item("old objective".into(), None, None, Vec::new())
        .await
        .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard
            .state
            .working_memory
            .current_working_memory
            .current_work_item_id = Some(old_work.id.clone());
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let capability = runtime
        .create_callback(
            "wait for old review".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:123".into()),
            CallbackDeliveryMode::WakeHint,
        )
        .await
        .unwrap();
    let old_waiting_created_at = Utc::now();

    runtime
        .complete_work_item(old_work.id.clone(), Vec::new())
        .await
        .unwrap();
    let new_work = runtime
        .create_work_item("new objective".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(new_work.id.clone()).await.unwrap();

    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "switch to the new target".into(),
        },
    );
    message.created_at = old_waiting_created_at + chrono::Duration::seconds(1);
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    assert_eq!(capability.status, ExternalTriggerStatus::Active);
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    assert_eq!(
        runtime
            .storage()
            .work_queue_prompt_projection()
            .unwrap()
            .current
            .as_ref()
            .map(|item| item.id.as_str()),
        Some(new_work.id.as_str())
    );
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "work_item_waiting_intents_cancelled"));
}

#[tokio::test]
async fn reconcile_waiting_contract_keeps_agent_scoped_waits_after_active_work_switch() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let old_work = runtime
        .create_work_item("old objective".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(old_work.id.clone()).await.unwrap();
    let capability = runtime
        .create_external_trigger(
            "Check durable inbox for unread entries".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    let old_waiting_created_at = Utc::now();

    runtime
        .complete_work_item(old_work.id.clone(), Vec::new())
        .await
        .unwrap();
    let new_work = runtime
        .create_work_item("new objective".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(new_work.id.clone()).await.unwrap();

    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "switch to the new target".into(),
        },
    );
    message.created_at = old_waiting_created_at + chrono::Duration::seconds(1);
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    assert_eq!(capability.status, ExternalTriggerStatus::Active);
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "stale_waiting_intents_cancelled"));
}

#[tokio::test]
async fn default_external_ingress_does_not_contribute_to_waiting_closure() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    runtime
        .create_external_trigger(
            "Check durable inbox for unread entries".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_ne!(closure.outcome, ClosureOutcome::Waiting);
    assert_ne!(
        closure.waiting_reason,
        Some(WaitingReason::AwaitingExternalChange)
    );
    assert!(closure
        .evidence
        .iter()
        .all(|item| item != "active_waiting_intents=1"));
}

#[tokio::test]
async fn current_closure_ignores_other_agent_timers() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    runtime
        .inner
        .storage
        .append_timer(&TimerRecord {
            id: "other-timer".into(),
            agent_id: "other".into(),
            created_at: Utc::now(),
            duration_ms: 1000,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: None,
            next_fire_at: Some(Utc::now()),
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();

    assert_ne!(closure.waiting_reason, Some(WaitingReason::AwaitingTimer));
    assert!(!closure
        .evidence
        .iter()
        .any(|evidence| evidence == "active_timers=1"));
}

#[tokio::test]
async fn reconcile_waiting_contract_preserves_agent_callback_when_only_waiting_anchor_exists() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let waiting_work = runtime
        .create_work_item("waiting-only objective".into(), None, None, Vec::new())
        .await
        .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard
            .state
            .working_memory
            .current_working_memory
            .current_work_item_id = Some(waiting_work.id.clone());
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let capability = runtime
        .create_callback(
            "wait for external response".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:456".into()),
            CallbackDeliveryMode::WakeHint,
        )
        .await
        .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "test".into(),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "tick".into(),
        },
    );
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    assert_eq!(capability.status, ExternalTriggerStatus::Active);
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    assert!(runtime
        .storage()
        .work_queue_prompt_projection()
        .unwrap()
        .current
        .is_none());
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "work_item_waiting_intents_cancelled"));
}

#[tokio::test]
async fn reconcile_waiting_contract_keeps_waits_when_anchor_is_newly_established() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("newly anchored objective".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let capability = runtime
        .create_callback(
            "wait for fresh review".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:789".into()),
            CallbackDeliveryMode::WakeHint,
        )
        .await
        .unwrap();
    let waiting_created_at = Utc::now();

    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "test".into(),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "tick".into(),
        },
    );
    message.created_at = waiting_created_at + chrono::Duration::seconds(1);
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    assert_eq!(capability.status, ExternalTriggerStatus::Active);
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
    assert_eq!(
        runtime
            .storage()
            .waiting_contract_anchor()
            .unwrap()
            .as_ref()
            .map(|item| item.id.as_str()),
        Some(work.id.as_str())
    );
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "stale_waiting_intents_cancelled"));
}

#[tokio::test]
async fn current_closure_reports_continuable_for_current_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    let active_id = active.id.clone();
    storage.append_work_item(&active).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active_id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Continuable);
    assert_eq!(closure.waiting_reason, None);
    let signal = closure.work_signal.expect("work signal should exist");
    assert_eq!(signal.work_item_id, active_id);
    assert_eq!(signal.state, WorkItemState::Open);
    assert_eq!(
        signal.reactivation_mode,
        WorkReactivationMode::ContinueActive
    );
}

#[tokio::test]
async fn current_needs_input_work_item_waits_for_operator_without_work_signal() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut active = WorkItemRecord::new(
        "default",
        "choose implementation direction",
        WorkItemState::Open,
    );
    active.plan_status = WorkItemPlanStatus::NeedsInput;
    let active_id = active.id.clone();
    storage.append_work_item(&active).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active_id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        closure.waiting_reason,
        Some(WaitingReason::AwaitingOperatorInput)
    );
    assert!(closure.work_signal.is_none());
    assert!(closure
        .evidence
        .iter()
        .any(|item| item == "awaiting_operator_input_signal"));

    let emitted = runtime.maybe_emit_pending_system_tick(None).await.unwrap();
    assert!(
        !emitted,
        "needs_input current work item must not auto-resume through work_queue"
    );
}

#[tokio::test]
async fn current_external_wait_does_not_suppress_queued_runnable_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let current = WorkItemRecord::new(
        "default",
        "waiting for external review",
        WorkItemState::Open,
    );
    let current_id = current.id.clone();
    storage.append_work_item(&current).unwrap();
    let queued = WorkItemRecord::new("default", "queued follow-up work", WorkItemState::Open);
    let queued_id = queued.id.clone();
    storage.append_work_item(&queued).unwrap();
    storage
        .append_waiting_intent(&WaitingIntentRecord {
            id: "wait-current".into(),
            agent_id: "default".into(),
            scope: WaitingIntentScope::WorkItem,
            work_item_id: Some(current_id.clone()),
            description: "wait for current review".into(),
            source: "github".into(),
            resource: Some("pull_request:1".into()),
            condition: None,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: "trigger-current".into(),
            created_at: Utc::now(),
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        })
        .unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(current_id);
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Continuable);
    assert_eq!(closure.waiting_reason, None);
    let signal = closure.work_signal.expect("work signal should exist");
    assert_eq!(signal.work_item_id, queued_id);
    assert_eq!(
        signal.reactivation_mode,
        WorkReactivationMode::ActivateQueued
    );
    assert!(closure
        .evidence
        .iter()
        .any(|item| item == "current_work_item_scheduling_state=WaitingExternal"));
}

#[tokio::test]
async fn current_closure_reports_continuable_for_queued_work_item_without_active_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let queued = WorkItemRecord::new(
        "default",
        "surface queued runtime cleanup",
        WorkItemState::Open,
    );
    let queued_id = queued.id.clone();
    storage.append_work_item(&queued).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Continuable);
    assert_eq!(closure.waiting_reason, None);
    let signal = closure.work_signal.expect("work signal should exist");
    assert_eq!(signal.work_item_id, queued_id);
    assert_eq!(signal.state, WorkItemState::Open);
    assert_eq!(
        signal.reactivation_mode,
        WorkReactivationMode::ActivateQueued
    );
}

#[tokio::test]
async fn queued_needs_input_work_item_is_not_runnable() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut queued =
        WorkItemRecord::new("default", "waiting planning candidate", WorkItemState::Open);
    queued.plan_status = WorkItemPlanStatus::NeedsInput;
    storage.append_work_item(&queued).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Completed);
    assert_eq!(closure.waiting_reason, None);
    assert!(closure.work_signal.is_none());

    let emitted = runtime.maybe_emit_pending_system_tick(None).await.unwrap();
    assert!(
        !emitted,
        "queued needs_input work item should not emit queued_available"
    );
}

#[tokio::test]
async fn queued_notification_keeps_working_memory_unfocused_before_pick() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let queued = WorkItemRecord::new("default", "queued runtime cleanup", WorkItemState::Open);
    storage.append_work_item(&queued).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "wrap up current work".into(),
            },
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let state = runtime.agent_state().await.unwrap();
    assert!(state.current_work_item_id.is_none());
    assert!(state
        .working_memory
        .current_working_memory
        .current_work_item_id
        .is_none());
    assert!(
        state.working_memory.working_memory_revision > 0,
        "working memory should refresh after queued notification"
    );
    let deltas = runtime
        .storage()
        .read_recent_working_memory_deltas(10)
        .unwrap();
    assert!(!deltas.iter().any(|delta| delta
        .changed_fields
        .iter()
        .any(|field| field == "current_work_item_id")));
    assert!(state
        .working_memory
        .active_episode_builder
        .as_ref()
        .and_then(|builder| builder.current_work_item_id.as_deref())
        .is_none());

    runtime_task.abort();
}

#[tokio::test]
async fn blocking_current_work_item_releases_focus_and_unblock_does_not_repick() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("wait for dependency".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();

    let blocked = runtime
        .update_work_item_fields(
            work.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("blocked on review".into())),
        )
        .await
        .unwrap();

    assert_eq!(blocked.readiness(), WorkItemReadiness::Blocked);
    let state = runtime.agent_state().await.unwrap();
    assert!(state.current_work_item_id.is_none());
    assert!(state.current_turn_work_item_id.is_none());
    let events = runtime.storage().read_recent_events(10).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "work_item_focus_released"
            && event.data["reason"] == "work_item_blocked"
            && event.data["work_item_id"].as_str() == Some(work.id.as_str())
    }));

    let unblocked = runtime
        .update_work_item_fields(work.id.clone(), None, None, None, None, Some(None))
        .await
        .unwrap();

    assert_eq!(unblocked.readiness(), WorkItemReadiness::Runnable);
    let state = runtime.agent_state().await.unwrap();
    assert!(state.current_work_item_id.is_none());
    assert!(state.current_turn_work_item_id.is_none());
}

#[tokio::test]
async fn wait_for_tool_result_reports_released_blocked_focus() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("block through tool".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();

    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "block".into(),
                name: "WaitFor".into(),
                input: serde_json::json!({
                    "wake": "external",
                    "resource": "github:holon-run/holon#1446",
                    "reason": "blocked through tool result",
                }),
            },
        )
        .await
        .unwrap();

    let payload = result.envelope.result.unwrap();
    assert_eq!(payload["work_item"]["readiness"].as_str(), Some("blocked"));
    assert_eq!(payload["work_item"]["is_current"].as_bool(), Some(false));
    assert_eq!(payload["work_item"]["focus"].as_str(), Some("blocked"));
    let state = runtime.agent_state().await.unwrap();
    assert!(state.current_work_item_id.is_none());
}

#[tokio::test]
async fn needs_input_current_work_item_releases_focus() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("ask operator".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();

    let waiting = runtime
        .update_work_item_fields(
            work.id.clone(),
            None,
            Some(WorkItemPlanStatus::NeedsInput),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(waiting.readiness(), WorkItemReadiness::WaitingForOperator);
    let state = runtime.agent_state().await.unwrap();
    assert!(state.current_work_item_id.is_none());
    assert!(state.current_turn_work_item_id.is_none());
    let events = runtime.storage().read_recent_events(10).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "work_item_focus_released"
            && event.data["reason"] == "work_item_needs_input"
            && event.data["work_item_id"].as_str() == Some(work.id.as_str())
    }));
}

#[tokio::test]
async fn pick_blocked_work_item_reports_inspection_focus() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let work = runtime
        .create_work_item("inspect blocker".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .update_work_item_fields(
            work.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("blocked on external signal".into())),
        )
        .await
        .unwrap();

    let picked = runtime
        .pick_work_item_with_reason(work.id.clone(), Some("inspect blocker details".into()))
        .await
        .unwrap();

    assert_eq!(
        picked.current_work_item.readiness(),
        WorkItemReadiness::Blocked
    );
    assert_eq!(
        picked.transition.current_readiness,
        WorkItemReadiness::Blocked
    );
    assert_eq!(picked.transition.current_focus_mode, "inspection");
    assert!(picked.transition.warnings.is_empty());
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state.current_work_item_id.as_deref(),
        Some(work.id.as_str())
    );

    runtime
        .update_work_item_fields(
            work.id.clone(),
            None,
            None,
            None,
            Some(vec![crate::types::TodoItem {
                text: "inspect blocker evidence".into(),
                state: crate::types::TodoItemState::InProgress,
            }]),
            None,
        )
        .await
        .unwrap();
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state.current_work_item_id.as_deref(),
        Some(work.id.as_str())
    );
}

#[tokio::test]
async fn pick_without_reason_warns_when_switching_from_runnable_current_work() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let current = runtime
        .create_work_item("current runnable".into(), None, None, Vec::new())
        .await
        .unwrap();
    let next = runtime
        .create_work_item("next runnable".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(current.id.clone()).await.unwrap();

    let picked = runtime
        .pick_work_item_with_reason(next.id.clone(), None)
        .await
        .unwrap();

    assert_eq!(picked.transition.previous_work_item_id, Some(current.id));
    assert_eq!(
        picked.transition.previous_readiness,
        Some(WorkItemReadiness::Runnable)
    );
    assert_eq!(picked.transition.switch_kind, "explicit_focus_override");
    assert_eq!(picked.transition.warnings.len(), 1);
    assert_eq!(
        picked.transition.warnings[0].code,
        "missing_pick_reason_for_runnable_focus_switch"
    );
    let events = runtime.storage().read_recent_events(10).unwrap();
    let event = events
        .iter()
        .find(|event| {
            event.kind == "work_item_picked"
                && event.data["current_work_item_id"].as_str() == Some(next.id.as_str())
        })
        .expect("work_item_picked event");
    assert_eq!(
        event.data["warnings"][0]["code"].as_str(),
        Some("missing_pick_reason_for_runnable_focus_switch")
    );
}
