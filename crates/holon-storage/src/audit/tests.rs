use super::*;
use chrono::TimeZone;
use rusqlite::params;

use crate::{migrations::current_schema_version, tests::migrated_connection};

fn audit_connection(
    connection: &Connection,
    options: RuntimeDbAuditOptions,
) -> Result<RuntimeDbAuditReport> {
    audit(connection, options, |connection| {
        Ok(RuntimeDbAuditDatabase {
            path: ":memory:".into(),
            schema_version: current_schema_version(connection)?,
            runtime_id: "runtime-test".into(),
            event_log_epoch: "epoch-test".into(),
        })
    })
}

fn insert_event(
    connection: &Connection,
    id: &str,
    seq: i64,
    agent_id: &str,
    kind: &str,
    created_at: &str,
    data_json: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO audit_events (
           audit_event_id, event_seq, agent_id, kind, created_at, data_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, seq, agent_id, kind, created_at, data_json],
    )?;
    Ok(())
}

fn insert_linked_brief(
    connection: &Connection,
    agent_id: &str,
    brief_id: &str,
    event_id: &str,
    event_seq: i64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO briefs (
           evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
         ) VALUES (
           ?1, ?2, '2026-01-01T00:00:00.000Z', 'result', 'bounded preview', '{}', ?3
         )",
        params![brief_id, agent_id, event_seq],
    )?;
    insert_event(
        connection,
        event_id,
        event_seq,
        agent_id,
        "brief_created",
        "2026-01-01T00:00:00.000Z",
        &serde_json::json!({
            "data": {"agent_id": agent_id, "brief_id": brief_id}
        })
        .to_string(),
    )
}

#[test]
fn projection_inventory_splits_mixed_legacy_and_typed_events_at_baseline() -> Result<()> {
    let connection = migrated_connection()?;
    insert_event(
        &connection,
        "event_legacy",
        1,
        "alpha",
        "message_enqueued",
        "2026-01-01T00:00:00.000Z",
        r#"{"data":{}}"#,
    )?;
    insert_event(
        &connection,
        "event_exact",
        2,
        "alpha",
        "brief_created",
        "2026-01-02T00:00:00.000Z",
        r#"{
          "contract_version":3,
          "payload_schema":"holon.runtime_event.brief_created",
          "payload_schema_version":1,
          "data":{"agent_id":"alpha","brief_id":"brief_exact"}
        }"#,
    )?;
    insert_event(
        &connection,
        "event_old_unsupported",
        3,
        "alpha",
        "future_kind",
        "2026-01-03T00:00:00.000Z",
        r#"{
          "contract_version":3,
          "payload_schema":"holon.runtime_event.future",
          "payload_schema_version":1,
          "data":{}
        }"#,
    )?;
    insert_event(
        &connection,
        "event_new_unsupported",
        4,
        "alpha",
        "brief_created",
        "2026-01-05T00:00:00.000Z",
        r#"{
          "contract_version":3,
          "payload_schema":"holon.runtime_event.brief_created",
          "payload_schema_version":2,
          "data":{"agent_id":"alpha","brief_id":"brief_future"}
        }"#,
    )?;

    let report = audit_connection(
        &connection,
        RuntimeDbAuditOptions {
            check: RuntimeDbAuditCheck::ProjectionEffects,
            baseline_through: Some(
                Utc.with_ymd_and_hms(2026, 1, 4, 0, 0, 0)
                    .single()
                    .expect("valid baseline"),
            ),
            sample_limit: 1,
        },
    )?;
    let audit = report
        .projection_effects
        .as_ref()
        .expect("projection audit");

    assert_eq!(audit.groups.len(), 4);
    assert_eq!(audit.totals.historical_baseline, 1);
    assert_eq!(audit.totals.new_violation, 1);
    assert!(report.has_new_violations());
    assert!(audit.groups.iter().any(|group| {
        group.classification == "conservative_legacy" && group.sample_event_ids == ["event_legacy"]
    }));
    assert!(audit.groups.iter().any(|group| {
        group.classification == "exact"
            && group.projection_effect == Some("display_invalidation")
            && group.sample_event_ids == ["event_exact"]
    }));
    assert!(audit.groups.iter().any(|group| {
        group.unsupported_reason == Some("unknown_typed_kind")
            && group.historical_baseline == 1
            && group.new_violation == 0
    }));
    assert!(audit.groups.iter().any(|group| {
        group.unsupported_reason == Some("future_payload_schema_version")
            && group.historical_baseline == 0
            && group.new_violation == 1
    }));
    Ok(())
}

#[test]
fn brief_integrity_reports_categories_a_through_e_without_content() -> Result<()> {
    let connection = migrated_connection()?;
    for (turn_id, created_at) in [
        ("turn_a", "2026-01-01T00:00:00.000Z"),
        ("turn_b", "2026-01-03T00:00:00.000Z"),
    ] {
        connection.execute(
            "INSERT INTO messages (
               evidence_id, agent_id, turn_id, created_at, kind, payload_json
             ) VALUES (?1, 'alpha', ?2, ?3, 'operator_prompt',
               '{\"authority_class\":\"operator_instruction\"}')",
            params![format!("message_{turn_id}"), turn_id, created_at],
        )?;
        connection.execute(
            "INSERT INTO turn_records (
               turn_id, turn_index, agent_id, trigger_message_id,
               terminal_kind, created_at, completed_at, payload_json
             ) VALUES (?1, ?2, 'alpha', ?3, 'completed', ?4, ?4, '{}')",
            params![
                turn_id,
                if turn_id == "turn_a" { 1 } else { 2 },
                format!("message_{turn_id}"),
                created_at
            ],
        )?;
    }
    connection.execute(
        "INSERT INTO transcript_entries (
           evidence_id, agent_id, turn_id, created_at, kind, payload_json
         ) VALUES (
           'transcript_b', 'alpha', 'turn_b', '2026-01-03T00:00:00.000Z',
           'assistant_round',
           '{\"data\":{\"visibility\":\"operator_visible\",\"blocks\":[{\"type\":\"text\",\"text\":\"delivered\"}]}}'
         )",
        [],
    )?;
    connection.execute(
        "INSERT INTO briefs (
           evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
         ) VALUES (
           'brief_c', 'alpha', '2026-01-01T00:00:00.000Z',
           'result', 'bounded preview', '{}', NULL
         )",
        [],
    )?;
    connection.execute(
        "INSERT INTO briefs (
           evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
         ) VALUES (
           'brief_d', 'alpha', '2026-01-03T00:00:00.000Z',
           'result', 'bounded preview', '{}', 5
         )",
        [],
    )?;
    insert_event(
        &connection,
        "event_floor",
        10,
        "alpha",
        "message_enqueued",
        "2026-01-03T00:00:00.000Z",
        r#"{"data":{}}"#,
    )?;

    let report = audit_connection(
        &connection,
        RuntimeDbAuditOptions {
            check: RuntimeDbAuditCheck::BriefIntegrity,
            baseline_through: Some(
                Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0)
                    .single()
                    .expect("valid baseline"),
            ),
            sample_limit: 2,
        },
    )?;
    let audit = report.brief_integrity.as_ref().expect("brief audit");
    let agent = audit
        .agents
        .iter()
        .find(|agent| agent.agent_id == "alpha")
        .unwrap();
    let category = |name| {
        agent
            .categories
            .iter()
            .find(|category| category.category == name)
            .unwrap()
    };

    assert_eq!(agent.retained_event_floor, Some(10));
    assert_eq!(agent.event_head, Some(10));
    assert_eq!(agent.counts.operator_messages, 2);
    assert_eq!(agent.counts.terminal_turns, 2);
    assert_eq!(agent.counts.operator_visible_assistant_deliveries, 1);
    assert_eq!(agent.counts.canonical_briefs, 2);
    assert_eq!(agent.counts.missing_or_ambiguous_linkage, 2);
    assert_eq!(category("A").historical_baseline, 1);
    assert_eq!(category("A").sample_ids, ["turn_a"]);
    assert_eq!(category("B").new_violation, 1);
    assert_eq!(category("B").sample_ids, ["turn_b"]);
    assert_eq!(category("C").historical_baseline, 1);
    assert_eq!(category("C").sample_ids, ["brief_c"]);
    assert_eq!(category("D").new_violation, 1);
    assert_eq!(category("D").sample_ids, ["brief_d"]);
    assert!(!category("E").observable_offline);
    assert!(category("E").sample_ids.is_empty());
    assert!(report.has_new_violations());
    Ok(())
}

#[test]
fn brief_integrity_scopes_shared_sequences_by_agent() -> Result<()> {
    let connection = migrated_connection()?;
    insert_linked_brief(&connection, "alpha", "brief_alpha", "event_alpha", 1)?;
    insert_linked_brief(&connection, "beta", "brief_beta", "event_beta", 1)?;
    connection.execute_batch(
        "DROP INDEX briefs_agent_created_event_seq;
         DROP INDEX idx_audit_events_agent_event_seq_unique;",
    )?;
    insert_linked_brief(
        &connection,
        "gamma",
        "brief_gamma_one",
        "event_gamma_one",
        1,
    )?;
    insert_linked_brief(
        &connection,
        "gamma",
        "brief_gamma_two",
        "event_gamma_two",
        1,
    )?;

    let report = audit_connection(
        &connection,
        RuntimeDbAuditOptions {
            check: RuntimeDbAuditCheck::BriefIntegrity,
            baseline_through: None,
            sample_limit: 10,
        },
    )?;
    let audit = report.brief_integrity.as_ref().expect("brief audit");
    for agent_id in ["alpha", "beta"] {
        let agent = audit
            .agents
            .iter()
            .find(|agent| agent.agent_id == agent_id)
            .expect("agent audit");
        assert_eq!(agent.counts.valid_linked_briefs, 1);
        assert_eq!(agent.counts.missing_or_ambiguous_linkage, 0);
        assert_eq!(
            agent
                .categories
                .iter()
                .find(|category| category.category == "C")
                .expect("category C")
                .new_violation,
            0
        );
    }
    let gamma = audit
        .agents
        .iter()
        .find(|agent| agent.agent_id == "gamma")
        .expect("gamma audit");
    assert_eq!(gamma.counts.valid_linked_briefs, 2);
    assert_eq!(gamma.counts.missing_or_ambiguous_linkage, 2);
    let category_c = gamma
        .categories
        .iter()
        .find(|category| category.category == "C")
        .expect("category C");
    assert_eq!(category_c.new_violation, 2);
    assert_eq!(
        category_c.sample_ids,
        ["brief_gamma_one", "brief_gamma_two"]
    );
    Ok(())
}

#[test]
fn projection_inventory_aggregates_large_event_sets_with_bounded_samples() -> Result<()> {
    let mut connection = migrated_connection()?;
    let transaction = connection.transaction()?;
    for index in 0..5_000 {
        transaction.execute(
            "INSERT INTO audit_events (
               audit_event_id, event_seq, agent_id, kind, created_at, data_json
             ) VALUES (?1, ?2, 'scale', 'message_enqueued',
               '2026-01-01T00:00:00.000Z', ?3)",
            params![
                format!("event_{index:05}"),
                index + 1,
                r#"{
                  "contract_version":3,
                  "payload_schema":"holon.runtime_event.message_lifecycle",
                  "payload_schema_version":1,
                  "data":{}
                }"#
            ],
        )?;
    }
    transaction.commit()?;

    let report = audit_connection(
        &connection,
        RuntimeDbAuditOptions {
            check: RuntimeDbAuditCheck::ProjectionEffects,
            baseline_through: None,
            sample_limit: 3,
        },
    )?;
    let groups = report.projection_effects.expect("projection audit").groups;

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].count, 5_000);
    assert_eq!(groups[0].new_violation, 5_000);
    assert_eq!(groups[0].sample_event_ids.len(), 3);
    assert_eq!(
        groups[0].sample_event_ids,
        ["event_00000", "event_00001", "event_00002"]
    );
    Ok(())
}
