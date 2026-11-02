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
