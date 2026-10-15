use super::super::tests::{fresh, scope};
use super::*;

#[test]
fn actual_task_input_scope_keys_gate_before_state_decode_and_retain_the_original_read() -> Result<()>
{
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let extension = serde_json::Value::Object(
        (0..512)
            .map(|n| (format!("actual-key-{n}"), serde_json::Value::Null))
            .collect(),
    );
    let extension = serde_json::to_string(&extension)?;
    let expected_state = AgentState::new("task-agent");
    let key = TaskKey::new(TaskFamily::Task, "genuinely-absent-scope-read");
    for target in ["identity", "state"] {
        db.transaction(|tx| {
            let original_identity: String = tx.query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id='task-agent'", [], |r| r.get(0),
            )?;
            let original_state: String = tx.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id='task-agent'", [], |r| r.get(0),
            )?;
            let revision: u64 = tx.query_row(
                "SELECT revision FROM task_domain_current WHERE agent_id='task-agent' AND identity_incarnation=1", [], |r| r.get(0),
            )?;
            if target == "identity" {
                tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.ignored_extension',json(?1)) WHERE agent_id='task-agent'", [&extension])?;
            } else {
                tx.execute("UPDATE agent_states SET payload_json=json_set(payload_json,'$.ignored_extension',json(?1),'$.status','invalid-state') WHERE agent_id='task-agent'", [&extension])?;
            }
            let (identity_values, identity_nodes): (usize, usize) = tx.query_row(
                "SELECT COUNT(*),COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities i,json_tree(i.payload_json) j WHERE i.agent_id='task-agent'", [], |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let (state_values, state_nodes): (usize, usize) = tx.query_row(
                "SELECT COUNT(*),COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_states s,json_tree(s.payload_json) j WHERE s.agent_id='task-agent'", [], |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let nodes = if target == "identity" {
                assert!(identity_nodes > identity_values + 512);
                // Owned request metadata (32), raw identity slot (8), then the
                // former values-only traversal would fit this actual request.
                tx.execute("UPDATE agent_states SET payload_json='!' WHERE agent_id='task-agent'", [])?;
                40 + (identity_values + 1) * 4
            } else {
                assert!(state_nodes > state_values + 512);
                48 + identity_nodes * 4 + (state_values + 1) * 4
            };
            let load = TaskDomainLoadRequest {
                query: Default::default(), capture: TaskCaptureRequest::current(),
                byte_limit: 64 * 1024 * 1024, record_limit: nodes,
            };
            let error = prepare_task_input_read_base_tx(
                tx, &scope(), &expected_state, revision, &key, None, None, &load, &ledger,
            ).err().context("full object keys were omitted before State decoder")?;
            assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::ResourceExhausted("records"))), "{target}: {error:#}");
            assert_eq!(ledger.report()?.observed_bytes, 0);
            // Restore only poisoned data; the actual ignored object extension
            // remains so the positive read still owns all its native material.
            if target == "identity" {
                tx.execute("UPDATE agent_states SET payload_json=?1 WHERE agent_id='task-agent'", [&original_state])?;
            } else {
                tx.execute("UPDATE agent_states SET payload_json=json_set(?1,'$.ignored_extension',json(?2)) WHERE agent_id='task-agent'", params![original_state, extension])?;
            }
            let mut enough = load.clone(); enough.record_limit = 65_536;
            let actual_scope = scope();
            let read = prepare_task_input_read_base_tx(
                tx, &actual_scope, &expected_state, revision, &key, None, None, &enough, &ledger,
            )?;
            assert_eq!(read.scope(), &actual_scope);
            assert!(read.material.material_estimate().records > nodes);
            let guards = read.material.into_guards();
            assert!(ledger.report()?.observed_bytes > 0);
            let last = guards.clone(); drop(guards);
            assert!(ledger.report()?.observed_bytes > 0);
            drop(last); assert_eq!(ledger.report()?.observed_bytes, 0);
            tx.execute("UPDATE agent_identities SET payload_json=?1 WHERE agent_id='task-agent'", [original_identity])?;
            tx.execute("UPDATE agent_states SET payload_json=?1 WHERE agent_id='task-agent'", [original_state])?;
            Ok(())
        })?;
    }
    Ok(())
}
