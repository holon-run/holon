//! Finite report bodies from the Work read's one publication. No storage,
//! Runtime/Root back-reference, mutable body cache or lazy qualification.
use super::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory},
    resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceScope,
        LegacyDeliverySourceAvailability, PreparedInputEvidence, QualifiedInputEvidence,
        ResidentInputEvidence,
    },
    resident_work::{
        CanonicalWorkRecord, CapturedWork, QualifiedWorkRecord, WorkDeliveryHead, WorkDeliveryHeads,
    },
};
use crate::types::{
    BriefContentSource, BriefRecord, DeliverySummaryRecord, MessageBody, TranscriptEntryKind,
    WorkItemRecord, WorkItemState,
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkReportReadError {
    #[error("Work report source requires preparation: {0:?}")]
    NeedsPreparation(Key),
    #[error("Work report Legacy source is pruned: {0}")]
    Pruned(String),
    #[error("Work report Legacy namespace is unavailable: {0}")]
    Unavailable(String),
}

pub(crate) struct CapturedWorkReportSources {
    scope: InputEvidenceScope,
    records: Vec<Arc<QualifiedInputEvidence>>,
    unavailable: BTreeSet<Key>,
    unprepared: BTreeSet<Key>,
    _pins: Option<Arc<BudgetViewGuard>>,
}
/// Only the actual report text/locators and resolver scratch are copied. In
/// particular a Legacy evidence Value is retained by Arc, never cloned here.
fn report_output_material(record: &PreparedInputEvidence) -> Result<(usize, usize)> {
    let (bytes, records) = match record {
        PreparedInputEvidence::LegacyDelivery(value) => (
            value
                .id
                .len()
                .checked_add(value.text.len())
                .context("Work report text overflow")?,
            16,
        ),
        PreparedInputEvidence::Brief(value) => (
            value
                .id
                .len()
                .checked_add(value.text.len())
                .context("Work report text overflow")?,
            16,
        ),
        PreparedInputEvidence::Message(value) => (
            match &value.body {
                MessageBody::Text { text } | MessageBody::Brief { text, .. } => text.len(),
                MessageBody::Json { value } => {
                    usize::try_from(super::input_evidence_budget::serialized_bytes(value)?)?
                }
            },
            16,
        ),
        PreparedInputEvidence::Transcript(value) => {
            // transcript_text first tries an incoming message text, then joins
            // text blocks. Its temporary Vec contains borrowed str slots only.
            let message_text = value
                .data
                .get("body")
                .and_then(|body| {
                    body.get("Text")
                        .and_then(|v| v.get("text"))
                        .or_else(|| body.get("Brief").and_then(|v| v.get("text")))
                        .or_else(|| body.get("text"))
                        .and_then(serde_json::Value::as_str)
                })
                .filter(|text| !text.trim().is_empty());
            if matches!(
                value.kind,
                TranscriptEntryKind::IncomingMessage | TranscriptEntryKind::RuntimeFailure
            ) && message_text.is_some()
            {
                (message_text.unwrap().len(), 16)
            } else {
                let mut bytes = 0usize;
                let mut records = 16usize;
                if let Some(blocks) = value
                    .data
                    .get("blocks")
                    .and_then(serde_json::Value::as_array)
                {
                    for block in blocks {
                        if block.get("type").and_then(serde_json::Value::as_str) != Some("text") {
                            continue;
                        }
                        if let Some(text) = block
                            .get("Text")
                            .and_then(|v| v.get("text"))
                            .or_else(|| block.get("text"))
                            .and_then(serde_json::Value::as_str)
                        {
                            bytes = bytes
                                .checked_add(text.len())
                                .and_then(|n| n.checked_add(1))
                                .context("Work report joined text overflow")?;
                            records = records
                                .checked_add(1)
                                .context("Work report block slots overflow")?;
                        }
                    }
                }
                (bytes, records)
            }
        }
        _ => anyhow::bail!("unsupported Work report source family"),
    };
    let bytes = bytes
        .checked_add(
            records
                .checked_mul(256)
                .context("Work report slots overflow")?,
        )
        .and_then(|n| n.checked_add(256))
        .and_then(|n| n.checked_mul(8))
        .context("Work report output bytes overflow")?;
    let records = records
        .checked_mul(8)
        .context("Work report output records overflow")?;
    Ok((bytes, records))
}
impl CapturedWorkReportSources {
    #[cfg(test)]
    pub(in crate::runtime) fn records_for_ack_test(&self) -> &[Arc<QualifiedInputEvidence>] {
        &self.records
    }
    pub(in crate::runtime) fn capture(
        work: &CapturedWork,
        inputs: Option<&ResidentInputEvidence>,
        ledger: &InputEvidenceBudgetLedger,
        observe: impl FnMut((usize, usize), bool) -> Result<()>,
    ) -> Result<Self> {
        Self::capture_records(
            &work.scope,
            work.records.iter(),
            &work.legacy_delivery_heads,
            inputs,
            ledger,
            observe,
        )
    }
    pub(in crate::runtime) fn capture_records<'a>(
        scope: &InputEvidenceScope,
        records: impl IntoIterator<Item = &'a Arc<QualifiedWorkRecord>>,
        heads: &BTreeMap<String, WorkDeliveryHeads>,
        inputs: Option<&ResidentInputEvidence>,
        ledger: &InputEvidenceBudgetLedger,
        mut observe: impl FnMut((usize, usize), bool) -> Result<()>,
    ) -> Result<Self> {
        if let Some(inputs) = inputs {
            ensure!(
                inputs.belongs_to(scope) && inputs.shares_query_ledger(ledger),
                "Work report Input differs from selected Work scope/ledger"
            );
        }
        // Scope/Vec/set slots and guard bookkeeping precede their first clone.
        let bytes = usize::try_from(super::input_evidence_budget::serialized_bytes(scope)?)?
            .checked_add(4096)
            .and_then(|n| n.checked_mul(8))
            .context("Work report capture address overflow")?;
        observe((bytes, 32), true)?;
        let mut captured = Self {
            scope: scope.clone(),
            records: Vec::new(),
            unavailable: BTreeSet::new(),
            unprepared: BTreeSet::new(),
            _pins: None,
        };
        for qualified in records {
            let CanonicalWorkRecord::Work(record) = &qualified.record else {
                continue;
            };
            if record.state != WorkItemState::Completed {
                continue;
            }
            if let Some(id) = record
                .result_brief_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
            {
                if let Some(head) = captured.add(inputs, Family::Brief, id, false, &mut observe)? {
                    let PreparedInputEvidence::Brief(brief) = &head.record else {
                        anyhow::bail!("Work report Brief family differs")
                    };
                    ensure!(
                        brief.agent_id == scope.agent_id
                            && brief
                                .work_item_id
                                .as_deref()
                                .is_none_or(|id| id == record.id),
                        "Work report Brief association differs"
                    );
                    if !brief.text.trim().is_empty() {
                        if let BriefContentSource::TranscriptEntry { entry_id, .. } =
                            &brief.content_source
                        {
                            let absent=inputs.is_some_and(|inputs|inputs.work_report_optional_absence(
                                head,head,super::completion_sources::CompletionSourceEdge::BriefTranscript,
                                Family::Transcript,entry_id));
                            if let Some(source) = captured.add(
                                inputs,
                                Family::Transcript,
                                entry_id,
                                absent,
                                &mut observe,
                            )? {
                                let PreparedInputEvidence::Transcript(entry) = &source.record
                                else {
                                    anyhow::bail!("Work report Transcript family differs")
                                };
                                if entry.kind == TranscriptEntryKind::IncomingMessage {
                                    if let Some(id) = entry.related_message_id.as_deref() {
                                        let absent=inputs.is_some_and(|inputs|inputs.work_report_optional_absence(
                                            head,source,super::completion_sources::CompletionSourceEdge::IncomingTranscriptMessage,
                                            Family::Message,id));
                                        captured.add(
                                            inputs,
                                            Family::Message,
                                            id,
                                            absent,
                                            &mut observe,
                                        )?;
                                    }
                                }
                            }
                        }
                        continue;
                    }
                }
            }
            if let Some(heads) = heads.get(&record.id) {
                for head in [&heads.latest_any, &heads.latest_nonempty] {
                    if let WorkDeliveryHead::Present {
                        record_id,
                        content_digest,
                        source_revision,
                        availability: LegacyDeliverySourceAvailability::Present,
                    } = head
                    {
                        if let Some(value) = captured.add(
                            inputs,
                            Family::LegacyDelivery,
                            record_id,
                            false,
                            &mut observe,
                        )? {
                            let PreparedInputEvidence::LegacyDelivery(body) = &value.record else {
                                anyhow::bail!("Work report Legacy family differs")
                            };
                            ensure!(
                                value.locator.content_digest == *content_digest
                                    && (*source_revision == 0
                                        || value.locator.revision == *source_revision)
                                    && body.agent_id == scope.agent_id
                                    && body.work_item_id == record.id,
                                "Work report Legacy source version/association differs"
                            );
                        }
                    }
                }
            }
        }
        captured._pins =
            Some(ledger.register_view(scope, InputMemoryCategory::Pins, &captured.records)?);
        Ok(captured)
    }
    fn add<'a>(
        &mut self,
        inputs: Option<&'a ResidentInputEvidence>,
        family: Family,
        id: &str,
        optional_absence: bool,
        observe: &mut impl FnMut((usize, usize), bool) -> Result<()>,
    ) -> Result<Option<&'a Arc<QualifiedInputEvidence>>> {
        let value = inputs.and_then(|inputs| inputs.borrowed_work_report_source(family, id));
        if let Some(value) = value {
            ensure!(
                value.locator.scope == self.scope,
                "Work report body scope differs"
            );
            if !self.records.iter().any(|old| Arc::ptr_eq(old, value)) {
                let body = value.resident_material_estimate()?;
                observe(body, false)?;
                self.records.push(value.clone());
            }
            // Every owning Work renders its own text/DTO. Only the immutable
            // source Arc is deduplicated across owners and the two head roles.
            observe(report_output_material(&value.record)?, true)?;
            return Ok(Some(value));
        }
        if !self
            .unavailable
            .iter()
            .chain(&self.unprepared)
            .any(|key| key.family == family && key.id == id)
        {
            observe(
                (
                    id.len()
                        .checked_add(256)
                        .and_then(|n| n.checked_mul(8))
                        .context("Work report missing locator overflow")?,
                    16,
                ),
                true,
            )?;
            let key = Key::new(family, id);
            if optional_absence
                || inputs.is_some_and(|inputs| inputs.work_report_source_unavailable(family, id))
            {
                self.unavailable.insert(key);
            } else {
                self.unprepared.insert(key);
            }
        }
        Ok(None)
    }
    fn lookup(&self, family: Family, id: &str) -> Result<Option<&QualifiedInputEvidence>> {
        if let Some(record) = self
            .records
            .iter()
            .find(|record| record.locator.key.family == family && record.locator.key.id == id)
        {
            return Ok(Some(record));
        }
        if self
            .unavailable
            .iter()
            .any(|key| key.family == family && key.id == id)
        {
            return Ok(None);
        }
        Err(WorkReportReadError::NeedsPreparation(Key::new(family, id)).into())
    }
    pub(crate) fn brief_text<'a>(
        &'a self,
        work: &WorkItemRecord,
    ) -> Result<Option<(&'a BriefRecord, String)>> {
        let Some(id) = work
            .result_brief_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        else {
            return Ok(None);
        };
        let Some(record) = self.lookup(Family::Brief, id)? else {
            return Ok(None);
        };
        let PreparedInputEvidence::Brief(brief) = &record.record else {
            anyhow::bail!("Work report Brief family differs")
        };
        if brief.text.trim().is_empty() {
            return Ok(None);
        }
        let mut resolved = None;
        if let BriefContentSource::TranscriptEntry { entry_id, .. } = &brief.content_source {
            if let Some(record) = self.lookup(Family::Transcript, entry_id)? {
                let PreparedInputEvidence::Transcript(entry) = &record.record else {
                    anyhow::bail!("Work report Transcript family differs")
                };
                if entry.kind == TranscriptEntryKind::IncomingMessage {
                    if let Some(id) = entry.related_message_id.as_deref() {
                        if let Some(record) = self.lookup(Family::Message, id)? {
                            let PreparedInputEvidence::Message(message) = &record.record else {
                                anyhow::bail!("Work report Message family differs")
                            };
                            let text = match &message.body {
                                MessageBody::Text { text } | MessageBody::Brief { text, .. } => {
                                    (!text.trim().is_empty()).then(|| text.clone())
                                }
                                MessageBody::Json { value } => Some(serde_json::to_string(value)?),
                            };
                            // The original resolver returns directly when the
                            // Message exists, even when its text is empty.
                            return Ok(Some((brief, text.unwrap_or_else(|| brief.text.clone()))));
                        }
                    }
                }
                if resolved.is_none() {
                    resolved = crate::object_resolver::transcript_text(entry);
                }
            }
        }
        Ok(Some((
            brief,
            resolved.unwrap_or_else(|| brief.text.clone()),
        )))
    }
    pub(in crate::runtime) fn legacy_summary(
        &self,
        work_id: &str,
        head: &WorkDeliveryHead,
    ) -> Result<Option<&DeliverySummaryRecord>> {
        match head {
            WorkDeliveryHead::Absent => Ok(None),
            WorkDeliveryHead::Unavailable => {
                Err(WorkReportReadError::Unavailable(work_id.into()).into())
            }
            WorkDeliveryHead::Present {
                record_id,
                availability: LegacyDeliverySourceAvailability::Pruned,
                ..
            } => Err(WorkReportReadError::Pruned(record_id.clone()).into()),
            WorkDeliveryHead::Present { record_id, .. } => {
                let Some(record) = self.lookup(Family::LegacyDelivery, record_id)? else {
                    return Err(WorkReportReadError::NeedsPreparation(Key::new(
                        Family::LegacyDelivery,
                        record_id,
                    ))
                    .into());
                };
                let PreparedInputEvidence::LegacyDelivery(body) = &record.record else {
                    anyhow::bail!("Work report Legacy family differs")
                };
                ensure!(
                    body.work_item_id == work_id,
                    "Work report Legacy Work association differs"
                );
                Ok(Some(body))
            }
        }
    }
}

#[cfg(test)]
#[path = "work_report_sources_tests.rs"]
mod tests;

#[derive(Debug, Clone, Copy)]
pub(crate) enum OriginalWorkReportKind {
    ResultBrief,
    WorkItemResultSummary,
    DeliverySummary,
}
#[derive(Debug)]
pub(crate) struct OriginalWorkReportProjection {
    pub(crate) text: String,
    pub(crate) kind: OriginalWorkReportKind,
    pub(crate) brief_id: Option<String>,
    pub(crate) delivery_summary_id: Option<String>,
    pub(crate) source_turn_index: Option<u64>,
    pub(crate) created_at: Option<chrono::DateTime<chrono::Utc>>,
}
pub(crate) struct OriginalWorkReportRead {
    pub(in crate::runtime) target: Arc<QualifiedWorkRecord>,
    pub(in crate::runtime) projection: Option<OriginalWorkReportProjection>,
    pub(in crate::runtime) _sources: CapturedWorkReportSources,
    pub(in crate::runtime) _work_pin: Arc<BudgetViewGuard>,
    pub(in crate::runtime) _guards: Vec<Arc<BudgetViewGuard>>,
    pub(in crate::runtime) total_cost: (u64, usize),
    pub(in crate::runtime) retained_cost: (u64, usize),
    pub(in crate::runtime) attempt: [u8; 32],
    pub(in crate::runtime) head_digest: [u8; 32],
}
impl std::fmt::Debug for OriginalWorkReportRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalWorkReportRead")
            .field("scope", &self.target.scope)
            .field("cost", &self.total_cost)
            .field("attempt", &self.attempt)
            .finish_non_exhaustive()
    }
}
impl OriginalWorkReportRead {
    pub(in crate::runtime) fn validate_record(&self, work: &WorkItemRecord) -> Result<()> {
        ensure!(
            matches!(&self.target.record,CanonicalWorkRecord::Work(actual) if actual==work),
            "acknowledged Work report differs from original successful record"
        );
        Ok(())
    }
}
impl CapturedWorkReportSources {
    pub(in crate::runtime) fn validate_original_sources(
        &self,
        inputs: &ResidentInputEvidence,
    ) -> Result<()> {
        ensure!(
            inputs.belongs_to(&self.scope),
            "retried report Input scope differs"
        );
        for original in &self.records {
            let fresh = inputs
                .borrowed_work_report_source(original.locator.key.family, &original.locator.key.id)
                .context("retried original report source unavailable")?;
            ensure!(
                fresh.as_ref() == original.as_ref(),
                "retried original report source changed"
            );
        }
        Ok(())
    }
    pub(in crate::runtime) fn has_brief_report(&self, work: &WorkItemRecord) -> Result<bool> {
        let Some(id) = work
            .result_brief_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        else {
            return Ok(false);
        };
        Ok(self.lookup(Family::Brief,id)?.is_some_and(|record|
            matches!(&record.record,PreparedInputEvidence::Brief(brief) if !brief.text.trim().is_empty())))
    }
    pub(in crate::runtime) fn render_original_report(
        &self,
        work: &WorkItemRecord,
        head: &WorkDeliveryHead,
    ) -> Result<Option<OriginalWorkReportProjection>> {
        if let Some((brief, text)) = self.brief_text(work)? {
            return Ok(Some(OriginalWorkReportProjection {
                text,
                kind: OriginalWorkReportKind::ResultBrief,
                brief_id: Some(brief.id.clone()),
                delivery_summary_id: None,
                source_turn_index: brief.turn_index,
                created_at: Some(brief.created_at),
            }));
        }
        let legacy = self.legacy_summary(&work.id, head)?;
        let (text, kind) =
            if let Some(text) = work.result_summary.as_ref().filter(|text| !text.is_empty()) {
                (text.as_str(), OriginalWorkReportKind::WorkItemResultSummary)
            } else if let Some(legacy) = legacy.filter(|value| !value.text.is_empty()) {
                (
                    legacy.text.as_str(),
                    OriginalWorkReportKind::DeliverySummary,
                )
            } else {
                return Ok(None);
            };
        let metadata = legacy.filter(|value| value.text == text);
        Ok(Some(OriginalWorkReportProjection {
            text: text.to_owned(),
            kind,
            brief_id: None,
            delivery_summary_id: metadata.map(|value| value.id.clone()),
            source_turn_index: metadata.and_then(|value| value.source_turn_index),
            created_at: metadata.map(|value| value.created_at),
        }))
    }
}
