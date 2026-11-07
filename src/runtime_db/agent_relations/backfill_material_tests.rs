use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::AgentVisibility,
};
use std::sync::Arc;

const CHILD: &str = "child-汉字\0\"\\\n{}[]:,";
const PARENT: &str = "parent-汉字\0\"\\\n";
const RUN: &str = "agent-relations-test";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "backfill-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn text_cost(text: &str) -> (u64, usize) {
    ((text.len() + std::mem::size_of::<String>()) as u64 * 8, 8)
}

// Count the decoded shape rather than reusing the production byte scanner.
fn json_cost(payload: &str) -> Result<(u64, usize)> {
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(_) => 1,
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::Object(values) => {
                2 + values.len().saturating_sub(1)
                    + values
                        .iter()
                        .map(|(_, value)| 2 + slots(value))
                        .sum::<usize>()
            }
            _ => 0,
        }
    }
    let count = slots(&serde_json::from_str(payload)?);
    Ok((
        (payload.len() as u64 * 4 + count as u64 * 128 + 512) * 8,
        (count * 4 + 16) * 8,
    ))
}

fn identity(id: &str, supervised: bool) -> AgentIdentityRecord {
    let mut identity = AgentIdentityRecord::new(
        id,
        if supervised {
            AgentKind::Child
        } else {
            AgentKind::Named
        },
        if supervised {
            AgentVisibility::Private
        } else {
            AgentVisibility::Public
        },
        if supervised {
            AgentOwnership::ParentSupervised
        } else {
            AgentOwnership::SelfOwned
        },
        if supervised {
            AgentProfilePreset::PrivateChild
        } else {
            AgentProfilePreset::PublicNamed
        },
        supervised.then(|| PARENT.into()),
        supervised.then(|| "task-汉字".into()),
    );
    let now = chrono::DateTime::from_timestamp_millis(1_760_000_000_123).unwrap();
    identity.created_at = now;
    identity.updated_at = now;
    identity
}

fn projection() -> Result<AgentCanonicalRelationsProjection> {
    let task = LegacySupervisionTaskEvidence {
        task_id: "task-汉字".into(),
        owner_agent_id: PARENT.into(),
        child_agent_id: Some(CHILD.into()),
        is_child_agent_task: true,
        delegated_from_work_item_id: Some("work-汉字".into()),
    };
    project_agent_canonical_relations(
        &identity(CHILD, true),
        AgentCanonicalRecordSet::default(),
        Some(&task),
    )
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}

fn insert_run(db: &crate::runtime_db::RuntimeDb) -> Result<()> {
    let now = timestamp(identity(CHILD, false).created_at);
    db.connection()?.execute(
        "INSERT INTO agent_relation_backfill_runs (run_id, status, started_at, updated_at)
         VALUES (?1, 'running', ?2, ?2)",
        params![RUN, now],
    )?;
    Ok(())
}

#[test]
fn backfill_axes_move_all_six_records_and_admit_only_exact_axis_slots() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let original = projection()?;
    assert!(original.issues.is_empty());
    let pointers = (
        original.lineage.as_ref().unwrap().child_agent_id.as_ptr(),
        original
            .supervision
            .as_ref()
            .unwrap()
            .supervision_id
            .as_ptr(),
        original.durability.as_ref().unwrap().agent_id.as_ptr(),
        original
            .lifecycle_attachment
            .as_ref()
            .unwrap()
            .agent_id
            .as_ptr(),
        original.capability_policy.as_ref().unwrap().rules.as_ptr(),
        original.message_policy.as_ref().unwrap().rules.as_ptr(),
    );
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let (records, axes, issues) = {
        let _active = phase.enter();
        legacy_axes_without_issues(original)?
    };
    assert!(issues.is_empty());
    assert_eq!(
        axes,
        [
            AgentCanonicalRelationAxis::Lineage,
            AgentCanonicalRelationAxis::Supervision,
            AgentCanonicalRelationAxis::Durability,
            AgentCanonicalRelationAxis::LifecycleAttachment,
            AgentCanonicalRelationAxis::CapabilityPolicy,
            AgentCanonicalRelationAxis::MessagePolicy,
        ]
    );
    assert_eq!(
        pointers,
        (
            records.lineage.as_ref().unwrap().child_agent_id.as_ptr(),
            records
                .supervision
                .as_ref()
                .unwrap()
                .supervision_id
                .as_ptr(),
            records.durability.as_ref().unwrap().agent_id.as_ptr(),
            records
                .lifecycle_attachment
                .as_ref()
                .unwrap()
                .agent_id
                .as_ptr(),
            records.capability_policy.as_ref().unwrap().rules.as_ptr(),
            records.message_policy.as_ref().unwrap().rules.as_ptr(),
        )
    );
    let slot_bytes = std::mem::size_of::<AgentCanonicalRelationAxis>() as u64 * 8;
    assert_eq!(phase.finish()?, add(header(), (slot_bytes * 6, 48)));
    drop((records, axes, issues, phase));
    for index in 0..6 {
        for records_limit in [false, true] {
            let original = projection()?;
            let limit = add(
                header(),
                (slot_bytes * (index + 1), (index as usize + 1) * 8),
            );
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if records_limit { limit.0 } else { limit.0 - 1 },
                if records_limit { limit.1 - 1 } else { limit.1 },
            )?;
            {
                let _active = phase.enter();
                let error = legacy_axes_without_issues(original).unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(phase.finish().is_err());
        }
    }
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn backfill_axis_filter_preserves_canonical_and_supervision_message_constraints() -> Result<()> {
    let mut original = projection()?;
    original.sources.lineage = Some(AgentCanonicalValueSource::Canonical);
    original.issues.push(AgentCanonicalProjectionIssue {
        axis: AgentCanonicalRelationAxis::Supervision,
        resolution: AgentCanonicalResolution::MissingEvidence,
        detail: "missing-汉字\0\"\\{}[]:,".into(),
    });
    let expected_issues = original.issues.clone();
    let (records, axes, issues) = legacy_axes_without_issues(original)?;
    assert!(records.lineage.is_none());
    assert!(records.supervision.is_none());
    assert!(records.message_policy.is_none());
    assert_eq!(
        axes,
        [
            AgentCanonicalRelationAxis::Durability,
            AgentCanonicalRelationAxis::LifecycleAttachment,
            AgentCanonicalRelationAxis::CapabilityPolicy,
        ]
    );
    assert_eq!(issues, expected_issues);
    Ok(())
}

#[test]
fn backfill_checkpoint_sql_json_exact_gates_and_same_phase_error_retry() -> Result<()> {
    let (_home, db) = fixture()?;
    db.agent_identities().upsert(&identity(CHILD, false))?;
    insert_run(&db)?;
    let axes = vec![
        AgentCanonicalRelationAxis::Durability,
        AgentCanonicalRelationAxis::MessagePolicy,
    ];
    let issues = vec![AgentCanonicalProjectionIssue {
        axis: AgentCanonicalRelationAxis::Supervision,
        resolution: AgentCanonicalResolution::MissingEvidence,
        detail: "checkpoint-汉字\0\"\\{}[]:,".into(),
    }];
    let axes_json = serde_json::to_string(&axes)?;
    let issues_json = serde_json::to_string(&issues)?;
    db.connection()?.execute(
        "INSERT INTO agent_relation_backfill_outcomes
         (run_id, agent_id, identity_revision, outcome, migrated_axes_json, issues_json, updated_at)
         VALUES (?1, ?2, 0, 'diagnostic', ?3, ?4, ?5)",
        params![
            RUN,
            CHILD,
            axes_json,
            issues_json,
            timestamp(identity(CHILD, false).created_at)
        ],
    )?;
    let steps = [
        text_cost(&axes_json),
        text_cost(&issues_json),
        json_cost(&axes_json)?,
        json_cost(&issues_json)?,
    ];
    let exact = steps.into_iter().fold(header(), add);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    {
        let _active = phase.enter();
        let actual =
            db.transaction(|tx| backfill_agent_relations_tx(tx, CHILD, true, Some(RUN)))?;
        assert_eq!(actual, (axes.clone(), issues.clone()));
    }
    assert_eq!(phase.finish()?, exact);
    drop(phase);
    let mut prefix = header();
    for step in steps {
        let end = add(prefix, step);
        for by_records in [false, true] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if by_records { end.0 } else { end.0 - 1 },
                if by_records { end.1 - 1 } else { end.1 },
            )?;
            {
                let _active = phase.enter();
                let error = db
                    .transaction(|tx| backfill_agent_relations_tx(tx, CHILD, true, Some(RUN)))
                    .unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
            }
            assert!(phase.finish().is_err());
        }
        prefix = end;
    }
    for column in ["migrated_axes_json", "issues_json"] {
        for value in ["CAST('bad' AS BLOB)", "'{'"] {
            let phase = ResourceReadMaterial::new(&ledger, &scope())?;
            {
                let _active = phase.enter();
                let error = db
                    .transaction(|tx| {
                        tx.execute(
                            &format!(
                                "UPDATE agent_relation_backfill_outcomes SET {column} = {value}"
                            ),
                            [],
                        )?;
                        backfill_agent_relations_tx(tx, CHILD, true, Some(RUN))
                    })
                    .unwrap_err();
                assert!(
                    !format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                let actual =
                    db.transaction(|tx| backfill_agent_relations_tx(tx, CHILD, true, Some(RUN)))?;
                assert_eq!(actual, (axes.clone(), issues.clone()));
            }
            assert!(phase.finish().is_ok());
        }
    }
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let resumed = {
        let _active = phase.enter();
        db.agent_canonical_relations().backfill(true, 1)?
    };
    assert_eq!(resumed.run_id.as_deref(), Some(RUN));
    assert_eq!(resumed.scanned_agents, 1);
    assert_eq!(resumed.diagnostics[0].migrated_axes, axes);
    assert_eq!(resumed.diagnostics[0].issues, issues);
    let (status, persisted): (String, String) = db.connection()?.query_row(
        "SELECT status, report_json FROM agent_relation_backfill_runs WHERE run_id = ?1",
        [RUN],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(status, "completed");
    assert_eq!(
        serde_json::to_value(&resumed)?,
        serde_json::from_str::<serde_json::Value>(&persisted)?
    );
    assert!(phase.finish().is_ok());
    drop((resumed, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn backfill_report_sample_moves_and_original_holder_survives_clone_and_serializer_error(
) -> Result<()> {
    let (_home, db) = fixture()?;
    for agent in ["a", "b", "c"] {
        db.agent_identities().upsert(&identity(agent, true))?;
    }
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let mut totals = Vec::new();
    for limit in [0, 1, 3, 100] {
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let report = {
            let _active = phase.enter();
            db.agent_canonical_relations().backfill(false, limit)?
        };
        assert_eq!(report.scanned_agents, 3);
        assert_eq!(report.diagnostic_agents, 3);
        assert_eq!(report.diagnostics.len(), limit.min(3));
        for (diagnostic, agent) in report.diagnostics.iter().zip(["a", "b", "c"]) {
            assert_eq!(diagnostic.agent_id, agent);
            let expected = project_agent_canonical_relations(
                &identity(agent, true),
                AgentCanonicalRecordSet::default(),
                None,
            )?;
            let (_, axes, issues) = legacy_axes_without_issues(expected)?;
            assert_eq!(diagnostic.migrated_axes, axes);
            assert_eq!(diagnostic.issues, issues);
        }
        totals.push(phase.finish()?);
        let raw = serde_json::to_value(&report.report)?;
        assert_eq!(serde_json::to_value(&report)?, raw);
        drop(phase);
        let before = ledger.report()?.observed_bytes;
        assert!(before > 0);
        let cloned = report.clone();
        assert_eq!(ledger.report()?.observed_bytes, before * 2);
        let last_holder = Arc::clone(cloned._material.as_ref().unwrap());
        drop(report);
        assert_eq!(ledger.report()?.observed_bytes, before);
        struct Reject;
        impl std::io::Write for Reject {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("backfill serializer rejection"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let error = serde_json::to_writer(Reject, &cloned).unwrap_err();
        assert!(error.to_string().contains("backfill serializer rejection"));
        assert_eq!(ledger.report()?.observed_bytes, before);
        drop(cloned);
        assert_eq!(ledger.report()?.observed_bytes, before);
        drop(last_holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let slot = (
        std::mem::size_of::<AgentRelationBackfillDiagnostic>() as u64 * 8,
        8,
    );
    assert_eq!(totals[1], add(totals[0], slot));
    assert_eq!(totals[2], add(add(totals[1], slot), slot));
    assert_eq!(totals[2], totals[3]);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let ordinary = db.agent_canonical_relations().backfill(false, 1)?;
    assert!(ordinary._material.is_none());
    Ok(())
}

#[test]
fn backfill_list_and_sample_denial_release_without_escaping_or_mutating_sql() -> Result<()> {
    let (_home, db) = fixture()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let empty_phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let empty = {
        let _active = empty_phase.enter();
        db.agent_canonical_relations().backfill(false, 1)?
    };
    assert_eq!(empty.scanned_agents, 0);
    assert_eq!(empty_phase.finish()?, header());
    drop((empty, empty_phase));
    db.agent_identities().upsert(&identity(CHILD, true))?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let full = {
        let _active = phase.enter();
        db.agent_canonical_relations().backfill(false, 1)?
    };
    assert_eq!(full.diagnostics.len(), 1);
    let total = phase.finish()?;
    drop((full, phase));
    let id_slot = (std::mem::size_of::<String>() as u64 * 8, 8);
    for limit in [
        add(header(), id_slot),
        add(add(header(), id_slot), text_cost(CHILD)),
        total,
    ] {
        for by_records in [false, true] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                limit.0 - u64::from(!by_records),
                limit.1 - usize::from(by_records),
            )?;
            {
                let _active = phase.enter();
                let error = db
                    .agent_canonical_relations()
                    .backfill(false, 1)
                    .unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    for table in [
        "agent_lineages",
        "agent_relation_backfill_runs",
        "agent_relation_backfill_outcomes",
    ] {
        assert_eq!(
            db.connection()?
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0),)?,
            0
        );
    }
    Ok(())
}

fn assert_string_gate(
    expected: &str,
    cost: (u64, usize),
    operation: impl Fn() -> Result<String>,
) -> Result<()> {
    assert_eq!(operation()?, expected);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let prior = (13 * 8, 3 * 8);
    let exact = add(add(header(), prior), cost);
    for denial in [None, Some(false), Some(true)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            exact.0 - u64::from(denial == Some(false)),
            exact.1 - usize::from(denial == Some(true)),
        )?;
        {
            let _active = phase.enter();
            charge_owned_metadata(13, 3)?;
            if denial.is_some() {
                let error = operation().unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
            } else {
                assert_eq!(operation()?, expected);
            }
        }
        if denial.is_some() {
            assert!(phase.finish().is_err());
        } else {
            assert_eq!(phase.finish()?, exact);
        }
    }
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn backfill_writer_json_enum_sql_and_signed_timestamp_admit_before_allocation() -> Result<()> {
    let value = serde_json::json!({"body": [CHILD, {}, [], {"escape": "\"{}[]:,\\\0"}]});
    let serialized = serde_json::to_string(&value)?;
    assert_string_gate(&serialized, json_cost(&serialized)?, || {
        relation_json(&value)
    })?;
    let outcome = AgentRelationBackfillOutcome::Diagnostic;
    let serialized = serde_json::to_string(&outcome)?;
    assert_string_gate("diagnostic", json_cost(&serialized)?, || {
        enum_string(&outcome)
    })?;
    let expected = format!("INSERT-汉字:{CHILD}:{}", 123);
    assert_string_gate(&expected, text_cost(&expected), || {
        relation_formatted_text(format_args!("INSERT-汉字:{CHILD}:{}", 123))
    })?;
    let check_timestamp = |value| {
        let expected = timestamp(value);
        assert_string_gate(&expected, text_cost(&expected), || {
            relation_timestamp(value)
        })
    };
    for raw in [
        "0000-01-01T00:00:00Z",
        "9999-12-31T23:59:59.999999999Z",
        "2016-12-31T23:59:60.123456789Z",
    ] {
        check_timestamp(chrono::DateTime::parse_from_rfc3339(raw)?.with_timezone(&Utc))?;
    }
    // RFC 3339 parsing accepts only four-digit years, unlike Chrono formatting.
    for (year, nanos) in [(10_000, 0), (262_000, 0), (-1, 123_456_789), (-10_000, 0)] {
        let value = chrono::NaiveDate::from_ymd_opt(year, 1, 1)
            .unwrap()
            .and_hms_nano_opt(0, 0, 0, nanos)
            .unwrap()
            .and_utc();
        check_timestamp(value)?;
    }
    Ok(())
}

#[test]
fn backfill_apply_rejection_rolls_back_prior_axis_and_sql_error_retries_same_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    db.agent_identities().upsert(&identity(CHILD, true))?;
    db.agent_identities().upsert(&identity(PARENT, false))?;
    let (records, _, _) = legacy_axes_without_issues(projection()?)?;
    let lineage = records.lineage.as_ref().unwrap();
    let first = [
        json_cost(&serde_json::to_string(lineage)?)?,
        json_cost(&serde_json::to_string(&lineage.creation_cause)?)?,
        text_cost(&timestamp(lineage.created_at)),
    ]
    .into_iter()
    .fold(header(), add);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    for by_records in [false, true] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            if by_records {
                64 * 1024 * 1024
            } else {
                first.0
            },
            if by_records { first.1 } else { 65_536 },
        )?;
        {
            let _active = phase.enter();
            let error = db
                .transaction(|tx| upsert_canonical_record_set_tx(tx, &records))
                .unwrap_err();
            assert!(format!("{error:#}").contains("ResourceExhausted"));
        }
        assert!(phase.finish().is_err());
        for table in [
            "agent_lineages",
            "agent_supervisions",
            "agent_durability_records",
        ] {
            let count: i64 = db.connection()?.query_row(
                &format!("SELECT COUNT(*) FROM {table}"),
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0);
        }
    }
    assert_eq!(ledger.report()?.observed_bytes, 0);
    // Ordinary SQL errors do not poison or replace the caller's phase.
    let (_sql_home, db) = fixture()?;
    db.agent_identities().upsert(&identity("worker", false))?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER backfill_fail BEFORE INSERT ON agent_lifecycle_attachment_records
         BEGIN SELECT RAISE(ABORT, 'backfill_sql_failure'); END;",
    )?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let applied = {
        let _active = phase.enter();
        let error = db
            .agent_canonical_relations()
            .backfill(true, 5)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("backfill_sql_failure"),
            "{error:#}"
        );
        let connection = db.connection()?;
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM agent_durability_records", [], |row| {
                row.get::<_, i64>(0)
            },)?,
            0
        );
        let (status, report): (String, String) = connection.query_row(
            "SELECT status, report_json FROM agent_relation_backfill_runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(status, "failed");
        assert_eq!(
            serde_json::from_str::<AgentRelationBackfillReport>(&report)?.scanned_agents,
            0
        );
        connection.execute_batch("DROP TRIGGER backfill_fail")?;
        drop(connection);
        db.agent_canonical_relations().backfill(true, 5)?
    };
    assert_eq!(applied.scanned_agents, 1);
    assert!(phase.finish().is_ok());
    drop(phase);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(applied);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
