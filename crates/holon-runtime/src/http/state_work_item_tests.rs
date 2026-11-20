use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{TodoItem, TodoItemState},
    work_item_scheduling::{
        derive_work_item_scheduling, WorkItemSchedulingFacts, WorkItemSchedulingProjection,
    },
};
use std::borrow::Cow;

fn state_dto_model() -> crate::types::AgentModelState {
    let route = crate::config::ModelRouteRef::parse("openai@default/fixture").unwrap();
    crate::types::AgentModelState {
        source: crate::types::AgentModelSource::RuntimeDefault,
        runtime_default_model: route.clone(),
        effective_model: route,
        requested_model: None,
        active_model: None,
        fallback_active: false,
        effective_fallback_models: Vec::new(),
        override_model: None,
        override_reasoning_effort: None,
        override_service_tier: None,
        effective_service_tier: None,
        resolved_policy: crate::model_catalog::ResolvedRuntimeModelPolicy::default(),
    }
}

fn state_dto_clone_cost(agent: &crate::types::AgentState) -> Result<(u64, usize)> {
    let input = agent.clone();
    let scope = InputEvidenceScope {
        agent_id: agent.id.clone(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let dto = phase.read(|| slim_state_agent_runtime_dto(input))?;
    let cost = phase.finish()?;
    drop(dto);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(cost)
}

#[test]
fn agent_dto_clone_admission_ignores_unselected_material() -> Result<()> {
    let mut agent = crate::types::AgentState::new("state-agent-dto");
    let expected = crate::http_dto::SlimAgentRuntimeDto::from(&agent);
    let cost = state_dto_clone_cost(&agent)?;
    agent.timezone_override = Some("omitted-汉🦀".repeat(256));
    agent.total_model_rounds = 100_000;
    assert_eq!(crate::http_dto::SlimAgentRuntimeDto::from(&agent), expected,);
    assert_eq!(state_dto_clone_cost(&agent)?, cost);
    assert_eq!(slim_state_agent_runtime_dto(agent)?.0, expected);
    Ok(())
}

#[test]
fn agent_dto_clone_admission_exact_and_short_axes() -> Result<()> {
    let mut agent = crate::types::AgentState::new("state-agent-dto");
    agent.current_run_id = Some("selected-run".into());
    assert_state_dto_clone_admission(agent)?;
    assert_state_dto_clone_admission(state_dto_terminal_agent())
}

fn assert_state_dto_clone_admission(agent: crate::types::AgentState) -> Result<()> {
    let cost = state_dto_clone_cost(&agent)?;
    let scope = InputEvidenceScope {
        agent_id: agent.id.clone(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let header_phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let header = header_phase.finish()?;
    drop(header_phase);
    // Independently derive the unchanged clone envelope from the JSON tree.
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::Object(values) => {
                2 + values.len().saturating_sub(1)
                    + values.values().map(|value| 2 + slots(value)).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let residual = serde_json::to_value((
        &agent.active_workspace_entry,
        &agent.current_run_id,
        &agent.last_turn_terminal,
        &agent.last_turn_terminal,
        &agent.last_runtime_failure,
    ))?;
    let json_bytes = serde_json::to_vec(&residual)?.len() as u64;
    let slots = slots(&residual);
    assert_eq!(
        cost,
        (
            header.0 + 8 * (json_bytes * 4 + slots as u64 * 128 + 512),
            header.1 + 8 * (slots + 16),
        ),
    );
    let mut expected = crate::http_dto::SlimAgentRuntimeDto::from(&agent);
    expected.last_turn_terminal = agent
        .last_turn_terminal
        .clone()
        .map(slim_state_turn_terminal_record);
    expected.last_runtime_failure = agent
        .last_runtime_failure
        .clone()
        .map(slim_state_runtime_failure);
    for (bytes, records) in [cost, (cost.0 - 1, cost.1), (cost.0, cost.1 - 1)] {
        let input = agent.clone();
        let id_pointer = input.id.as_ptr();
        let run_pointer = input.current_run_id.as_ref().unwrap().as_ptr();
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
        let result = phase.read(|| slim_state_agent_runtime_dto(input));
        if (bytes, records) == cost {
            let (runtime, session) = result?;
            assert_eq!(runtime, expected);
            assert_eq!(runtime.id.as_ptr(), id_pointer);
            assert_eq!(
                runtime.current_run_id.as_ref().unwrap().as_ptr(),
                run_pointer
            );
            assert_eq!(session.current_run_id, agent.current_run_id);
            assert_eq!(phase.finish()?, cost);
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            assert!(phase.read(|| Ok(())).is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn agent_dto_runtime_moves_selected_cooldown_without_new_material() -> Result<()> {
    let mut agent = crate::types::AgentState::new("state-agent-dto");
    let before = state_dto_clone_cost(&agent)?;
    agent.provider_cooldown_wait = Some(serde_json::from_value(serde_json::json!({
        "turn_index": 3,
        "turn_id": "selected-turn",
        "attempt_id": "selected-attempt",
        "started_at": "2026-11-05T00:00:00Z",
        "not_before": "2026-11-05T00:01:00Z"
    }))?);
    agent.attached_workspaces = vec!["selected-workspace".into(); 8];
    let cooldown = state_dto_clone_cost(&agent)?;
    assert_eq!(cooldown, before);
    let expected = crate::http_dto::SlimAgentRuntimeDto::from(&agent);
    let cooldown_pointer = agent
        .provider_cooldown_wait
        .as_ref()
        .unwrap()
        .turn_id
        .as_ref()
        .unwrap()
        .as_ptr();
    let workspaces_pointer = agent.attached_workspaces.as_ptr();
    let dto = slim_state_agent_runtime_dto(agent)?.0;
    assert_eq!(dto, expected);
    assert_eq!(dto.attached_workspaces.as_ptr(), workspaces_pointer);
    assert_eq!(
        dto.provider_cooldown_wait
            .as_ref()
            .unwrap()
            .turn_id
            .as_ref()
            .unwrap()
            .as_ptr(),
        cooldown_pointer,
    );
    Ok(())
}

#[test]
fn agent_dto_model_moves_selected_fields_and_drops_policy() -> Result<()> {
    let mut model = state_dto_model();
    model.effective_fallback_models = vec![model.effective_model.clone(); 8];
    model.requested_model = Some(model.effective_model.clone());
    model.active_model = Some(model.effective_model.clone());
    model.override_model = Some(model.effective_model.clone());
    model.override_reasoning_effort = Some("selected-effort".into());
    model.resolved_policy.description = "omitted policy".repeat(256);
    model.resolved_policy.reasoning_effort_options = vec!["omitted".into(); 64];
    let expected = crate::http_dto::SlimAgentModelDto::from(&model);
    let model_pointer = model.effective_model.model.as_ptr();
    let fallbacks_pointer = model.effective_fallback_models.as_ptr();
    let scope = InputEvidenceScope {
        agent_id: "state-model-dto".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let header = phase.finish()?;
    let dto = phase.read(|| Ok(crate::http_dto::SlimAgentModelDto::from(model)))?;
    assert_eq!(phase.finish()?, header);
    assert_eq!(dto, expected);
    assert_eq!(dto.effective_model.model.as_ptr(), model_pointer);
    assert_eq!(dto.effective_fallback_models.as_ptr(), fallbacks_pointer);
    drop(dto);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn state_dto_terminal_agent() -> crate::types::AgentState {
    let mut agent = crate::types::AgentState::new("state-agent-terminal");
    agent.current_run_id = Some("run-1".into());
    agent.pending = 3;
    agent.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
        workspace_id: "workspace-汉🦀".into(),
        workspace_anchor: "/workspace/汉🦀".into(),
        execution_root_id: "root-1".into(),
        execution_root: "/workspace/汉🦀".into(),
        projection_kind: crate::system::WorkspaceProjectionKind::CanonicalRoot,
        access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
        cwd: "/workspace/汉🦀/src".into(),
        occupancy_id: None,
        projection_metadata: None,
    });
    agent.last_turn_terminal = Some(crate::types::TurnTerminalRecord {
        turn_id: "turn-1".into(),
        turn_index: 4,
        kind: crate::types::TurnTerminalKind::Completed,
        reason: Some("reason".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT)),
        last_assistant_message: Some("汉🦀".repeat(STATE_BOOTSTRAP_LAST_TURN_TEXT_LIMIT)),
        no_brief_reason: None,
        checkpoint: None,
        completed_at: chrono::Utc::now(),
        duration_ms: 8,
    });
    agent.last_runtime_failure = Some(crate::types::RuntimeFailureSummary {
        occurred_at: chrono::Utc::now(),
        phase: crate::types::RuntimeFailurePhase::RuntimeTurn,
        summary: "failure".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT),
        detail_hint: Some("hint".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT)),
        failure_artifact: None,
    });
    agent
}

#[test]
fn agent_dto_runtime_preserves_slimmed_terminal_failure_and_session() -> Result<()> {
    let agent = state_dto_terminal_agent();
    let mut expected = crate::http_dto::SlimAgentRuntimeDto::from(&agent);
    expected.last_turn_terminal = agent
        .last_turn_terminal
        .clone()
        .map(slim_state_turn_terminal_record);
    expected.last_runtime_failure = agent
        .last_runtime_failure
        .clone()
        .map(slim_state_runtime_failure);
    let cost = state_dto_clone_cost(&agent)?;
    let scope = InputEvidenceScope {
        agent_id: agent.id.clone(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, cost.0, cost.1)?;
    let (dto, session) = phase.read(|| slim_state_agent_runtime_dto(agent))?;
    assert_eq!(dto, expected);
    assert_eq!(session.last_turn, dto.last_turn_terminal);
    assert_eq!(session.current_run_id, dto.current_run_id);
    assert_eq!(session.pending_count, dto.pending);
    assert_eq!(phase.finish()?, cost);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn projection(item: &WorkItemRecord) -> WorkItemSchedulingProjection {
    derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: item,
        is_current: false,
        is_yielded: false,
        active_wait_conditions: &[],
        trigger_delivery_by_id: &std::collections::BTreeMap::new(),
    })
}

#[test]
fn active_state_window_excludes_history_and_prioritizes_current() -> Result<()> {
    let mut items = (0..60)
        .map(|_| {
            projection(&WorkItemRecord::new(
                "state-window",
                "history",
                WorkItemState::Completed,
            ))
        })
        .collect::<Vec<_>>();
    let completing = WorkItemRecord::new("state-window", "completing", WorkItemState::Completing);
    let completing_id = completing.id.clone();
    items.push(projection(&completing));
    items.extend((0..60).map(|_| {
        projection(&WorkItemRecord::new(
            "state-window",
            "queued",
            WorkItemState::Open,
        ))
    }));
    let current = WorkItemRecord::new("state-window", "current", WorkItemState::Open);
    let current_id = current.id.clone();
    let mut current = projection(&current);
    current.is_current = true;
    items.push(current);
    let result = slim_state_work_items(&items)?;
    assert_eq!(result.len(), STATE_BOOTSTRAP_WORK_ITEM_LIMIT);
    assert_eq!(result[0].id, current_id);
    assert!(result.iter().any(|item| item.id == completing_id));
    assert!(result
        .iter()
        .all(|item| item.state != WorkItemState::Completed));
    Ok(())
}

#[test]
fn borrows_short_fields_and_preserves_unicode_preview() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "state-dto".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let mut item = WorkItemRecord::new(&scope.agent_id, "短文汉🦀", WorkItemState::Open);
    item.todo_list = vec![TodoItem {
        text: "next 汉🦀".into(),
        state: TodoItemState::InProgress,
    }];
    let projection = projection(&item);
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let header = phase.finish()?;
    let dto = phase.read(|| slim_state_work_item_dto(&projection))?;
    assert_eq!(phase.finish()?, header);
    assert!(matches!(dto.id, Cow::Borrowed(_)));
    assert!(matches!(dto.objective, Cow::Borrowed(_)));
    assert_eq!(dto.id.as_ptr(), projection.record().id.as_ptr());
    assert!(std::ptr::eq(
        dto.current_todo.unwrap(),
        projection.current_todo.as_ref().unwrap(),
    ));
    assert_eq!(
        serde_json::to_value(&dto)?,
        serde_json::to_value(crate::http_dto::SlimWorkItemDto::from(&projection))?
    );
    drop(dto);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    let unicode = "汉🦀é".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    for limit in [0, 1, 2, 3, 4, 5, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT] {
        let expected = if limit <= 3 {
            unicode.chars().take(limit).collect::<String>()
        } else {
            format!("{}...", unicode.chars().take(limit - 3).collect::<String>())
        };
        assert_eq!(truncate_state_bootstrap_string(&unicode, limit), expected);
    }
    Ok(())
}

#[test]
fn owned_preview_and_slots_reject_each_short_axis() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "state-dto-budget".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let item = WorkItemRecord::new(
        &scope.agent_id,
        "汉🦀".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT),
        WorkItemState::Open,
    );
    let items = [projection(&item)];
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let dto = phase.read(|| slim_state_work_items(&items))?;
    let cost = phase.finish()?;
    assert!(matches!(dto[0].objective, Cow::Owned(_)));
    drop(dto);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for (bytes, records) in [cost, (cost.0 - 1, cost.1), (cost.0, cost.1 - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
        let result = phase.read(|| slim_state_work_items(&items));
        if (bytes, records) == cost {
            assert_eq!(result?.len(), 1);
            assert_eq!(phase.finish()?, cost);
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            assert!(phase.read(|| Ok(())).is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn known_display_window_reserves_only_selected_slots() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "state-dto-window".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let items = (0..200)
        .map(|index| {
            projection(&WorkItemRecord::new(
                &scope.agent_id,
                format!("window item {index}"),
                WorkItemState::Open,
            ))
        })
        .collect::<Vec<_>>();
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let header = phase.finish()?;
    drop(phase);
    let count = STATE_BOOTSTRAP_WORK_ITEM_LIMIT;
    let cost = (
        header.0 + u64::try_from(count * std::mem::size_of::<StateWorkItemDto<'_>>())? * 8,
        header.1 + count * 8,
    );
    let expected = serde_json::to_value(
        items
            .iter()
            .take(count)
            .map(crate::http_dto::SlimWorkItemDto::from)
            .collect::<Vec<_>>(),
    )?;
    for (bytes, records) in [cost, (cost.0 - 1, cost.1), (cost.0, cost.1 - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
        let result = phase.read(|| slim_state_work_items(&items));
        if (bytes, records) == cost {
            let dto = result?;
            assert_eq!(dto.len(), count);
            assert_eq!(dto.capacity(), count);
            assert_eq!(serde_json::to_value(&dto)?, expected);
            assert_eq!(phase.finish()?, cost);
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            assert!(phase.read(|| Ok(())).is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
