//! Single decode entry for persisted execution outcomes.
//!
//! Every persisted `execution_protocol_outcomes.payload_json` read goes through
//! [`decode_outcome_payload`]; otherwise the same stored row is decoded with
//! different tolerance depending on the entry, which is how the retired
//! WorkItem `needs_repair` encoding broke prompt admission.

use anyhow::Result;

use crate::domain::execution_protocol::ExecutionOutcomeRecord;

/// Decode a persisted execution outcome row.
///
/// The migration decoder preserves every stored field and only translates the
/// retired WorkItem `needs_repair` outcome into its surviving `interrupted`
/// form. Unknown variants and unrelated damaged JSON still fail closed.
pub(crate) fn decode_outcome_payload(payload: &str) -> Result<ExecutionOutcomeRecord> {
    holon_storage::execution::decode_outcome_record_with_migration(payload)
}
