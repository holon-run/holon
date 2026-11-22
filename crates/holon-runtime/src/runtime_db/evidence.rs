//! Evidence insertion helpers and evidence query types.

use anyhow::Context;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
#[cfg(test)]
use rusqlite::params;
use rusqlite::Transaction;
use std::borrow::Cow;

use crate::runtime_db::index_outbox::RuntimeIndexChange;
pub(crate) use holon_storage::evidence::{
    committed_brief_tx, committed_message_tx, committed_tool_tx, committed_transcript_tx,
    content_hash, evidence_preview, insert_agent_deletion_job_tx, upsert_execution_root_entry_tx,
    upsert_workspace_entry_tx, upsert_workspace_occupancy_tx,
};
pub use holon_storage::evidence::{
    CommittedConversationSourceRevision, CommittedConversationTurnRevision,
    CommittedTurnInputAssignment, CommittedTurnRecord, EvidenceInsert, EvidenceKind,
    EvidencePayloadRow, EvidenceQuery, EvidenceRow,
};
pub(crate) use holon_storage::index_outbox::{
    insert_runtime_index_changes_at_tx, insert_runtime_index_changes_tx,
};

pub(crate) use holon_storage::evidence::MESSAGE_SEQUENCE_DOMAIN;

const EVIDENCE_WRITES: holon_storage::evidence::EvidenceWrites =
    holon_storage::evidence::EvidenceWrites::new(super::conversation::REVISION_WRITES);
use crate::types::{
    AgentIdentityRecord, AgentState, AuditEvent, BriefRecord, DeliverySummaryRecord,
    MessageEnvelope, ToolExecutionRecord, TranscriptEntry,
};

#[cfg(all(test, holon_lib_test_shard = "b"))]
use crate::types::{AgentKind, AgentRegistryStatus};
#[cfg(all(test, holon_lib_test_shard = "b"))]
use holon_storage::codec::timestamp;

#[cfg(all(test, holon_lib_test_shard = "b"))]
fn registry_timestamp(value: DateTime<Utc>) -> Result<String> {
    super::AGENT_REGISTRY_MATERIAL.timestamp(value)
}

pub(crate) const AUDIT_EVENT_SEQUENCE_DOMAIN: &str = "audit_event";

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
#[path = "evidence/audit_sql_budget_tests.rs"]
mod audit_sql_budget_tests;

#[cfg(test)]
mod registry_incarnation_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod registry_post_write_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod registry_enum_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod registry_name_key_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod registry_timestamp_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod registry_clock_budget_tests;

pub(crate) use holon_storage::sequences::{
    agent_sequence_scope, allocate_runtime_sequence_tx, audit_event_sequence_scope,
    observe_runtime_sequence_tx,
};

fn audit_write_sequence_scope(agent_id: Option<&str>) -> Result<Cow<'static, str>> {
    let Some(agent_id) = agent_id else {
        return Ok(Cow::Borrowed("host"));
    };
    crate::diagnostics::resource_io::charge_owned_metadata(
        ("agent:".len() + agent_id.len() + std::mem::size_of::<String>()) as u64,
        1,
    )?;
    Ok(Cow::Owned(agent_sequence_scope(agent_id)))
}

pub(crate) fn insert_message_evidence_tx(
    tx: &Transaction<'_>,
    message: &MessageEnvelope,
) -> Result<()> {
    EVIDENCE_WRITES.insert_message_evidence_tx(tx, message)
}

pub(crate) fn append_message_tx(
    tx: &Transaction<'_>,
    message: &MessageEnvelope,
) -> Result<(MessageEnvelope, bool)> {
    EVIDENCE_WRITES.append_message_tx(tx, message)
}

pub(crate) fn insert_transcript_evidence_tx(
    tx: &Transaction<'_>,
    entry: &TranscriptEntry,
) -> Result<()> {
    EVIDENCE_WRITES.insert_transcript_evidence_tx(tx, entry)
}

pub(crate) fn append_transcript_entry_tx(
    tx: &Transaction<'_>,
    entry: &TranscriptEntry,
) -> Result<(TranscriptEntry, bool)> {
    EVIDENCE_WRITES.append_transcript_entry_tx(tx, entry)
}

pub(crate) fn upsert_agent_state_tx(tx: &Transaction<'_>, record: &AgentState) -> Result<()> {
    upsert_agent_state_at_tx(tx, record, Utc::now())
}

pub(crate) fn upsert_agent_state_at_tx(
    tx: &Transaction<'_>,
    record: &AgentState,
    updated_at: DateTime<Utc>,
) -> Result<()> {
    holon_storage::agent_state::upsert_agent_state_at_tx(
        tx,
        record,
        updated_at,
        crate::diagnostics::resource_io::admit_json_buffer,
        crate::diagnostics::resource_io::admit_serialized_value,
    )
}

#[cfg(test)]
pub(crate) fn reserve_agent_identity_tx(
    tx: &Transaction<'_>,
    record: &AgentIdentityRecord,
) -> Result<()> {
    super::AGENT_REGISTRY_MATERIAL.reserve_agent_identity_tx(tx, record)
}

pub(crate) fn release_agent_identity_reservation_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<()> {
    super::AGENT_REGISTRY_MATERIAL.release_agent_identity_reservation_tx(tx, agent_id)
}

fn registry_agent_name_key(name: &str) -> Result<String> {
    use unicode_casefold::UnicodeCaseFold;

    let bytes =
        name.case_fold()
            .try_fold(std::mem::size_of::<String>() as u64, |bytes, character| {
                bytes
                    .checked_add(character.len_utf8() as u64)
                    .ok_or_else(|| anyhow!("ResourceExhausted: registry name key"))
            })?;
    crate::diagnostics::resource_io::charge_owned_metadata(bytes, 1)?;
    Ok(crate::types::agent_name_key(name))
}

pub(crate) fn upsert_agent_identity_tx(
    tx: &Transaction<'_>,
    record: &AgentIdentityRecord,
) -> Result<()> {
    if super::AGENT_REGISTRY_MATERIAL.upsert_agent_identity_tx(
        tx,
        record,
        registry_agent_name_key,
    )? {
        super::episode_snapshot::ensure_fresh_episode_continuity_tx(tx, record, record.updated_at)?;
    }
    Ok(())
}

pub(crate) fn upsert_message_tx(tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
    EVIDENCE_WRITES.upsert_message_tx(tx, message)
}

pub(crate) fn upsert_transcript_entry_tx(
    tx: &Transaction<'_>,
    entry: &TranscriptEntry,
) -> Result<()> {
    EVIDENCE_WRITES.upsert_transcript_entry_tx(tx, entry)
}

pub(crate) fn insert_tool_evidence_tx(
    tx: &Transaction<'_>,
    record: &ToolExecutionRecord,
) -> Result<()> {
    EVIDENCE_WRITES.insert_tool_evidence_tx(tx, record)
}

pub(crate) fn insert_brief_evidence_tx(tx: &Transaction<'_>, brief: &BriefRecord) -> Result<()> {
    EVIDENCE_WRITES.insert_brief_evidence_tx(tx, brief)
}

/// Outcome of the single-transition Brief publication: the stored Brief
/// record (with its immutable `created_event_seq` linkage), the committed
/// `brief_created` audit event, and whether this call inserted the event or
/// reused an idempotently identical existing row.
#[derive(Debug, Clone)]
pub struct BriefCreatedCommit {
    pub brief: BriefRecord,
    pub event: AuditEvent,
    pub event_inserted: bool,
}

/// Inserts or links a Brief record against an already-allocated
/// `brief_created` event sequence, inside the caller's transaction. The
/// linkage is immutable: an existing different linkage is a conflict, an
/// absent linkage is filled, and the payload's non-linkage content must
/// match on retry.
pub(crate) fn upsert_brief_with_created_event_seq_tx(
    tx: &Transaction<'_>,
    brief: &BriefRecord,
    event_seq: u64,
) -> Result<BriefRecord> {
    EVIDENCE_WRITES.upsert_brief_with_created_event_seq_tx(tx, brief, event_seq)
}

/// Single-transition Brief publication: allocates the audit event sequence,
/// appends the idempotent `brief_created` event, links the Brief record to
/// the committed sequence, and records index changes, all in one SQLite
/// transaction. When the event is observed, the Brief record is already
/// readable.
pub(crate) fn append_brief_with_created_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    brief: &BriefRecord,
    event: &AuditEvent,
    index_changes: &[RuntimeIndexChange],
) -> Result<BriefCreatedCommit> {
    let (event, event_inserted) = append_audit_event_tx(tx, agent_id, event)?;
    let brief = upsert_brief_with_created_event_seq_tx(tx, brief, event.event_seq)?;
    if event_inserted {
        super::notifications::project_brief(tx, &brief, &[])?;
    }
    insert_runtime_index_changes_tx(tx, index_changes)?;
    let brief = committed_brief_tx(tx, &brief.agent_id, &brief.id)?;
    Ok(BriefCreatedCommit {
        brief,
        event,
        event_inserted,
    })
}

/// Links transition `brief_evidence` rows against the `brief_created` events
/// committed by the same transition. At most one event may reference a
/// carried Brief; a Brief without its event in this transition stays
/// unlinked rather than blocking the committed transition.
pub(crate) fn link_transition_brief_created_events_tx(
    tx: &Transaction<'_>,
    brief_evidence: &[BriefRecord],
    committed_events: &[super::transitions::AuditEventAck],
) -> Result<()> {
    const BRIEF_CREATED_WIRE_NAME: &str = "brief_created";
    let registered_wait_ids: Vec<_> = committed_events
        .iter()
        .filter(|event| event.kind == "wait_condition_registered")
        .filter_map(|event| event.data.get("wait_condition_id")?.as_str())
        .collect();
    for brief in brief_evidence {
        let matches = committed_events
            .iter()
            .filter(|event| {
                event.kind == BRIEF_CREATED_WIRE_NAME
                    && event
                        .data
                        .get("brief_id")
                        .and_then(|value| value.as_str())
                        .is_some_and(|brief_id| brief_id == brief.id)
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matches.len() <= 1,
            "transition carried {} brief_created events for brief {}",
            matches.len(),
            brief.id
        );
        if let Some(event) = matches.first() {
            let brief = upsert_brief_with_created_event_seq_tx(tx, brief, event.event_seq)?;
            super::notifications::project_brief(tx, &brief, &registered_wait_ids)?;
        }
    }
    Ok(())
}

pub(crate) fn insert_delivery_summary_evidence_tx(
    tx: &Transaction<'_>,
    record: &DeliverySummaryRecord,
) -> Result<()> {
    EVIDENCE_WRITES.insert_delivery_summary_evidence_tx(tx, record)
}

#[cfg(test)]
pub(crate) fn insert_audit_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<()> {
    append_audit_event_tx(tx, agent_id, event).map(|_| ())
}

pub(crate) fn append_audit_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<(AuditEvent, bool)> {
    if let Some(existing) = audit_event_by_id_tx(tx, agent_id, &event.id)? {
        ensure_same_audit_event_content(&existing, event, "audit_event_id")?;
        return Ok((existing, false));
    }
    crate::diagnostics::resource_io::admit_cloned_value(event)?;
    let mut event = event.clone();
    event.event_log_epoch = event_log_epoch_tx(tx, None)?.into_owned();
    event.event_seq = allocate_runtime_sequence_tx(
        tx,
        AUDIT_EVENT_SEQUENCE_DOMAIN,
        &audit_write_sequence_scope(agent_id)?,
    )?;
    insert_audit_event_with_sequence_tx(tx, agent_id, &event)?;
    if let Some(agent_id) = agent_id {
        super::write_queue::record_audit_insert(agent_id, event.event_seq);
    }
    Ok((event, true))
}

/// Capture material only at the canonical writer/decoder, not from a later
/// ambient phase or an arbitrary owned event. This carries no Current authority.
pub(crate) fn append_audit_event_ack_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<(super::transitions::AuditEventAck, bool)> {
    let (event, inserted) = append_audit_event_tx(tx, agent_id, event)?;
    let material = crate::diagnostics::resource_io::ResourceReadMaterial::active_guard()?;
    Ok((
        super::transitions::AuditEventAck::with_original_material(event, material)?,
        inserted,
    ))
}

// Historical SQL replay fixtures; runtime writers use append_audit_event_tx.
#[cfg(test)]
pub(crate) fn import_audit_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<()> {
    let current_epoch = event_log_epoch_tx(
        tx,
        (!event.event_log_epoch.is_empty()).then_some(event.event_log_epoch.as_str()),
    )?;
    crate::diagnostics::resource_io::admit_cloned_value(event)?;
    let mut event = event.clone();
    if let Cow::Owned(current_epoch) = current_epoch {
        event.event_log_epoch = current_epoch;
    }
    if event.event_seq == 0 {
        append_audit_event_tx(tx, agent_id, &event)?;
        return Ok(());
    }
    if let Some(existing) = audit_event_by_id_tx(tx, agent_id, &event.id)? {
        anyhow::ensure!(
            existing.event_seq == event.event_seq,
            "conflicting audit event sequence for audit_event_id {}",
            event.id
        );
        ensure_same_audit_event_identity_content(&existing, &event)?;
        return Ok(());
    }
    let event_seq = i64::try_from(event.event_seq)
        .context("audit event sequence exceeds SQLite integer range")?;
    let mut statement =
        tx.prepare("SELECT data_json FROM audit_events WHERE agent_id IS ?1 AND event_seq = ?2")?;
    let existing = statement
        .query(params![agent_id, event_seq])?
        .next()?
        .map(|row| decode_audit_event_row(row, 0))
        .transpose()?;
    if let Some(existing) = existing {
        ensure_same_audit_event_identity_content(&existing, &event)?;
        return Ok(());
    }
    insert_audit_event_with_sequence_tx(tx, agent_id, &event)?;
    observe_runtime_sequence_tx(
        tx,
        AUDIT_EVENT_SEQUENCE_DOMAIN,
        &audit_write_sequence_scope(agent_id)?,
        event.event_seq,
    )
}

pub(super) fn audit_event_by_id_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event_id: &str,
) -> Result<Option<AuditEvent>> {
    holon_storage::evidence::audit_event_by_id_tx(
        tx,
        agent_id,
        event_id,
        crate::diagnostics::resource_io::admit_json_payload,
    )
}

#[cfg(test)]
fn decode_audit_event_row(row: &rusqlite::Row<'_>, column: usize) -> Result<AuditEvent> {
    let payload = row.get_ref(column)?.as_str()?;
    crate::diagnostics::resource_io::admit_json_payload(payload)?;
    serde_json::from_str(payload).map_err(Into::into)
}

fn event_log_epoch_tx<'a>(tx: &Transaction<'_>, expected: Option<&'a str>) -> Result<Cow<'a, str>> {
    let mut statement =
        tx.prepare("SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'")?;
    let mut rows = statement.query([])?;
    let row = rows.next()?.context("runtime event-log epoch is missing")?;
    let current = row.get_ref(0)?.as_str()?;
    if let Some(expected) = expected {
        anyhow::ensure!(
            expected == current,
            "audit event event-log epoch {} does not match runtime epoch {}",
            expected,
            current
        );
        return Ok(Cow::Borrowed(expected));
    }
    crate::diagnostics::resource_io::charge_owned_metadata(
        (current.len() + std::mem::size_of::<String>()) as u64,
        1,
    )?;
    Ok(Cow::Owned(current.to_owned()))
}

fn ensure_same_audit_event_content(
    existing: &AuditEvent,
    incoming: &AuditEvent,
    identity: &str,
) -> Result<()> {
    anyhow::ensure!(
        existing.created_at == incoming.created_at
            && existing.kind == incoming.kind
            && existing.contract_version == incoming.contract_version
            && existing.payload_schema == incoming.payload_schema
            && existing.payload_schema_version == incoming.payload_schema_version
            && existing.data == incoming.data,
        "conflicting audit event content for {identity} {}",
        existing.id
    );
    Ok(())
}

#[cfg(test)]
fn ensure_same_audit_event_identity_content(
    existing: &AuditEvent,
    incoming: &AuditEvent,
) -> Result<()> {
    anyhow::ensure!(
        existing.id == incoming.id
            && existing.created_at == incoming.created_at
            && existing.event_log_epoch == incoming.event_log_epoch,
        "conflicting audit event identity ({}, agent sequence {})",
        existing.event_log_epoch,
        existing.event_seq
    );
    ensure_same_audit_event_content(existing, incoming, "event identity")
}

fn insert_audit_event_with_sequence_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<()> {
    holon_storage::evidence::insert_audit_event_with_sequence_tx(
        tx,
        agent_id,
        event,
        crate::diagnostics::resource_io::admit_serialized_value,
        crate::diagnostics::resource_io::admit_serialized_value,
    )?;
    crate::runtime_db::observer_sync::record_appended_event_projection_effect(tx, event)?;
    Ok(())
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
#[path = "standalone_evidence_tests.rs"]
mod standalone_evidence_tests;
