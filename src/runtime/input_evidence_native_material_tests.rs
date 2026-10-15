use super::*;
use crate::{
    runtime::resident_input_evidence::{
        EvidenceLocator, InputEvidenceFamily, InputEvidenceKey, PreparedInputEvidence,
    },
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::{register_committed_input_evidence_tx, CanonicalInputEvidence},
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority,
    },
};
fn native(value: &serde_json::Value) -> usize {
    1 + match value {
        serde_json::Value::Array(values) => values.iter().map(native).sum(),
        serde_json::Value::Object(values) => {
            values.len() + values.values().map(native).sum::<usize>()
        }
        _ => 0,
    }
}
#[test]
fn borrowed_input_material_counts_complete_values_keys_and_escaped_strings() {
    let value = serde_json::json!({"\"key\\":"[{}] escaped \" and \\" ,"items":[0,-2,1.5,true,null,"",[],{},"中文"],"nested":{"a":[false]}});
    let (bytes, nodes) = borrowed_json_material(&value).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&value).unwrap().len() as u64);
    assert_eq!(nodes, native(&value));
    assert_eq!(borrowed_json_material(&serde_json::json!({})).unwrap().1, 1);
    assert_eq!(
        borrowed_json_material(&serde_json::json!([null, null]))
            .unwrap()
            .1,
        3
    );
}
#[test]
fn actual_d3_input_native_footprint_survives_zero_serde_estimate_and_pins_deduplicate(
) -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let scope = InputEvidenceScope {
        agent_id: "native-weak-input".into(),
        identity_incarnation: 1,
    };
    let mut message = MessageEnvelope::new(
        &scope.agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Json {
            value: serde_json::json!({"items":vec![0;4096]}),
        },
    );
    message.id = "actual-native-weak-input".into();
    let delta = db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                &scope.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
        let (actual, inserted) = append_message_tx(tx, &message)?;
        assert!(inserted);
        register_committed_input_evidence_tx(tx, &scope, &[CanonicalInputEvidence::Message(actual)])
    })?;
    let actual = &delta.records[0];
    assert!(
        actual.resident_bytes < 64 * 1024,
        "encoded source fits original small policy"
    );
    let expected = actual.resident_material_estimate()?.0 as u64;
    assert!(
        expected > 64 * 1024,
        "native container floor exceeds encoded policy"
    );
    let mut serialized = serde_json::to_value(actual.as_ref())?;
    serialized["resident_bytes"] = serde_json::json!(0);
    let replay: QualifiedInputEvidence = serde_json::from_value(serialized)?;
    assert_eq!(replay.locator, actual.locator);
    assert_eq!(replay.record, actual.record);
    assert_eq!(replay.resident_material_estimate()?.0 as u64, expected);
    let item = Arc::new(replay);
    let weak = Arc::downgrade(&item);
    let mut limits = StateMemoryLimits::default();
    limits.history_per_agent_bytes = 64 * 1024;
    let ledger = InputEvidenceBudgetLedger::new(limits)?;
    let history = ledger.register_view(
        &scope,
        InputMemoryCategory::History,
        &[item.clone(), item.clone()],
    )?;
    let pins = ledger.register_view(&scope, InputMemoryCategory::Pins, &[item.clone()])?;
    let report = ledger.report()?;
    assert_eq!(
        report.physical_records,
        AllocationUsage {
            allocations: 1,
            bytes: expected
        }
    );
    assert_eq!(
        report.categories[&InputMemoryCategory::History].bytes,
        expected
    );
    assert_eq!(
        report.categories[&InputMemoryCategory::Pins].bytes,
        expected
    );
    assert_eq!(report.reclaimable_history.bytes, 0);
    assert!(matches!(
        ledger.check_history_budget(),
        Err(InputEvidenceBudgetError::ResourceExhausted {
            resource: BudgetResource::HistoryAgentBytes,
            ..
        })
    ));
    let clone = pins.clone();
    drop(pins);
    drop(history);
    assert_eq!(ledger.report()?.physical_records.bytes, expected);
    drop(clone);
    assert_eq!(ledger.report()?.retained_unclassified.bytes, expected);
    drop(item);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    drop(delta);
    Ok(())
}
#[test]
fn borrowed_input_native_array_and_same_size_text_have_distinct_actual_costs() -> anyhow::Result<()>
{
    let scope = InputEvidenceScope {
        agent_id: "same-native-size".into(),
        identity_incarnation: 1,
    };
    let build = |body| {
        let mut message = MessageEnvelope::new(
            &scope.agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            body,
        );
        message.id = "same-id".into();
        QualifiedInputEvidence {
            locator: EvidenceLocator {
                scope: scope.clone(),
                key: InputEvidenceKey::new(InputEvidenceFamily::Message, "same-id"),
                revision: 1,
                content_digest: "a".repeat(64),
            },
            record: PreparedInputEvidence::Message(message),
            created_at: "2026-10-14T00:00:00Z".into(),
            order_sequence: Some(1),
            turn_index: None,
            owner: None,
            resident_bytes: 0,
            attempt_turn_keys: None,
            tool_run_report: None,
        }
    };
    let array = build(MessageBody::Json {
        value: serde_json::json!([]),
    });
    // Explicit construction keeps the counter assertion independent from any
    // SQL or recorded resident_bytes estimate.
    let mut array = array;
    if let PreparedInputEvidence::Message(message) = &mut array.record {
        message.body = MessageBody::Json {
            value: serde_json::json!(vec![0; 2048]),
        };
    }
    let text = build(MessageBody::Text {
        text: "0,".repeat(2048),
    });
    let (array_bytes, array_nodes) = array.resident_material_estimate()?;
    let (text_bytes, text_nodes) = text.resident_material_estimate()?;
    assert!(array_nodes > text_nodes + 2000);
    assert!(array_bytes > text_bytes + 2000 * 256);
    assert!(array_bytes > array.resident_bytes && text_bytes > text.resident_bytes);
    Ok(())
}
