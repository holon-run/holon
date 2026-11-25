//! Bounded application of prepared sources and persisted index rebuild phases.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::{
    all_backfill_source_kinds, IndexStore, MemoryDocument, PendingSource, PreparedPendingSource,
    RebuildJob, MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING,
    MEMORY_INDEX_REBUILD_PHASE_CLEANUP_SEEN, MEMORY_INDEX_REBUILD_PHASE_FINALIZE,
    MEMORY_INDEX_REBUILD_PHASE_FINALIZE_COMMIT, MEMORY_INDEX_REBUILD_PHASE_PRUNE,
    MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIR, MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIRED,
    MEMORY_INDEX_REBUILD_PHASE_SCAN, MEMORY_INDEX_REBUILD_SOURCE_KIND,
};

pub struct RebuildStart {
    pub generation: String,
    pub runtime_high_watermark: i64,
    pub now: DateTime<Utc>,
}

/// A bounded request against the authoritative source named by the job.
pub struct RebuildPage<'a> {
    pub source_kind: &'a str,
    pub cursor: &'a str,
    pub document_offset: usize,
    pub limit: usize,
}

#[derive(Debug)]
pub struct RebuildSourceRecord {
    pub cursor: String,
    pub documents: Vec<MemoryDocument>,
    pub document_offset: usize,
    pub total_documents: usize,
}

/// Authority access and dirty marker handling are supplied by the caller.
pub trait RebuildSources {
    fn start(&self, agent_id: &str) -> Result<RebuildStart>;
    fn consume_initial_dirty_marker(&self, job: &RebuildJob) -> Result<()>;
    fn read_page(&self, page: RebuildPage<'_>) -> Result<Vec<RebuildSourceRecord>>;
}

pub enum RebuildAdvance {
    Idle,
    Pending,
    /// The caller owns finalization and acknowledgment of the authoritative
    /// outbox after the index commits its checkpoint.
    ReadyToFinalize(RebuildJob),
}

pub struct IndexOutboxChange {
    pub change_seq: i64,
    pub source: PendingSource,
}

pub struct AppliedOutboxPrefix {
    pub acknowledged_through: i64,
    pub error: Option<anyhow::Error>,
}

impl IndexStore {
    pub fn advance_rebuild(
        &mut self,
        sources: &dyn RebuildSources,
        agent_id: &str,
        slice_limit: usize,
    ) -> Result<RebuildAdvance> {
        let mut remaining = slice_limit.max(1);
        let started_job = if self.rebuild_job(agent_id)?.is_none() {
            self.start_rebuild_job(sources, agent_id)?
        } else {
            false
        };
        let Some(initial_job) = self.rebuild_job(agent_id)? else {
            return Ok(RebuildAdvance::Idle);
        };
        if started_job {
            sources.consume_initial_dirty_marker(&initial_job)?;
        }
        loop {
            let Some(job) = self.rebuild_job(agent_id)? else {
                return Ok(RebuildAdvance::Idle);
            };
            let used = match job.phase.as_str() {
                MEMORY_INDEX_REBUILD_PHASE_SCAN => {
                    self.scan_rebuild_slice(sources, &job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_PRUNE | MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIRED => {
                    self.prune_rebuild_slice(&job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIR => {
                    self.prune_rebuild_fts_repair_slice(&job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_FINALIZE => {
                    // Also upgrades jobs persisted in the old unsliced final
                    // phase. Do not publish completion before cleanup resumes.
                    self.update_rebuild_phase(&job, MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING)?;
                    0
                }
                MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING => {
                    self.cleanup_rebuild_pending_slice(&job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_CLEANUP_SEEN => {
                    self.cleanup_rebuild_seen_slice(&job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_FINALIZE_COMMIT => {
                    return Ok(RebuildAdvance::ReadyToFinalize(job));
                }
                phase => anyhow::bail!(
                    "unsupported memory index rebuild phase {phase:?} for agent {}",
                    job.agent_id
                ),
            };
            remaining = remaining.saturating_sub(used);
            if used > 0 && remaining == 0 {
                return Ok(RebuildAdvance::Pending);
            }
        }
    }

    fn start_rebuild_job(&mut self, sources: &dyn RebuildSources, agent_id: &str) -> Result<bool> {
        let Some(_intent) = self.rebuild_intents_for_agent(agent_id)?.into_iter().next() else {
            return Ok(false);
        };
        let RebuildStart {
            generation,
            runtime_high_watermark,
            now,
        } = sources.start(agent_id)?;
        self.start_rebuild(agent_id, &generation, runtime_high_watermark, now)?;
        tracing::info!(
            agent_id = %agent_id,
            generation = %generation,
            runtime_high_watermark,
            "started bounded memory index rebuild"
        );
        Ok(true)
    }

    fn scan_rebuild_slice(
        &mut self,
        sources: &dyn RebuildSources,
        job: &RebuildJob,
        slice_limit: usize,
    ) -> Result<usize> {
        let source_kinds = all_backfill_source_kinds();
        if job.source_kind_index >= source_kinds.len() {
            self.update_rebuild_phase(job, MEMORY_INDEX_REBUILD_PHASE_PRUNE)?;
            return Ok(0);
        }
        let source_kind = source_kinds[job.source_kind_index];
        let records = sources.read_page(RebuildPage {
            source_kind,
            cursor: &job.source_cursor,
            document_offset: job.source_offset,
            limit: slice_limit,
        })?;
        if records.is_empty() {
            let next_index = job.source_kind_index + 1;
            let phase = if next_index >= source_kinds.len() {
                MEMORY_INDEX_REBUILD_PHASE_PRUNE
            } else {
                MEMORY_INDEX_REBUILD_PHASE_SCAN
            };
            self.advance_rebuild_source_kind(job, next_index, phase)?;
            return Ok(0);
        }

        let mut remaining = slice_limit;
        let mut next_cursor = job.source_cursor.clone();
        let mut next_offset = job.source_offset;
        let mut documents = Vec::new();
        for record in records {
            let offset = record.document_offset.min(record.total_documents);
            let available = record.total_documents.saturating_sub(offset);
            let take = record.documents.len().min(remaining);
            documents.extend(record.documents.into_iter().take(take));
            remaining = remaining.saturating_sub(take.max(1));
            if take < available {
                next_cursor = record.cursor;
                next_offset = offset + take;
                break;
            }
            next_cursor = record.cursor;
            next_offset = 0;
            if remaining == 0 {
                break;
            }
        }

        self.apply_rebuild_scan(job, &documents, &next_cursor, next_offset)?;
        tracing::debug!(
            agent_id = %job.agent_id,
            generation = %job.generation,
            source_kind,
            documents = documents.len(),
            "advanced bounded memory index rebuild scan"
        );
        Ok(slice_limit.saturating_sub(remaining))
    }

    pub fn consume_pending_sources(
        &mut self,
        agent_id: &str,
        limit: usize,
        prepare: &dyn Fn(&PendingSource) -> Result<PreparedPendingSource>,
    ) -> Result<()> {
        let pending = self.pending_sources_for_agent_with_limit(agent_id, limit)?;
        for pending_source in pending {
            let source = &pending_source.source;
            if source.source_kind == MEMORY_INDEX_REBUILD_SOURCE_KIND {
                continue;
            }
            let prepared = prepare(source);
            self.consume_pending_source_snapshot(&pending_source, &prepared)?;
        }
        Ok(())
    }

    pub fn consume_stale_sources(
        &mut self,
        agent_id: &str,
        limit: usize,
        prepare: &dyn Fn(&PendingSource) -> Result<PreparedPendingSource>,
    ) -> Result<()> {
        let stale_sources = self.stale_source_states_for_agent_with_limit(agent_id, limit)?;
        for source in stale_sources {
            let prepared = match prepare(&source) {
                Ok(prepared) => prepared,
                Err(error) => {
                    tracing::warn!(
                        agent_id,
                        source_kind = %source.source_kind,
                        source_ref = %source.source_ref,
                        error = %error,
                        "skipping failed stale memory index source refresh"
                    );
                    continue;
                }
            };
            self.apply_stale_source(agent_id, &source, &prepared)?;
        }
        self.refresh_stale_meta(agent_id)
    }

    /// Apply rows in source order. Each successful index write and cursor
    /// update commit together before the caller can acknowledge that row.
    pub fn apply_outbox_prefix(
        &mut self,
        runtime_id: &str,
        agent_id: &str,
        cursor: i64,
        changes: &mut dyn Iterator<Item = IndexOutboxChange>,
        prepare: &dyn Fn(&PendingSource) -> Result<PreparedPendingSource>,
    ) -> AppliedOutboxPrefix {
        let mut last_successful_seq = cursor;
        for IndexOutboxChange { change_seq, source } in changes {
            let result = prepare(&source).and_then(|prepared| {
                self.apply_outbox_change(runtime_id, agent_id, &source, &prepared, change_seq)
            });
            if let Err(error) = result {
                return AppliedOutboxPrefix {
                    acknowledged_through: last_successful_seq,
                    error: Some(error.context(format!(
                        "agent_id={} change_seq={} source_kind={} source_ref={}",
                        agent_id, change_seq, source.source_kind, source.source_ref
                    ))),
                };
            }
            last_successful_seq = change_seq;
        }
        AppliedOutboxPrefix {
            acknowledged_through: last_successful_seq,
            error: None,
        }
    }
}
