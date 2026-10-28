use super::super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    provider::StubProvider,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentDurability, AgentOwnership, AgentProfilePreset},
};

fn text(value: &str) -> String {
    let mut text = String::with_capacity(value.len() + 79);
    text.push_str(value);
    text
}

fn record() -> AgentIdentityRecord {
    let mut record = AgentIdentityRecord::new(
        text("web"),
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        Some(text("parent")),
        Some(text("task")),
    );
    record.name = Some(text("汉字 🦀"));
    record.lineage_parent_agent_id = Some(text("lineage"));
    record.incarnation = 7;
    record
}

fn record_fields(record: &AgentIdentityRecord) -> [Option<&String>; 5] {
    [
        Some(&record.agent_id),
        record.name.as_ref(),
        record.parent_agent_id.as_ref(),
        record.lineage_parent_agent_id.as_ref(),
        record.delegated_from_task_id.as_ref(),
    ]
}

fn view_fields(view: &AgentIdentityView) -> [Option<&String>; 5] {
    [
        Some(&view.agent_id),
        view.name.as_ref(),
        view.parent_agent_id.as_ref(),
        view.lineage_parent_agent_id.as_ref(),
        view.delegated_from_task_id.as_ref(),
    ]
}

fn allocations(fields: [Option<&String>; 5]) -> [Option<(*const u8, usize)>; 5] {
    fields.map(|field| field.map(|text| (text.as_ptr(), text.capacity())))
}

#[path = "projection_identity_move_tests/detail_tests.rs"]
mod detail_tests;

#[path = "projection_identity_move_tests/cold_tests.rs"]
mod cold_tests;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-identity-move".into(),
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

#[test]
fn owned_identity_matches_borrowed_policy_and_preserves_all_text_allocations() {
    // Includes explicit overrides and every legacy fallback input; lineage
    // alone must not imply supervision. Compare every view field, not JSON
    // which intentionally omits the internal policy axes.
    let mut cases = 0;
    for kind in [AgentKind::Default, AgentKind::Named, AgentKind::Child] {
        for visibility in [AgentVisibility::Public, AgentVisibility::Private] {
            for ownership in [
                None,
                Some(AgentOwnership::SelfOwned),
                Some(AgentOwnership::ParentSupervised),
            ] {
                for durability in [
                    None,
                    Some(AgentDurability::Persistent),
                    Some(AgentDurability::Ephemeral),
                ] {
                    for preset in [
                        None,
                        Some(AgentProfilePreset::PublicNamed),
                        Some(AgentProfilePreset::PrivateChild),
                    ] {
                        for mask in 0..8 {
                            for status in [
                                AgentRegistryStatus::Active,
                                AgentRegistryStatus::Deleting,
                                AgentRegistryStatus::Deleted,
                            ] {
                                for default in ["web", "other"] {
                                    let mut record = record();
                                    record.kind = kind;
                                    record.visibility = visibility;
                                    record.ownership = ownership;
                                    record.durability = durability;
                                    record.profile_preset = preset;
                                    record.status = status;
                                    if mask & 1 == 0 {
                                        record.parent_agent_id = None;
                                    }
                                    if mask & 2 == 0 {
                                        record.lineage_parent_agent_id = None;
                                    }
                                    if mask & 4 == 0 {
                                        record.delegated_from_task_id = None;
                                    }
                                    let expected = AgentIdentityView::from_record(&record, default);
                                    let original = allocations(record_fields(&record));
                                    let view =
                                        AgentIdentityView::from_owned_record(record, default);
                                    assert_eq!(view, expected);
                                    assert_eq!(allocations(view_fields(&view)), original);
                                    cases += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 7776);
}

#[test]
fn owned_identity_absent_and_reserved_empty_text_need_no_new_material() -> Result<()> {
    let ledger = ledger()?;
    let header = header()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header.0, header.1)?;
    {
        let _entry = phase.enter();
        for present in [false, true] {
            let mut record = record();
            record.agent_id = text("");
            record.name = present.then(|| text(""));
            record.parent_agent_id = present.then(|| text(""));
            record.lineage_parent_agent_id = present.then(|| text(""));
            record.delegated_from_task_id = present.then(|| text(""));
            let expected = AgentIdentityView::from_record(&record, "");
            let original = allocations(record_fields(&record));
            let view = AgentIdentityView::from_owned_record(record, "");
            assert_eq!(view, expected);
            assert_eq!(allocations(view_fields(&view)), original);
            assert_eq!(phase.finish()?, header);
        }
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn owned_identity_keeps_source_charge_original_phase_and_failure_latch() -> Result<()> {
    let source = record();
    let bytes: u64 = record_fields(&source)
        .into_iter()
        .flatten()
        .map(|text| (text.len() + std::mem::size_of::<String>()) as u64)
        .sum();
    let initial = header()?;
    let full = (initial.0 + bytes * 8, initial.1 + 5 * 8);
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), full.0, full.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        // Admit the fixture's source copies, then consume those copies.
        charge_owned_metadata(bytes, 5)?;
        let source = source.clone();
        let original = allocations(record_fields(&source));
        let view = AgentIdentityView::from_owned_record(source, "web");
        assert_eq!(allocations(view_fields(&view)), original);
        assert_eq!(phase.finish()?, full);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        drop(view);
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

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    host.rename_public_agent("web", "汉字 🦀", "fixture")
        .map_err(|error| anyhow!("{error}"))?;
    host.runtime_db().connection()?.execute(
        "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, preview, payload_json)
         VALUES ('identity-brief', 'web', '2026-02-01T00:00:00.000Z', 'result', 'preview', '{}')",
        [],
    )?;
    Ok((home, host))
}

#[tokio::test]
async fn committed_entry_consumes_all_original_identity_text_fields() -> Result<()> {
    let (_home, host) = fixture().await?;
    let record = record();
    let expected = AgentIdentityView::from_record(&record, &host.config().default_agent_id);
    let original = allocations(record_fields(&record));
    let catalog = RuntimeModelCatalog::from_config(&host.config());
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let entry = host.agent_list_entry_from_committed_state(
            record,
            Some(AgentState::new("web")),
            &catalog,
        )?;
        assert_eq!(entry.identity, expected);
        assert_eq!(allocations(view_fields(&entry.identity)), original);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn identity_snapshot_and_http_consumers_keep_the_original_holder() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host) = fixture().await?;
    let expected = host
        .agent_projection_snapshot("web")?
        .unwrap()
        .agent
        .identity;
    for roster in [false, true] {
        let ledger = ledger()?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let (identity, retained) = {
            let _entry = phase.enter();
            if roster {
                let mut snapshot = host.agent_roster_snapshot()?;
                assert_eq!(snapshot.agents.len(), 1);
                assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
                (
                    snapshot.agents.pop().unwrap().agent.identity,
                    snapshot._material,
                )
            } else {
                let snapshot = host.agent_projection_snapshot("web")?.unwrap();
                assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
                (snapshot.agent.identity, snapshot._material)
            }
        };
        assert_eq!(identity, expected);
        let cost = phase.finish()?;
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        struct Checked<'a> {
            identity: &'a AgentIdentityView,
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
                    return Err(serde::ser::Error::custom("identity fixture failure"));
                }
                serde::Serialize::serialize(self.identity, serializer)
            }
        }
        for fail in [false, true] {
            let result = crate::http::serialize_json(
                "/identity-fixture",
                &Checked {
                    identity: &identity,
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
        drop(identity);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let router = crate::http::router(crate::http::AppState::for_tcp(host));
    for (uri, pointer) in [
        ("/api/agents/snapshot", "/agents/0/agent/identity"),
        (
            "/api/agents/web/projection-snapshot",
            "/projection/agent/identity",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(
            value.pointer(pointer),
            Some(&serde_json::to_value(&expected)?)
        );
        assert!(!String::from_utf8(body.to_vec())?.contains("_material"));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[tokio::test]
async fn identity_later_anchor_failure_retries_on_same_phase_and_releases_last_holder() -> Result<()>
{
    let (_home, host) = fixture().await?;
    host.runtime_db().connection()?.execute(
        "UPDATE briefs SET created_at = 'invalid' WHERE evidence_id = 'identity-brief'",
        [],
    )?;
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let error = host.agent_projection_snapshot("web").unwrap_err();
        assert!(
            format!("{error:#}").contains("unreadable latest brief timestamp for web"),
            "{error:#}"
        );
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    let failed_cost = phase.finish()?;
    host.runtime_db().connection()?.execute(
        "UPDATE briefs SET created_at = '2026-02-01T00:00:00.000Z' WHERE evidence_id = 'identity-brief'", [],
    )?;
    let snapshot = {
        let _entry = phase.enter();
        host.agent_projection_snapshot("web")?.unwrap()
    };
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    let full = phase.finish()?;
    assert!(full.0 > failed_cost.0 && full.1 > failed_cost.1);
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, full.0);
    drop(snapshot);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
