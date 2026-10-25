//! Compile-time-only controls for isolated daemon recovery acceptance.
//! No control is an ingress envelope or an authorization to bypass a fence.

use super::*;
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementRecord, TASK_RESULT_RECOVERY_LIMIT,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Fence {
    result_message_id: String,
    result_identity: String,
    rejoin_generation: u64,
    parent_turn_id: String,
    recovery_epoch: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Control {
    protocol: u32,
    request_id: String,
    agent_id: String,
    task_id: String,
    work_item_id: Option<String>,
    hold_scanner: bool,
    #[serde(default)]
    hold_task_result: bool,
    inject_sixth: bool,
    fence: Option<Fence>,
}

pub(super) fn directory(runtime: &RuntimeHandle) -> PathBuf {
    runtime.agent_home().join("recovery-acceptance")
}

fn load(runtime: &RuntimeHandle) -> Result<Option<Control>> {
    let path = directory(runtime).join("control.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let control: Control = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        control.protocol == 1
            && !control.request_id.is_empty()
            && control.request_id.len() <= 80
            && control
                .request_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
        "invalid private recovery acceptance protocol/request id"
    );
    Ok(Some(control))
}

fn matches(control: &Control, record: &TaskResultSettlementRecord) -> bool {
    control.agent_id == record.agent_id
        && control.task_id == record.task_id
        && control.work_item_id == record.work_item_id
        && control.fence.as_ref().is_none_or(|fence| {
            fence.result_message_id == record.message_id
                && fence.result_identity == record.result_identity
                && fence.rejoin_generation == record.rejoin_generation
                && fence.parent_turn_id == record.parent_turn_id
                && fence.recovery_epoch == record.recovery_epoch
        })
}

fn event_path(runtime: &RuntimeHandle, control: &Control, event: &str) -> PathBuf {
    directory(runtime).join(format!("{}.{}.json", control.request_id, event))
}

fn evidence(
    runtime: &RuntimeHandle,
    control: &Control,
    record: &TaskResultSettlementRecord,
    event: &str,
    source: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "protocol": 1,
        "build": env!("HOLON_VERSION"),
        "event": event,
        "request_id": control.request_id,
        "agent_id": record.agent_id,
        "task_id": record.task_id,
        "work_item_id": record.work_item_id,
        "fence": {
            "result_message_id": record.message_id,
            "result_identity": record.result_identity,
            "rejoin_generation": record.rejoin_generation,
            "parent_turn_id": record.parent_turn_id,
            "recovery_epoch": record.recovery_epoch,
        },
        "recovery_attempts": record.recovery_attempts,
        "last_recovery_root_attempt_id": record.last_recovery_root_attempt_id,
        "activation_id": record.activation_id,
        "recovery_paused_at": record.recovery_paused_at,
        "source_message_id": source,
        "observed_at": runtime.now(),
    })
}

fn publish(
    runtime: &RuntimeHandle,
    control: &Control,
    record: &TaskResultSettlementRecord,
    event: &str,
    source: Option<&str>,
) -> Result<()> {
    let path = event_path(runtime, control, event);
    if path.exists() {
        return Ok(());
    }
    let temporary = path.with_extension("pending");
    let mut value = evidence(runtime, control, record, event, source);
    if let Some(source) = source {
        value["source"] = serde_json::to_value(runtime.inner.storage.read_message_by_id(source)?)?;
        value["queue_entry"] =
            serde_json::to_value(runtime.inner.runtime_db.queue_entries().latest(source)?)?;
    }
    if event == "budget-rejected" {
        value["rejection_stage"] = "scheduler_canonical_claim".into();
        value["reason"] = "recovery_budget_exhausted".into();
    }
    let bytes = serde_json::to_vec_pretty(&value)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
    }
    #[cfg(not(unix))]
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn bound(
    runtime: &RuntimeHandle,
    control: &Control,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    if !matches(control, record) {
        return Ok(false);
    }
    let path = event_path(runtime, control, "target-observed");
    if path.exists() {
        let previous: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        let current = evidence(runtime, control, record, "target-observed", None);
        return Ok(["agent_id", "task_id", "work_item_id", "fence"]
            .into_iter()
            .all(|key| previous[key] == current[key]));
    }
    // A prearmed task gate binds its first concrete result, never a later
    // generation or operator-rearmed recovery epoch using the same request id.
    publish(runtime, control, record, "target-observed", None)?;
    Ok(true)
}

pub(super) async fn service(runtime: &RuntimeHandle) -> Result<(Option<Control>, bool)> {
    let Some(control) = load(runtime)? else {
        return Ok((None, false));
    };
    let state = runtime.agent_state().await?;
    if control.agent_id != state.id {
        return Ok((None, false));
    }
    let Some(task) = runtime.inner.storage.latest_task_record(&control.task_id)? else {
        return Ok((Some(control), false));
    };
    let Some(result_id) = task.parent_message_id.as_deref() else {
        return Ok((Some(control), false));
    };
    let Some(record) = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_id)?
    else {
        return Ok((Some(control), false));
    };
    if !bound(runtime, &control, &record)? {
        return Ok((None, false));
    }
    if record.last_recovery_root_attempt_id.is_some() {
        publish(runtime, &control, &record, "canonical-admitted", None)?;
    }
    if !hold_scanner(runtime, Some(&control), &record)? {
        publish(runtime, &control, &record, "scanner-released", None)?;
    }
    if !control.hold_task_result {
        publish(runtime, &control, &record, "task-result-released", None)?;
    }
    if !control.inject_sixth {
        return Ok((Some(control), false));
    }
    anyhow::ensure!(
        control.fence.is_some(),
        "sixth recovery source requires the complete observed result fence"
    );
    anyhow::ensure!(
        record.recovery_attempts == TASK_RESULT_RECOVERY_LIMIT
            && record.recovery_paused_at.is_some(),
        "sixth recovery source requires an actually exhausted recovery budget"
    );
    anyhow::ensure!(
        task.agent_id == record.agent_id
            && task.effective_work_item_id() == record.work_item_id.as_deref()
            && super::task_state_reducer::is_terminal_task_status(&task.status)
            && task.rejoin_fence().is_ok_and(|fence| {
                fence.generation == record.rejoin_generation
                    && fence.parent_turn_id == record.parent_turn_id
            }),
        "sixth recovery source task/result fence mismatch"
    );
    if event_path(runtime, &control, "sixth-enqueued").exists() {
        return Ok((Some(control), false));
    }
    let expected = format!("msg_task_result_recovery_acceptance_{}", control.request_id);
    let (mut message, existing) =
        runtime.task_result_repair_source(&record, record.work_item_id.as_deref())?;
    anyhow::ensure!(
        existing.is_none() || message.id == expected,
        "another recovery source is outstanding"
    );
    // The runtime constructor supplies all authority/fences; a deterministic
    // test-only id prevents a crash between queue persistence and ACK from
    // injecting a second source for the same request.
    message.id = expected;
    message.source_refs.insert(
        "recovery_acceptance_request_id".into(),
        control.request_id.clone(),
    );
    if runtime
        .inner
        .storage
        .read_message_by_id(&message.id)?
        .is_none()
    {
        runtime.enqueue(message.clone()).await?;
    }
    publish(
        runtime,
        &control,
        &record,
        "sixth-enqueued",
        Some(&message.id),
    )?;
    Ok((Some(control), true))
}

pub(super) fn hold_scanner(
    runtime: &RuntimeHandle,
    control: Option<&Control>,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    let Some(control) = control.filter(|control| control.hold_scanner) else {
        return Ok(false);
    };
    if !bound(runtime, control, record)? {
        return Ok(false);
    }
    publish(runtime, control, record, "scanner-held", None)?;
    Ok(true)
}

pub(super) fn hold_task_result(runtime: &RuntimeHandle, source: &MessageEnvelope) -> Result<bool> {
    if source.kind != MessageKind::TaskResult {
        return Ok(false);
    }
    let Some(control) = load(runtime)?.filter(|control| control.hold_task_result) else {
        return Ok(false);
    };
    let Some(record) = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&source.id)?
    else {
        return Ok(false);
    };
    if !bound(runtime, &control, &record)? {
        return Ok(false);
    }
    publish(
        runtime,
        &control,
        &record,
        "task-result-held",
        Some(&source.id),
    )?;
    Ok(true)
}

pub(super) fn scanner_enqueued(
    runtime: &RuntimeHandle,
    control: Option<&Control>,
    source: &MessageEnvelope,
) -> Result<()> {
    let Some(control) = control else {
        return Ok(());
    };
    let Some(result_id) = source.source_refs.get("task_result_message_id") else {
        return Ok(());
    };
    // An exact-continuation transition can legitimately lose its fence. Never
    // acknowledge the proposed source unless its real queue write committed.
    if runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest(&source.id)?
        .is_none()
    {
        return Ok(());
    }
    let Some(record) = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_id)?
    else {
        return Ok(());
    };
    if bound(runtime, control, &record)? {
        publish(
            runtime,
            control,
            &record,
            "scanner-enqueued",
            Some(&source.id),
        )?;
    }
    Ok(())
}

pub(super) fn budget_rejected(
    runtime: &RuntimeHandle,
    record: &TaskResultSettlementRecord,
    source_id: Option<&str>,
) -> Result<()> {
    let Some(control) = load(runtime)? else {
        return Ok(());
    };
    let expected = format!("msg_task_result_recovery_acceptance_{}", control.request_id);
    if !control.inject_sixth
        || source_id != Some(expected.as_str())
        || !bound(runtime, &control, record)?
    {
        return Ok(());
    }
    let Some(source) = runtime.inner.storage.read_message_by_id(&expected)? else {
        return Ok(());
    };
    if source.source_refs.get("recovery_acceptance_request_id") != Some(&control.request_id) {
        return Ok(());
    }
    publish(
        runtime,
        &control,
        record,
        "budget-rejected",
        Some(&expected),
    )
}
