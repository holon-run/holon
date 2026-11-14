use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{ActiveSkillRecord, SkillActivationSource, SkillActivationState},
};

#[tokio::test]
async fn get_agent_reads_unloaded_peer_with_task_result_settlement() -> Result<()> {
    let (_home, host) = canonical_test_host();
    let caller = host.default_runtime().await?;
    let agent_id = "summary-peer";
    host.create_named_agent(agent_id, None).await?;
    let target = host.get_or_create_agent(agent_id).await?;
    let pause = target.pause_run_loop_for_monitor_test().await;
    let task = target
        .schedule_command_task(
            "Produce a task result".into(),
            crate::types::CommandTaskSpec {
                cmd: "printf summary-result".into(),
                workdir: None,
                shell: Some("sh".into()),
                login: false,
                tty: false,
                yield_time_ms: 100,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: true,
            },
            AuthorityClass::RuntimeInstruction,
        )
        .await?;
    let task = wait_for_terminal_task(&target, &task.id).await;
    assert_eq!(task.status, TaskStatus::Completed);
    let message_id = task
        .parent_message_id
        .as_deref()
        .context("task result message")?;
    let settlements = host.runtime_db().task_result_settlements();
    host.drain_runtime_incarnation(agent_id, None).await?;
    drop(pause);
    assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
    let settlement = settlements
        .latest_for_message(message_id)?
        .context("task result settlement")?;
    let storage = host.agent_storage_read_only(agent_id)?;
    let queue = storage.latest_queue_entries()?;

    let call = crate::tool::ToolCall {
        id: "get-cold-peer".into(),
        name: "GetAgent".into(),
        input: json!({"agent_id": agent_id}),
    };
    let registry = crate::tool::ToolRegistry::new(caller.workspace_root());
    let (result, _) = registry
        .execute(
            &caller,
            caller.instance_agent_id(),
            &AuthorityClass::OperatorInstruction,
            &call,
        )
        .await?;
    assert_eq!(
        result.envelope.result.context("target summary")?["agent"]["identity"]["agent_id"],
        agent_id
    );
    assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
    assert_eq!(storage.latest_queue_entries()?, queue);
    assert_eq!(
        settlements.latest_for_message(message_id)?,
        Some(settlement)
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_summary_skills_uses_captured_state_without_state_sql_and_latches_allowance(
) -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let identity_view = AgentIdentityView::from_record(&identity, &host.config().default_agent_id);
    let skill = host
        .agent_data_dir(&identity.agent_id)
        .join("skills/captured");
    std::fs::create_dir_all(&skill)?;
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: captured\ndescription: captured State 汉🦀\n---\n",
    )?;
    let mut state = AgentState::new(&identity.agent_id);
    let view = host
        .operator_agent_skills_view_for_state(&identity_view, &state, None)
        .await?;
    let entry = view
        .discoverable_skills
        .iter()
        .find(|entry| entry.name == "captured")
        .context("fixture skill must be visible")?;
    state.active_skills.push(ActiveSkillRecord {
        skill_id: entry.skill_id.clone(),
        name: entry.name.clone(),
        path: entry.path.clone(),
        scope: entry.scope,
        agent_id: identity.agent_id.clone(),
        activation_source: SkillActivationSource::Explicit,
        activation_state: SkillActivationState::SessionActive,
        activated_at_turn: 7,
    });
    let expected = host
        .operator_agent_skills_view_for_state(&identity_view, &state, None)
        .await?;
    assert_eq!(expected.active_skills.len(), 1);
    host.runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE agent_states RENAME TO cold_summary_skills_state_forbidden")?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    let actual = host
        .operator_agent_skills_view_for_state(&identity_view, &state, Some(&phase))
        .await?;
    assert_eq!(
        serde_json::to_value(&actual)?,
        serde_json::to_value(&expected)?
    );
    let usage = phase.finish()?;
    assert!(usage.1 > 64);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((actual, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for shortage in [None, Some(true), Some(false)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            usage.0 - u64::from(shortage == Some(true)),
            usage.1 - usize::from(shortage == Some(false)),
        )?;
        let result = host
            .operator_agent_skills_view_for_state(&identity_view, &state, Some(&phase))
            .await;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        if shortage.is_none() {
            assert_eq!(
                serde_json::to_value(result?)?,
                serde_json::to_value(&expected)?
            );
            assert_eq!(phase.finish()?, usage);
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                format!("{error:#}"),
                "resourceexhausted: context resource read material",
            );
            let held = ledger.report()?.observed_bytes;
            let retry = host
                .operator_agent_skills_view_for_state(&identity_view, &state, Some(&phase))
                .await
                .unwrap_err();
            assert_eq!(format!("{retry:#}"), format!("{error:#}"));
            assert_eq!(ledger.report()?.observed_bytes, held);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_summary_skills_rejects_foreign_state_before_material_or_registry() -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let identity = AgentIdentityView::from_record(&identity, &host.config().default_agent_id);
    let state = AgentState::new("foreign-skills");
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    let before = phase.finish()?;
    let registry = host.inner.skills_registry.write().await;
    for material in [None, Some(&phase)] {
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            host.operator_agent_skills_view_for_state(&identity, &state, material),
        )
        .await?
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "skills State differs from captured identity"
        );
        assert_eq!(phase.finish()?, before);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    drop((registry, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}
