use super::super::*;
use crate::{
    config::{ModelRef, ModelRouteOptions, ModelRouteRef, ServiceTier, StateMemoryLimits},
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    provider::StubProvider,
    runtime::{
        agent_model_list_summary_for_catalog, input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope, try_agent_model_list_summary_for_catalog,
    },
    types::AgentListModelSummary,
};

const ALIAS: &str = "devstral-medium-latest";
const CANONICAL: &str = "mistral-medium-latest";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-model-tier-material".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn route(value: &str) -> ModelRouteRef {
    ModelRouteRef::parse(value).unwrap()
}

fn catalog() -> RuntimeModelCatalog {
    let mut catalog = RuntimeModelCatalog::default();
    catalog.default_model = route("mistral@remote/devstral-medium-latest");
    catalog.disable_provider_fallback = true;
    catalog.fallback_models.clear();
    catalog.route_options.clear();
    catalog.route_options.insert(
        "mistral@remote/mistral-medium-latest".into(),
        ModelRouteOptions {
            service_tier: Some(ServiceTier::Fast),
        },
    );
    catalog
}

fn header() -> Result<(u64, usize)> {
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let cost = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(cost)
}

fn plus(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

// Independent two-step oracle. Canonical model text is a fixture expectation,
// not obtained by calling production canonicalization or reading paid deltas.
fn lookup_steps(route: &ModelRouteRef, canonical: &str) -> [(u64, usize); 2] {
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
                + canonical.len()
                + 2
                + std::mem::size_of::<String>())
                * 8) as u64,
            8,
        ),
    ]
}

fn fields(
    catalog: &RuntimeModelCatalog,
    state: &AgentState,
    active: &ModelRouteRef,
) -> (u64, usize) {
    assert!(catalog.disable_provider_fallback);
    assert!(state.model_override_reasoning_effort.is_none());
    let effective = state
        .model_override
        .as_ref()
        .unwrap_or(&catalog.default_model);
    [&catalog.default_model, effective, effective, active]
        .into_iter()
        .chain(state.model_override.as_ref())
        .fold((0, 0), |cost, route| {
            plus(
                cost,
                (
                    ((route.provider.as_str().len()
                        + route.endpoint.as_str().len()
                        + route.model.len()
                        + 3 * std::mem::size_of::<String>())
                        * 8) as u64,
                    24,
                ),
            )
        })
}

#[test]
fn checked_lookup_preserves_alias_endpoint_provider_and_missing_semantics() -> Result<()> {
    let mut catalog = catalog();
    for (key, tier) in [
        (
            "mistral@remote/devstral-medium-latest",
            Some(ServiceTier::Default),
        ),
        ("mistral@none/mistral-medium-latest", None),
        ("openai@remote/unknown/汉字\0🦀", Some(ServiceTier::Default)),
        (
            "dashscope-token-plan@remote/qwen3.7-max",
            Some(ServiceTier::Fast),
        ),
        ("dashscope@remote/qwen3.7-max", Some(ServiceTier::Default)),
    ] {
        catalog
            .route_options
            .insert(key.into(), ModelRouteOptions { service_tier: tier });
    }
    for (input, expected) in [
        (
            "mistral@remote/devstral-medium-latest",
            Some(ServiceTier::Fast),
        ),
        (
            "mistral@remote/mistral-medium-latest",
            Some(ServiceTier::Fast),
        ),
        ("mistral@default/devstral-medium-latest", None),
        ("mistral@none/devstral-medium-latest", None),
        ("openai@remote/unknown/汉字\0🦀", Some(ServiceTier::Default)),
        ("openai@remote/unknown", None),
        (
            "dashscope-token-plan@remote/qwen-3.7",
            Some(ServiceTier::Fast),
        ),
    ] {
        let route = route(input);
        assert_eq!(catalog.route_service_tier(&route), expected, "{input}");
        assert_eq!(catalog.try_route_service_tier(&route)?, expected, "{input}");
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn canonical_lookup_borrows_alias_target_or_original_input() {
    let catalog = catalog();
    for (model, expected, aliases) in [
        (ALIAS, CANONICAL, true),
        ("unknown-汉字", "unknown-汉字", false),
    ] {
        let input = ModelRef::new(catalog.default_model.provider.clone(), model);
        let borrowed = catalog.built_in_catalog.canonical_model_ref(&input);
        assert_eq!(borrowed.model, expected);
        assert_eq!(std::ptr::eq(borrowed, &input), !aliases);
        assert!(std::ptr::eq(
            borrowed,
            catalog.built_in_catalog.canonical_model_ref(&input),
        ));
        assert_eq!(
            catalog.built_in_catalog.canonicalize_model_ref(&input),
            *borrowed
        );
    }
}

#[test]
fn lookup_exact_cost_and_each_gate_rejects_and_latches_on_original_phase() -> Result<()> {
    let catalog = catalog();
    for (input, canonical, expected) in [
        (
            "mistral@remote/devstral-medium-latest",
            CANONICAL,
            Some(ServiceTier::Fast),
        ),
        ("openai@other/unknown/汉字\0🦀", "unknown/汉字\0🦀", None),
    ] {
        let route = route(input);
        let initial = header()?;
        let steps = lookup_steps(&route, canonical);
        let full = steps.into_iter().fold(initial, plus);
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            assert_eq!(catalog.try_route_service_tier(&route)?, expected);
            assert_eq!(phase.finish()?, full);
            // Ordinary lookup neither charges nor fails under the same phase.
            assert_eq!(catalog.route_service_tier(&route), expected);
            assert_eq!(phase.finish()?, full);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, full.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);

        let mut prefix = initial;
        for step in steps {
            let next = plus(prefix, step);
            for limits in [(next.0 - 1, full.1), (full.0, next.1 - 1)] {
                let ledger = self::ledger()?;
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
                        assert!(catalog
                            .try_route_service_tier(&route)
                            .unwrap_err()
                            .to_string()
                            .contains("context resource read material"));
                        assert!(phase.finish().is_err());
                        assert!(ResourceReadMaterial::active_guard().is_err());
                        assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                        assert_eq!(catalog.route_service_tier(&route), expected);
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
    }
    Ok(())
}

#[test]
fn checked_summary_tier_short_circuit_and_fallback_have_exact_costs() -> Result<()> {
    let mut catalog = catalog();
    let active = route("openai@active/fallback-汉字");
    catalog.route_options.insert(
        active.as_string(),
        ModelRouteOptions {
            service_tier: Some(ServiceTier::Default),
        },
    );
    for history in [0, 1, 2] {
        for override_tier in [None, Some(ServiceTier::Fast)] {
            let mut state = AgentState::new("web");
            state.model_override_service_tier = override_tier;
            if history != 0 {
                state.last_requested_model = Some(if history == 1 {
                    route("openai@stale/history")
                } else {
                    catalog.default_model.clone()
                });
                state.last_active_model = Some(active.clone());
            }
            let selected = if history == 2 {
                &active
            } else {
                &catalog.default_model
            };
            let mut full = plus(header()?, fields(&catalog, &state, selected));
            if history == 2 || override_tier.is_none() {
                let canonical = if history == 2 {
                    "fallback-汉字"
                } else {
                    CANONICAL
                };
                full = lookup_steps(selected, canonical)
                    .into_iter()
                    .fold(full, plus);
            }
            let expected = agent_model_list_summary_for_catalog(&catalog, &state);
            assert_eq!(
                expected.effective_service_tier,
                Some(if history == 2 {
                    ServiceTier::Default
                } else {
                    ServiceTier::Fast
                })
            );
            let ledger = ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
            let actual = {
                let _entry = phase.enter();
                let actual = try_agent_model_list_summary_for_catalog(&catalog, &state)?;
                assert_eq!(phase.finish()?, full);
                assert!(charge_owned_metadata(1, 1).is_err());
                assert!(try_agent_model_list_summary_for_catalog(&catalog, &state).is_err());
                actual
            };
            assert_eq!(actual, expected);
            assert_eq!(
                serde_json::to_value(&actual)?,
                serde_json::to_value(&expected)?
            );
            drop((actual, phase));
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

fn config(home: &std::path::Path) -> Result<AppConfig> {
    std::fs::write(
        home.join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    AppConfig::load_with_home(Some(home.to_path_buf()))
}

#[tokio::test]
async fn host_entry_propagates_each_tier_gate_without_partial_result() -> Result<()> {
    let home = tempfile::tempdir()?;
    let host =
        RuntimeHost::new_with_provider(config(home.path())?, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let catalog = catalog();
    let state = AgentState::new("web");
    let identity = host.agent_identity_record("web")?.unwrap();
    let initial = plus(header()?, fields(&catalog, &state, &catalog.default_model));
    let steps = lookup_steps(&catalog.default_model, CANONICAL);
    let full = steps.into_iter().fold(initial, plus);
    let mut prefix = initial;
    for step in steps {
        let next = plus(prefix, step);
        for limits in [(next.0 - 1, full.1), (full.0, next.1 - 1)] {
            let ledger = ledger()?;
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
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
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
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

#[tokio::test]
async fn alias_tier_snapshots_retain_original_holder_through_http_serialization() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut config = config(home.path())?;
    config.stored_config.model.route_options = catalog().route_options;
    let mut state = AgentState::new("web");
    state.model_override = Some(route("mistral@remote/devstral-medium-latest"));
    let expected =
        agent_model_list_summary_for_catalog(&RuntimeModelCatalog::from_config(&config), &state);
    assert_eq!(expected.effective_service_tier, Some(ServiceTier::Fast));
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
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
        drop((phase, holder));
        struct Checked<'a> {
            model: &'a AgentListModelSummary,
            ledger: &'a InputEvidenceBudgetLedger,
            bytes: u64,
            fail: bool,
        }
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.bytes);
                if self.fail {
                    return Err(serde::ser::Error::custom("tier material fixture"));
                }
                serde::Serialize::serialize(self.model, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/tier-material",
                &Checked {
                    model: &model,
                    ledger: &ledger,
                    bytes: cost.0,
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
