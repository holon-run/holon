use crate::{
    config::{AppConfig, StateMemoryLimits},
    diagnostics::resource_io::ResourceReadMaterial,
    host::RuntimeHost,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::AgentState,
};
use anyhow::Result;
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "roster-material".into(),
        identity_incarnation: 1,
    }
}

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let mut state = serde_json::to_value(AgentState::new("web"))?;
    // Unknown nested fields remain ignored by the typed reader, but still
    // require the complete JSON decode envelope. Quoted punctuation is not
    // structural, including escaped quotes/backslashes and multibyte text.
    state["roster_fixture"] = serde_json::json!({
        "nested": [null, {"quoted": "汉字\0\"\\\n{}[]:,"}], "empty": []
    });
    set_state(&host, Some(&serde_json::to_string(&state)?))?;
    Ok((home, host))
}

fn set_state(host: &RuntimeHost, payload: Option<&str>) -> Result<()> {
    let db = host.runtime_db();
    let connection = db.connection()?;
    connection.execute("DELETE FROM agent_states WHERE agent_id = 'web'", [])?;
    if let Some(payload) = payload {
        connection.execute(
            "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
             VALUES ('web', 'idle', '2026-01-01T00:00:00Z', ?1)",
            [payload],
        )?;
    }
    Ok(())
}

fn add(left: (u64, usize), right: (u64, usize)) -> (u64, usize) {
    (left.0 + right.0, left.1 + right.1)
}

fn entry_slots(count: usize) -> (u64, usize) {
    (
        (count * std::mem::size_of::<crate::host::AgentRosterEntryData>()) as u64 * 8,
        count * 8,
    )
}

// Independent final-field oracle; do not call the selector or admission helper.
fn model_cost(summary: &crate::types::AgentListModelSummary) -> (u64, usize) {
    let routes = [&summary.runtime_default_model, &summary.effective_model]
        .into_iter()
        .chain(summary.requested_model.as_ref())
        .chain(summary.active_model.as_ref())
        .chain(summary.effective_fallback_models.iter())
        .chain(summary.override_model.as_ref())
        .fold((0, 0), |cost, route| {
            let text =
                route.provider.as_str().len() + route.endpoint.as_str().len() + route.model.len();
            add(
                cost,
                ((text + 3 * std::mem::size_of::<String>()) as u64 * 8, 24),
            )
        });
    let fields = summary
        .override_reasoning_effort
        .as_ref()
        .map_or(routes, |reasoning| {
            add(
                routes,
                (
                    (reasoning.len() + std::mem::size_of::<String>()) as u64 * 8,
                    8,
                ),
            )
        });
    // This fixture selects the non-aliased openai@default/gpt-5.4 route.
    assert_eq!(
        summary.effective_model.as_string(),
        "openai@default/gpt-5.4"
    );
    assert!(!summary.fallback_active);
    assert!(summary.override_service_tier.is_none());
    add(
        fields,
        (
            ((2 * "openai".len()
                + "default".len()
                + 2 * "gpt-5.4".len()
                + 2
                + 3 * std::mem::size_of::<String>())
                * 8) as u64,
            24,
        ),
    )
}

// Independent cost oracle: count only punctuation outside JSON strings.
fn decode_cost(payload: &str) -> (u64, usize) {
    let (mut quoted, mut escaped, mut slots) = (false, false, 0);
    for byte in payload.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            slots += usize::from(matches!(
                byte,
                b'"' | b'{' | b'}' | b'[' | b']' | b':' | b','
            ));
            quoted = byte == b'"';
        }
    }
    (
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

/// Raw RuntimeDb roster material cost plus the fixture member's payloads.
/// The fixture keeps exactly one public member so per-member gates are
/// deterministic, matching the projection decode harness.
fn raw_cost(host: &RuntimeHost) -> Result<((u64, usize), String, Option<String>)> {
    let payloads = host.runtime_db().agent_roster_snapshot_rows()?;
    assert_eq!(
        payloads.rows.len(),
        1,
        "fixture must keep one roster member"
    );
    let row = &payloads.rows[0];
    assert_eq!(row.agent_id, "web");
    let (identity, state) = (row.identity_json.clone(), row.agent_state_json.clone());
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let rows = {
        let _entry = phase.enter();
        host.runtime_db().agent_roster_snapshot_rows()?
    };
    let cost = phase.finish()?;
    drop((rows, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok((cost, identity, state))
}

fn rejects_host_gate(host: &RuntimeHost, index: usize) -> Result<()> {
    let (raw, identity, state) = raw_cost(host)?;
    let gates = [
        entry_slots(1),
        decode_cost(&identity),
        decode_cost(state.as_deref().unwrap()),
    ];
    let before = gates[..index].iter().copied().fold(raw, add);
    let gate = gates[index];
    let full = add(before, gate);
    assert!(full.0 < 64 * 1024 * 1024 && full.1 < 65_536);
    for byte_limit in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            full.0 - u64::from(byte_limit),
            full.1 - usize::from(!byte_limit),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = host.agent_roster_snapshot().unwrap_err();
            assert!(
                format!("{error:#}").contains("ResourceExhausted"),
                "{error:#}"
            );
            // The refused gate charges nothing: only raw material, Host
            // entry slots and any preceding identity decode survive.
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            assert!(phase.finish().is_err());
            assert!(host.agent_roster_snapshot().is_err());
            assert_eq!(ledger.report()?.observed_bytes, before.0);
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, before.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn roster_identity_decode_rejects_bytes_and_records_before_allocation() -> Result<()> {
    let (_home, host) = fixture().await?;
    rejects_host_gate(&host, 0)?;
    rejects_host_gate(&host, 1)
}

#[tokio::test]
async fn roster_state_decode_rejects_bytes_and_records_before_allocation() -> Result<()> {
    let (_home, host) = fixture().await?;
    rejects_host_gate(&host, 2)
}

#[tokio::test]
async fn roster_snapshot_admits_material_and_retains_original_holder() -> Result<()> {
    let (_home, host) = fixture().await?;
    let baseline = host.agent_roster_snapshot()?;
    assert!(baseline._material.is_some());
    assert_eq!(baseline.agents.len(), 1);
    let (raw, identity, state) = raw_cost(&host)?;
    let minimum = add(
        add(add(raw, entry_slots(1)), decode_cost(&identity)),
        decode_cost(state.as_deref().unwrap()),
    );
    let minimum = add(minimum, model_cost(&baseline.agents[0].agent.model));
    // This fixture has an idle posture with no Work/Task/Run IDs.
    let minimum = add(
        minimum,
        (
            (("no queued input, active turn, runnable work, or active waits".len()
                + std::mem::size_of::<String>())
                * 8) as u64,
            8,
        ),
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_roster_snapshot()?
    };
    assert_eq!(snapshot, baseline);
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    let used = phase.finish()?;
    assert_eq!(used, minimum);
    drop((phase, holder, snapshot));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn roster_malformed_state_keeps_error_and_original_phase_for_retry() -> Result<()> {
    let (_home, host) = fixture().await?;
    let (_, _, valid) = raw_cost(&host)?;
    set_state(&host, Some("null"))?;
    let error = host.agent_roster_snapshot().unwrap_err();
    assert!(
        format!("{error:#}").contains("unreadable roster agent state payload for web"),
        "{error:#}"
    );
    let (raw, identity, _) = raw_cost(&host)?;
    let expected = add(
        add(add(raw, entry_slots(1)), decode_cost(&identity)),
        decode_cost("null"),
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let error = host.agent_roster_snapshot().unwrap_err();
        assert!(
            format!("{error:#}").contains("unreadable roster agent state payload for web"),
            "{error:#}"
        );
        assert_eq!(phase.finish()?, expected);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    set_state(&host, valid.as_deref())?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_roster_snapshot()?
    };
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    assert!(phase.finish()?.0 > expected.0);
    drop((phase, holder));
    assert!(ledger.report()?.observed_bytes > expected.0);
    drop(snapshot);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn roster_snapshot_retains_holder_through_clone_and_http_serialization() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let (_home, host) = fixture().await?;
    let baseline = host.agent_roster_snapshot()?;
    assert!(baseline._material.is_some());
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_roster_snapshot()?
    };
    let used = phase.finish()?;
    let cloned = snapshot.clone();
    assert_eq!(cloned, snapshot);
    assert!(!Arc::ptr_eq(
        cloned._material.as_ref().unwrap(),
        snapshot._material.as_ref().unwrap()
    ));
    assert_eq!(ledger.report()?.observed_bytes, used.0 * 2);
    drop((phase, holder, snapshot));
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    // Match the route's ownership transition: retain the holder while moving
    // the fields into the wire value and serializing it, including failure.
    let _material = cloned._material;
    drop(cloned.acquisition);
    let wire = super::AgentRosterSnapshot {
        contract_version: super::AGENT_ROSTER_SNAPSHOT_CONTRACT_VERSION,
        runtime_id: cloned.runtime_id,
        event_log_epoch: cloned.event_log_epoch,
        visibility_scope_id: "fixture-scope".into(),
        agents: cloned
            .agents
            .into_iter()
            .map(|entry| super::AgentRosterEntry {
                agent: entry.agent,
                event_window: super::AgentEventWindow {
                    event_head_seq: entry.event_head_seq,
                    oldest_retained_seq: entry.oldest_retained_seq,
                },
                latest_brief: entry.latest_brief.map(|brief| super::AgentLatestBrief {
                    brief_id: brief.brief_id,
                    created_event_seq: brief.created_event_seq,
                    created_at: brief.created_at,
                    preview: brief.preview,
                }),
            })
            .collect(),
    };
    struct CheckedWire<'a> {
        wire: &'a super::AgentRosterSnapshot,
        ledger: &'a InputEvidenceBudgetLedger,
        expected: u64,
        fail: bool,
    }
    impl serde::Serialize for CheckedWire<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            assert_eq!(self.ledger.report().unwrap().observed_bytes, self.expected);
            if self.fail {
                return Err(serde::ser::Error::custom("fixture serialization failure"));
            }
            serde::Serialize::serialize(self.wire, serializer)
        }
    }
    for fail in [false, true] {
        let result = crate::http::serialize_json(
            "/agents/snapshot",
            &CheckedWire {
                wire: &wire,
                ledger: &ledger,
                expected: used.0,
                fail,
            },
        );
        assert_eq!(result.is_err(), fail);
        assert_eq!(ledger.report()?.observed_bytes, used.0);
    }
    drop(wire);
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    drop(_material);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    // The real route assembles and serializes the wire snapshot without the
    // test thread's phase; it does not propagate across spawn_blocking.
    // Separately verify the contract shape and member content.
    let response = crate::http::router(crate::http::AppState::for_tcp(host))
        .oneshot(
            Request::builder()
                .uri("/api/agents/snapshot")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(!String::from_utf8(bytes.to_vec())?.contains("_material"));
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let agents = value["agents"].as_array().unwrap();
    let web = agents
        .iter()
        .find(|entry| entry["agent"]["identity"]["agent_id"].as_str() == Some("web"))
        .expect("web on the roster wire");
    let baseline_web = baseline
        .agents
        .iter()
        .find(|entry| entry.agent.identity.agent_id == "web")
        .expect("web in the baseline snapshot");
    assert_eq!(web["agent"], serde_json::to_value(&baseline_web.agent)?);
    Ok(())
}
