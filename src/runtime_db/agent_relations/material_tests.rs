use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

const AGENT: &str = "relation-汉字";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "relation-material".into(),
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
    (((text.len() + std::mem::size_of::<String>()) as u64) * 8, 8)
}

fn json_cost(payload: &str) -> (u64, usize) {
    let mut quoted = false;
    let mut escaped = false;
    let mut slots = 0;
    for byte in payload.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            slots += usize::from(matches!(
                byte,
                b'"' | b'{' | b'}' | b'[' | b']' | b':' | b','
            ));
            quoted = byte == b'"';
        }
    }
    (
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    for id in [AGENT, "parent"] {
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            id,
            AgentKind::Named,
            crate::types::AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
    }
    let now = Utc::now();
    let repository = db.agent_canonical_relations();
    repository.upsert_lineage(&AgentLineageRecord {
        child_agent_id: AGENT.into(),
        parent_agent_id: "parent".into(),
        creation_cause: AgentLineageCreationCause::Migration,
        revision: 1,
        created_at: now,
    })?;
    repository.upsert_supervision(&AgentSupervisionRecord {
        supervision_id: "supervision-汉字".into(),
        supervisor_agent_id: "parent".into(),
        child_agent_id: AGENT.into(),
        delegated_from_work_item_id: Some("work-汉字\0\"\\\n".into()),
        delegated_from_task_id: Some("task-汉字".into()),
        state: AgentSupervisionState::Closed,
        revision: 1,
        created_at: now,
        updated_at: now,
    })?;
    repository.upsert_durability(&AgentDurabilityRecord {
        agent_id: AGENT.into(),
        durability: AgentCanonicalDurability::Persistent,
        revision: 1,
        created_at: now,
    })?;
    repository.upsert_lifecycle_attachment(&AgentLifecycleAttachmentRecord {
        agent_id: AGENT.into(),
        attachment: AgentLifecycleAttachment::Independent,
        revision: 1,
        created_at: now,
    })?;
    repository.upsert_capability_policy(&AgentCapabilityPolicyRecord {
        agent_id: AGENT.into(),
        revision: 1,
        rules: [AgentPolicyEffect::Allow, AgentPolicyEffect::Deny]
            .into_iter()
            .map(|effect| AgentCapabilityPolicyRule {
                family: AgentCapabilityFamily::CoreAgent,
                effect,
            })
            .collect(),
        created_at: now,
    })?;
    repository.upsert_message_policy(&AgentMessagePolicyRecord {
        agent_id: AGENT.into(),
        revision: 1,
        default_effect: AgentPolicyEffect::Deny,
        rules: vec![
            AgentMessagePolicyRule {
                principal_kind: AgentMessagePrincipalKind::SupervisingParent,
                principal_id: Some("parent-汉字\0\"\\\n".into()),
                route: Some("route-汉字\0\"\\\n".into()),
                effect: AgentPolicyEffect::Allow,
            },
            AgentMessagePolicyRule {
                principal_kind: AgentMessagePrincipalKind::SupervisingParent,
                principal_id: None,
                route: None,
                effect: AgentPolicyEffect::Deny,
            },
        ],
        created_at: now,
    })?;
    Ok((home, db))
}

fn read_axis(
    connection: &rusqlite::Connection,
    axis: usize,
    id: &str,
) -> Result<AgentCanonicalRecordSet> {
    let mut records = AgentCanonicalRecordSet::default();
    match axis {
        0 => records.lineage = latest_lineage(connection, id)?,
        1 => records.supervision = latest_supervision(connection, id)?,
        2 => records.durability = latest_durability(connection, id)?,
        3 => records.lifecycle_attachment = latest_lifecycle_attachment(connection, id)?,
        4 => records.capability_policy = latest_capability_policy(connection, id)?,
        5 => records.message_policy = latest_message_policy(connection, id)?,
        _ => unreachable!(),
    }
    Ok(records)
}

struct Step {
    cost: (u64, usize),
    column: Option<(String, String)>,
}

// Independent SQL source lengths in allocation order, not the admission helpers.
fn steps(connection: &rusqlite::Connection, axis: usize) -> Result<Vec<Step>> {
    let (table, fields, key) = match axis {
        0 => ("agent_lineages", "parent_agent_id,creation_cause,created_at", "child_agent_id"),
        1 => (
            "agent_supervisions",
            "supervision_id,supervisor_agent_id,delegated_from_work_item_id,delegated_from_task_id,state,created_at,updated_at",
            "child_agent_id",
        ),
        2 => ("agent_durability_records", "durability,created_at", "agent_id"),
        3 => ("agent_lifecycle_attachment_records", "attachment,created_at", "agent_id"),
        4 => ("agent_capability_policy_records", "created_at", "agent_id"),
        5 => ("agent_message_policy_records", "default_effect,created_at", "agent_id"),
        _ => unreachable!(),
    };
    let mut out = Vec::new();
    let mut append = |table: &str, fields: &str, key: &str, slot: Option<usize>| -> Result<()> {
        let mut stmt = connection.prepare(&format!(
            "SELECT {fields} FROM {table} WHERE {key} = ?1 {}",
            if slot.is_some() {
                "ORDER BY rule_index"
            } else {
                ""
            },
        ))?;
        let mut rows = stmt.query([AGENT])?;
        while let Some(row) = rows.next()? {
            for (index, column) in fields.split(',').enumerate() {
                if let Some(text) = row.get::<_, Option<String>>(index)? {
                    out.push(Step {
                        cost: text_cost(&text),
                        column: Some((table.into(), column.into())),
                    });
                }
            }
            if let Some(bytes) = slot {
                out.push(Step {
                    cost: (bytes as u64 * 8, 8),
                    column: None,
                });
            }
        }
        Ok(())
    };
    append(table, fields, key, None)?;
    if axis == 4 {
        append(
            "agent_capability_policy_rules",
            "capability_family,effect",
            "agent_id",
            Some(std::mem::size_of::<AgentCapabilityPolicyRule>()),
        )?;
    } else if axis == 5 {
        append(
            "agent_message_policy_rules",
            "principal_kind,principal_id,route,effect",
            "agent_id",
            Some(std::mem::size_of::<AgentMessagePolicyRule>()),
        )?;
    }
    out.push(Step {
        cost: text_cost(AGENT),
        column: None,
    });
    Ok(out)
}

#[test]
fn typed_relations_exact_cost_and_every_text_id_slot_denial() -> Result<()> {
    let (_home, db) = fixture()?;
    let connection = db.connection()?;
    let used = (17 * 8, 2 * 8);
    for axis in 0..6 {
        let baseline = read_axis(&connection, axis, AGENT)?;
        let steps = steps(&connection, axis)?;
        let exact = steps
            .iter()
            .fold(add(header(), used), |sum, step| add(sum, step.cost));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(17, 2)?;
            assert_eq!(read_axis(&connection, axis, AGENT)?, baseline);
        }
        assert_eq!(phase.finish()?, exact, "axis {axis}");
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let mut before = add(header(), used);
        for (index, step) in steps.iter().enumerate() {
            let next = add(before, step.cost);
            for bytes in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    next.0 - u64::from(bytes),
                    next.1 - usize::from(!bytes),
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    charge_owned_metadata(17, 2)?;
                    let error = read_axis(&connection, axis, AGENT).unwrap_err();
                    assert!(
                        format!("{error:#}").contains("ResourceExhausted"),
                        "{axis}/{index}: {error:#}"
                    );
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
                assert!(phase.finish().is_err());
                assert_eq!(ledger.report()?.observed_bytes, before.0, "{axis}/{index}");
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, before.0);
                drop(holder);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
            before = next;
        }
    }
    Ok(())
}

#[test]
fn typed_relations_sql_types_rollback_and_retry_on_original_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let mut expected = header();
    let mut corruptions = 0;
    assert_eq!(
        connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?,
        1
    );
    connection.execute_batch("PRAGMA ignore_check_constraints = ON")?;
    for axis in 0..6 {
        let baseline = read_axis(&connection, axis, AGENT)?;
        let ordered = steps(&connection, axis)?;
        let total = ordered.iter().fold((0, 0), |sum, step| add(sum, step.cost));
        let mut seen = std::collections::HashSet::new();
        for (index, step) in ordered.iter().enumerate() {
            let Some((table, column)) = step.column.as_ref() else {
                continue;
            };
            if !seen.insert((table, column)) {
                continue;
            }
            let _entry = phase.enter();
            let tx = connection.transaction()?;
            // Corrupt FK-backed TEXT only inside this rolled-back transaction.
            tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            tx.execute(&format!("UPDATE {table} SET {column} = x'ff'"), [])?;
            assert!(read_axis(&tx, axis, AGENT).is_err(), "{table}.{column}");
            corruptions += 1;
            expected = ordered[..index]
                .iter()
                .fold(expected, |sum, step| add(sum, step.cost));
            assert_eq!(phase.finish()?, expected, "{table}.{column}");
            tx.rollback()?;
            assert_eq!(
                connection
                    .query_row("PRAGMA defer_foreign_keys", [], |row| row.get::<_, i64>(0))?,
                0
            );
            assert_eq!(read_axis(&connection, axis, AGENT)?, baseline);
            expected = add(expected, total);
            assert_eq!(phase.finish()?, expected);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
    }
    connection.execute_batch("PRAGMA ignore_check_constraints = OFF")?;
    assert_eq!(corruptions, 23);
    assert_eq!(
        connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?,
        1
    );
    assert!(connection.is_autocommit());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, expected.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn typed_relations_missing_rows_nulls_and_empty_rules_have_no_phantom_cost() -> Result<()> {
    let (_home, db) = fixture()?;
    let connection = db.connection()?;
    connection.execute_batch(
        "UPDATE agent_supervisions SET delegated_from_work_item_id = NULL, delegated_from_task_id = NULL;
         DELETE FROM agent_capability_policy_rules;
         DELETE FROM agent_message_policy_rules;",
    )?;
    for axis in 0..6 {
        let baseline = read_axis(&connection, axis, AGENT)?;
        let exact = steps(&connection, axis)?
            .iter()
            .fold(header(), |sum, step| add(sum, step.cost));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _entry = phase.enter();
            assert_eq!(
                read_axis(&connection, axis, "missing")?,
                AgentCanonicalRecordSet::default()
            );
            assert_eq!(phase.finish()?, header());
            assert_eq!(read_axis(&connection, axis, AGENT)?, baseline);
        }
        assert_eq!(phase.finish()?, exact);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn typed_relations_raw_snapshot_retains_original_holder_through_owning_clone() -> Result<()> {
    let (_home, db) = fixture()?;
    let baseline = db.agent_projection_snapshot_rows(AGENT)?;
    assert!(baseline._material.is_none());
    let issues = &baseline.row.as_ref().unwrap().canonical_relations.issues;
    let details = [
        "canonical supervision differs from legacy ownership, parent, or task evidence",
        "supervision state Closed conflicts with identity lifecycle Active",
    ];
    assert_eq!(
        issues
            .iter()
            .map(|issue| issue.detail.as_str())
            .collect::<Vec<_>>(),
        details,
    );
    for issue in issues {
        assert_eq!(issue.axis, AgentCanonicalRelationAxis::Supervision);
        assert_eq!(issue.resolution, AgentCanonicalResolution::Contradictory);
    }
    let connection = db.connection()?;
    let mut total = (0, 0);
    for sql in [
        "SELECT value FROM runtime_metadata WHERE key = 'runtime_id'",
        "SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'",
        "SELECT value FROM runtime_metadata WHERE key = 'visibility_policy_generation'",
        "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
    ] {
        let mut stmt = connection.prepare(sql)?;
        let text: String = if stmt.parameter_count() == 0 {
            stmt.query_row([], |row| row.get(0))?
        } else {
            stmt.query_row([AGENT], |row| row.get(0))?
        };
        total = add(total, text_cost(&text));
        // Canonical relations separately read and decode this identity payload.
        if stmt.parameter_count() != 0 {
            total = add(total, add(text_cost(&text), json_cost(&text)));
        }
    }
    for axis in 0..6 {
        total = steps(&connection, axis)?
            .iter()
            .fold(total, |sum, step| add(sum, step.cost));
    }
    for detail in details {
        total = add(
            total,
            (
                ((detail.len()
                    + std::mem::size_of::<String>()
                    + std::mem::size_of::<AgentCanonicalProjectionIssue>())
                    as u64)
                    * 8,
                16,
            ),
        );
    }
    // The projection ID and the raw snapshot row ID are separate owned copies.
    let exact = add(
        header(),
        add(total, add(text_cost(AGENT), text_cost(AGENT))),
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    let snapshot = {
        let _entry = phase.enter();
        db.agent_projection_snapshot_rows(AGENT)?
    };
    assert_eq!(snapshot, baseline);
    assert_eq!(phase.finish()?, exact);
    assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
    let cloned = snapshot.clone();
    assert_eq!(cloned, snapshot);
    assert!(!Arc::ptr_eq(cloned._material.as_ref().unwrap(), &holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0 * 2);
    drop((phase, holder, snapshot));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    assert_eq!(
        cloned.row.as_ref().unwrap().canonical_relations,
        baseline.row.unwrap().canonical_relations
    );
    drop(cloned);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
