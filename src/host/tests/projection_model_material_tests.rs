use super::super::*;
use crate::{
    config::{ModelRouteOptions, ModelRouteRef, ServiceTier, StateMemoryLimits},
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    provider::StubProvider,
    runtime::{
        agent_model_list_summary_for_catalog, input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope, try_agent_model_list_summary_for_catalog,
    },
    types::AgentListModelSummary,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-model-material".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn header() -> Result<(u64, usize)> {
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let cost = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(cost)
}

fn route(value: &str) -> ModelRouteRef {
    ModelRouteRef::parse(value).unwrap()
}

fn config(home: &std::path::Path) -> Result<AppConfig> {
    std::fs::write(
        home.join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    AppConfig::load_with_home(Some(home.to_path_buf()))
}

fn selection(home: &std::path::Path) -> Result<(RuntimeModelCatalog, AgentState)> {
    let mut catalog = RuntimeModelCatalog::from_config(&config(home)?);
    catalog.default_model = route("openai@default/default");
    let mut state = AgentState::new("web");
    state.model_override = Some(route("openai@other/override-汉字"));
    state.last_requested_model = state.model_override.clone();
    state.last_active_model = Some(route("openai@active/fallback"));
    state.model_override_reasoning_effort = Some("high-\0汉字🦀".into());
    catalog.fallback_models = vec![
        state.model_override.clone().unwrap(),
        route("openai@one/fallback"),
        route("openai@one/fallback"),
        route("openai@two/fallback"),
    ];
    catalog.route_options.insert(
        state.last_active_model.as_ref().unwrap().as_string(),
        ModelRouteOptions {
            service_tier: Some(ServiceTier::Fast),
        },
    );
    Ok((catalog, state))
}

// Independent cost/order for the fixed fixture, not the production selector or
// admission helper. Three String headers include each fallback Vec slot.
fn steps(catalog: &RuntimeModelCatalog, state: &AgentState) -> Vec<(u64, usize)> {
    let override_model = state.model_override.as_ref().unwrap();
    let routes = [
        &catalog.default_model,
        override_model,
        override_model,
        state.last_active_model.as_ref().unwrap(),
        &catalog.fallback_models[1],
        &catalog.fallback_models[3],
        override_model,
    ];
    let mut steps: Vec<_> = routes
        .into_iter()
        .map(|route| {
            let text =
                route.provider.as_str().len() + route.endpoint.as_str().len() + route.model.len();
            ((text + 3 * std::mem::size_of::<String>()) as u64 * 8, 24)
        })
        .collect();
    steps.push((
        (state
            .model_override_reasoning_effort
            .as_ref()
            .unwrap()
            .len()
            + std::mem::size_of::<String>()) as u64
            * 8,
        8,
    ));
    steps.extend(tier_steps(state.last_active_model.as_ref().unwrap()));
    steps
}

// These fixtures have no aliases. Count the alias-query key and final map key
// independently, without calling the checked lookup or observing its charges.
fn tier_steps(route: &ModelRouteRef) -> [(u64, usize); 2] {
    [
        (
            ((route.provider.as_str().len()
                + route.model.len()
                + 2 * std::mem::size_of::<String>())
                * 8) as u64,
            16,
        ),
        (
            ((route.provider.as_str().len()
                + route.endpoint.as_str().len()
                + route.model.len()
                + 2
                + std::mem::size_of::<String>())
                * 8) as u64,
            8,
        ),
    ]
}

fn plus(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

#[test]
fn checked_summary_preserves_selection_and_wire_without_phase() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (mut catalog, original) = selection(home.path())?;
    for overridden in [false, true] {
        for disabled in [false, true] {
            for history in [0, 1, 2] {
                let mut state = original.clone();
                if !overridden {
                    state.model_override = None;
                }
                if history == 0 {
                    state.last_requested_model = None;
                } else if history == 1 {
                    state.last_requested_model = Some(route("openai@stale/stale"));
                }
                catalog.disable_provider_fallback = disabled;
                let expected = agent_model_list_summary_for_catalog(&catalog, &state);
                let actual = try_agent_model_list_summary_for_catalog(&catalog, &state)?;
                assert_eq!(actual, expected);
                assert_eq!(actual.fallback_active, overridden && history == 2);
                if disabled {
                    assert!(actual.effective_fallback_models.is_empty());
                } else {
                    let mut fallbacks = vec![
                        catalog.fallback_models[1].clone(),
                        catalog.fallback_models[3].clone(),
                    ];
                    if !overridden {
                        fallbacks.insert(0, original.model_override.clone().unwrap());
                    }
                    assert_eq!(actual.effective_fallback_models, fallbacks);
                }
                assert_eq!(
                    serde_json::to_value(actual)?,
                    serde_json::to_value(expected)?
                );
            }
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn summary_exact_final_field_cost_uses_original_phase_and_last_holder() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (catalog, state) = selection(home.path())?;
    let expected = agent_model_list_summary_for_catalog(&catalog, &state);
    assert_eq!(expected.effective_service_tier, Some(ServiceTier::Fast));
    let full = steps(&catalog, &state).into_iter().fold(header()?, plus);
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
    let holder = phase.guard()?;
    let actual = {
        let _entry = phase.enter();
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        let actual = try_agent_model_list_summary_for_catalog(&catalog, &state)?;
        assert_eq!(phase.finish()?, full);
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        assert!(charge_owned_metadata(1, 1).is_err());
        assert!(try_agent_model_list_summary_for_catalog(&catalog, &state).is_err());
        // The unchecked model API retains its pre-existing contract.
        assert_eq!(
            agent_model_list_summary_for_catalog(&catalog, &state),
            expected
        );
        assert!(phase.finish().is_err());
        actual
    };
    assert_eq!(actual, expected);
    drop((actual, phase));
    assert_eq!(ledger.report()?.observed_bytes, full.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn every_final_field_gate_rejects_bytes_and_records_and_latches() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (catalog, state) = selection(home.path())?;
    let initial = header()?;
    let steps = steps(&catalog, &state);
    let full = steps.iter().copied().fold(initial, plus);
    let mut prefix = initial;
    for step in steps {
        let next = plus(prefix, step);
        for limits in [(next.0 - 1, full.1), (full.0, next.1 - 1)] {
            let ledger = ledger()?;
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &holder,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                for _ in 0..2 {
                    let error =
                        try_agent_model_list_summary_for_catalog(&catalog, &state).unwrap_err();
                    assert!(error.to_string().contains("context resource read material"));
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        prefix = next;
    }
    assert_eq!(prefix, full);
    Ok(())
}

#[test]
fn default_disabled_summary_omits_fallback_override_and_absent_reasoning_costs() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (mut catalog, _) = selection(home.path())?;
    catalog.disable_provider_fallback = true;
    for reasoning in [None, Some(String::new())] {
        let mut state = AgentState::new("web");
        state.model_override_reasoning_effort = reasoning;
        let route = &catalog.default_model;
        let text =
            route.provider.as_str().len() + route.endpoint.as_str().len() + route.model.len();
        let route_bytes = ((text + 3 * std::mem::size_of::<String>()) * 8) as u64;
        let mut full = plus(header()?, (4 * route_bytes, 4 * 24));
        if state.model_override_reasoning_effort.is_some() {
            full = plus(full, (std::mem::size_of::<String>() as u64 * 8, 8));
        }
        full = tier_steps(route).into_iter().fold(full, plus);
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
        let summary = {
            let _entry = phase.enter();
            try_agent_model_list_summary_for_catalog(&catalog, &state)?
        };
        assert_eq!(phase.finish()?, full);
        assert_eq!(
            summary,
            agent_model_list_summary_for_catalog(&catalog, &state)
        );
        assert!(summary.effective_fallback_models.is_empty());
        assert!(summary.override_model.is_none());
        drop((summary, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn shared_host_entry_propagates_model_gate_failure_without_partial_result() -> Result<()> {
    let home = tempfile::tempdir()?;
    let host =
        RuntimeHost::new_with_provider(config(home.path())?, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let (catalog, state) = selection(home.path())?;
    let identity = host.agent_identity_record("web")?.unwrap();
    let initial = header()?;
    let first = plus(initial, steps(&catalog, &state)[0]);
    for limits in [(first.0 - 1, first.1), (first.0, first.1 - 1)] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let holder = phase.guard()?;
        let inputs = [
            (identity.clone(), state.clone()),
            (identity.clone(), state.clone()),
        ];
        {
            let _entry = phase.enter();
            for (identity, state) in inputs {
                let error = host
                    .agent_list_entry_from_committed_state(identity, Some(state), &catalog)
                    .unwrap_err();
                assert!(error.to_string().contains("context resource read material"));
                assert!(phase.finish().is_err());
                assert_eq!(ledger.report()?.observed_bytes, initial.0);
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, initial.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_model_material_keeps_holder_through_serialization_errors() -> Result<()> {
    let home = tempfile::tempdir()?;
    let host =
        RuntimeHost::new_with_provider(config(home.path())?, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let (_, state) = selection(home.path())?;
    let expected = agent_model_list_summary_for_catalog(
        &RuntimeModelCatalog::from_config(&host.config()),
        &state,
    );
    host.runtime_db().connection()?.execute(
        "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'idle', '2026-01-01T00:00:00Z', ?1)
         ON CONFLICT(agent_id) DO UPDATE SET payload_json = excluded.payload_json",
        [serde_json::to_string(&state)?],
    )?;
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (model, retained) = {
            let _entry = phase.enter();
            if roster {
                let mut snapshot = host.agent_roster_snapshot()?;
                (
                    snapshot.agents.pop().unwrap().agent.model,
                    snapshot._material,
                )
            } else {
                let snapshot = host.agent_projection_snapshot("web")?.unwrap();
                (snapshot.agent.model, snapshot._material)
            }
        };
        assert_eq!(model, expected);
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &holder));
        let cost = phase.finish()?;
        // The snapshot also carries its SQL/decode material, not just the model.
        assert!(cost.0 > header()?.0);
        drop((phase, holder));
        struct Checked<'a> {
            model: &'a AgentListModelSummary,
            ledger: &'a InputEvidenceBudgetLedger,
            cost: u64,
            fail: bool,
        }
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.cost);
                if self.fail {
                    return Err(serde::ser::Error::custom("model material fixture"));
                }
                serde::Serialize::serialize(self.model, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/model-material",
                &Checked {
                    model: &model,
                    ledger: &ledger,
                    cost: cost.0,
                    fail,
                },
            );
            assert_eq!(result.is_err(), fail);
            if let Ok(bytes) = result {
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes)?,
                    serde_json::to_value(&expected)?
                );
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop(model);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
