use super::*;
use crate::{
    provider::StubProvider, runtime::resident_input_evidence::InputEvidenceScope,
    runtime_db::current_query::append_qualified_event_for_fixture,
};

pub(super) async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = Some(home.path().to_path_buf());
    config.default_agent_id = "default".into();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    let identity = host.ensure_default_agent_identity()?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id,
        identity_incarnation: identity.incarnation,
    };
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&AgentState::new(&scope.agent_id))?;
    for index in 0..4 {
        let event = AuditEvent::legacy("qualified-history", serde_json::json!({"index": index}));
        let ledger = &host.inner.input_evidence_budget;
        append_qualified_event_for_fixture(host.runtime_db(), &scope, ledger, &event)?;
    }
    Ok((home, host))
}

struct BorrowedEvents<'a>(&'a OwnedEventPageRead);

impl serde::Serialize for BorrowedEvents<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.events().len()))?;
        for event in self.0.events() {
            seq.serialize_element(event)?;
        }
        seq.end()
    }
}

#[tokio::test]
async fn finite_history_http_page_uses_qualified_read_and_retains_its_response_body() -> Result<()>
{
    use axum::{
        extract::{Path, Query, State},
        response::IntoResponse,
    };
    let (_home, host) = fixture().await?;
    let state = Arc::new(crate::http::AppState::for_tcp(host.clone()));
    let response = crate::http::events(
        Path("default".to_owned()),
        State(state),
        axum::http::HeaderMap::new(),
        Query(serde_json::from_value(serde_json::json!({
            "limit": 2, "order": "asc", "event_kind": "qualified-history"
        }))?),
    )
    .await
    .map_err(|(status, body)| anyhow::anyhow!("{status}: {:?}", body.0))?
    .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "application/json"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value["cursor_seq"], 4);
    assert_eq!(value["oldest_seq"], 1);
    assert_eq!(value["newest_seq"], 2);
    assert_eq!(value["has_newer"], true);
    assert_eq!(value["has_older"], false);
    assert_eq!(value["events"][0]["payload"]["index"], 0);
    assert_eq!(value["events"].as_array().unwrap().len(), 2);
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > 0);
    drop(bytes);
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn finite_history_cold_read_matches_original_page_without_activation_or_live_publication(
) -> Result<()> {
    let (_home, host) = fixture().await?;
    let storage = host.agent_storage_read_only("default")?;
    let revision: u64 = host.runtime_db().connection()?.query_row(
        "SELECT revision FROM current_read_revisions WHERE agent_id='default'",
        [],
        |row| row.get(0),
    )?;
    for order in [EventLogPageOrder::Asc, EventLogPageOrder::Desc] {
        let expected = storage.read_event_page_matching(None, Some(0), 1, order, |event| {
            event.data["index"].as_u64().unwrap() % 2 == 0
        })?;
        let page = host
            .operator_event_page("default", None, Some(0), 1, order, |_| {
                Ok(|event: &AuditEvent| event.data["index"].as_u64().unwrap() % 2 == 0)
            })
            .await?;
        assert_eq!(
            page.events().collect::<Vec<_>>(),
            expected.events.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            (page.has_older, page.has_newer),
            (expected.has_older, expected.has_newer)
        );
        assert_eq!(page.cursor_seq, Some(4));
        drop(page);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    }
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    let after: u64 = host.runtime_db().connection()?.query_row(
        "SELECT revision FROM current_read_revisions WHERE agent_id='default'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        after, revision,
        "reads must not create Current publications"
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn finite_history_wire_keeps_original_source_through_cache_and_last_slice() -> Result<()> {
    let (_home, host) = fixture().await?;
    let page = host
        .operator_event_page("default", None, None, 2, EventLogPageOrder::Desc, |_| {
            Ok(|_: &AuditEvent| true)
        })
        .await?;
    let guard = Arc::downgrade(&page.0.material.guard()?);
    // Serializing a qualified read is pure: it must not return to SQLite.
    host.runtime_db().connection()?.execute_batch(
        "ALTER TABLE audit_events RENAME TO forbidden_history_events;
         ALTER TABLE agent_identities RENAME TO forbidden_history_identity;",
    )?;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let bytes = page.json_bytes(&BorrowedEvents(&page))?;
    assert_eq!(denied.attempts(), 0);
    drop(denied);
    let decoded: Vec<AuditEvent> = serde_json::from_slice(&bytes)?;
    assert_eq!(decoded.len(), 2);
    let cached = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    drop((page, bytes));
    assert!(guard.upgrade().is_some());
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > 0);
    drop(cached);
    assert!(guard.upgrade().is_some());
    drop(slice);
    assert!(guard.upgrade().is_none());
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.runtime_db().connection()?.execute_batch(
        "ALTER TABLE forbidden_history_events RENAME TO audit_events;
         ALTER TABLE forbidden_history_identity RENAME TO agent_identities;",
    )?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn finite_history_wire_exact_cumulative_axes_and_last_slice() -> Result<()> {
    let (_home, host) = fixture().await?;
    for (short_bytes, short_records) in [(0, 0), (1, 0), (0, 1)] {
        let mut page = host
            .operator_event_page("default", None, None, 2, EventLogPageOrder::Desc, |_| {
                Ok(|_: &AuditEvent| true)
            })
            .await?;
        let ledger = &host.inner.input_evidence_budget;
        let header = ResourceReadMaterial::new(ledger, &page.0.scope)?.finish()?;
        let size = crate::runtime::input_evidence_budget::serialized_bytes(&BorrowedEvents(&page))?;
        let bytes =
            header.0 + 2 * (size + 8 * u64::try_from(std::mem::size_of::<EventPageWire>() + 128)?);
        let records = header.1 + 2 * 8;
        let material = ResourceReadMaterial::with_test_limits(
            ledger,
            &page.0.scope,
            bytes - short_bytes,
            records - short_records,
        )?;
        Arc::get_mut(&mut page.0)
            .context("fixture page must have one owner")?
            .material = material;
        let first = page.json_bytes(&BorrowedEvents(&page))?;
        assert_eq!(u64::try_from(first.len())?, size);
        drop(first); // Dropping a wire product must not replenish its phase.
        let second = page.json_bytes(&BorrowedEvents(&page));
        if short_bytes != 0 || short_records != 0 {
            assert!(second.is_err());
            assert!(page.0.material.finish().is_err());
            assert!(page.json_bytes(&BorrowedEvents(&page)).is_err());
            drop(page);
        } else {
            let second = second?;
            assert_eq!(page.0.material.finish()?, (bytes, records));
            let guard = Arc::downgrade(&page.0.material.guard()?);
            let slice = second.slice(1..second.len() - 1);
            drop((page, second));
            assert!(guard.upgrade().is_some());
            drop(slice);
            assert!(guard.upgrade().is_none());
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    host.shutdown().await?;
    Ok(())
}
