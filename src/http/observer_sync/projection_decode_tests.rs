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
        agent_id: "projection-decode".into(),
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
    state["decode_fixture"] = serde_json::json!({
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

fn raw_cost(host: &RuntimeHost, agent_id: &str) -> Result<((u64, usize), String, Option<String>)> {
    let payloads = host.runtime_db().agent_projection_snapshot_rows(agent_id)?;
    let (identity, state) = payloads
        .row
        .map(|row| (row.identity_json, row.agent_state_json))
        .unwrap_or_default();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let rows = {
        let _entry = phase.enter();
        host.runtime_db().agent_projection_snapshot_rows(agent_id)?
    };
    let cost = phase.finish()?;
    drop((rows, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok((cost, identity, state))
}

fn rejects_decode(host: &RuntimeHost, state_gate: bool) -> Result<()> {
    let (raw, identity, state) = raw_cost(host, "web")?;
    let before = if state_gate {
        add(raw, decode_cost(&identity))
    } else {
        raw
    };
    let gate = decode_cost(if state_gate {
        state.as_deref().unwrap()
    } else {
        &identity
    });
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
            let error = host.agent_projection_snapshot("web").unwrap_err();
            assert!(
                format!("{error:#}").contains("ResourceExhausted"),
                "{error:#}"
            );
            // The refused gate must allocate nothing: exactly the SQL/raw
            // material and, for State, the preceding identity decode survive.
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            assert!(phase.finish().is_err());
            assert!(host.agent_projection_snapshot("web").is_err());
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
async fn identity_decode_rejects_bytes_and_records_before_allocation() -> Result<()> {
    let (_home, host) = fixture().await?;
    rejects_decode(&host, false)
}

#[tokio::test]
async fn state_decode_rejects_bytes_and_records_before_allocation() -> Result<()> {
    let (_home, host) = fixture().await?;
    rejects_decode(&host, true)
}

#[tokio::test]
async fn null_state_and_unknown_member_skip_the_absent_decode() -> Result<()> {
    let (_home, host) = fixture().await?;
    set_state(&host, None)?;
    let baseline = host.agent_projection_snapshot("web")?.unwrap();
    assert!(baseline._material.is_some());
    let (raw, identity, state) = raw_cost(&host, "web")?;
    assert!(state.is_none());
    let minimum = add(raw, decode_cost(&identity));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_projection_snapshot("web")?.unwrap()
    };
    assert_eq!(snapshot, baseline);
    let used = phase.finish()?;
    assert!(used.0 >= minimum.0 && used.1 >= minimum.1);
    drop((phase, snapshot));
    assert_eq!(ledger.report()?.observed_bytes, 0);

    let (raw, identity, state) = raw_cost(&host, "absent")?;
    assert!(identity.is_empty() && state.is_none());
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), raw.0, raw.1)?;
    {
        let _entry = phase.enter();
        assert!(host.agent_projection_snapshot("absent")?.is_none());
    }
    assert_eq!(phase.finish()?, raw);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn malformed_state_keeps_decode_error_and_original_phase_for_retry() -> Result<()> {
    let (_home, host) = fixture().await?;
    let (_, _, valid) = raw_cost(&host, "web")?;
    set_state(&host, Some("null"))?;
    let error = host.agent_projection_snapshot("web").unwrap_err();
    assert!(format!("{error:#}").contains("unreadable projection agent state payload for web"));
    let (raw, identity, _) = raw_cost(&host, "web")?;
    let expected = add(add(raw, decode_cost(&identity)), decode_cost("null"));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let error = host.agent_projection_snapshot("web").unwrap_err();
        assert!(format!("{error:#}").contains("unreadable projection agent state payload for web"));
        assert_eq!(phase.finish()?, expected);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    set_state(&host, valid.as_deref())?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_projection_snapshot("web")?.unwrap()
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
async fn decoded_projection_retains_original_holder_through_clone_and_http() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let (_home, host) = fixture().await?;
    let baseline = host.agent_projection_snapshot("web")?.unwrap();
    assert!(baseline._material.is_some());
    let (raw, identity, state) = raw_cost(&host, "web")?;
    let minimum = add(
        add(raw, decode_cost(&identity)),
        decode_cost(state.as_deref().unwrap()),
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_projection_snapshot("web")?.unwrap()
    };
    assert_eq!(snapshot, baseline);
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    let used = phase.finish()?;
    assert!(used.0 >= minimum.0 && used.1 >= minimum.1);
    let mut cloned = snapshot.clone();
    assert_eq!(cloned, baseline);
    assert_eq!(ledger.report()?.observed_bytes, used.0 * 2);
    drop((phase, holder, snapshot));
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    // Exercise the synchronous HTTP serializer without copying the typed
    // fields; the real route below separately verifies their wire mapping.
    let material = cloned._material.take();
    #[derive(serde::Serialize)]
    struct Wire<'a> {
        agent: &'a crate::types::AgentListEntry,
        relations: &'a crate::types::AgentCanonicalRelationsProjection,
    }
    let wire = Wire {
        agent: &cloned.agent,
        relations: &cloned.canonical_relations,
    };
    let bytes = super::serialize_json("/agents/{agent_id}/projection-snapshot", &wire)
        .map_err(|(status, _)| anyhow::anyhow!("HTTP serialization failed: {status}"))?;
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    drop((cloned, material));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value["agent"], serde_json::to_value(&baseline.agent)?);
    assert_eq!(
        value["relations"],
        serde_json::to_value(&baseline.canonical_relations)?
    );
    assert!(!String::from_utf8(bytes.to_vec())?.contains("_material"));
    let response = crate::http::router(crate::http::AppState::for_tcp(host))
        .oneshot(
            Request::builder()
                .uri("/api/agents/web/projection-snapshot")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        value["projection"]["agent"],
        serde_json::to_value(&baseline.agent)?
    );
    assert_eq!(
        value["projection"]["relations"],
        serde_json::to_value(&baseline.canonical_relations)?
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
