//! Borrowed retained material for the scheduler read's next acquisition.
use super::*;
use std::io::Write;

/// Stream the original typed values; never allocate a JSON DTO or body copy.
/// Count container/value tokens and object keys, including native Value trees.
fn nodes(value: &impl serde::Serialize) -> Result<usize> {
    #[derive(Default)]
    struct Counter {
        nodes: usize,
        string: bool,
        escape: bool,
        primitive: bool,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for byte in bytes {
                if self.string {
                    if self.escape {
                        self.escape = false;
                    } else if *byte == b'\\' {
                        self.escape = true;
                    } else if *byte == b'"' {
                        self.string = false;
                    }
                    continue;
                }
                let start = match *byte {
                    b'"' => {
                        self.string = true;
                        self.primitive = false;
                        true
                    }
                    b'{' | b'[' => {
                        self.primitive = false;
                        true
                    }
                    b'}' | b']' | b',' | b':' => {
                        self.primitive = false;
                        false
                    }
                    b' ' | b'\n' | b'\r' | b'\t' => false,
                    _ if !self.primitive => {
                        self.primitive = true;
                        true
                    }
                    _ => false,
                };
                if start {
                    self.nodes = self
                        .nodes
                        .checked_add(1)
                        .ok_or_else(|| std::io::Error::other("scheduler native count overflow"))?;
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.nodes)
}

/// Admit the existing Summary's State/Wait/posture copies before constructing them.
pub(super) fn summary_output(
    state: &AgentState,
    posture: &crate::types::AgentPostureProjection,
    waits: &[WaitConditionRecord],
) -> Result<(u64, usize)> {
    let value = (state, posture, waits);
    let count = nodes(&value)?;
    let bytes = serialized_bytes(&value)?
        .checked_add(
            u64::try_from(count)?
                .checked_mul(256)
                .context("summary native bytes overflow")?,
        )
        .and_then(|n| n.checked_add(1024))
        .and_then(|n| n.checked_mul(4))
        .context("summary output copies overflow")?;
    let records = count
        .checked_add(8)
        .and_then(|n| n.checked_mul(4))
        .context("summary output count overflow")?;
    Ok((bytes, records))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borrowed_native_counter_counts_nested_and_escaped_values_without_dto() -> Result<()> {
        let value = serde_json::json!({"quoted\\\"key":[null, {"nested": [true,12,"\\\\\""]}]});
        // Compare the independent SQLite native count plus its object keys.
        let connection = rusqlite::Connection::open_in_memory()?;
        let encoded = serde_json::to_string(&value)?;
        let expected: usize = connection.query_row(
            "SELECT COUNT(*) + COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(?1)",
            [encoded],
            |row| row.get(0),
        )?;
        assert_eq!(nodes(&value)?, expected);
        Ok(())
    }
}

/// Existing selected allocations debit the next finite Current selection. This
/// is a borrowed estimate; the original domain/selector guards stay alive.
#[cfg(test)]
pub(super) fn selected_phase(
    state: &AgentState,
    work: &CapturedWork,
    inputs: &super::super::super::resident_input_evidence::CapturedInputEvidence,
    tasks: &super::super::super::resident_task::CapturedTasks,
    scheduling: &super::super::super::resident_scheduling::CapturedScheduling,
    selectors: &BudgetViewGuard,
    task_count: usize,
) -> Result<(u64, usize)> {
    let mut size = work.selected_material_estimate()?;
    for next in [
        inputs.selected_material_estimate()?,
        tasks.selected_material_estimate()?,
        scheduling.selected_material_estimate()?,
    ] {
        size.0 = size
            .0
            .checked_add(next.0)
            .context("scheduler selected bytes overflow")?;
        size.1 = size
            .1
            .checked_add(next.1)
            .context("scheduler selected records overflow")?;
    }
    for record in &inputs.records {
        // selected_material_estimate already includes resident_bytes and one
        // body slot; add only the actual native floor still missing there.
        let actual = record.resident_material_estimate()?;
        size.0 = size
            .0
            .checked_add(u64::try_from(
                actual.0.saturating_sub(record.resident_bytes),
            )?)
            .context("scheduler Input actual bytes overflow")?;
        size.1 = size
            .1
            .checked_add(actual.1.saturating_sub(1))
            .context("scheduler Input actual records overflow")?;
    }
    add_task_native(&mut size, tasks)?;
    let native = nodes(state)?;
    let selector_bytes = selectors.owned_material_bytes()?;
    size.0 = size
        .0
        .checked_add(serialized_bytes(state)?)
        .and_then(|n| n.checked_add(u64::try_from(native).ok()?.checked_mul(256)?))
        .and_then(|n| n.checked_add(std::mem::size_of::<AgentState>() as u64))
        .and_then(|n| n.checked_add(selector_bytes))
        .context("scheduler State/selector bytes overflow")?;
    size.1 = size
        .1
        .checked_add(native)
        .and_then(|n| n.checked_add(task_count.checked_mul(8)?))
        .and_then(|n| n.checked_add(1))
        .context("scheduler State/selector records overflow")?;
    Ok(size)
}

/// Same immutable allocation, no second ledger registration. The original
/// encoded selected estimate remains included exactly once.
#[cfg(test)]
fn native_adjustment(value: &impl serde::Serialize, encoded_floor: u64) -> Result<(u64, usize)> {
    let (encoded, count) = crate::runtime::input_evidence_budget::borrowed_json_material(value)?;
    let actual = encoded
        .max(encoded_floor)
        .checked_add(
            u64::try_from(count)?
                .checked_mul(256)
                .context("selected native bytes overflow")?,
        )
        .and_then(|n| n.checked_add(std::mem::size_of_val(value) as u64))
        .context("selected actual material overflow")?;
    Ok((
        actual
            .checked_sub(encoded_floor)
            .context("selected body byte debit underflow")?,
        count,
    ))
}
#[cfg(test)]
fn add_task_native(
    total: &mut (u64, usize),
    tasks: &super::super::super::resident_task::CapturedTasks,
) -> Result<()> {
    for record in &tasks.records {
        let extra = native_adjustment(record.as_ref(), u64::try_from(record.resident_bytes)?)?;
        total.0 = total
            .0
            .checked_add(extra.0)
            .context("scheduler Task actual bytes overflow")?;
        total.1 = total
            .1
            .checked_add(extra.1)
            .context("scheduler Task actual records overflow")?;
    }
    for record in &tasks.stop_records {
        let extra = native_adjustment(record.as_ref(), record.resident_bytes()?)?;
        total.0 = total
            .0
            .checked_add(extra.0)
            .context("scheduler Stop actual bytes overflow")?;
        total.1 = total
            .1
            .checked_add(extra.1)
            .context("scheduler Stop actual records overflow")?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn remaining_current_request(
    prior: (u64, usize),
    byte_limit: usize,
    record_limit: usize,
) -> Result<crate::runtime::current_read_facts::CurrentReadRequest> {
    let byte_limit = byte_limit.min(64 * 1024 * 1024);
    let record_limit = record_limit.min(65_536);
    anyhow::ensure!(
        prior.0 < u64::try_from(byte_limit)? && prior.1 < record_limit,
        crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted
    );
    let mut request = crate::runtime::current_read_facts::CurrentReadRequest::current(
        byte_limit - usize::try_from(prior.0)?,
        record_limit - prior.1,
    );
    request.include_pending_queue = false;
    Ok(request)
}

#[cfg(test)]
#[path = "scheduler_selected_native_tests.rs"]
mod selected_native_tests;
