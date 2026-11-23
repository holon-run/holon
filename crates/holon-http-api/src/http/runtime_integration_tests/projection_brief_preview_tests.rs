use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

const MAX: usize = crate::host::LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-brief-preview".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn expected(text: &str) -> String {
    text.chars()
        .scan(0, |bytes, character| {
            *bytes += character.len_utf8();
            (*bytes <= MAX).then_some(character)
        })
        .collect()
}

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost, String)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let preview = format!("{}汉\0tail", "x".repeat(MAX - 1));
    host.runtime_db().connection()?.execute(
        "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, preview, payload_json)
         VALUES ('brief-preview', 'web', '2026-02-01T00:00:00.000Z', 'result', ?1, '{}')",
        [&preview],
    )?;
    Ok((home, host, preview))
}

#[tokio::test]
async fn preview_snapshot_keeps_holder_through_clone_and_http() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host, source) = fixture().await?;
    let expected = expected(&source);
    let baseline = host.agent_projection_snapshot("web")?.unwrap();
    assert!(baseline._material.is_some());
    assert_eq!(baseline.latest_brief.as_ref().unwrap().preview, expected);
    let roster = host.agent_roster_snapshot()?;
    assert_eq!(
        roster
            .agents
            .iter()
            .find(|row| row.agent.identity.agent_id == "web")
            .unwrap()
            .latest_brief
            .as_ref()
            .unwrap()
            .preview,
        expected
    );
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_projection_snapshot("web")?.unwrap()
    };
    assert_eq!(snapshot, baseline);
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    let used = phase.finish()?;
    let clone = snapshot.clone();
    assert_eq!(clone, baseline);
    assert!(!Arc::ptr_eq(clone._material.as_ref().unwrap(), &holder));
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, used.0 * 2);
    drop((snapshot, phase, holder));
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, used.0);
    let bytes = crate::http::serialize_json(
        "/agents/{agent_id}/projection-snapshot",
        &clone.latest_brief.as_ref().unwrap().preview,
    )
    .map_err(|(status, _)| anyhow!("HTTP serialization failed: {status}"))?;
    assert_eq!(serde_json::from_slice::<String>(&bytes)?, expected);
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, used.0);
    drop(clone);
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
    // The separate Axum request checks the real wire mapping, not TLS
    // propagation across the route's spawn_blocking boundary.
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
    assert_eq!(value["projection"]["latest_brief"]["preview"], expected);
    assert!(!String::from_utf8(bytes.to_vec())?.contains("_material"));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
    Ok(())
}
