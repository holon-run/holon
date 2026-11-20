use anyhow::{Context, Result};
use holon_domain::{ExternalTriggerRecord, WorkItemContinuationFrame};

pub fn decode_work_item_continuation_payload(payload: &str) -> Result<WorkItemContinuationFrame> {
    serde_json::from_str(payload).context("decoding work item continuation payload from runtime db")
}

pub fn decode_external_trigger_payload(payload: &str) -> Result<ExternalTriggerRecord> {
    serde_json::from_str(payload).context("decoding external trigger payload from runtime db")
}
