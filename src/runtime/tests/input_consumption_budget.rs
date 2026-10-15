use super::super::agent_message_delivery::PreparedAgentMessageDelivery;
use super::super::operator_dispatch::idle_input_available_bytes;
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
    let (provider, tools, surface, _, _) =
        runtime.provider_tool_selection(&identity).await.unwrap();
    let skills = runtime
        .skills_runtime_view_for_state(&state, &identity)
        .await
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
        runtime.loaded_agents_md_for_state(&state).unwrap(),
        runtime.loaded_agent_memory_for_state().unwrap(),
        &skills,
        &provider.prompt_tool_specs(&tools),
        surface,
        None,
    )
    .unwrap()
}

async fn build_with_input_capacity(
    runtime: &RuntimeHandle,
    anchor: &MessageEnvelope,
    wanted: usize,
) -> (ContextConfig, EffectivePrompt) {
    let mut config = context_config();
    // Calibrate against the real planner, rather than a synthetic section cost.
    for _ in 0..12 {
        let built = build(runtime, anchor, &config).await;
        let available = idle_input_available_bytes(&built);
        if available >= wanted && available - wanted < 4 {
            return (config, built);
        }
        if available > wanted {
            config.prompt_budget_estimated_tokens -= (available - wanted).div_ceil(4);
        } else {
            config.prompt_budget_estimated_tokens += (wanted - available).div_ceil(4);
        }
        config.turn_projection_min_budget = config.prompt_budget_estimated_tokens;
    }
    panic!("context planner did not converge on the input capacity");
}

#[tokio::test]
async fn p1_context_budget_bounds_admitted_anchor_after_runtime_time_reserve() {
    let (_home, _workspace, runtime) = runtime_fixture();
    let prepared = prepared_input(
        "anchor",
        "a".repeat(input_consumption::INPUT_BATCH_BYTES * 2),
    );
    runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap();
    let anchor = start(&runtime).await;
    let (config, built) = build_with_input_capacity(&runtime, &anchor, 3072).await;
    let available = idle_input_available_bytes(&built);
    assert_eq!(
        built.context_plan_evidence.total_budget_estimated_tokens
            + crate::prompt::time::RUNTIME_TIME_RESERVED_TOKENS,
        config.prompt_budget_estimated_tokens
    );
    assert!(runtime
        .attach_idle_sender_inputs(&anchor, available)
        .await
        .unwrap());
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let batch = turn.idle_input_batch.unwrap();
    assert_eq!(batch.max_bytes, available);
    assert_eq!(batch.rendered_bytes, available);
    assert_eq!(batch.items.len(), 1);
    assert!(batch.items[0].rendered.contains("[bounded preview;"));
    let rebuilt = build(&runtime, &anchor, &config).await;
    assert!(
        rebuilt.context_plan_evidence.allocated_estimated_tokens
            <= rebuilt.context_plan_evidence.total_budget_estimated_tokens
    );
    assert!(rebuilt
        .context_sections
        .iter()
        .find(|section| section.name == "current_input")
        .unwrap()
        .content
        .contains(&input_consumption::render_batch(&batch)));
}

#[tokio::test]
async fn p1_context_budget_defers_whole_follower_inside_runtime_time_reserve() {
    let prepared = [
        prepared_input(
            "anchor",
            "a".repeat(input_consumption::INPUT_BATCH_BYTES * 2),
        ),
        prepared_input("follower", "normal-follower:".repeat(80)),
    ];
    // Measure full evidence through admission, using the identical envelopes.
    let (_probe_home, _probe_workspace, probe) = runtime_fixture();
    for input in &prepared {
        probe
            .agent_message_delivery_service()
            .deliver(input)
            .await
            .unwrap();
    }
    let probe_anchor = start(&probe).await;
    assert!(probe
        .attach_idle_sender_inputs(&probe_anchor, input_consumption::INPUT_BATCH_BYTES)
        .await
        .unwrap());
    let probe_turn = probe.storage().read_recent_turns(1).unwrap().remove(0);
    let full_batch = probe_turn.idle_input_batch.unwrap();
    assert_eq!(full_batch.items.len(), 2);
    assert!(!full_batch.items[1].rendered.contains("[bounded preview;"));

    let (_home, _workspace, runtime) = runtime_fixture();
    for input in &prepared {
        runtime
            .agent_message_delivery_service()
            .deliver(input)
            .await
            .unwrap();
    }
    let anchor = start(&runtime).await;
    let wanted = full_batch.rendered_bytes - 256;
    let (config, built) = build_with_input_capacity(&runtime, &anchor, wanted).await;
    let available = idle_input_available_bytes(&built);
    assert!(full_batch.rendered_bytes > available);
    assert!(
        full_batch.rendered_bytes
            <= available + crate::prompt::time::RUNTIME_TIME_RESERVED_TOKENS * 4
    );
    assert!(
        full_batch.items[1].rendered.len() <= available - input_consumption::BATCH_HEADER.len()
    );
    assert!(runtime
        .attach_idle_sender_inputs(&anchor, available)
        .await
        .unwrap());
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [anchor.id.clone()]);
    let batch = turn.idle_input_batch.unwrap();
    assert_eq!(batch.items, full_batch.items[..1]);
    assert!(batch.rendered_bytes <= available);
    let follower_id = &prepared[1].message.id;
    assert_eq!(
        runtime.inner.agent.lock().await.queue.peek().unwrap().id,
        *follower_id
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(follower_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    let rebuilt = build(&runtime, &anchor, &config).await;
    assert!(
        rebuilt.context_plan_evidence.allocated_estimated_tokens
            <= rebuilt.context_plan_evidence.total_budget_estimated_tokens
    );
    assert!(!rebuilt
        .context_sections
        .iter()
        .find(|section| section.name == "current_input")
        .unwrap()
        .content
        .contains("normal-follower:"));
}
