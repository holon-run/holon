use super::*;

fn advisory_window_start() -> DateTime<Utc> {
    DateTime::from_timestamp(1_792_540_800, 0).expect("fixed advisory fixture timestamp is valid")
}

fn host_advisory(message_id: &str, created_at: DateTime<Utc>) -> AuditEvent {
    let mut event = AuditEvent::legacy(
        "scheduling_advisory",
        serde_json::json!({"evidence": [format!("message_id={message_id}")]}),
    );
    event.created_at = created_at;
    event
}

fn explain_plan(
    db: &crate::runtime_db::RuntimeDb,
    sql: &str,
    bind_count: usize,
) -> Result<Vec<String>> {
    db.transaction(|tx| {
        let mut statement = tx.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let binds = vec!["message_id=plan"; bind_count];
        let mut rows = statement.query(params_from_iter(binds))?;
        let mut plan = Vec::new();
        while let Some(row) = rows.next()? {
            plan.push(row.get::<_, String>(3)?);
        }
        Ok(plan)
    })
}

fn insert_raw_event(
    db: &crate::runtime_db::RuntimeDb,
    event_seq: i64,
    agent_id: Option<&str>,
    kind: &str,
    data_json: &str,
) -> Result<()> {
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO audit_events (audit_event_id, event_seq, agent_id, kind, created_at, data_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                format!("fixture-{event_seq}-{}", agent_id.unwrap_or("host")),
                event_seq,
                agent_id,
                kind,
                advisory_window_start().to_rfc3339(),
                data_json,
            ],
        )?;
        Ok(())
    })
}

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
    // Host scope never matches agent-scoped events (#4491).
    assert!(!events.has_recent_scheduling_advisory(None, "message_id=head", before)?);
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

#[test]
fn advisory_host_scope_dedup_is_host_wide_and_ignores_agent_events() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let at = DateTime::from_timestamp(1_792_540_800, 0).unwrap();
    let before = at - chrono::Duration::nanoseconds(1);
    let events = db.audit_events();
    events.append(Some("agent-a"), &host_advisory("head", at))?;
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=head", before)?,
        "an agent-scoped advisory must not rate-limit the host-wide advisory"
    );
    events.append(None, &host_advisory("head", at))?;
    assert!(
        events.has_recent_scheduling_advisory(None, "message_id=head", before)?,
        "a host advisory inside the window de-duplicates"
    );
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=head", at)?,
        "an advisory older than the window no longer de-duplicates"
    );
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=other", before)?,
        "different queue heads must not suppress each other"
    );
    Ok(())
}

#[test]
fn advisory_host_scope_candidates_are_bounded_to_recent_host_events() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let at = DateTime::from_timestamp(1_792_540_800, 0).unwrap();
    let before = at - chrono::Duration::nanoseconds(1);
    let events = db.audit_events();
    events.append(None, &host_advisory("head", at))?;
    let noise = (0..63)
        .map(|_| AuditEvent::legacy("unrelated", serde_json::json!({})))
        .collect::<Vec<_>>();
    events.append_many(None, &noise)?;
    assert!(
        events.has_recent_scheduling_advisory(None, "message_id=head", before)?,
        "the advisory is still inside the 64-candidate host window"
    );
    events.append_many(
        None,
        &[AuditEvent::legacy("unrelated", serde_json::json!({}))],
    )?;
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=head", before)?,
        "only the 64 most recent host candidates are inspected"
    );
    Ok(())
}

#[test]
fn advisory_candidate_sql_orders_by_event_seq_without_temp_btree() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    for (sql, bind_count) in [
        (SCHEDULING_ADVISORY_HOST_CANDIDATES_SQL, 1),
        (SCHEDULING_ADVISORY_AGENT_CANDIDATES_SQL, 2),
    ] {
        let plan = explain_plan(&db, sql, bind_count)?;
        let joined = plan.join("\n");
        assert!(
            !joined.contains("TEMP B-TREE"),
            "candidate read must not sort: {joined}"
        );
        assert!(
            joined.contains("USING INDEX"),
            "candidate read must use an event_seq index: {joined}"
        );
        let audit_lines = plan
            .iter()
            .filter(|line| line.contains("audit_events"))
            .collect::<Vec<_>>();
        assert!(
            !audit_lines.is_empty() && audit_lines.iter().all(|line| line.contains("USING INDEX")),
            "candidate read must reach audit_events through an index: {joined}"
        );
    }
    let host = explain_plan(&db, SCHEDULING_ADVISORY_HOST_CANDIDATES_SQL, 1)?.join("\n");
    assert!(
        host.contains("idx_audit_events_host_event_seq_unique")
            || host.contains("idx_audit_events_agent_seq_created"),
        "host candidate read must use a host event_seq ordering index: {host}"
    );
    Ok(())
}

#[test]
fn advisory_host_scope_dedup_stays_bounded_on_million_row_audit_log() -> Result<()> {
    const LAST_NOISE_SEQ: i64 = 999_900;
    // Reserved for the stale advisory below so the dense noise fixture cannot
    // already occupy this host-scoped sequence.
    const STALE_ADVISORY_SEQ: i64 = 999_400;
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        tx.execute_batch(&format!(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < {LAST_NOISE_SEQ})
             INSERT INTO audit_events (audit_event_id, event_seq, agent_id, kind, created_at, data_json)
             SELECT 'noise-' || n,
                    n,
                    CASE WHEN n % 4 = 0 THEN NULL ELSE 'agent-' || (n % 11) END,
                    'unrelated',
                    '2026-11-20T00:00:00Z',
                    '{{}}'
             FROM seq WHERE n <> {STALE_ADVISORY_SEQ}"
        ))?;
        Ok(())
    })?;
    let at = advisory_window_start();
    let before = at - chrono::Duration::nanoseconds(1);
    let events = db.audit_events();
    insert_raw_event(
        &db,
        999_995,
        None,
        "scheduling_advisory",
        &serde_json::to_string(&host_advisory("head", at))?,
    )?;
    insert_raw_event(
        &db,
        999_994,
        None,
        "scheduling_advisory",
        &serde_json::to_string(&host_advisory("other", at))?,
    )?;
    insert_raw_event(
        &db,
        999_993,
        Some("agent-b"),
        "scheduling_advisory",
        &serde_json::to_string(&host_advisory("agentonly", at))?,
    )?;
    insert_raw_event(
        &db,
        999_992,
        None,
        "scheduling_advisory",
        &format!(
            "{{\"created_at\":\"{}\",\"data\":{{\"evidence\":\"message_id=legacy\"}}}}",
            at.to_rfc3339()
        ),
    )?;
    assert!(
        events.has_recent_scheduling_advisory(None, "message_id=head", before)?,
        "a recent host advisory still de-duplicates inside a million-row audit log"
    );
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=head", at)?,
        "the advisory window boundary is preserved"
    );
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=missing", before)?,
        "an absent queue head does not match unrelated advisories"
    );
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=agentonly", before)?,
        "agent-scoped advisories never match the host path"
    );
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=legacy", before)?,
        "legacy non-array evidence stays filtered"
    );
    insert_raw_event(
        &db,
        STALE_ADVISORY_SEQ,
        None,
        "scheduling_advisory",
        &serde_json::to_string(&host_advisory("stale", at))?,
    )?;
    assert!(
        !events.has_recent_scheduling_advisory(None, "message_id=stale", before)?,
        "candidates older than the 64-event bound are not decoded"
    );
    Ok(())
}
