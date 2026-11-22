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

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-hydration".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn build(ids: [Option<&str>; 3]) -> Result<Vec<AgentHydrationReferenceData>> {
    projection_hydration_references(ids[0], ids[1], ids[2])
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
    let connection = host.runtime_db().connection()?;
    connection.execute(
        "INSERT INTO messages (evidence_id, agent_id, created_at, kind, payload_json)
         VALUES (?1, 'web', '2026-02-01T00:00:00.000Z', 'operator_prompt', '{}')",
        [IDS[0]],
    )?;
    connection.execute(
        "INSERT INTO transcript_entries (evidence_id, agent_id, created_at, kind, payload_json)
         VALUES (?1, 'web', '2026-02-01T00:00:00.000Z', 'assistant', '{}')",
        [IDS[1]],
    )?;
    connection.execute(
        "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, preview, payload_json)
         VALUES (?1, 'web', '2026-02-01T00:00:00.000Z', 'result', 'preview', '{}')",
        [IDS[2]],
    )?;
    drop(connection);
    Ok((home, host))
}

#[tokio::test]
async fn hydration_snapshot_keeps_original_holder_through_clone_and_http() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let (_home, host) = fixture().await?;
    let baseline = host.agent_projection_snapshot("web")?.unwrap();
    assert!(baseline._material.is_some());
    assert_eq!(baseline.hydration_references, build(IDS.map(Some))?);
    assert_eq!(
        baseline.conversation.latest_message_id.as_deref(),
        Some(IDS[0])
    );
    assert_eq!(
        baseline.conversation.latest_transcript_entry_id.as_deref(),
        Some(IDS[1])
    );
    assert_eq!(baseline.latest_brief.as_ref().unwrap().brief_id, IDS[2]);
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
    drop((snapshot, holder, phase));
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, used.0);
    assert_eq!(clone.hydration_references, baseline.hydration_references);
    #[derive(serde::Serialize)]
    struct HydrationWire<'a> {
        record_kind: &'static str,
        record_id: &'a str,
    }
    let wire: [HydrationWire<'_>; 3] = std::array::from_fn(|index| {
        let reference = &clone.hydration_references[index];
        HydrationWire {
            record_kind: match reference.record_kind {
                ObserverSyncRecordKindData::Message => "message",
                ObserverSyncRecordKindData::TranscriptEntry => "transcript_entry",
                ObserverSyncRecordKindData::Brief => "brief",
            },
            record_id: &reference.record_id,
        }
    });
    let bytes = crate::http::serialize_json("/agents/{agent_id}/projection-snapshot", &wire)
        .map_err(|(status, _)| anyhow!("HTTP serialization failed: {status}"))?;
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, used.0);
    let serialized: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(serialized[1]["record_id"], IDS[1]);
    drop(clone);
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
    // The actual Axum route separately proves the wire mapping. The thread-local
    // test phase above is not claimed to cross spawn_blocking in that route.
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
        value["projection"]["hydration_references"],
        serde_json::json!([
            {"record_kind": "message", "record_id": IDS[0]},
            {"record_kind": "transcript_entry", "record_id": IDS[1]},
            {"record_kind": "brief", "record_id": IDS[2]}
        ])
    );
    assert!(!String::from_utf8(bytes.to_vec())?.contains("_material"));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(ledger.report_for_http_test()?.observed_bytes, 0);
    Ok(())
}

const IDS: [&str; 3] = ["shared-汉字", "transcript-\"\\字", "shared-汉字"];
