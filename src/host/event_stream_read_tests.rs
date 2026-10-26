use super::*;
use crate::runtime_db::current_query::append_qualified_event_for_fixture;

async fn read_page(host: &RuntimeHost) -> Result<OwnedEventPageRead> {
    Ok(host
        .operator_event_page("default", None, None, 4, EventLogPageOrder::Desc, |_| {
            Ok(|_: &AuditEvent| true)
        })
        .await?)
}

fn publish_fixture(
    host: &RuntimeHost,
) -> Result<crate::runtime_db::transitions::PostCommitEffects> {
    let identity = host.active_agent_identity("default")?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id,
        identity_incarnation: identity.incarnation,
    };
    append_qualified_event_for_fixture(
        host.runtime_db(),
        &scope,
        &host.inner.input_evidence_budget,
        &AuditEvent::legacy(
            "live-qualified",
            serde_json::json!({"message": "unchanged"}),
        ),
    )
}

#[tokio::test]
async fn qualified_stream_live_bus_shares_original_body_and_refuses_raw_foreign_and_changed(
) -> Result<()> {
    let (_home, host) = super::super::tests::fixture().await?;
    let page = read_page(&host).await?;
    let scope = host.event_stream_scope(&page)?;
    drop(page);
    let mut rx = host.subscribe_events();
    let effects = publish_fixture(&host)?;
    let storage = host.agent_storage("default")?;
    assert!(storage.publish_transition_events(&effects).is_empty());
    let published = rx.try_recv()?;
    assert!(std::ptr::eq(&*published.event, &*effects.audit_events[0]));
    let read = scope.live_frame(&published)?;
    assert_eq!(read.event(), &*published.event);
    let raw = PublishedAuditEvent {
        qualified: None,
        ..published.clone()
    };
    assert!(scope.live_frame(&raw).is_err());
    let mut changed = published.clone();
    changed.event = crate::runtime_db::transitions::AuditEventAck::new(AuditEvent::legacy(
        "changed",
        serde_json::json!({}),
    ))?;
    assert!(scope.live_frame(&changed).is_err());
    let wrong_incarnation = EventStreamScope {
        scope: InputEvidenceScope {
            identity_incarnation: scope.scope.identity_incarnation + 1,
            ..scope.scope.clone()
        },
        ledger: scope.ledger.clone(),
        _material: scope._material.clone(),
    };
    assert!(wrong_incarnation.live_frame(&published).is_err());
    let foreign = EventStreamScope {
        ledger: InputEvidenceBudgetLedger::new(Default::default())?,
        scope: scope.scope.clone(),
        _material: scope._material.clone(),
    };
    assert!(foreign.live_frame(&published).is_err());
    drop((
        foreign,
        wrong_incarnation,
        raw,
        changed,
        read,
        published,
        effects,
        scope,
        rx,
    ));
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn qualified_stream_wire_is_pure_and_retains_original_body_through_last_slice() -> Result<()>
{
    let (_home, host) = super::super::tests::fixture().await?;
    let page = read_page(&host).await?;
    let scope = host.event_stream_scope(&page)?;
    let effects = publish_fixture(&host)?;
    let published = PublishedAuditEvent {
        agent_id: Some("default".into()),
        event: effects.audit_events[0].clone(),
        qualified: effects
            .current_read_writes
            .as_ref()
            .unwrap()
            .event_notification(&effects.audit_events[0])?,
    };
    let read = scope.live_frame(&published)?;
    let history = scope.history_frame(&page, 0)?;
    host.runtime_db().connection()?.execute_batch(
        "ALTER TABLE audit_events RENAME TO forbidden_stream_events;
         ALTER TABLE agent_identities RENAME TO forbidden_stream_identity;",
    )?;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let bytes = read.sse_bytes(read.event())?;
    let replay = history.sse_bytes(history.event())?;
    assert_eq!(denied.attempts(), 0);
    drop(denied);
    let text = std::str::from_utf8(&bytes)?;
    assert!(text.starts_with(&format!(
        "id: {}\nevent: live-qualified\ndata: ",
        read.event().event_seq
    )));
    assert!(text.ends_with("\n\n"));
    let payload = text.split_once("\ndata: ").unwrap().1.trim_end();
    assert_eq!(serde_json::from_str::<AuditEvent>(payload)?, *read.event());
    let guard = Arc::downgrade(&read.material.guard()?);
    let cached = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    drop((
        bytes, read, replay, history, scope, page, published, effects,
    ));
    assert!(guard.upgrade().is_some());
    drop(cached);
    assert!(guard.upgrade().is_some());
    drop(slice);
    assert!(guard.upgrade().is_none());
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.runtime_db().connection()?.execute_batch(
        "ALTER TABLE forbidden_stream_events RENAME TO audit_events;
         ALTER TABLE forbidden_stream_identity RENAME TO agent_identities;",
    )?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn qualified_stream_wire_exact_cumulative_allowance_and_one_short_latch() -> Result<()> {
    let (_home, host) = super::super::tests::fixture().await?;
    let page = read_page(&host).await?;
    let scope = host.event_stream_scope(&page)?;
    let read = scope.history_frame(&page, 0)?;
    let bytes = read.sse_bytes(read.event())?;
    let size = bytes.len();
    drop(bytes);
    // Independent oracle: the existing resource phase header, payload bytes,
    // and the actual final wire owner; no second Event/JSON body is built.
    let header = ResourceReadMaterial::new(&scope.ledger, &scope.scope)?.finish()?;
    let charge_bytes =
        u64::try_from(size)? + 8 * u64::try_from(std::mem::size_of::<EventFrameWire>() + 128)?;
    let full_bytes = header.0 + 2 * charge_bytes;
    let full_records = header.1 + 2 * 8;
    let exact = EventFrameRead {
        source: read.source.clone(),
        material: ResourceReadMaterial::with_test_limits(
            &scope.ledger,
            &scope.scope,
            full_bytes,
            full_records,
        )?,
    };
    let exact_bytes = exact.sse_bytes(exact.event())?;
    let second = exact.sse_bytes(exact.event())?;
    assert_eq!(exact.material.finish()?, (full_bytes, full_records));
    for (bytes, records) in [
        (full_bytes - 1, full_records),
        (full_bytes, full_records - 1),
    ] {
        let short = EventFrameRead {
            source: read.source.clone(),
            material: ResourceReadMaterial::with_test_limits(
                &scope.ledger,
                &scope.scope,
                bytes,
                records,
            )?,
        };
        drop(short.sse_bytes(short.event())?);
        assert!(short.sse_bytes(short.event()).is_err());
        assert!(short.material.finish().is_err());
    }
    assert_eq!(exact_bytes.len(), size);
    assert_eq!(second, exact_bytes);
    drop((exact_bytes, second, exact, read, scope, page));
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}
