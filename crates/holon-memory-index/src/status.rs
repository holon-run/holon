//! Index freshness derived from the observed index and runtime outbox.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{IndexStore, MemorySearchResult, RebuildJob};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemorySearchIndexStatus {
    pub freshness: String,
    /// Highest applied contiguous runtime outbox `change_seq`.
    pub cursor: i64,
    /// Monotonic produced watermark: highest `change_seq` ever appended for
    /// the agent. Unlike the drained-outbox maximum, it never returns to 0.
    pub high_watermark: i64,
    /// Sequence distance `produced - applied`. Cross-agent global
    /// autoincrement makes this an upper-bound hint only; `pending_count` is
    /// the exact backlog metric.
    pub lag: i64,
    /// Exact pending outbox row count for this agent.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub pending_count: i64,
    /// Age of the oldest pending outbox row, the real propagation delay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_pending_age_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub indexing_needed: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub results_may_be_incomplete: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub consumption_was_limited: bool,
    /// Failures hit by this index handle's current consume pass. Transient
    /// by design; not a durable health history.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped_error_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_at: Option<DateTime<Utc>>,
    /// Machine-readable reasons the index is not currently fresh:
    /// `index_missing`, `dirty_marker`, `backfill_incomplete`, `outbox_lag`,
    /// `pending_sources`, `stale_projection`, `consume_error`. Empty when
    /// fresh. The background daemon self-heals every reason; they are
    /// diagnostic, not a manual-work order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_documents_processed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_last_progress_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemorySearchQueryResult {
    pub results: Vec<MemorySearchResult>,
    /// Aggregate status across the agents actually queried. For a
    /// default-agent-only search this equals that agent's status exactly.
    pub index_status: MemorySearchIndexStatus,
    /// Per-agent status covering exactly the queried agents. Present only
    /// when the query spanned more than one agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_status_by_agent: Option<std::collections::BTreeMap<String, MemorySearchIndexStatus>>,
}

/// Merge per-agent statuses into one aggregate without pretending multiple

/// agents' cursors are a single agent's: freshness is the worst, reasons are

/// the union, pending counts sum, and sequence fields report cross-agent

/// maxima with the details available per agent.

pub fn aggregate_index_status(
    statuses: &BTreeMap<String, MemorySearchIndexStatus>,
) -> MemorySearchIndexStatus {
    let mut aggregate = MemorySearchIndexStatus {
        freshness: "fresh".into(),
        cursor: 0,
        high_watermark: 0,
        lag: 0,
        pending_count: 0,
        oldest_pending_age_ms: None,
        indexing_needed: false,
        results_may_be_incomplete: false,
        consumption_was_limited: false,
        skipped_error_count: 0,
        last_indexed_at: None,
        stale_reasons: Vec::new(),
        rebuild_phase: None,
        rebuild_documents_processed: None,
        rebuild_started_at: None,
        rebuild_last_progress_at: None,
    };
    let mut freshness_rank = 0; // fresh < stale < missing
    let mut reasons_seen = std::collections::BTreeSet::new();
    for status in statuses.values() {
        let rank = match status.freshness.as_str() {
            "missing" => 2,
            "stale" => 1,
            _ => 0,
        };
        if rank > freshness_rank {
            freshness_rank = rank;
            aggregate.freshness = status.freshness.clone();
        }
        aggregate.cursor = aggregate.cursor.max(status.cursor);
        aggregate.high_watermark = aggregate.high_watermark.max(status.high_watermark);
        aggregate.lag = aggregate.lag.max(status.lag);
        aggregate.pending_count += status.pending_count;
        aggregate.oldest_pending_age_ms = aggregate
            .oldest_pending_age_ms
            .max(status.oldest_pending_age_ms);
        aggregate.consumption_was_limited |= status.consumption_was_limited;
        aggregate.skipped_error_count += status.skipped_error_count;
        aggregate.last_indexed_at = match (aggregate.last_indexed_at, status.last_indexed_at) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        if aggregate.rebuild_phase.is_none() {
            aggregate.rebuild_phase = status.rebuild_phase.clone();
            aggregate.rebuild_documents_processed = status.rebuild_documents_processed;
            aggregate.rebuild_started_at = status.rebuild_started_at;
            aggregate.rebuild_last_progress_at = status.rebuild_last_progress_at;
        }
        reasons_seen.extend(status.stale_reasons.iter().cloned());
    }
    aggregate.indexing_needed = aggregate.freshness != "fresh";
    aggregate.results_may_be_incomplete = aggregate.indexing_needed;
    aggregate.stale_reasons = reasons_seen.into_iter().collect();
    aggregate
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// The runtime acquires these values from its canonical outbox and the
/// index's committed cursor.
#[derive(Default)]
pub struct IndexOutboxStatus {
    pub cursor: i64,
    pub high_watermark: i64,
    pub pending_count: i64,
    pub oldest_pending_age_ms: Option<i64>,
    pub index_exists: bool,
    pub last_indexed_at: Option<DateTime<Utc>>,
}

pub struct IndexMaintenanceStatus {
    has_stale_projection: bool,
    has_pending_sources: bool,
    lacks_full_backfill: bool,
    is_dirty: bool,
    rebuild_job: Option<RebuildJob>,
}

impl IndexStore {
    pub fn read_maintenance_status(
        &self,
        agent_id: &str,
        is_dirty: &dyn Fn() -> bool,
    ) -> Result<IndexMaintenanceStatus> {
        Ok(IndexMaintenanceStatus {
            has_stale_projection: self.has_stale_source_states_for_agent(agent_id)?,
            has_pending_sources: self.has_pending_sources_for_agent(agent_id)?,
            lacks_full_backfill: !self.has_backfill_checkpoints_for_agent(agent_id)?,
            is_dirty: is_dirty(),
            rebuild_job: self.rebuild_job(agent_id)?,
        })
    }
}

impl IndexMaintenanceStatus {
    pub fn into_search_status(
        self,
        initial_index_exists: bool,
        outbox: Option<IndexOutboxStatus>,
        outbox_reached_limit: bool,
        outbox_error_count: usize,
    ) -> MemorySearchIndexStatus {
        let Self {
            has_stale_projection,
            has_pending_sources,
            lacks_full_backfill,
            is_dirty,
            rebuild_job,
        } = self;
        // Order matters for readability, not semantics: every reason is
        // independently healed by the background daemon.
        let mut stale_reasons = Vec::new();
        if !initial_index_exists {
            stale_reasons.push("index_missing".to_string());
        }
        if is_dirty {
            stale_reasons.push("dirty_marker".to_string());
        }
        if lacks_full_backfill {
            stale_reasons.push("backfill_incomplete".to_string());
        }
        if has_stale_projection {
            stale_reasons.push("stale_projection".to_string());
        }
        if has_pending_sources {
            stale_reasons.push("pending_sources".to_string());
        }
        if rebuild_job.is_some() {
            stale_reasons.push("rebuild_in_progress".to_string());
        }
        if outbox_error_count > 0 {
            stale_reasons.push("consume_error".to_string());
        }
        let index_missing = outbox.as_ref().is_some_and(|outbox| !outbox.index_exists);
        let IndexOutboxStatus {
            cursor,
            high_watermark,
            pending_count,
            oldest_pending_age_ms,
            last_indexed_at,
            ..
        } = outbox.unwrap_or_default();
        let lag = (high_watermark - cursor).max(0);
        if lag > 0 || pending_count > 0 {
            stale_reasons.push("outbox_lag".to_string());
        }
        let freshness = if index_missing {
            "missing"
        } else if is_dirty
            || lag > 0
            || pending_count > 0
            || has_stale_projection
            || has_pending_sources
            || lacks_full_backfill
            || rebuild_job.is_some()
        {
            "stale"
        } else {
            "fresh"
        };
        let indexing_needed = freshness != "fresh";
        MemorySearchIndexStatus {
            freshness: freshness.into(),
            cursor,
            high_watermark,
            lag,
            pending_count,
            oldest_pending_age_ms,
            indexing_needed,
            results_may_be_incomplete: indexing_needed,
            consumption_was_limited: outbox_reached_limit && lag > 0,
            skipped_error_count: outbox_error_count,
            last_indexed_at,
            stale_reasons,
            rebuild_phase: rebuild_job.as_ref().map(|job| job.phase.clone()),
            rebuild_documents_processed: rebuild_job.as_ref().map(|job| job.documents_processed),
            rebuild_started_at: rebuild_job.as_ref().map(|job| job.started_at),
            rebuild_last_progress_at: rebuild_job.as_ref().map(|job| job.last_progress_at),
        }
    }
}

#[cfg(test)]
mod tests;
