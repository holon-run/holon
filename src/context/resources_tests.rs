use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        mutation_owner::MutationOwner,
        resident_input_evidence::InputEvidenceScope,
        resource_observation::{
            ContextResourceBody, ContextResourceRegistry, ContextResourceSource,
        },
    },
    types::WorkItemState,
};

#[test]
fn loaded_resource_views_keep_selected_plans_notes_and_original_guard() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "context-resource-views".into(),
        identity_incarnation: 1,
    };
    let source = ContextResourceSource {
        scope: scope.clone(),
        agent_home: "/agent".into(),
        user_home: None,
        workspace_id: None,
        workspace_anchor: None,
        execution_root_id: None,
        execution_root: None,
        include_user_skills: false,
        config_generation: 1,
        host_generation: 0,
        active_skills_selector: [0; 32],
        work_revision: 0,
        plans: Vec::new(),
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let registry = ContextResourceRegistry::new(ledger.clone());
    let work = WorkItemRecord::new(&scope.agent_id, "selected", WorkItemState::Open);
    let mut missing = work.clone();
    missing.id = "missing".into();
    let mut not_recorded = work.clone();
    not_recorded.id = "not-recorded".into();
    let mut absent = work.clone();
    absent.id = "absent".into();
    let mut unreadable = work.clone();
    unreadable.id = "unreadable".into();
    let observation = registry.refresh(
        &source,
        || {
            let artifact = WorkItemPlanArtifact {
                owner_agent_id: scope.agent_id.clone(),
                workspace_id: "agent-home".into(),
                workspace_alias: None,
                relative_path: "plan.md".into(),
                path: "/agent/plan.md".into(),
                hash: "original".into(),
                bytes: 128 * 1024,
                updated_at: chrono::Utc::now(),
                preview: "plan".repeat(32 * 1024),
                preview_complete: true,
            };
            let body = ContextResourceBody {
                notes: Some("notes".repeat(32 * 1024)),
                plans: BTreeMap::from([
                    (work.id.clone(), PlanArtifactQuery::Available(artifact)),
                    (missing.id.clone(), PlanArtifactQuery::Missing),
                    (not_recorded.id.clone(), PlanArtifactQuery::NotRecorded),
                    (
                        unreadable.id.clone(),
                        PlanArtifactQuery::Unreadable("denied".into()),
                    ),
                ]),
                ..Default::default()
            };
            resource_io::admit_cloned_value(&(&body.notes, &body.plans))?;
            Ok(body)
        },
        || Ok(MutationOwner::standalone(&scope.agent_id, 1).try_reserve()?),
        |_, _| Ok(()),
    )?;
    let input = ContextResourceInput::Loaded(observation.clone());
    let mut views = ContextResourceViews::new(&input);
    let selector_bytes = work.id.len() + std::mem::size_of::<String>() + 128;
    let digest_bytes = std::mem::size_of::<String>() + 64;
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        ((scope.agent_id.len() + 256 + selector_bytes + digest_bytes) * 8) as u64,
        128,
    )?;
    let entry = phase.enter();
    let before = phase.finish()?;
    let plan = input.plan(&work)?.unwrap();
    let original = match observation.body().plans.get(&work.id).unwrap() {
        PlanArtifactQuery::Available(artifact) => artifact,
        _ => unreachable!(),
    };
    assert!(std::ptr::eq(plan.as_ref(), original));
    assert!(views.plan(&work.id).is_none());
    views.insert_plan(&work.id, plan)?;
    views.set_notes(input.notes());
    let after = phase.finish()?;
    assert_eq!(
        after.0 - before.0,
        (work.id.len() + std::mem::size_of::<String>() + 128) as u64 * 8
    );
    assert_eq!(after.1 - before.1, 8);
    assert!(std::ptr::eq(views.plan(&work.id).unwrap(), original));
    assert!(std::ptr::eq(
        views.notes().unwrap(),
        observation.body().notes.as_ref().unwrap()
    ));
    assert_eq!(
        views.digest()?,
        format!("{:x}", Sha256::digest(observation.digest()))
    );
    let after_digest = phase.finish()?;
    assert_eq!(
        after_digest,
        (
            after.0 + (std::mem::size_of::<String>() + 64) as u64 * 8,
            after.1 + 8
        )
    );
    assert!(input.plan(&missing)?.is_none());
    assert!(input.plan(&not_recorded)?.is_none());
    assert!(input
        .plan(&absent)
        .unwrap_err()
        .to_string()
        .contains("was not prepared"));
    assert!(input
        .plan(&unreadable)
        .unwrap_err()
        .to_string()
        .contains("plan observation failed: denied"));
    assert_eq!(phase.finish()?, after_digest);
    // An actual owning copy still has to admit its private body.
    assert!(resource_io::admit_cloned_value(&(
        &observation.body().notes,
        &observation.body().plans
    ))
    .is_err());
    assert!(input.plan(&work).is_err());
    assert!(views.digest().is_err());
    drop((entry, phase));
    registry.close()?;
    drop((registry, input, observation));
    assert!(ledger.report()?.observed_bytes > 0);
    assert_eq!(views.notes().unwrap().len(), 5 * 32 * 1024);
    assert_eq!(views.plan(&work.id).unwrap().preview.len(), 128 * 1024);
    drop(views);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
