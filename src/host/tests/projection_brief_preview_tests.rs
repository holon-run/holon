use super::super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

const MAX: usize = crate::http::observer_sync::LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-brief-preview".into(),
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

// Independent character-by-character oracle, not the production byte scan.
fn expected(text: &str) -> String {
    text.chars()
        .scan(0, |bytes, character| {
            *bytes += character.len_utf8();
            (*bytes <= MAX).then_some(character)
        })
        .collect()
}

#[test]
fn preview_moves_original_allocation_at_every_utf8_boundary() {
    let mut cases = vec![
        String::new(),
        "short 汉字\0\"\\\n".into(),
        "a".repeat(MAX),
        "b".repeat(MAX + 1),
    ];
    for character in ['é', '汉', '🦀'] {
        for offset in 0..=character.len_utf8() {
            cases.push(format!("{}{}tail", "x".repeat(MAX - offset), character));
        }
    }
    for mut text in cases {
        text.reserve(73);
        let pointer = text.as_ptr();
        let capacity = text.capacity();
        let expected = expected(&text);
        let preview = brief_preview(Some(text));
        assert_eq!(preview, expected);
        assert!(preview.len() <= MAX);
        assert_eq!(preview.as_ptr(), pointer);
        assert_eq!(preview.capacity(), capacity);
    }
}

#[test]
fn preview_absent_and_reserved_empty_need_no_new_material() -> Result<()> {
    let ledger = ledger()?;
    let cost = header()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
    let empty = String::with_capacity(31);
    let pointer = empty.as_ptr();
    let capacity = empty.capacity();
    {
        let _entry = phase.enter();
        let absent = brief_preview(None);
        assert!(absent.is_empty());
        assert_eq!(absent.capacity(), 0);
        let preview = brief_preview(Some(empty));
        assert!(preview.is_empty());
        assert_eq!(preview.as_ptr(), pointer);
        assert_eq!(preview.capacity(), capacity);
        assert_eq!(phase.finish()?, cost);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn preview_uses_admitted_source_without_refund_or_phase_reset() -> Result<()> {
    let source = format!("{}汉tail", "x".repeat(MAX - 1));
    let source_bytes = (source.len() + std::mem::size_of::<String>()) as u64;
    let initial = header()?;
    let full = (initial.0 + source_bytes * 8, initial.1 + 8);
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(source_bytes, 1)?;
        let owned = source.clone();
        let pointer = owned.as_ptr();
        let capacity = owned.capacity();
        let preview = brief_preview(Some(owned));
        assert_eq!(preview, expected(&source));
        assert_eq!(preview.as_ptr(), pointer);
        assert_eq!(preview.capacity(), capacity);
        assert_eq!(phase.finish()?, full);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        drop(preview);
        assert_eq!(ledger.report()?.observed_bytes, full.0);
        assert!(charge_owned_metadata(1, 1).is_err());
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard().is_err());
        assert!(charge_owned_metadata(1, 1).is_err());
        assert_eq!(ledger.report()?.observed_bytes, full.0);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, full.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
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
    assert_eq!(ledger.report()?.observed_bytes, used.0 * 2);
    drop((snapshot, phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    let bytes = crate::http::serialize_json(
        "/agents/{agent_id}/projection-snapshot",
        &clone.latest_brief.as_ref().unwrap().preview,
    )
    .map_err(|(status, _)| anyhow!("HTTP serialization failed: {status}"))?;
    assert_eq!(serde_json::from_slice::<String>(&bytes)?, expected);
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    drop(clone);
    assert_eq!(ledger.report()?.observed_bytes, 0);
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
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn preview_later_assembly_failure_preserves_phase_until_last_holder() -> Result<()> {
    let (_home, host, _source) = fixture().await?;
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_projection_snapshot("web")?.unwrap()
    };
    let full = phase.finish()?;
    drop((snapshot, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    // The final hydration ID is allocated after preview has moved. Reject
    // that allocation separately on bytes and records; keep its strict gate.
    for bytes in [true, false] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            if bytes { full.0 - 1 } else { full.0 },
            if bytes { full.1 } else { full.1 - 1 },
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = host.agent_projection_snapshot("web").unwrap_err();
            assert!(
                format!("{error:#}").contains("ResourceExhausted"),
                "{error:#}"
            );
            let consumed = ledger.report()?.observed_bytes;
            assert!(consumed > 0 && consumed < full.0);
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard().is_err());
            assert!(host.agent_projection_snapshot("web").is_err());
            assert_eq!(ledger.report()?.observed_bytes, consumed);
        }
        let consumed = ledger.report()?.observed_bytes;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, consumed);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
