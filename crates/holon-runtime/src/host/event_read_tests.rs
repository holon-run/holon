use super::*;
use crate::{
    provider::StubProvider, runtime::resident_input_evidence::InputEvidenceScope,
    runtime_db::current_query::append_qualified_event_for_fixture,
};

#[cfg(holon_lib_test_shard = "d")]
#[path = "event_page_barrier_tests.rs"]
mod barrier;

pub(super) use crate::runtime::http_test_support::qualified_event_history_fixture as fixture;

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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn finite_history_cold_read_matches_original_page_without_activation_or_live_publication(
) -> Result<()> {
    let (_home, host) = fixture().await?;
    let storage = host.agent_storage_read_only("default")?;
    // A committed startup observation has no live Current qualification.
    let event = AuditEvent::legacy(
        "startup_external_side_effect_indeterminate",
        serde_json::json!({"index": 4}),
    );
    host.runtime_db()
        .audit_events()
        .append(Some("default"), &event)?;
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
        assert_eq!(page.cursor_seq, Some(5));
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
    let qualifications: usize = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM current_read_qualification WHERE record_id=?1",
        [&event.id],
        |row| row.get(0),
    )?;
    assert_eq!(qualifications, 0);
    host.shutdown().await?;
    Ok(())
}

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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
