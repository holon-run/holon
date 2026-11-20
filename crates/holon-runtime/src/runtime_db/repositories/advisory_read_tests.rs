use super::*;

#[test]
fn advisory_rate_limit_keeps_agent_window_evidence_shape_and_timestamp_precision() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let at = DateTime::from_timestamp(1_792_540_800, 123_456_789).unwrap();
    let before = at - chrono::Duration::nanoseconds(1);
    let mut event = AuditEvent::legacy(
        "scheduling_advisory",
        serde_json::json!({"evidence": ["message_id=head"]}),
    );
    event.created_at = at;
    let events = db.audit_events();
    events.append(Some("a"), &event)?;
    assert!(events.has_recent_scheduling_advisory(Some("a"), "message_id=head", before)?);
    assert!(!events.has_recent_scheduling_advisory(Some("a"), "message_id=head", at)?);
    assert!(!events.has_recent_scheduling_advisory(Some("b"), "message_id=head", before)?);
    assert!(events.has_recent_scheduling_advisory(None, "message_id=head", before)?);
    for (kind, evidence) in [
        (
            "scheduling_advisory",
            serde_json::json!({"not_an_array": "message_id=head"}),
        ),
        ("other_kind", serde_json::json!(["message_id=head"])),
        (
            "scheduling_advisory",
            serde_json::json!(["message_id=other"]),
        ),
    ] {
        events.append(
            Some("b"),
            &AuditEvent::legacy(kind, serde_json::json!({"evidence": evidence})),
        )?;
    }
    assert!(!events.has_recent_scheduling_advisory(Some("b"), "message_id=head", before)?);
    let noise = (0..64)
        .map(|_| AuditEvent::legacy("unrelated", serde_json::json!({})))
        .collect::<Vec<_>>();
    events.append_many(Some("b"), &noise)?;
    assert!(events.has_recent_scheduling_advisory(Some("a"), "message_id=head", before)?);
    assert!(!events.has_recent_scheduling_advisory(None, "message_id=head", before)?);
    let noise = (0..64)
        .map(|_| AuditEvent::legacy("unrelated", serde_json::json!({})))
        .collect::<Vec<_>>();
    events.append_many(Some("a"), &noise)?;
    assert!(!events.has_recent_scheduling_advisory(Some("a"), "message_id=head", before)?);
    Ok(())
}
