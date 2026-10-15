//! Real accepted sources feed the actual scheduler phase gate; no alias/model grant.
use super::*;
use crate::{
    runtime::{
        current_read_facts::CurrentReadError,
        tests::support::{context_config, tempdir},
        RuntimeHandle,
    },
    runtime_db::transitions::TaskTransitionCommand,
    types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        TaskKind, TaskRecord, TaskStatus,
    },
};

fn sql_nodes(value: &impl serde::Serialize) -> Result<(usize, usize)> {
    let encoded = serde_json::to_string(value)?;
    let c = rusqlite::Connection::open_in_memory()?;
    let count = c.query_row(
        "SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(?1)",
        [&encoded],
        |r| r.get(0),
    )?;
    Ok((encoded.len(), count))
}
#[tokio::test]
async fn actual_input_and_task_native_bytes_share_current_selection_allowance_without_new_guard(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "scheduler-native-selected",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let message = MessageEnvelope::new(
        runtime.instance_agent_id(),
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Json {
            value: serde_json::json!({"dense":vec![serde_json::Value::Null;1024]}),
        },
    );
    let message = runtime.enqueue(message).await?;
    let mut mutation = runtime.begin_mutation().await?;
    let at = runtime.now();
    let id = "actual-selected-native-task";
    mutation
        .commit_task_transition(
            "actual-native-task-ack".into(),
            TaskTransitionCommand {
                agent_id: runtime.instance_agent_id().into(),
                task: TaskRecord {
                    id: id.into(),
                    agent_id: runtime.instance_agent_id().into(),
                    kind: TaskKind::CommandTask,
                    status: TaskStatus::Running,
                    created_at: at,
                    updated_at: at,
                    parent_message_id: None,
                    work_item_id: None,
                    summary: None,
                    // Original Task payload keeps at most64 members per array.
                    // Eight finite rows preserve all512 leaves through real W1.
                    detail: Some(
                        serde_json::json!({"dense":vec![vec![serde_json::Value::Null;64];8]}),
                    ),
                    recovery: None,
                },
                agent_deletion: None,
                task_result_settlement: None,
                queue_entry: None,
                work_items: Vec::new(),
                expected_wait_conditions: Vec::new(),
                wait_conditions: Vec::new(),
                agent_state: None,
                message_evidence: Vec::new(),
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: false,
                commit_on_idempotent: false,
                fault: None,
            },
            Default::default(),
            true,
        )
        .await?;
    let root = mutation.capture_published_root()?;
    let scope = mutation.input_evidence_scope();
    let request = WorkCaptureRequest::current();
    let (task_request, selectors) = crate::runtime::task_reads::scheduler_task_request(
        &scope,
        &request,
        Some(root.tasks.as_ref().unwrap().coverage()),
        &runtime.inner.input_evidence_budget,
        65536,
        64 * 1024 * 1024,
    )?;
    let (state, work, inputs, tasks) =
        RuntimeMutation::capture_scheduler_task_domains(&root, &scope, &request, &task_request)?;
    let scheduling = root
        .scheduling
        .as_ref()
        .unwrap()
        .capture(&crate::runtime::resident_scheduling::SchedulingCaptureRequest::current())?;
    assert!(inputs
        .records
        .iter()
        .any(|r| r.locator.key.id == message.id));
    assert!(tasks.records.iter().any(|r| r.locator.key.id == id));
    let input = inputs
        .records
        .iter()
        .find(|r| r.locator.key.id == message.id)
        .unwrap();
    let crate::runtime::resident_input_evidence::PreparedInputEvidence::Message(accepted) =
        &input.record
    else {
        panic!("accepted original Message body expected")
    };
    let MessageBody::Json { value } = &accepted.body else {
        panic!("accepted dense JSON body changed")
    };
    assert_eq!(
        value["dense"].as_array().unwrap().len(),
        1024,
        "actual admitted Input body must retain its original dense tree"
    );
    let selected_task = tasks
        .records
        .iter()
        .find(|r| r.locator.key.id == id)
        .unwrap();
    let crate::runtime::resident_task::CanonicalTaskFact::Task(actual_task) = &selected_task.fact
    else {
        panic!("actual selected original Task body expected")
    };
    let dense = actual_task.detail.as_ref().unwrap()["dense"]
        .as_array()
        .unwrap();
    assert_eq!(dense.len(), 8, "actual qualified Task outer array changed");
    assert!(
        dense.iter().all(|row| row
            .as_array()
            .is_some_and(|row| row.len() == 64 && row.iter().all(serde_json::Value::is_null))),
        "every actual qualified Task row must retain its64 leaves"
    );

    let mut encoded_prior = work.selected_material_estimate()?.0;
    for next in [
        inputs.selected_material_estimate()?,
        tasks.selected_material_estimate()?,
        scheduling.selected_material_estimate()?,
    ] {
        encoded_prior = encoded_prior
            .checked_add(next.0)
            .context("fixture selected byte overflow")?;
    }
    let (state_encoded, state_nodes) = sql_nodes(state.as_ref())?;
    encoded_prior = encoded_prior
        .checked_add(u64::try_from(
            state_encoded + state_nodes * 256 + std::mem::size_of::<AgentState>(),
        )?)
        .and_then(|n| n.checked_add(selectors.owned_material_bytes().ok()?))
        .context("fixture State byte overflow")?;
    // Independent SQLite tree counts prove the missing lower bound, including
    // object keys; the production estimator uses a streaming borrowed writer.
    let mut missing_native = 0u64;
    for record in &inputs.records {
        let (encoded, nodes) = sql_nodes(&(
            &record.locator,
            &record.record,
            &record.created_at,
            record.order_sequence,
            record.turn_index,
            &record.owner,
            &record.attempt_turn_keys,
        ))?;
        let actual = encoded.max(record.resident_bytes)
            + nodes * 256
            + std::mem::size_of_val(record.as_ref());
        missing_native = missing_native
            .checked_add(u64::try_from(actual - record.resident_bytes)?)
            .context("fixture Input byte overflow")?;
    }
    for record in &tasks.records {
        let (encoded, nodes) = sql_nodes(record.as_ref())?;
        let actual = encoded.max(record.resident_bytes)
            + nodes * 256
            + std::mem::size_of_val(record.as_ref());
        missing_native = missing_native
            .checked_add(u64::try_from(actual - record.resident_bytes)?)
            .context("fixture Task byte overflow")?;
    }
    assert!(tasks.stop_records.is_empty());
    assert!(
        missing_native > 256 * 1500,
        "both original dense sources must survive their actual ACKs"
    );
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let prior = selected_phase(
        &state,
        &work,
        &inputs,
        &tasks,
        &scheduling,
        &selectors,
        request.required_task_ids.len(),
    )?;
    assert_eq!(prior.0, encoded_prior + missing_native);
    assert!(prior.1 < 65536);
    let cap = usize::try_from(encoded_prior + missing_native / 2)?;
    assert!(encoded_prior < u64::try_from(cap)? && prior.0 > u64::try_from(cap)?);
    // These are the real production gate and real source allocations. They
    // cannot be independently granted a fresh full allowance for Current.
    assert!(remaining_current_request((encoded_prior, prior.1), cap, 65536).is_ok());
    let error = remaining_current_request(prior, cap, 65536)
        .err()
        .context("actual native sources bypassed the joint byte gate")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    let current_request = remaining_current_request(prior, 64 * 1024 * 1024, 65536)?;
    let selected = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&current_request)?;
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes >= baseline,
        true
    );
    drop(selected);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline,
        "borrowed selected/prior calculation must not register immutable bodies again"
    );
    assert!(
        remaining_current_request((64 * 1024 * 1024, 1), 80 * 1024 * 1024, 100000).is_err(),
        "hard cap must be clamped before prior debit"
    );
    drop((
        state, work, inputs, tasks, scheduling, selectors, root, mutation,
    ));
    Ok(())
}
