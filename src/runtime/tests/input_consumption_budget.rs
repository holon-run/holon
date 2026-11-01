use super::super::agent_message_delivery::PreparedAgentMessageDelivery;
use super::super::operator_dispatch::idle_input_budget;
use super::super::*;
use super::support::*;
use crate::types::{AgentMessageCallerContext, AgentMessagePrincipalKind, AgentMessageSendRequest};

fn runtime_fixture() -> (TempDir, TempDir, RuntimeHandle) {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "budget-target",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    (home, workspace, runtime)
}

fn prepared_input(key: &str, text: String) -> PreparedAgentMessageDelivery {
    AgentMessageDeliveryService::prepare(
        AgentMessageSendRequest {
            target_agent_id: "budget-target".into(),
            content: MessageBody::Text { text },
            client_idempotency_key: key.into(),
            correlation_id: None,
            causation_id: None,
            requested_priority: Some(Priority::Normal),
            forward: false,
            reply_context_id: None,
        },
        AgentMessageCallerContext {
            caller_principal: "agent:budget-sender".into(),
            caller_agent_id: Some("budget-sender".into()),
            principal_kind: AgentMessagePrincipalKind::PeerAgent,
            route: "agent_message".into(),
            origin: MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: None,
            current_task_id: None,
            current_work_item_id: None,
            inherited_routing_context: None,
            routing_activation_id: None,
        },
    )
    .unwrap()
}

async fn start(runtime: &RuntimeHandle) -> MessageEnvelope {
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("expected canonical model admission");
    };
    assert!(scheduled.scheduler_decision.model_reentry);
    runtime
        .begin_interactive_turn_with_provenance(
            Some(&scheduled.message),
            None,
            None,
            scheduled.dispatch_plan.execution_admission_provenance,
        )
        .await
        .unwrap();
    runtime
        .record_incoming_transcript_entry(&scheduled.message)
        .await
        .unwrap();
    scheduled.message
}

async fn build(
    runtime: &RuntimeHandle,
    anchor: &MessageEnvelope,
    config: &ContextConfig,
) -> EffectivePrompt {
    let state = runtime.agent_state().await.unwrap();
    let identity = runtime.agent_identity_view().await.unwrap();
    let execution = runtime.execution_snapshot().await.unwrap();
    let (provider, tools, surface, _, _) = runtime.provider_tool_selection().await.unwrap();
    let skills = runtime
        .skills_runtime_view_for_state(&state, &identity)
        .await
        .unwrap();
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: state.id.clone(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let material = crate::diagnostics::resource_io::ResourceReadMaterial::new(
        &runtime.inner.input_evidence_budget,
        &scope,
    )
    .unwrap();
    let resources = runtime
        .capture_context_resources(
            &state,
            &runtime.inner.config_snapshot.load_full(),
            &material,
        )
        .unwrap();
    build_effective_prompt_with_apply_patch_surface(
        runtime.storage(),
        &state,
        &execution,
        anchor,
        config,
        &execution.execution_root,
        runtime.agent_home().as_path(),
        &identity,
        resources.body().guidance.clone(),
        resources.body().memory.clone(),
        &skills,
        &provider.prompt_tool_specs(&tools),
        surface,
        None,
    )
    .unwrap()
}

#[tokio::test]
async fn context_admission_rejects_oversized_instructions_before_claim() {
    let (_home, _workspace, runtime) = runtime_fixture();
    let prepared = prepared_input("oversized", "instruction".repeat(6000));
    let message = runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap();
    let outcome = scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        scheduler_executor::RunLoopPoll::AuthorityBlocked
    ));
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(message.message_id.as_deref().unwrap())
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    assert!(runtime.storage().read_recent_turns(1).unwrap().is_empty());
    assert!(runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("budget-target")
        .unwrap()
        .is_none_or(|state| state.open_attempt().is_none()));
}

#[tokio::test]
async fn context_admission_exact_minimum_keeps_claimed_representation_and_defers_follower() {
    let (_home, _workspace, runtime) = runtime_fixture();
    let prepared = [
        prepared_input("anchor", "authoritative instruction".repeat(40)),
        prepared_input("follower", "follower instruction".repeat(40)),
    ];
    for input in &prepared {
        runtime
            .agent_message_delivery_service()
            .deliver(input)
            .await
            .unwrap();
    }
    let anchor = start(&runtime).await;
    let item = input_consumption::projection::admitted_item(
        runtime.storage(),
        "budget-target",
        &anchor.id,
    )
    .unwrap()
    .unwrap();
    assert!(item.rendered.contains("representation: full"));
    let current = format!("{}{}", input_consumption::BATCH_HEADER, item.rendered);
    let tokens = crate::context::estimate_text_tokens(&format!("[current_input]\n{current}"));
    let budget = input_consumption::projection::InputBudget {
        tokens,
        bytes: input_consumption::INPUT_BATCH_BYTES,
    };
    assert!(runtime
        .attach_idle_sender_inputs(&anchor, budget)
        .await
        .unwrap());
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let batch = turn.idle_input_batch.unwrap();
    assert_eq!(batch.items, vec![item.clone()]);
    assert!(budget.fits("current_input", &input_consumption::render_batch(&batch)));
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&prepared[1].message.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    assert_eq!(
        input_consumption::projection::admitted_item(
            runtime.storage(),
            "budget-target",
            &anchor.id
        )
        .unwrap(),
        Some(item)
    );
    let built = build(&runtime, &anchor, &context_config()).await;
    let available = idle_input_budget(&built);
    assert!(available.tokens > 0);
    assert!(
        built.context_plan_evidence.allocated_estimated_tokens
            <= built.context_plan_evidence.total_budget_estimated_tokens
    );
}

#[tokio::test]
async fn context_admission_does_not_reselect_claimed_input_below_exact_minimum() {
    let (_home, _workspace, runtime) = runtime_fixture();
    let prepared = prepared_input("anchor", "authoritative instruction".repeat(80));
    runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap();
    let anchor = start(&runtime).await;
    let item = input_consumption::projection::admitted_item(
        runtime.storage(),
        "budget-target",
        &anchor.id,
    )
    .unwrap()
    .unwrap();
    let tokens = crate::context::estimate_text_tokens(&format!(
        "[current_input]\n{}{}",
        input_consumption::BATCH_HEADER,
        item.rendered
    ));
    assert!(!runtime
        .attach_idle_sender_inputs(
            &anchor,
            input_consumption::projection::InputBudget {
                tokens: tokens - 1,
                bytes: input_consumption::INPUT_BATCH_BYTES,
            }
        )
        .await
        .unwrap());
    assert_eq!(
        input_consumption::projection::admitted_item(
            runtime.storage(),
            "budget-target",
            &anchor.id
        )
        .unwrap(),
        Some(item)
    );
    assert!(runtime.storage().read_recent_turns(1).unwrap()[0]
        .idle_input_batch
        .is_none());
}

#[tokio::test]
async fn context_admission_uses_claim_owner_minimum_instead_of_current_focus() {
    let (_home, _workspace, runtime) = runtime_fixture();
    let focus = runtime
        .create_work_item(
            "A".into(),
            Some(crate::types::WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let owner = runtime
        .create_work_item(
            "B".repeat(96),
            Some(crate::types::WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(focus.id.clone()).await.unwrap();
    let mut prepared = prepared_input("different-owner", "complete instruction".into());
    prepared.message.work_item_id = Some(owner.id.clone());
    prepared.message.priority = Priority::Next;
    runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap();
    let message = &prepared.message;
    let mut state = runtime.agent_state().await.unwrap();
    state.current_turn_id = None;
    state.current_execution_binding = None;
    let identity = runtime.agent_identity_view().await.unwrap();
    let skills = runtime
        .skills_runtime_view_for_state(&state, &identity)
        .await
        .unwrap();
    let execution = runtime.execution_snapshot().await.unwrap();
    let mut config = runtime.current_context_config().await;
    config.prompt_budget_estimated_tokens -= crate::prompt::time::RUNTIME_TIME_RESERVED_TOKENS;
    let available = |state: &AgentState| {
        crate::context::input_admission_tokens(
            runtime.storage(),
            state,
            &execution,
            &skills,
            message,
            None,
            &config,
            runtime.agent_home().as_path(),
            None,
        )
        .unwrap()
    };
    let focused_available = available(&state);
    let mut owner_state = state.clone();
    owner_state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
        activation_id: None,
        admission_provenance: None,
        source_message_id: message.id.clone(),
        turn_id: String::new(),
        owner: Some(crate::types::TurnOwner::WorkItem {
            work_item_id: owner.id.clone(),
        }),
        work_item_id: Some(owner.id.clone()),
        claimed_work_revision: None,
    });
    assert!(available(&owner_state) < focused_available);
    let full = input_consumption::projection::persisted_item(
        runtime.storage(),
        message,
        usize::MAX,
        usize::MAX,
    )
    .unwrap()
    .unwrap();
    let input_cost = crate::context::estimate_text_tokens(&format!(
        "[current_input]\n{}{}",
        input_consumption::BATCH_HEADER,
        full.rendered
    ));
    runtime
        .inner
        .context_config
        .write()
        .await
        .prompt_budget_estimated_tokens = config.prompt_budget_estimated_tokens - focused_available
        + input_cost
        + crate::prompt::time::RUNTIME_TIME_RESERVED_TOKENS;
    assert!(runtime
        .prepare_input_projection(message, &state, None)
        .await
        .unwrap()
        .is_some());
    assert!(runtime
        .prepare_input_projection(message, &owner_state, None)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap(),
        scheduler_executor::RunLoopPoll::AuthorityBlocked
    ));
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&message.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    assert!(runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("budget-target")
        .unwrap()
        .is_none_or(|state| state.open_attempt().is_none()));
    assert_eq!(
        runtime.agent_state().await.unwrap().current_work_item_id,
        Some(focus.id)
    );
}

#[derive(Clone)]
struct InputProjectionProvider {
    requests: Arc<Mutex<Vec<ProviderTurnRequest>>>,
}

#[async_trait]
impl AgentProvider for InputProjectionProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let material = request.material.clone();
        self.requests.lock().await.push(request);
        Ok(ProviderTurnResponse {
            material,
            blocks: vec![ModelBlock::Text {
                text: "Evidence received.".into(),
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 5,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }

    fn select_model_lineage(
        &self,
        _: &crate::config::ModelRouteRef,
    ) -> Option<Arc<dyn AgentProvider>> {
        Some(Arc::new(self.clone()))
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![crate::provider::ProviderPromptCapability::FullRequestOnly]
    }
}

#[tokio::test]
async fn context_admission_compact_survives_restart_and_reaches_provider_with_original_ref() {
    let (home, workspace, runtime) = runtime_fixture();
    let body = format!(
        "EVIDENCE_HEAD:{}:ORIGINAL_UNREAD_TAIL",
        "证据🙂".repeat(30000)
    );
    let source = MessageEnvelope::new(
        "budget-target",
        MessageKind::WebhookEvent,
        MessageOrigin::Webhook {
            source: "compact-restart-fixture".into(),
            event_type: Some("evidence".into()),
        },
        AuthorityClass::ExternalEvidence,
        Priority::Normal,
        MessageBody::Text { text: body.clone() },
    );
    let source = runtime.enqueue(source).await.unwrap();
    let anchor = start(&runtime).await;
    assert_eq!(anchor.id, source.id);
    let before = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("budget-target")
        .unwrap()
        .unwrap();
    let prior = before.open_attempt().unwrap();
    let projection = prior.input_projection.clone().unwrap();
    assert!(projection
        .rendered
        .contains("representation: bounded_preview"));
    assert!(!projection.rendered.contains("ORIGINAL_UNREAD_TAIL"));
    assert!(projection.rendered.contains("external_evidence"));
    assert!(projection.rendered.contains("compact-restart-fixture"));
    let original =
        crate::memory::get_memory_snapshot(runtime.storage(), &projection.source_ref, None)
            .unwrap()
            .expect("the selected reference must resolve through authorized memory retrieval");
    assert!(original.content.contains(&body));
    finish_claimed_test_run(&runtime).await;
    let database = runtime.runtime_db().clone();
    let ledger = runtime.inner.input_evidence_budget.clone();
    runtime
        .retire_instance_locally("fixture_daemon_restart")
        .await;
    runtime.join_retiring_resources().await.unwrap();
    drop(runtime);
    // The pre-activation actor owns Attempt/Outcome writes and their source
    // projection together; the unscoped repair leaves qualified metadata stale.
    let report = {
        let path = home.path().join(".holon/state/runtime-maintenance.lock");
        let lease = crate::runtime_db::RuntimeDbLock::try_lock(&path).unwrap();
        let handoff = tokio::sync::Mutex::new(());
        let held = handoff.try_lock().unwrap();
        let capability = crate::runtime_db::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery::for_pre_activation_host(
            &database, &lease, &path, &held,
        )
        .unwrap();
        let scopes = database
            .startup_input_recovery_scopes(&["budget-target".into()])
            .unwrap();
        database
            .recover_interrupted_runtime_state_at_startup_with_tasks_in_scopes(
                &scopes,
                &capability,
                &ledger,
            )
            .unwrap()
            .report
    };
    assert_eq!(report.interrupted_execution_attempts, 1);
    assert_eq!(report.interrupted_turns, 1);

    let provider = InputProjectionProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let runtime = RuntimeHandle::new(
        "budget-target",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(provider.clone()),
        "default".into(),
        context_config(),
    )
    .unwrap();
    runtime.recover_scheduler_bootstrap_claims().await.unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("interrupted evidence must retain its canonical admission");
    };
    assert_eq!(scheduled.message.id, source.id);
    let recovered = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("budget-target")
        .unwrap()
        .unwrap();
    let recovered = recovered.open_attempt().unwrap();
    assert_eq!(
        recovered.recovery_of_attempt_id.as_deref(),
        Some(prior.attempt_id.as_str())
    );
    assert_eq!(recovered.input_projection.as_ref(), Some(&projection));
    assert_eq!(recovered.source, prior.source);
    assert_eq!(recovered.binding, prior.binding);
    runtime
        .process_interactive_message_deferred_with_cleanup(
            &scheduled.message,
            scheduled.dispatch_plan.continuation_resolution.as_ref(),
            scheduled.dispatch_plan.execution_admission_provenance,
            turn::LoopControlOptions {
                max_tool_rounds: None,
            },
            None,
        )
        .await
        .unwrap();
    let requests = provider.requests.lock().await;
    assert_eq!(
        requests.len(),
        1,
        "the compact must pass the real request gate"
    );
    let request = &requests[0];
    let current = request
        .prompt_frame
        .context_blocks
        .iter()
        .find(|block| block.text.starts_with("## current_input\n"))
        .expect("provider request must contain the admitted input");
    assert!(current.text.contains(&projection.rendered));
    assert!(current.text.contains(&projection.source_ref));
    assert!(!format!("{request:?}").contains("ORIGINAL_UNREAD_TAIL"));
    assert_eq!(
        runtime
            .storage()
            .read_message_by_id(&source.id)
            .unwrap()
            .unwrap()
            .body,
        source.body
    );
}
