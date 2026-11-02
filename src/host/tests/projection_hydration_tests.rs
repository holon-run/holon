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

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-hydration".into(),
        identity_incarnation: 1,
    }
}

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn plus(left: (u64, usize), right: (u64, usize)) -> (u64, usize) {
    (left.0 + right.0, left.1 + right.1)
}

fn header() -> Result<(u64, usize)> {
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let cost = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(cost)
}

// Independent oracle: one allocation for the selected Vec slots, then one
// String header and its UTF-8 bytes for each present ID, including empty IDs.
fn gates(ids: [Option<&str>; 3]) -> Vec<(u64, usize)> {
    let count = ids.iter().flatten().count();
    if count == 0 {
        return Vec::new();
    }
    let mut costs = vec![(
        (count * std::mem::size_of::<AgentHydrationReferenceData>()) as u64 * 8,
        count * 8,
    )];
    costs.extend(
        ids.into_iter()
            .flatten()
            .map(|id| ((id.len() + std::mem::size_of::<String>()) as u64 * 8, 8)),
    );
    costs
}

fn build(ids: [Option<&str>; 3]) -> Result<Vec<AgentHydrationReferenceData>> {
    projection_hydration_references(ids[0], ids[1], ids[2])
}

#[test]
fn hydration_optional_selection_order_and_cost_are_exact() -> Result<()> {
    // Equal IDs across record families are not duplicates; Some("") remains
    // present. UTF-8 and embedded NUL must be counted by bytes, not characters.
    let values = ["shared-汉\0字", "", "shared-汉\0字"];
    for mask in 0..8 {
        let ids = std::array::from_fn(|index| (mask & (1 << index) != 0).then_some(values[index]));
        let kinds = [
            ObserverSyncRecordKindData::Message,
            ObserverSyncRecordKindData::TranscriptEntry,
            ObserverSyncRecordKindData::Brief,
        ];
        let expected: Vec<_> = (0..3)
            .filter(|index| mask & (1 << index) != 0)
            .map(|index| AgentHydrationReferenceData {
                record_kind: kinds[index],
                record_id: values[index].into(),
            })
            .collect();
        assert_eq!(build(ids)?, expected);
        let cost = gates(ids).into_iter().fold(header()?, plus);
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
        let references = {
            let _entry = phase.enter();
            build(ids)?
        };
        assert_eq!(references, expected);
        if mask == 0 {
            assert_eq!(references.capacity(), 0);
        }
        assert_eq!(phase.finish()?, cost);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop((references, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn hydration_each_allocation_rejects_before_charge_and_latches_original_phase() -> Result<()> {
    let ids = [Some("message-汉"), Some(""), Some("message-汉")];
    let gates = gates(ids);
    let full = gates.iter().copied().fold(header()?, plus);
    let mut before = header()?;
    for gate in gates {
        for byte_limit in [true, false] {
            let boundary = plus(before, gate);
            let ledger = ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if byte_limit { boundary.0 - 1 } else { full.0 },
                if byte_limit { full.1 } else { boundary.1 - 1 },
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                let error = build(ids).unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                assert_eq!(ledger.report()?.observed_bytes, before.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(build(ids).is_err());
                assert_eq!(ledger.report()?.observed_bytes, before.0);
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            // A failed partial Vec must not retain a hidden phase owner.
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        before = plus(before, gate);
    }
    Ok(())
}

#[test]
fn hydration_consumes_existing_remainder_without_reset_or_empty_allocation() -> Result<()> {
    let ids = [None, Some("transcript-多字"), None];
    let prior = (17 * 8, 2 * 8);
    let full = gates(ids).into_iter().fold(plus(header()?, prior), plus);
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(17, 2)?;
        let references = build(ids)?;
        assert_eq!(references.len(), 1);
        assert_eq!(phase.finish()?, full);
        assert_eq!(build([None; 3])?.capacity(), 0);
        assert_eq!(phase.finish()?, full);
        assert!(build(ids).is_err());
        assert_eq!(ledger.report()?.observed_bytes, full.0);
        assert!(phase.finish().is_err());
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

const IDS: [&str; 3] = ["shared-汉字", "transcript-\"\\字", "shared-汉字"];

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
    assert_eq!(ledger.report()?.observed_bytes, used.0 * 2);
    drop((snapshot, holder, phase));
    assert_eq!(ledger.report()?.observed_bytes, used.0);
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
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    let serialized: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(serialized[1]["record_id"], IDS[1]);
    drop(clone);
    assert_eq!(ledger.report()?.observed_bytes, 0);
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
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn hydration_host_late_failure_drops_partial_snapshot_without_refunding_phase() -> Result<()>
{
    let (_home, host) = fixture().await?;
    let hydration_gates = gates(IDS.map(Some));
    let hydration = hydration_gates.iter().copied().fold((0, 0), plus);
    // Measure only the already-accepted upstream chain. Independently subtract
    // the complete new allocation sequence, which is the last assembly step.
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_projection_snapshot("web")?.unwrap()
    };
    let full = phase.finish()?;
    drop((snapshot, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut before = (full.0 - hydration.0, full.1 - hydration.1);
    for gate in hydration_gates {
        let boundary = plus(before, gate);
        for byte_limit in [true, false] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if byte_limit { boundary.0 - 1 } else { full.0 },
                if byte_limit { full.1 } else { boundary.1 - 1 },
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                let error = host.agent_projection_snapshot("web").unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                assert_eq!(ledger.report()?.observed_bytes, before.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(host.agent_projection_snapshot("web").is_err());
                assert_eq!(ledger.report()?.observed_bytes, before.0);
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        before = boundary;
    }
    assert!(host
        .agent_projection_snapshot("web")?
        .unwrap()
        ._material
        .is_some());
    Ok(())
}
