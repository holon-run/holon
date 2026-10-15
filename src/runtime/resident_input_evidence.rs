//! Qualified, bounded input/evidence allocations. No storage capability lives here.

use super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionOutcomeRecord, WorkItemExecutionRecord,
    },
    types::{
        AuthorityClass, BriefRecord, DeliverySummaryRecord, MessageEnvelope, ToolExecutionRecord,
        ToolExecutionStatus, TranscriptEntry, TurnOwner, TurnRecord,
    },
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[path = "input_capture_material.rs"]
mod capture_material;
#[path = "input_evidence_output_recall.rs"]
mod output_recall;
#[path = "input_query_reads.rs"]
mod query_reads;
use capture_material::CapturePhase;
pub(crate) use capture_material::{InputCaptureError, InputCaptureMaterial};

#[path = "execution_command_reads.rs"]
mod command_reads;
pub(crate) use command_reads::{ExecutionCommandRead, ExecutionCommandReadSource};

#[path = "input_evidence_execution.rs"]
mod execution;
pub(crate) use execution::{
    ExecutionInputCoverage, ExecutionInputDelta, ExecutionSourceCoverage, ExecutionWorkObservation,
    ExecutionWorkPresence, ObservedWorkExecution, SourceAttemptObservation,
};

pub(crate) const INPUT_EVIDENCE_POLICY: i64 = 1;
pub(crate) const LEGACY_INPUT_WINDOW: usize = 512;
pub(crate) const MAX_COMMAND_PREVIEW_INPUT_BYTES: usize = 64 * 1024;
pub(crate) const OVERSIZED_COMMAND_PREVIEW: &str =
    "[omitted: command exceeds context preview preparation limit]";
pub(crate) fn safe_command_preview(command: &str) -> String {
    if command.len() > MAX_COMMAND_PREVIEW_INPUT_BYTES {
        OVERSIZED_COMMAND_PREVIEW.to_string()
    } else if ["authorization:", "x-api-key:", "api-key:"]
        .iter()
        .any(|header| command.to_ascii_lowercase().contains(header))
    {
        "[omitted: command contains credential header]".to_string()
    } else {
        crate::tool::helpers::command_preview(command)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InputEvidenceScope {
    pub agent_id: String,
    pub identity_incarnation: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputEvidenceFamily {
    Message,
    Turn,
    ToolExecution,
    Brief,
    LegacyDelivery,
    Transcript,
    Attempt,
    Outcome,
    WorkExecution,
}
impl InputEvidenceFamily {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Turn => "turn",
            Self::ToolExecution => "tool_execution",
            Self::Brief => "brief",
            Self::LegacyDelivery => "legacy_delivery",
            Self::Transcript => "transcript",
            Self::Attempt => "attempt",
            Self::Outcome => "outcome",
            Self::WorkExecution => "work_execution",
        }
    }
    pub(crate) fn from_str(value: &str) -> Result<Self> {
        Ok(match value {
            "message" => Self::Message,
            "turn" => Self::Turn,
            "tool_execution" => Self::ToolExecution,
            "brief" => Self::Brief,
            "legacy_delivery" => Self::LegacyDelivery,
            "transcript" => Self::Transcript,
            "attempt" => Self::Attempt,
            "outcome" => Self::Outcome,
            "work_execution" => Self::WorkExecution,
            _ => anyhow::bail!("unsupported input evidence family: {value}"),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct InputEvidenceKey {
    pub family: InputEvidenceFamily,
    pub id: String,
}
impl InputEvidenceKey {
    pub(crate) fn new(family: InputEvidenceFamily, id: impl Into<String>) -> Self {
        Self {
            family,
            id: id.into(),
        }
    }
}

/// Accepted LegacyDelivery metadata keeps the two existing report selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LegacyDeliveryHeadSelector {
    LatestAny,
    LatestNonempty,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LegacyDeliverySourceAvailability {
    Present,
    Pruned,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LegacyDeliverySourceLocator {
    pub scope: InputEvidenceScope,
    pub key: InputEvidenceKey,
    /// Zero names a sealed fixed source; it is not a fabricated Input alias.
    pub source_revision: u64,
    pub content_digest: String,
    pub availability: LegacyDeliverySourceAvailability,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "presence", rename_all = "snake_case")]
pub(crate) enum LegacyDeliveryHeadObservation {
    Present { source: LegacyDeliverySourceLocator },
    Absent,
    Unavailable,
}

/// A durable locator describes canonical bytes; it does not retain those bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EvidenceLocator {
    pub scope: InputEvidenceScope,
    pub key: InputEvidenceKey,
    pub revision: u64,
    pub content_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolCommandProjection {
    pub command_preview: Option<String>,
    pub disposition: Option<String>,
    pub task_id: Option<String>,
    pub exit_status: Option<i64>,
    pub has_output_evidence: bool,
    pub truncated: bool,
    pub has_artifact: bool,
}
/// Input/output JSON, stdout and artifacts are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolContextProjection {
    pub id: String,
    pub agent_id: String,
    pub work_item_id: Option<String>,
    pub turn_id: Option<String>,
    pub turn_index: u64,
    pub tool_name: String,
    pub created_at: DateTime<Utc>,
    pub authority_class: AuthorityClass,
    pub status: ToolExecutionStatus,
    pub summary: String,
    pub command: ToolCommandProjection,
    pub batch_total: usize,
    pub batch_succeeded: usize,
    pub batch_failed: usize,
    pub batch_promoted: usize,
    pub batch_unknown: usize,
}
/// SQL already returned this exact canonical record. Project by borrowing its
/// JSON instead of serializing/cloning large command outputs into the root.
pub(crate) fn project_tool_record(record: &ToolExecutionRecord) -> Result<ToolContextProjection> {
    let output = record
        .output
        .get("result")
        .or_else(|| record.output.get("envelope").and_then(|x| x.get("result")))
        .unwrap_or(&record.output);
    let cut = |text: &str, limit: usize| text.chars().take(limit).collect::<String>();
    let disposition = output
        .get("disposition")
        .and_then(serde_json::Value::as_str);
    let task_id = output
        .get("task_handle")
        .and_then(|x| x.get("task_id"))
        .and_then(serde_json::Value::as_str);
    ensure!(
        record.id.len() <= 4096
            && record.agent_id.len() <= 4096
            && record.tool_name.len() <= 512
            && record.work_item_id.as_ref().is_none_or(|x| x.len() <= 4096)
            && record.turn_id.as_ref().is_none_or(|x| x.len() <= 4096)
            && disposition.is_none_or(|x| x.len() <= 512)
            && task_id.is_none_or(|x| x.len() <= 4096),
        "tool context metadata exceeds bounded projection"
    );
    let command = ToolCommandProjection {
        command_preview: record
            .input
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .map(safe_command_preview),
        disposition: disposition.map(str::to_owned),
        task_id: task_id.map(str::to_owned),
        exit_status: output
            .get("exit_status")
            .or_else(|| output.get("exit_code"))
            .and_then(serde_json::Value::as_i64),
        has_output_evidence: [
            "stdout_preview",
            "stderr_preview",
            "initial_output_preview",
            "stdout_artifact",
            "stderr_artifact",
        ]
        .iter()
        .any(|key| output.get(key).is_some()),
        truncated: [
            "truncated",
            "initial_output_truncated",
            "stdout_truncated",
            "stderr_truncated",
            "output_truncated",
        ]
        .iter()
        .any(|key| {
            output
                .get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        }),
        has_artifact: output
            .get("artifacts")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|x| !x.is_empty())
            || [
                "artifact",
                "stdout_artifact",
                "stderr_artifact",
                "output_artifact",
            ]
            .iter()
            .any(|key| output.get(key).is_some()),
    };
    let total = record
        .input
        .get("items")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let mut result = ToolContextProjection {
        id: record.id.clone(),
        agent_id: record.agent_id.clone(),
        work_item_id: record.work_item_id.clone(),
        turn_id: record.turn_id.clone(),
        turn_index: record.turn_index,
        tool_name: record.tool_name.clone(),
        created_at: record.created_at,
        authority_class: record.authority_class,
        status: record.status.clone(),
        summary: cut(&record.summary, 4096),
        command,
        batch_total: total,
        batch_succeeded: 0,
        batch_failed: 0,
        batch_promoted: 0,
        batch_unknown: 0,
    };
    if record.tool_name == "ExecCommandBatch" {
        let mut seen = 0;
        if let Some(items) = output.get("items").and_then(serde_json::Value::as_array) {
            for item in items.iter().take(total) {
                seen += 1;
                let value = item.get("result").unwrap_or(item);
                if matches!(
                    value.get("disposition").and_then(serde_json::Value::as_str),
                    Some("promoted_to_task" | "already_running")
                ) {
                    result.batch_promoted += 1
                } else {
                    match value
                        .get("exit_status")
                        .or_else(|| value.get("exit_code"))
                        .and_then(serde_json::Value::as_i64)
                    {
                        Some(0) => result.batch_succeeded += 1,
                        Some(_) => result.batch_failed += 1,
                        None => result.batch_unknown += 1,
                    }
                }
            }
        }
        result.batch_unknown += total.saturating_sub(seen);
    }
    Ok(result)
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", content = "record", rename_all = "snake_case")]
pub(crate) enum PreparedInputEvidence {
    Message(MessageEnvelope),
    Turn(TurnRecord),
    ToolExecution(ToolContextProjection),
    Brief(BriefRecord),
    LegacyDelivery(DeliverySummaryRecord),
    Transcript(TranscriptEntry),
    Attempt(ExecutionAttempt),
    Outcome(ExecutionOutcomeRecord),
    WorkExecution {
        work_item_id: String,
        record: WorkItemExecutionRecord,
    },
}
impl PreparedInputEvidence {
    pub(crate) fn key(&self) -> InputEvidenceKey {
        match self {
            Self::Message(x) => InputEvidenceKey::new(InputEvidenceFamily::Message, &x.id),
            Self::Turn(x) => InputEvidenceKey::new(InputEvidenceFamily::Turn, &x.turn_id),
            Self::ToolExecution(x) => {
                InputEvidenceKey::new(InputEvidenceFamily::ToolExecution, &x.id)
            }
            Self::Brief(x) => InputEvidenceKey::new(InputEvidenceFamily::Brief, &x.id),
            Self::LegacyDelivery(x) => {
                InputEvidenceKey::new(InputEvidenceFamily::LegacyDelivery, &x.id)
            }
            Self::Transcript(x) => InputEvidenceKey::new(InputEvidenceFamily::Transcript, &x.id),
            Self::Attempt(x) => InputEvidenceKey::new(InputEvidenceFamily::Attempt, &x.attempt_id),
            Self::Outcome(x) => InputEvidenceKey::new(InputEvidenceFamily::Outcome, &x.outcome_id),
            Self::WorkExecution { work_item_id, .. } => {
                InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, work_item_id)
            }
        }
    }
    pub(crate) fn references(&self) -> BTreeSet<InputEvidenceKey> {
        let mut refs = BTreeSet::new();
        self.visit_references(|family, id| {
            refs.insert(InputEvidenceKey::new(family, id));
        });
        refs
    }
    /// Borrow original owning IDs before allocating their selector containers.
    pub(crate) fn visit_references(&self, mut add: impl FnMut(InputEvidenceFamily, &str)) {
        match self {
            Self::Message(x) => {
                if let Some(id) = crate::wake_contract::agent_message_reply_reference(x) {
                    add(InputEvidenceFamily::Message, id)
                }
            }
            Self::Turn(x) => {
                if let Some(id) = x
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref())
                {
                    add(InputEvidenceFamily::Message, id)
                }
                for id in &x.input_message_ids {
                    add(InputEvidenceFamily::Message, id)
                }
                for id in &x.tool_execution_ids {
                    add(InputEvidenceFamily::ToolExecution, id)
                }
                for id in &x.produced_brief_ids {
                    add(InputEvidenceFamily::Brief, id)
                }
                if let Some(replay) = &x.replay {
                    // The source message's actual assignment owns the prior
                    // Turn. source_turn_id may name an unstarted preallocation.
                    add(InputEvidenceFamily::Message, &replay.source_message_id);
                }
            }
            Self::Brief(x) => {
                if let Some(id) = x.related_message_id.as_deref() {
                    add(InputEvidenceFamily::Message, id)
                }
                if let Some(id) = x.content_source.transcript_entry_id() {
                    add(InputEvidenceFamily::Transcript, id)
                }
            }
            Self::Attempt(x) => {
                if let Some(id) = x.source_message_id.as_deref() {
                    add(InputEvidenceFamily::Message, id)
                }
                if let Some(id) = x.recovery_of_attempt_id.as_deref() {
                    add(InputEvidenceFamily::Attempt, id)
                }
                if let Some(id) = x.terminal_outcome_id.as_deref() {
                    add(InputEvidenceFamily::Outcome, id)
                }
                if let crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                    work_item_id,
                } = &x.binding
                {
                    add(InputEvidenceFamily::WorkExecution, work_item_id)
                }
            }
            Self::Transcript(x) => {
                if let Some(id) = x.related_message_id.as_deref() {
                    add(InputEvidenceFamily::Message, id)
                }
            }
            Self::Outcome(x) => add(InputEvidenceFamily::Attempt, &x.attempt_id),
            Self::WorkExecution { record, .. } => {
                if let crate::domain::execution_protocol::WorkItemExecutionState::InFlight {
                    attempt_id,
                    ..
                } = &record.state
                {
                    add(InputEvidenceFamily::Attempt, attempt_id)
                }
            }
            _ => {}
        }
    }
}
/// Only the original named read actor constructs this from an exact D3 locator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QualifiedToolRunReport {
    source: EvidenceLocator,
    projection: super::run_report_projection::ToolRunReportProjection,
}
impl QualifiedToolRunReport {
    pub(crate) fn from_qualified_source(
        source: EvidenceLocator,
        projection: super::run_report_projection::ToolRunReportProjection,
    ) -> Self {
        Self { source, projection }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QualifiedInputEvidence {
    pub locator: EvidenceLocator,
    pub record: PreparedInputEvidence,
    pub created_at: String,
    pub order_sequence: Option<u64>,
    pub turn_index: Option<u64>,
    pub owner: Option<TurnOwner>,
    pub resident_bytes: usize,
    /// Some(empty) is an exact qualified claim-only observation. An Attempt
    /// without this metadata is not a prepared closure.
    pub attempt_turn_keys: Option<BTreeSet<InputEvidenceKey>>,
    /// Old projections remain incomplete for reports; the prompt cap is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_run_report: Option<Box<QualifiedToolRunReport>>,
}
impl QualifiedInputEvidence {
    /// Actual immutable allocation floor, independent of caller/Serde estimates.
    /// The encoded projection field retains its original policy semantics.
    pub(crate) fn resident_material_estimate(&self) -> Result<(usize, usize)> {
        let (encoded, nodes) = super::input_evidence_budget::borrowed_json_material(&(
            &self.locator,
            &self.record,
            &self.created_at,
            self.order_sequence,
            self.turn_index,
            &self.owner,
            &self.attempt_turn_keys,
            &self.tool_run_report,
        ))?;
        let encoded = usize::try_from(encoded)?.max(self.resident_bytes);
        let bytes = nodes
            .checked_mul(256)
            .and_then(|n| n.checked_add(encoded))
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>()))
            .context("Input native allocation estimate overflow")?;
        let records = nodes
            .checked_add(1)
            .context("Input native record count overflow")?;
        Ok((bytes, records))
    }
    pub(crate) fn tool_run_report(
        &self,
    ) -> Result<&super::run_report_projection::ToolRunReportProjection> {
        let report = self.tool_run_report.as_ref().filter(|report| {
            self.locator.key.family == InputEvidenceFamily::ToolExecution
                && matches!(&self.record, PreparedInputEvidence::ToolExecution(tool) if tool.id == self.locator.key.id && tool.agent_id == self.locator.scope.agent_id
                    && self.created_at.parse::<DateTime<Utc>>().ok() == Some(tool.created_at))
                && report.source == self.locator
        }).ok_or_else(|| InputEvidenceReadError::NeedsPreparation(self.locator.key.clone()))?;
        Ok(&report.projection)
    }
    /// A named read may enrich one exact locator without changing its canonical facts.
    fn same_revision_allocation(original: &Arc<Self>, incoming: &Arc<Self>) -> Result<Arc<Self>> {
        ensure!(
            original.locator == incoming.locator
                && original.record == incoming.record
                && original.created_at == incoming.created_at
                && original.order_sequence == incoming.order_sequence
                && original.turn_index == incoming.turn_index
                && original.owner == incoming.owner
                && original.resident_bytes == incoming.resident_bytes
                && original.attempt_turn_keys == incoming.attempt_turn_keys,
            "same qualified revision has different canonical evidence"
        );
        if original.tool_run_report.is_some() {
            original.tool_run_report()?;
        }
        if incoming.tool_run_report.is_some() {
            incoming.tool_run_report()?;
        }
        match (&original.tool_run_report, &incoming.tool_run_report) {
            (Some(before), Some(after)) => {
                ensure!(
                    before == after,
                    "same qualified Tool source has different reports"
                );
                Ok(original.clone())
            }
            (None, Some(_)) => Ok(incoming.clone()),
            _ => Ok(original.clone()),
        }
    }
    pub(crate) fn visit_references(&self, mut add: impl FnMut(InputEvidenceFamily, &str)) {
        self.record.visit_references(&mut add);
        if let Some(keys) = &self.attempt_turn_keys {
            for key in keys {
                add(key.family, &key.id);
            }
        }
    }
    pub(crate) fn references(&self) -> BTreeSet<InputEvidenceKey> {
        let mut refs = self.record.references();
        if let Some(keys) = &self.attempt_turn_keys {
            refs.extend(keys.iter().cloned());
        }
        refs
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "selector", rename_all = "snake_case")]
pub(crate) enum InputWindowSelector {
    GlobalTurns,
    OwnerTurns { owner: TurnOwner },
    RecentBriefs,
    RecentTools,
    RecentTranscript,
    TrustedOperator,
}
impl InputWindowSelector {
    /// Public history policy remains <=512; only internal Turn acquisition may
    /// prepare one additional already-qualified candidate to exclude current input.
    pub(crate) fn maximum_candidates(&self) -> usize {
        LEGACY_INPUT_WINDOW
            + usize::from(matches!(self, Self::GlobalTurns | Self::OwnerTurns { .. }))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InputWindowCoverage {
    pub selector: InputWindowSelector,
    pub limit: usize,
    pub keys: Vec<InputEvidenceKey>,
    pub is_exhaustive: bool,
}
/// Focused same-read-view absence of an asserted recovery source. This is not
/// ordinary lookup availability or permission to omit a required context body.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RecoverySourceAbsence {
    pub recovery_message: EvidenceLocator,
    pub source_message_id: String,
    pub source_turn_id: String,
    pub terminal_kind: crate::types::TurnTerminalKind,
    pub absent: BTreeSet<InputEvidenceKey>,
}
impl RecoverySourceAbsence {
    fn matches(&self, record: &QualifiedInputEvidence) -> bool {
        let PreparedInputEvidence::Message(message) = &record.record else {
            return false;
        };
        let Ok(Some((turn, source, terminal))) =
            super::provider_recovery::continuity_sources(message)
        else {
            return false;
        };
        self.recovery_message == record.locator
            && self.source_message_id == source
            && self.source_turn_id == turn
            && self.terminal_kind == terminal
            && !self.absent.is_empty()
            && self.absent.iter().all(|key| {
                (key.family == InputEvidenceFamily::Message && key.id == source)
                    || (key.family == InputEvidenceFamily::Turn && key.id == turn)
            })
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InputEvidenceCoverage {
    pub query: super::input_queries::InputQueryCoverage,
    pub selected_completion_brief_ids: BTreeSet<String>,
    pub completion_source_observations:
        Vec<super::completion_sources::CompletionOptionalSourceObservation>,
    pub recovery_source_absences: BTreeMap<String, RecoverySourceAbsence>,
    /// Qualified incoming Messages whose raw recovery lineage was observed.
    pub recovery_lineage_messages: BTreeSet<String>,
    pub execution: ExecutionInputCoverage,
    pub windows: Vec<InputWindowCoverage>,
    pub mandatory_roots: BTreeSet<InputEvidenceKey>,
    /// Frozen qualified Transcript IDs for selected Turns, including tombstones.
    pub turn_transcript_keys: BTreeMap<String, BTreeSet<InputEvidenceKey>>,
    /// None is an actual same-view unassigned input, not a missing preparation.
    pub message_turn_keys: BTreeMap<String, Option<String>>,
    pub unavailable: BTreeSet<InputEvidenceKey>,
    pub record_limit: usize,
    pub byte_limit: usize,
}
#[derive(Debug, Clone)]
pub(crate) struct CommittedEvidenceRemoval {
    pub scope: InputEvidenceScope,
    pub key: InputEvidenceKey,
    pub revision: u64,
    pub previous_content_digest: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct CommittedInputEvidenceDelta {
    pub query_advance:
        Option<Arc<crate::runtime_db::input_evidence_snapshot::CommittedInputQueryAdvance>>,
    pub output_recall_invalidation:
        Option<crate::runtime_db::input_evidence_snapshot::OutputRecallInvalidation>,
    pub execution: ExecutionInputDelta,
    pub records: Vec<Arc<QualifiedInputEvidence>>,
    pub removals: Vec<CommittedEvidenceRemoval>,
    /// Exact qualified Transcript frontier for each Turn included in this commit.
    pub turn_transcript_keys: BTreeMap<String, BTreeSet<InputEvidenceKey>>,
    /// None is an actual same-view unassigned input, not a missing preparation.
    pub message_turn_keys: BTreeMap<String, Option<String>>,
}

#[path = "input_publication_material.rs"]
mod publication_material;
pub(in crate::runtime) use publication_material::OriginalInputPublicationMaterial;

#[derive(Debug, Clone)]
pub(crate) struct ResidentInputEvidence {
    output_recall_roots: BTreeSet<InputEvidenceKey>,
    output_recall_task: Option<super::resident_task::TaskFactLocator>,
    scope: InputEvidenceScope,
    records: BTreeMap<InputEvidenceKey, Arc<QualifiedInputEvidence>>,
    coverage: InputEvidenceCoverage,
    budget: Option<InputEvidenceBudgetLedger>,
    budget_views: Vec<Arc<BudgetViewGuard>>,
}
/// This object retains selected allocations, not the domain map or published root.
#[derive(Debug)]
pub(crate) struct CapturedInputEvidence {
    #[cfg(test)]
    pub selected_completion_brief_ids: BTreeSet<String>,
    #[cfg(test)]
    pub completion_source_observations:
        Vec<super::completion_sources::CompletionOptionalSourceObservation>,
    pub recovery_source_absences: BTreeMap<String, RecoverySourceAbsence>,
    pub execution: ExecutionInputCoverage,
    pub scope: InputEvidenceScope,
    pub budget: Option<InputEvidenceBudgetLedger>,
    pub budget_views: Vec<Arc<BudgetViewGuard>>,
    pub windows: Vec<InputWindowCoverage>,
    pub records: Vec<Arc<QualifiedInputEvidence>>,
    pub unavailable: BTreeSet<InputEvidenceKey>,
    /// Cloned metadata for selected Turns only; no domain/root map is retained.
    pub turn_transcript_keys: BTreeMap<String, BTreeSet<InputEvidenceKey>>,
    /// None is an actual same-view unassigned input, not a missing preparation.
    pub message_turn_keys: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum InputEvidenceReadError {
    #[error("input evidence was not prepared: {0:?}")]
    NeedsPreparation(InputEvidenceKey),
    #[error("input evidence window was not prepared for the requested policy")]
    WindowNotPrepared,
}
impl CapturedInputEvidence {
    pub(crate) fn selected_material_estimate(&self) -> Result<(u64, usize)> {
        let mut bytes = serialized_bytes(&(
            &self.scope,
            &self.windows,
            &self.unavailable,
            &self.turn_transcript_keys,
            &self.message_turn_keys,
            &self.recovery_source_absences,
            &self.execution,
        ))?;
        let mut count = self
            .records
            .len()
            .checked_add(self.windows.len())
            .and_then(|n| n.checked_add(self.unavailable.len()))
            .and_then(|n| n.checked_add(self.turn_transcript_keys.len()))
            .and_then(|n| n.checked_add(self.message_turn_keys.len()))
            .and_then(|n| n.checked_add(self.recovery_source_absences.len()))
            .and_then(|n| n.checked_add(self.execution.metadata_count()))
            .context("selected Input material count overflow")?;
        for window in &self.windows {
            count = count
                .checked_add(window.keys.len())
                .context("selected Input window count overflow")?;
        }
        for keys in self.turn_transcript_keys.values() {
            count = count
                .checked_add(keys.len())
                .context("selected Input frontier count overflow")?;
        }
        for proof in self.recovery_source_absences.values() {
            count = count
                .checked_add(proof.absent.len())
                .context("selected Input absence count overflow")?;
        }
        bytes = bytes
            .checked_add(u64::try_from(
                count
                    .checked_mul(256)
                    .context("selected Input metadata slots overflow")?,
            )?)
            .context("selected Input metadata bytes overflow")?;
        for record in &self.records {
            bytes = bytes
                .checked_add(u64::try_from(record.resident_bytes)?)
                .context("selected Input body material overflow")?;
        }
        Ok((bytes, count))
    }
    /// Authority validation only. Unknown/pruned/unqualified keys never become
    /// None through this proof; ordinary lookup remains strict.
    pub(crate) fn reject_proven_missing_recovery_source(
        &self,
        message: &MessageEnvelope,
    ) -> Result<()> {
        let Some(proof) = self.recovery_source_absences.get(&message.id) else {
            return Ok(());
        };
        let key = InputEvidenceKey::new(InputEvidenceFamily::Message, &message.id);
        let record = self
            .lookup(&key)?
            .ok_or_else(|| InputEvidenceReadError::NeedsPreparation(key))?;
        ensure!(
            proof.matches(record)
                && matches!(&record.record, PreparedInputEvidence::Message(stored) if stored == message),
            "recovery source absence differs from its qualified message version"
        );
        // Any non-absent asserted source was independently prepared and must
        // still be present; a prune/missing preparation is not invalid authority.
        for key in [
            InputEvidenceKey::new(InputEvidenceFamily::Message, &proof.source_message_id),
            InputEvidenceKey::new(InputEvidenceFamily::Turn, &proof.source_turn_id),
        ] {
            if !proof.absent.contains(&key) && self.lookup(&key)?.is_none() {
                return Err(InputEvidenceReadError::NeedsPreparation(key).into());
            }
        }
        anyhow::bail!(
            "provider recovery asserted source is absent in the captured canonical read view"
        )
    }
    pub(crate) fn lookup(
        &self,
        key: &InputEvidenceKey,
    ) -> std::result::Result<Option<&Arc<QualifiedInputEvidence>>, InputEvidenceReadError> {
        if let Some(record) = self
            .records
            .iter()
            .find(|record| &record.locator.key == key)
        {
            return Ok(Some(record));
        }
        if self.unavailable.contains(key) {
            return Ok(None);
        }
        Err(InputEvidenceReadError::NeedsPreparation(key.clone()))
    }
    /// Take newest K qualified window members, then return canonical chronology.
    pub(crate) fn window(
        &self,
        selector: &InputWindowSelector,
        limit: usize,
    ) -> std::result::Result<Vec<Arc<QualifiedInputEvidence>>, InputEvidenceReadError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let window = self
            .windows
            .iter()
            .find(|window| &window.selector == selector)
            .filter(|window| limit <= window.limit)
            .ok_or(InputEvidenceReadError::WindowNotPrepared)?;
        let mut records = window
            .keys
            .iter()
            .take(limit)
            .map(|key| {
                self.records
                    .iter()
                    .find(|record| &record.locator.key == key)
                    .cloned()
                    .ok_or_else(|| InputEvidenceReadError::NeedsPreparation(key.clone()))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        records.reverse();
        Ok(records)
    }
    pub(crate) fn transcripts_for_turns(
        &self,
        turn_ids: &[String],
    ) -> std::result::Result<Vec<Arc<QualifiedInputEvidence>>, InputEvidenceReadError> {
        let mut ids = BTreeSet::new();
        for turn_id in turn_ids {
            let Some(keys) = self.turn_transcript_keys.get(turn_id) else {
                return Err(InputEvidenceReadError::NeedsPreparation(
                    InputEvidenceKey::new(InputEvidenceFamily::Turn, turn_id),
                ));
            };
            ids.extend(keys.iter());
        }
        let mut entries = Vec::new();
        for key in ids {
            match self.lookup(key)? {
                Some(evidence)
                    if matches!(evidence.record, PreparedInputEvidence::Transcript(_)) =>
                {
                    entries.push(evidence.clone())
                }
                None => {} // Explicit prune availability remains in `unavailable`.
                _ => return Err(InputEvidenceReadError::NeedsPreparation(key.clone())),
            }
        }
        entries.sort_by(|left, right| {
            left.order_sequence
                .unwrap_or(u64::MAX)
                .cmp(&right.order_sequence.unwrap_or(u64::MAX))
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.locator.key.id.cmp(&right.locator.key.id))
        });
        Ok(entries)
    }
    pub(crate) fn protocol_state(
        &self,
    ) -> Result<crate::domain::execution_protocol::ExecutionProtocolState> {
        self.protocol_state_observed(&mut None)
    }
    pub(in crate::runtime) fn protocol_state_observed(
        &self,
        observer: &mut Option<&mut dyn FnMut(u64, usize) -> Result<()>>,
    ) -> Result<crate::domain::execution_protocol::ExecutionProtocolState> {
        use crate::work_item_scheduling::{
            observe_scheduling_clone as observe, observe_scheduling_slots as slots,
        };
        observe(&self.scope.agent_id, observer)?;
        slots(16, observer)?;
        let mut state =
            crate::domain::execution_protocol::ExecutionProtocolState::empty(&self.scope.agent_id);
        for evidence in &self.records {
            match &evidence.record {
                PreparedInputEvidence::Attempt(x) => {
                    observe(&x.attempt_id, observer)?;
                    observe(x, observer)?;
                    slots(2, observer)?;
                    state.attempts.insert(x.attempt_id.clone(), x.clone());
                }
                PreparedInputEvidence::Outcome(x) => {
                    observe(&x.outcome_id, observer)?;
                    observe(x, observer)?;
                    slots(2, observer)?;
                    state.outcomes.insert(x.outcome_id.clone(), x.clone());
                }
                PreparedInputEvidence::WorkExecution {
                    work_item_id,
                    record,
                } => {
                    observe(work_item_id, observer)?;
                    observe(record, observer)?;
                    slots(2, observer)?;
                    state
                        .work_items
                        .insert(work_item_id.clone(), record.clone());
                }
                _ => {}
            }
        }
        crate::domain::execution_protocol::assert_invariants(&state).map_err(anyhow::Error::msg)?;
        Ok(state)
    }
}
impl ResidentInputEvidence {
    pub(crate) fn bootstrap(
        scope: InputEvidenceScope,
        records: Vec<Arc<QualifiedInputEvidence>>,
        coverage: InputEvidenceCoverage,
    ) -> Result<Self> {
        Self::bootstrap_observed(scope, records, coverage, &mut CapturePhase::unobserved())
    }
    fn bootstrap_observed(
        scope: InputEvidenceScope,
        records: Vec<Arc<QualifiedInputEvidence>>,
        coverage: InputEvidenceCoverage,
        phase: &mut CapturePhase<'_>,
    ) -> Result<Self> {
        ensure!(
            scope.identity_incarnation > 0 && !scope.agent_id.is_empty(),
            "invalid input evidence scope"
        );
        let mut map = BTreeMap::new();
        for record in records {
            ensure!(
                record.locator.scope == scope,
                "input evidence scope mismatch"
            );
            phase.json(&record.locator.key, 1)?;
            let key = record.record.key();
            ensure!(key == record.locator.key, "input evidence key mismatch");
            if let Some(previous) = map.insert(key, record.clone()) {
                ensure!(
                    previous == record,
                    "conflicting bootstrap evidence allocation"
                );
            }
        }
        let mut result = Self {
            output_recall_roots: BTreeSet::new(),
            output_recall_task: None,
            scope,
            records: map,
            coverage,
            budget: None,
            budget_views: Vec::new(),
        };
        result.trim_and_validate_observed(phase)?;
        Ok(result)
    }
    /// Observation guards own no evidence. The real domain/capture owns the
    /// allocations and the ledger keeps only Weak references to them.
    pub(crate) fn with_budget(mut self, budget: InputEvidenceBudgetLedger) -> Result<Self> {
        self.budget = Some(budget);
        self.refresh_budget_views()?;
        if let Some(budget) = &self.budget {
            budget.check_history_budget()?;
        }
        Ok(self)
    }

    fn record_closure(
        &self,
        roots: BTreeSet<InputEvidenceKey>,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        let mut selected = roots;
        let mut pending = VecDeque::from_iter(selected.iter().cloned());
        while let Some(key) = pending.pop_front() {
            if let Some(record) = self.records.get(&key) {
                for child in self.references(record)? {
                    if selected.insert(child.clone()) {
                        pending.push_back(child);
                    }
                }
            }
            ensure!(
                selected.len() <= self.coverage.record_limit,
                "input memory closure exceeds count limit"
            );
        }
        Ok(selected)
    }

    fn refresh_budget_views(&mut self) -> Result<()> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        let mut strict_current = self.coverage.mandatory_roots.clone();
        strict_current.extend(
            self.coverage
                .execution
                .required_body_keys(&self.coverage.mandatory_roots),
        );
        let mut current_roots = strict_current.clone();
        current_roots.extend(self.coverage.query.observation_keys().cloned());
        current_roots.extend(
            self.coverage
                .selected_completion_brief_ids
                .iter()
                .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Brief, id)),
        );
        let current = self.completion_closure(
            current_roots,
            strict_current,
            &self.coverage.selected_completion_brief_ids,
            self.coverage.record_limit,
        )?;
        let history = self.record_closure(
            self.coverage
                .windows
                .iter()
                .flat_map(|window| window.keys.iter().cloned())
                .chain(self.output_recall_roots.iter().cloned())
                .collect(),
        )?;
        let records = |keys: BTreeSet<InputEvidenceKey>| {
            keys.into_iter()
                .filter_map(|key| self.records.get(&key).cloned())
                .collect::<Vec<_>>()
        };
        let views = vec![
            budget.register_view(&self.scope, InputMemoryCategory::Current, &records(current))?,
            budget.register_view(&self.scope, InputMemoryCategory::History, &records(history))?,
            budget.register_owned_bytes(
                &self.scope,
                InputMemoryCategory::Current,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: self.metadata_estimate_bytes()?,
                },
            )?,
        ];
        self.budget_views = views;
        Ok(())
    }

    fn metadata_estimate_bytes(&self) -> Result<u64> {
        let nodes = self
            .records
            .len()
            .checked_mul(std::mem::size_of::<(
                InputEvidenceKey,
                Arc<QualifiedInputEvidence>,
            )>())
            .ok_or_else(|| anyhow::anyhow!("input metadata estimate overflow"))?;
        let recall_task_bytes = serialized_bytes(&self.output_recall_task)?;
        Ok(serialized_bytes(&self.coverage)?
            .checked_add(serialized_bytes(&self.output_recall_roots)?)
            .and_then(|bytes| bytes.checked_add(recall_task_bytes))
            .and_then(|bytes| bytes.checked_add(nodes as u64))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>() as u64))
            .ok_or_else(|| anyhow::anyhow!("input metadata estimate overflow"))?)
    }

    pub(in crate::runtime) fn query_coverage(&self) -> &super::input_queries::InputQueryCoverage {
        &self.coverage.query
    }
    pub(in crate::runtime) fn shares_query_ledger(
        &self,
        ledger: &InputEvidenceBudgetLedger,
    ) -> bool {
        self.budget
            .as_ref()
            .is_some_and(|actual| actual.same_ledger(ledger))
    }
    pub(in crate::runtime) fn acquisition_registration_count(&self) -> Result<usize> {
        Ok(self.records.len())
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(in crate::runtime) fn validate_capture_material_binding(
        &self,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.shares_query_ledger(ledger) && !self.budget_views.is_empty(),
            "scheduler domain material ledger differs"
        );
        for guard in &self.budget_views {
            guard.validate_owned_material_binding(&self.scope, ledger, 0)?;
        }
        Ok(())
    }
    /// Check only original selected physical allocations, without a Root loan.
    pub(in crate::runtime) fn validate_scheduler_body_loan(
        &self,
        loan: &CapturedInputEvidence,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == loan.scope
                && self.shares_query_ledger(ledger)
                && loan
                    .budget
                    .as_ref()
                    .is_some_and(|actual| actual.same_ledger(ledger))
                && !loan.budget_views.is_empty(),
            "scheduler Input loan scope or ledger differs"
        );
        for guard in &loan.budget_views {
            guard.validate_owned_material_binding(&self.scope, ledger, 0)?;
        }
        for record in &loan.records {
            ensure!(
                self.records
                    .get(&record.locator.key)
                    .is_some_and(|current| Arc::ptr_eq(current, record)),
                "scheduler Input loan is not this publication allocation"
            );
        }
        Ok(())
    }
    pub(crate) fn belongs_to(&self, scope: &InputEvidenceScope) -> bool {
        &self.scope == scope
    }
    pub(crate) fn budget_records(&self) -> Vec<Arc<QualifiedInputEvidence>> {
        self.records.values().cloned().collect()
    }
    pub(crate) fn mandatory_roots(&self) -> &BTreeSet<InputEvidenceKey> {
        &self.coverage.mandatory_roots
    }
    pub(in crate::runtime) fn borrowed_work_report_source(
        &self,
        family: InputEvidenceFamily,
        id: &str,
    ) -> Option<&Arc<QualifiedInputEvidence>> {
        self.records
            .values()
            .find(|record| record.locator.key.family == family && record.locator.key.id == id)
    }
    pub(in crate::runtime) fn work_report_source_unavailable(
        &self,
        family: InputEvidenceFamily,
        id: &str,
    ) -> bool {
        self.coverage
            .unavailable
            .iter()
            .any(|key| key.family == family && key.id == id)
    }
    pub(in crate::runtime) fn work_report_optional_absence(
        &self,
        head: &QualifiedInputEvidence,
        source: &QualifiedInputEvidence,
        edge: super::completion_sources::CompletionSourceEdge,
        family: InputEvidenceFamily,
        id: &str,
    ) -> bool {
        self.coverage
            .completion_source_observations
            .iter()
            .any(|proof| {
                proof.head == head.locator
                    && proof.source == source.locator
                    && proof.edge == edge
                    && proof.declared_target.family == family
                    && proof.declared_target.id == id
            })
    }
    /// Pure dependency check only; the Work owner validates the shared scope.
    pub(crate) fn covers_work_input_sources(
        &self,
        coverage: &super::resident_work::WorkCoverage,
        roots: &BTreeSet<InputEvidenceKey>,
    ) -> bool {
        if !roots.iter().all(|key| {
            self.coverage.mandatory_roots.contains(key) && self.records.contains_key(key)
        }) {
            return false;
        }
        if !coverage.latest_results.values().flatten().all(|head| {
            self.coverage
                .selected_completion_brief_ids
                .contains(&head.brief_id)
        }) {
            return false;
        }
        coverage.legacy_delivery_heads.iter().all(|(work_id, heads)|
            [&heads.latest_any, &heads.latest_nonempty].into_iter().all(|head| match head {
                super::resident_work::WorkDeliveryHead::Present {record_id,content_digest,source_revision,
                    availability:LegacyDeliverySourceAvailability::Present} => self.coverage.mandatory_roots.iter().any(|key|
                        key.family == InputEvidenceFamily::LegacyDelivery && key.id == *record_id)
                        && self.records.values().any(|record|
                    record.locator.key.family == InputEvidenceFamily::LegacyDelivery
                    && record.locator.key.id == *record_id && record.locator.content_digest == *content_digest
                    && (*source_revision == 0 || record.locator.revision == *source_revision)
                    && matches!(&record.record, PreparedInputEvidence::LegacyDelivery(body) if body.work_item_id == *work_id)),
                _ => true,
            }))
    }
    /// Preserve already prepared selectors at a named Work owner boundary.
    /// The callback admits these new containers before their first clone.
    pub(crate) fn work_owner_preparation_request(
        &self,
        record_limit: usize,
        byte_limit: usize,
        mut observe: impl FnMut(u64, usize) -> Result<()>,
    ) -> Result<crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest> {
        let (bytes, nodes) = super::input_evidence_budget::borrowed_json_material(&self.coverage)?;
        let records = nodes
            .checked_add(32)
            .and_then(|n| n.checked_mul(8))
            .context("Work Input preserved selector count overflow")?;
        let bytes = bytes
            .checked_add(
                (nodes as u64)
                    .checked_mul(256)
                    .context("Work Input selector slots overflow")?,
            )
            .and_then(|n| n.checked_mul(8))
            .context("Work Input preserved selector bytes overflow")?;
        observe(bytes, records)?;
        Ok(
            crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
                windows: self
                    .coverage
                    .windows
                    .iter()
                    .map(
                        |window| crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                            selector: window.selector.clone(),
                            limit: window.limit,
                        },
                    )
                    .collect(),
                mandatory_roots: self.coverage.mandatory_roots.clone(),
                interrupted_sources: self.coverage.execution.sources.keys().cloned().collect(),
                selected_completion_brief_ids: self.coverage.selected_completion_brief_ids.clone(),
                record_limit,
                byte_limit,
            },
        )
    }
    pub(crate) fn capture(&self) -> Result<CapturedInputEvidence> {
        let Some(ledger) = &self.budget else {
            return self.capture_observed(&mut CapturePhase::unobserved());
        };
        let mut phase = CapturePhase::new(
            self,
            ledger,
            // Coverage bytes bound selected bodies, not owned phase metadata.
            // CapturePhase applies the actual ledger and hard phase limits.
            (usize::MAX, usize::MAX),
            (0, 0),
        )?;
        let captured = self.capture_observed(&mut phase)?;
        phase
            .finish_captured(captured)
            .map(|(captured, _material)| captured)
    }
    /// Full current/mandatory observation, sharing the original algorithm.
    /// Material limits are separate from logical coverage/body/window bounds.
    pub(crate) fn capture_with_material(
        &self,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: (u64, usize),
        body_loan: Option<&CapturedInputEvidence>,
    ) -> Result<(CapturedInputEvidence, InputCaptureMaterial)> {
        let mut phase = CapturePhase::new_with_body_loan(self, ledger, limits, prior, body_loan)?;
        let captured = self.capture_observed(&mut phase)?;
        phase.finish_captured(captured)
    }
    fn capture_observed(&self, phase: &mut CapturePhase<'_>) -> Result<CapturedInputEvidence> {
        self.validate_observed(phase)?;
        let selected = self.required_keys_observed(phase)?;
        phase.json(
            &(
                &self.scope,
                &self.coverage.windows,
                &self.coverage.unavailable,
                &self.coverage.turn_transcript_keys,
                &self.coverage.message_turn_keys,
                &self.coverage.recovery_source_absences,
                &self.coverage.execution,
            ),
            1,
        )?;
        #[cfg(test)]
        phase.json(
            &(
                &self.coverage.selected_completion_brief_ids,
                &self.coverage.completion_source_observations,
            ),
            1,
        )?;
        phase.charge(
            u64::try_from(selected.len())?
                .checked_mul(256)
                .context(InputCaptureError::ResourceExhausted)?,
            selected.len(),
        )?;
        for key in &selected {
            if let Some(record) = self.records.get(key) {
                phase.body(record)?;
            }
        }
        let captured = CapturedInputEvidence {
            #[cfg(test)]
            selected_completion_brief_ids: self.coverage.selected_completion_brief_ids.clone(),
            #[cfg(test)]
            completion_source_observations: self.coverage.completion_source_observations.clone(),
            recovery_source_absences: self.coverage.recovery_source_absences.clone(),
            execution: self.coverage.execution.clone(),
            scope: self.scope.clone(),
            budget: self.budget.clone(),
            budget_views: Vec::new(),
            windows: self.coverage.windows.clone(),
            records: selected
                .iter()
                .filter_map(|key| self.records.get(key).cloned())
                .collect(),
            unavailable: self.coverage.unavailable.clone(),
            turn_transcript_keys: self.coverage.turn_transcript_keys.clone(),
            message_turn_keys: self.coverage.message_turn_keys.clone(),
        };
        for evidence in &captured.records {
            if matches!(
                evidence.record,
                PreparedInputEvidence::Attempt(_)
                    | PreparedInputEvidence::Outcome(_)
                    | PreparedInputEvidence::WorkExecution { .. }
            ) {
                phase.json(&evidence.record, 1)?;
            }
        }
        captured.protocol_state()?;
        Ok(captured)
    }
    /// A request owns only its selected closure. Resident mandatory roots also
    /// contain other accepted inputs and must not be pinned by every prompt.
    pub(crate) fn capture_for_request(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    ) -> Result<CapturedInputEvidence> {
        self.capture_for_request_inner(request, false)
    }
    pub(crate) fn capture_for_request_with_material(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        observation_only: bool,
        limits: (usize, usize),
        prior: (u64, usize),
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(CapturedInputEvidence, InputCaptureMaterial)> {
        let mut phase = CapturePhase::new(self, ledger, limits, prior)?;
        let captured = self.capture_request_observed(request, observation_only, &mut phase)?;
        phase.finish_captured(captured)
    }
    /// Only a validated outer same-publication consumer may discount an
    /// already-held physical body. This changes neither roots nor qualification.
    pub(in crate::runtime) fn capture_for_request_with_body_loan(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        limits: (usize, usize),
        prior: (u64, usize),
        ledger: &InputEvidenceBudgetLedger,
        loan: &CapturedInputEvidence,
    ) -> Result<(CapturedInputEvidence, InputCaptureMaterial)> {
        self.validate_scheduler_body_loan(loan, ledger)?;
        let mut phase = CapturePhase::new_with_body_loan(self, ledger, limits, prior, Some(loan))?;
        let captured = self.capture_request_observed(request, false, &mut phase)?;
        phase.finish_captured(captured)
    }
    fn capture_for_request_inner(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        observation_only: bool,
    ) -> Result<CapturedInputEvidence> {
        if let Some(ledger) = &self.budget {
            return self
                .capture_for_request_with_material(
                    request,
                    observation_only,
                    // Preserve the request's logical body/closure policy;
                    // the original ledger bounds this implicit material phase.
                    (usize::MAX, usize::MAX),
                    (0, 0),
                    ledger,
                )
                .map(|(captured, _material)| captured);
        }
        self.capture_request_observed(request, observation_only, &mut CapturePhase::unobserved())
    }
    fn capture_request_observed(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        observation_only: bool,
        phase: &mut CapturePhase<'_>,
    ) -> Result<CapturedInputEvidence> {
        use InputEvidenceReadError::WindowNotPrepared;
        ensure!(
            request.record_limit > 0 && request.byte_limit > 0,
            "input request needs positive resource bounds"
        );
        // A writer may have supplied the incoming body without observing its
        // asserted lineage. Only an explicit request can require that SQL view.
        phase.json(
            &(
                &request.mandatory_roots,
                &request.interrupted_sources,
                &request.selected_completion_brief_ids,
            ),
            4,
        )?;
        for window in &request.windows {
            phase.json(&(&window.selector, window.limit), 2)?;
        }
        let requested_messages = request
            .mandatory_roots
            .iter()
            .filter(|key| key.family == InputEvidenceFamily::Message)
            .map(|key| key.id.clone())
            .chain(request.interrupted_sources.iter().cloned())
            .collect::<BTreeSet<_>>();
        for id in requested_messages {
            let key = InputEvidenceKey::new(InputEvidenceFamily::Message, &id);
            if let Some(record) = self.records.get(&key) {
                if let PreparedInputEvidence::Message(message) = &record.record {
                    phase.continuity(message)?;
                    if matches!(
                        super::provider_recovery::continuity_sources(message),
                        Ok(Some(_))
                    ) && !self.coverage.recovery_lineage_messages.contains(&id)
                    {
                        return Err(InputEvidenceReadError::NeedsPreparation(key).into());
                    }
                }
            }
        }
        let mut windows = Vec::new();
        let mut mandatory_roots = if observation_only {
            BTreeSet::new()
        } else {
            request.mandatory_roots.clone()
        };
        for id in &request.selected_completion_brief_ids {
            let key = InputEvidenceKey::new(InputEvidenceFamily::Brief, id);
            if !self.coverage.selected_completion_brief_ids.contains(id) {
                return Err(InputEvidenceReadError::NeedsPreparation(key).into());
            }
        }
        mandatory_roots.extend(
            self.coverage
                .execution
                .required_interrupted_body_keys_observed(&request.interrupted_sources, phase)?,
        );
        phase.json(&mandatory_roots, 1)?;
        let mut selected = request.mandatory_roots.clone();
        selected.extend(mandatory_roots.iter().cloned());
        selected.extend(
            request
                .selected_completion_brief_ids
                .iter()
                .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Brief, id)),
        );
        selected.extend(
            self.coverage
                .execution
                .required_body_keys_observed(&request.mandatory_roots, phase)?,
        );
        for wanted in &request.windows {
            let prepared = self
                .coverage
                .windows
                .iter()
                .find(|window| window.selector == wanted.selector)
                .filter(|window| wanted.limit <= window.limit)
                .ok_or(WindowNotPrepared)?;
            ensure!(
                !windows
                    .iter()
                    .any(|window: &InputWindowCoverage| { window.selector == wanted.selector }),
                "duplicate input request selector"
            );
            for key in prepared.keys.iter().take(wanted.limit) {
                phase.json(key, 2)?;
            }
            phase.json(&wanted.selector, 1)?;
            let keys = prepared
                .keys
                .iter()
                .take(wanted.limit)
                .cloned()
                .collect::<Vec<_>>();
            selected.extend(keys.iter().cloned());
            windows.push(InputWindowCoverage {
                selector: wanted.selector.clone(),
                limit: wanted.limit,
                is_exhaustive: prepared.is_exhaustive && keys.len() == prepared.keys.len(),
                keys,
            });
        }
        ensure!(
            selected.len() <= request.record_limit,
            "input request closure record limit exceeded"
        );
        phase.json(&(&mandatory_roots, &windows), 1)?;
        let strict_roots = mandatory_roots
            .iter()
            .cloned()
            .chain(
                self.coverage
                    .execution
                    .required_body_keys_observed(&request.mandatory_roots, phase)?,
            )
            .chain(
                windows
                    .iter()
                    .flat_map(|window| window.keys.iter().cloned()),
            )
            .collect();
        selected = self.completion_closure_observed(
            selected,
            strict_roots,
            &request.selected_completion_brief_ids,
            request.record_limit,
            phase,
        )?;
        for key in &selected {
            if !self.records.contains_key(key) && !self.coverage.unavailable.contains(key) {
                return Err(InputEvidenceReadError::NeedsPreparation(key.clone()).into());
            }
        }
        for (id, keys) in &self.coverage.turn_transcript_keys {
            if selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Turn && key.id == *id)
            {
                phase.json(&(id, keys), 1)?;
            }
        }
        for (id, turn) in &self.coverage.message_turn_keys {
            if selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Message && key.id == *id)
            {
                phase.json(&(id, turn), 1)?;
            }
        }
        let turns = self
            .coverage
            .turn_transcript_keys
            .iter()
            .filter(|(id, _)| {
                selected
                    .iter()
                    .any(|key| key.family == InputEvidenceFamily::Turn && key.id == **id)
            })
            .map(|(id, keys)| (id.clone(), keys.clone()))
            .collect();
        let assignments = self
            .coverage
            .message_turn_keys
            .iter()
            .filter(|(id, _)| {
                selected
                    .iter()
                    .any(|key| key.family == InputEvidenceFamily::Message && key.id == **id)
            })
            .map(|(id, turn)| (id.clone(), turn.clone()))
            .collect();
        for key in &selected {
            phase.json(&key.id, 1)?;
        }
        phase.json(&request.interrupted_sources, 1)?;
        let mut source_ids = selected
            .iter()
            .filter(|key| key.family == InputEvidenceFamily::Message)
            .map(|key| key.id.clone())
            .collect::<BTreeSet<_>>();
        source_ids.extend(request.interrupted_sources.iter().cloned());
        phase.json(&request.selected_completion_brief_ids, 1)?;
        for proof in &self.coverage.completion_source_observations {
            if request
                .selected_completion_brief_ids
                .contains(&proof.head.key.id)
            {
                phase.json(proof, 1)?;
            }
        }
        for (id, proof) in &self.coverage.recovery_source_absences {
            if selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Message && key.id == *id)
            {
                phase.json(&(id, proof), 1)?;
            }
        }
        for id in &self.coverage.recovery_lineage_messages {
            if selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Message && key.id == *id)
            {
                phase.json(&id, 1)?;
            }
        }
        phase.json(&selected, 2)?;
        for key in &selected {
            if let Some(record) = self.records.get(key) {
                capture_execution_work_ids_observed(record, phase)?;
            }
        }
        let coverage = InputEvidenceCoverage {
            query: if observation_only {
                super::input_queries::InputQueryCoverage {
                    required_keys: request.mandatory_roots.clone(),
                    ..Default::default()
                }
            } else {
                Default::default()
            },
            selected_completion_brief_ids: request.selected_completion_brief_ids.clone(),
            completion_source_observations: self
                .coverage
                .completion_source_observations
                .iter()
                .filter(|proof| {
                    request
                        .selected_completion_brief_ids
                        .contains(&proof.head.key.id)
                })
                .cloned()
                .collect(),
            recovery_source_absences: self
                .coverage
                .recovery_source_absences
                .iter()
                .filter(|(id, _)| {
                    selected
                        .iter()
                        .any(|key| key.family == InputEvidenceFamily::Message && key.id == **id)
                })
                .map(|(id, proof)| (id.clone(), proof.clone()))
                .collect(),
            recovery_lineage_messages: self
                .coverage
                .recovery_lineage_messages
                .iter()
                .filter(|id| {
                    selected
                        .iter()
                        .any(|key| key.family == InputEvidenceFamily::Message && key.id == **id)
                })
                .cloned()
                .collect(),
            execution: self.coverage.execution.for_sources_observed(
                &source_ids,
                &selected_execution_work_ids(
                    &selected
                        .iter()
                        .filter_map(|key| self.records.get(key))
                        .collect::<Vec<_>>(),
                    &selected,
                ),
                phase,
            )?,
            windows,
            mandatory_roots,
            turn_transcript_keys: turns,
            message_turn_keys: assignments,
            unavailable: self
                .coverage
                .unavailable
                .intersection(&selected)
                .cloned()
                .collect(),
            record_limit: request.record_limit,
            byte_limit: request.byte_limit,
        };
        phase.json(&self.scope, 1)?;
        for key in &selected {
            phase.json(key, 1)?;
            if let Some(record) = self.records.get(key) {
                phase.body(record)?;
            }
        }
        let mut selected_domain = Self::bootstrap_observed(
            self.scope.clone(),
            selected
                .iter()
                .filter_map(|key| self.records.get(key).cloned())
                .collect(),
            coverage,
            phase,
        )?;
        selected_domain.budget = self.budget.clone();
        let captured = selected_domain.capture_observed(phase)?;
        for source in &request.interrupted_sources {
            if let Some(coverage) = captured.execution.sources.get(source) {
                phase.charge(
                    u64::try_from(coverage.attempts.len())?
                        .checked_mul(256)
                        .context(InputCaptureError::ResourceExhausted)?,
                    coverage.attempts.len(),
                )?;
            }
            captured.execution_interrupted_attempts_for_source(source)?;
        }
        Ok(captured)
    }
    /// `mandatory_roots` is the actual committed queue/protocol/root selection,
    /// supplied by the owner installer, never inferred from command arguments.
    pub(crate) fn with_committed(
        &self,
        delta: &CommittedInputEvidenceDelta,
        mandatory_roots: BTreeSet<InputEvidenceKey>,
    ) -> Result<Option<Self>> {
        self.with_committed_work_sources(delta, mandatory_roots, None)
    }
    /// Hydrated owning bodies remain distinct from D3's actual write footprint.
    /// Preserve unrelated query/recall metadata on the original existing domain.
    pub(crate) fn with_committed_work_sources(
        &self,
        delta: &CommittedInputEvidenceDelta,
        mandatory_roots: BTreeSet<InputEvidenceKey>,
        work_sources: Option<&Self>,
    ) -> Result<Option<Self>> {
        if let Some(advance) = &delta.query_advance {
            advance.validate_delta(&self.scope, &delta.records)?;
        }
        if let Some(prepared) = work_sources {
            ensure!(
                prepared.scope == self.scope
                    && prepared
                        .budget
                        .as_ref()
                        .zip(self.budget.as_ref())
                        .is_some_and(|(prepared, actual)| prepared.same_ledger(actual)),
                "prepared Work Input belongs to another original domain/ledger"
            );
        }
        let _candidate_views = self
            .budget
            .as_ref()
            .map(|budget| -> Result<_> {
                Ok(vec![
                    budget.register_owned_bytes(
                        &self.scope,
                        InputMemoryCategory::Candidate,
                        OwnedBytesEstimate {
                            payload_bytes: 0,
                            metadata_estimate_bytes: self.metadata_estimate_bytes()?,
                        },
                    )?,
                    budget.register_view(
                        &self.scope,
                        InputMemoryCategory::Candidate,
                        &delta.records,
                    )?,
                ])
            })
            .transpose()?;
        let mut next = self.clone();
        if !delta.records.is_empty() || !delta.removals.is_empty() {
            next.coverage.query = Default::default();
        }
        // A real D3 write invalidates read-only recall. Old captures keep their
        // pinned bytes; the old locator cannot recreate durable qualification.
        if let Some(invalidation) = &delta.output_recall_invalidation {
            ensure!(
                invalidation.belongs_to(&self.scope),
                "Output invalidation scope differs"
            );
        }
        if delta.output_recall_invalidation.is_some()
            || !delta.records.is_empty()
            || !delta.removals.is_empty()
            || delta.execution.initialized.is_some()
            || !delta.execution.sources.is_empty()
            || !delta.execution.open_attempts.is_empty()
            || !delta.execution.nonterminal_work_items.is_empty()
            || !delta.execution.work_items.is_empty()
        {
            next.output_recall_roots.clear();
            next.output_recall_task = None;
            next.trim_and_validate()?;
        }
        let changed = delta
            .records
            .iter()
            .map(|record| &record.locator.key)
            .chain(delta.removals.iter().map(|removal| &removal.key))
            .collect::<BTreeSet<_>>();
        next.coverage.selected_completion_brief_ids.retain(|id| {
            let head_key = InputEvidenceKey::new(InputEvidenceFamily::Brief, id);
            if changed.contains(&head_key) {
                return false;
            }
            if self
                .coverage
                .completion_source_observations
                .iter()
                .any(|proof| {
                    proof.head.key == head_key && changed.iter().any(|key| proof.observes_key(key))
                })
            {
                return false;
            }
            let Some(head) = self.records.get(&head_key) else {
                return false;
            };
            let PreparedInputEvidence::Brief(brief) = &head.record else {
                return false;
            };
            if let Some(source) = brief.content_source.transcript_entry_id() {
                let source_key = InputEvidenceKey::new(InputEvidenceFamily::Transcript, source);
                if changed.contains(&source_key) {
                    return false;
                }
                if self
                    .records
                    .get(&source_key)
                    .is_some_and(|record| match &record.record {
                        PreparedInputEvidence::Transcript(entry) => {
                            entry.related_message_id.as_ref().is_some_and(|id| {
                                changed.contains(&InputEvidenceKey::new(
                                    InputEvidenceFamily::Message,
                                    id,
                                ))
                            })
                        }
                        _ => false,
                    })
                {
                    return false;
                }
            }
            true
        });
        next.coverage
            .completion_source_observations
            .retain(|proof| {
                next.coverage
                    .selected_completion_brief_ids
                    .contains(&proof.head.key.id)
            });
        // An actual source/body write invalidates only related observations.
        // The next explicit owner preparation re-observes absence/presence.
        next.coverage.recovery_source_absences.retain(|_, proof| {
            !delta.records.iter().any(|record| {
                record.locator.key == proof.recovery_message.key
                    || record.locator.key
                        == InputEvidenceKey::new(
                            InputEvidenceFamily::Message,
                            &proof.source_message_id,
                        )
                    || record.locator.key
                        == InputEvidenceKey::new(InputEvidenceFamily::Turn, &proof.source_turn_id)
            }) && !delta.removals.iter().any(|record| {
                record.key == proof.recovery_message.key
                    || record.key
                        == InputEvidenceKey::new(
                            InputEvidenceFamily::Message,
                            &proof.source_message_id,
                        )
                    || record.key
                        == InputEvidenceKey::new(InputEvidenceFamily::Turn, &proof.source_turn_id)
            })
        });
        next.coverage.mandatory_roots = mandatory_roots;
        next.coverage.execution.apply(&delta.execution)?;
        for locator in &delta.removals {
            ensure!(locator.scope == self.scope, "removal scope mismatch");
            if let Some(old) = next.records.get(&locator.key) {
                ensure!(
                    locator.revision > old.locator.revision,
                    "stale evidence removal"
                );
                if old.locator.revision.checked_add(1) == Some(locator.revision) {
                    ensure!(
                        locator.previous_content_digest.as_ref()
                            == Some(&old.locator.content_digest),
                        "evidence removal differs from its immediate predecessor"
                    );
                }
            }
            if next
                .coverage
                .windows
                .iter()
                .any(|window| !window.is_exhaustive && window.keys.contains(&locator.key))
            {
                return Ok(None);
            }
            next.records.remove(&locator.key);
            next.coverage.unavailable.insert(locator.key.clone());
        }
        for record in &delta.records {
            if let PreparedInputEvidence::Message(message) = &record.record {
                ensure!(
                    delta.message_turn_keys.contains_key(&message.id),
                    "committed Message is missing its canonical assignment observation"
                );
            }
            if let PreparedInputEvidence::Turn(turn) = &record.record {
                ensure!(
                    delta.turn_transcript_keys.contains_key(&turn.turn_id),
                    "committed Turn is missing its qualified transcript frontier"
                );
            }
            ensure!(
                record.locator.scope == self.scope && record.locator.key == record.record.key(),
                "committed evidence scope/key mismatch"
            );
            if let Some(old) = next.records.get(&record.locator.key) {
                if old.locator.revision == record.locator.revision {
                    let selected = QualifiedInputEvidence::same_revision_allocation(old, record)?;
                    next.records.insert(record.locator.key.clone(), selected);
                    continue;
                }
                ensure!(
                    old.locator.revision < record.locator.revision,
                    "stale committed evidence"
                );
            }
            next.coverage.unavailable.remove(&record.locator.key);
            next.records
                .insert(record.locator.key.clone(), record.clone());
        }
        for (message_id, turn) in &delta.message_turn_keys {
            next.coverage
                .message_turn_keys
                .insert(message_id.clone(), turn.clone());
        }
        for (turn_id, keys) in &delta.turn_transcript_keys {
            ensure!(
                keys.iter()
                    .all(|key| key.family == InputEvidenceFamily::Transcript),
                "Turn transcript frontier contains another evidence family"
            );
            next.coverage
                .turn_transcript_keys
                .insert(turn_id.clone(), keys.clone());
        }
        for evidence in &delta.records {
            if let PreparedInputEvidence::Transcript(entry) = &evidence.record {
                for keys in next.coverage.turn_transcript_keys.values_mut() {
                    keys.remove(&evidence.locator.key);
                }
                if let Some(turn_id) = entry
                    .data
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                {
                    next.coverage
                        .turn_transcript_keys
                        .entry(turn_id.to_string())
                        .or_default()
                        .insert(evidence.locator.key.clone());
                }
            }
        }
        if let Some(prepared) = work_sources {
            for (key, record) in &prepared.records {
                let selected = if let Some(actual) = next.records.get(key) {
                    ensure!(
                        actual.locator.revision <= record.locator.revision,
                        "prepared Work Input is older than the actual ACK"
                    );
                    if actual.locator.revision == record.locator.revision {
                        QualifiedInputEvidence::same_revision_allocation(actual, record)?
                    } else {
                        record.clone()
                    }
                } else {
                    record.clone()
                };
                next.coverage.unavailable.remove(key);
                next.records.insert(key.clone(), selected);
            }
            next.coverage.selected_completion_brief_ids =
                prepared.coverage.selected_completion_brief_ids.clone();
            next.coverage.completion_source_observations =
                prepared.coverage.completion_source_observations.clone();
            next.coverage
                .recovery_source_absences
                .extend(prepared.coverage.recovery_source_absences.clone());
            next.coverage
                .recovery_lineage_messages
                .extend(prepared.coverage.recovery_lineage_messages.iter().cloned());
            next.coverage.execution = prepared.coverage.execution.clone();
            next.coverage
                .turn_transcript_keys
                .extend(prepared.coverage.turn_transcript_keys.clone());
            next.coverage
                .message_turn_keys
                .extend(prepared.coverage.message_turn_keys.clone());
            next.coverage
                .unavailable
                .extend(prepared.coverage.unavailable.iter().cloned());
        }
        for window in &mut next.coverage.windows {
            let mut candidates = next
                .records
                .values()
                .filter(|record| matches_selector(&record.record, &window.selector))
                .collect::<Vec<_>>();
            candidates.sort_by(|a, b| compare_record_order(b, a));
            if candidates.len() > window.limit {
                window.is_exhaustive = false;
            }
            window.keys = candidates
                .into_iter()
                .take(window.limit)
                .map(|record| record.locator.key.clone())
                .collect();
        }
        if next.trim_and_validate().is_err() {
            return Ok(None);
        }
        next.refresh_budget_views()?;
        if let Some(budget) = &next.budget {
            budget.check_history_budget()?;
        }
        Ok(Some(next))
    }
    fn references(&self, record: &QualifiedInputEvidence) -> Result<BTreeSet<InputEvidenceKey>> {
        self.references_observed(record, &mut CapturePhase::unobserved())
    }
    fn references_observed(
        &self,
        record: &QualifiedInputEvidence,
        phase: &mut CapturePhase<'_>,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        let checkpoint = phase.checkpoint()?;
        let result = self.references_observed_inner(record, phase)?;
        phase.collapse(checkpoint, CapturePhase::selector_cost(&result)?)?;
        Ok(result)
    }
    fn references_observed_inner(
        &self,
        record: &QualifiedInputEvidence,
        phase: &mut CapturePhase<'_>,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        let mut failure = None;
        record.visit_references(|family, id| {
            if failure.is_none() {
                failure = phase.key(family, id, 1).err();
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        phase.json(&(), 1)?;
        let mut refs = record.references();
        if let PreparedInputEvidence::Message(message) = &record.record {
            phase.continuity(message)?;
            if let Some((turn, source, _)) = self
                .coverage
                .recovery_lineage_messages
                .contains(&message.id)
                .then(|| {
                    super::provider_recovery::continuity_sources(message)
                        .ok()
                        .flatten()
                })
                .flatten()
            {
                phase.json(&(&turn, &source), 4)?;
                if self
                    .coverage
                    .recovery_source_absences
                    .contains_key(&message.id)
                {
                    phase.continuity(message)?;
                }
                let absent = self
                    .coverage
                    .recovery_source_absences
                    .get(&message.id)
                    .filter(|proof| proof.matches(record));
                let source_key = InputEvidenceKey::new(InputEvidenceFamily::Message, &source);
                let lineage_roots = BTreeSet::from([
                    source_key.clone(),
                    InputEvidenceKey::new(InputEvidenceFamily::Turn, turn),
                ]);
                for key in &lineage_roots {
                    if absent.is_none_or(|proof| !proof.absent.contains(key)) {
                        refs.insert(key.clone());
                    }
                }
                if absent.is_none() {
                    // Every observed hop needs its complete candidate bodies,
                    // including after a request slices the prepared domain.
                    self.coverage.execution.source(&source)?;
                    refs.extend(
                        self.coverage
                            .execution
                            .required_body_keys_observed(&lineage_roots, phase)?,
                    );
                    if let Some(source_record) = self.records.get(&source_key) {
                        if let PreparedInputEvidence::Message(source_message) =
                            &source_record.record
                        {
                            phase.continuity(source_message)?;
                            if matches!(
                                super::provider_recovery::continuity_sources(source_message),
                                Ok(Some(_))
                            ) && !self.coverage.recovery_lineage_messages.contains(&source)
                            {
                                return Err(
                                    InputEvidenceReadError::NeedsPreparation(source_key).into()
                                );
                            }
                        }
                    }
                }
            }
            if let Some(id) = &message.work_item_id {
                phase.key(InputEvidenceFamily::WorkExecution, id, 1)?;
                let key = InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, id);
                if self
                    .coverage
                    .execution
                    .work_items
                    .get(id)
                    .is_some_and(|x| x.presence == ExecutionWorkPresence::Present)
                    || self.records.contains_key(&key)
                {
                    refs.insert(key);
                }
            }
            if let Some(Some(turn_id)) = self.coverage.message_turn_keys.get(&message.id) {
                phase.key(InputEvidenceFamily::Turn, turn_id, 1)?;
                refs.insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, turn_id));
            }
        }
        if let PreparedInputEvidence::Turn(turn) = &record.record {
            if let Some(keys) = self.coverage.turn_transcript_keys.get(&turn.turn_id) {
                phase.json(keys, 1)?;
                refs.extend(keys.iter().cloned());
            }
        }
        Ok(refs)
    }
    fn completion_closure(
        &self,
        roots: BTreeSet<InputEvidenceKey>,
        strict_roots: BTreeSet<InputEvidenceKey>,
        heads: &BTreeSet<String>,
        record_limit: usize,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        self.completion_closure_observed(
            roots,
            strict_roots,
            heads,
            record_limit,
            &mut CapturePhase::unobserved(),
        )
    }
    fn completion_closure_observed(
        &self,
        roots: BTreeSet<InputEvidenceKey>,
        strict_roots: BTreeSet<InputEvidenceKey>,
        heads: &BTreeSet<String>,
        record_limit: usize,
        phase: &mut CapturePhase<'_>,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        let checkpoint = phase.checkpoint()?;
        let result = self.completion_closure_observed_inner(
            roots,
            strict_roots,
            heads,
            record_limit,
            phase,
        )?;
        phase.collapse(checkpoint, CapturePhase::selector_cost(&result)?)?;
        Ok(result)
    }
    fn completion_closure_observed_inner(
        &self,
        roots: BTreeSet<InputEvidenceKey>,
        strict_roots: BTreeSet<InputEvidenceKey>,
        heads: &BTreeSet<String>,
        record_limit: usize,
        phase: &mut CapturePhase<'_>,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        use super::completion_sources::{completion_references, CompletionInputPath};
        phase.json(&roots, 2)?;
        phase.json(&strict_roots, 1)?;
        phase.json(heads, 1)?;
        let mut selected = roots.clone();
        let mut pending = VecDeque::from_iter(roots.into_iter().map(|key| {
            let path = if key.family == InputEvidenceFamily::Brief && heads.contains(&key.id) {
                CompletionInputPath::Head(key.id.clone())
            } else {
                CompletionInputPath::Strict
            };
            (key, path)
        }));
        pending.extend(
            strict_roots
                .into_iter()
                .map(|key| (key, CompletionInputPath::Strict)),
        );
        let mut expanded = BTreeSet::new();
        while let Some((key, path)) = pending.pop_front() {
            phase.key(key.family, &key.id, 2)?;
            match &path {
                CompletionInputPath::Head(id) | CompletionInputPath::Transcript(id) => {
                    phase.json(id, 3)?
                }
                CompletionInputPath::Strict => {}
            }
            selected.insert(key.clone());
            ensure!(
                selected.len() <= record_limit,
                "input evidence closure record limit exceeded"
            );
            if !expanded.insert((key.clone(), path.clone())) {
                continue;
            }
            // There are at most the bounded strict graph plus two selected
            // completion edges per head, never a path cross-product.
            ensure!(
                expanded.len() <= record_limit.saturating_mul(3),
                "input evidence path limit exceeded"
            );
            let Some(record) = self.records.get(&key) else {
                continue;
            };
            let head = match &path {
                CompletionInputPath::Strict => None,
                CompletionInputPath::Head(id) | CompletionInputPath::Transcript(id) => self
                    .records
                    .get(&InputEvidenceKey::new(InputEvidenceFamily::Brief, id))
                    .map(Arc::as_ref),
            };
            let references = self.references_observed(record, phase)?;
            phase.json(&references, 1)?;
            // Optional-path matching builds temporary target keys; it never
            // owns or decodes another body.
            if head.is_some() {
                phase.json(&self.coverage.completion_source_observations, 1)?;
                phase.completion_sources(head, record)?;
            }
            pending.extend(completion_references(
                record,
                &path,
                head,
                &self.coverage.completion_source_observations,
                references,
            )?);
        }
        Ok(selected)
    }
    fn required_keys(&self) -> Result<BTreeSet<InputEvidenceKey>> {
        self.required_keys_observed(&mut CapturePhase::unobserved())
    }
    fn required_keys_observed(
        &self,
        phase: &mut CapturePhase<'_>,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        let checkpoint = phase.checkpoint()?;
        let result = self.required_keys_observed_inner(phase)?;
        phase.collapse(checkpoint, CapturePhase::selector_cost(&result)?)?;
        Ok(result)
    }
    fn required_keys_observed_inner(
        &self,
        phase: &mut CapturePhase<'_>,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        phase.json(
            &(
                &self.coverage.mandatory_roots,
                &self.coverage.query,
                &self.output_recall_roots,
                &self.coverage.windows,
                &self.coverage.selected_completion_brief_ids,
            ),
            2,
        )?;
        let mut roots = self.coverage.mandatory_roots.clone();
        roots.extend(self.coverage.query.observation_keys().cloned());
        roots.extend(self.output_recall_roots.iter().cloned());
        roots.extend(
            self.coverage
                .execution
                .required_body_keys_observed(&self.coverage.mandatory_roots, phase)?,
        );
        roots.extend(
            self.coverage
                .selected_completion_brief_ids
                .iter()
                .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Brief, id)),
        );
        let strict = self
            .coverage
            .mandatory_roots
            .iter()
            .cloned()
            .chain(
                self.coverage
                    .execution
                    .required_body_keys_observed(&self.coverage.mandatory_roots, phase)?,
            )
            .chain(
                self.coverage
                    .windows
                    .iter()
                    .flat_map(|window| window.keys.iter().cloned()),
            )
            .collect::<BTreeSet<_>>();
        phase.json(&strict, 1)?;
        roots.extend(strict.iter().cloned());
        self.completion_closure_observed(
            roots,
            strict,
            &self.coverage.selected_completion_brief_ids,
            self.coverage.record_limit,
            phase,
        )
    }
    fn validate(&self) -> Result<()> {
        self.validate_observed(&mut CapturePhase::unobserved())
    }
    fn validate_observed(&self, phase: &mut CapturePhase<'_>) -> Result<()> {
        let checkpoint = phase.checkpoint()?;
        let result = self.validate_observed_inner(phase)?;
        phase.collapse(checkpoint, (0, 0))?;
        Ok(result)
    }
    fn validate_observed_inner(&self, phase: &mut CapturePhase<'_>) -> Result<()> {
        let selected = self.required_keys_observed(phase)?;
        phase.json(
            &(
                &self.coverage.mandatory_roots,
                &self.coverage.selected_completion_brief_ids,
            ),
            2,
        )?;
        let mut mandatory = self.coverage.mandatory_roots.clone();
        mandatory.extend(
            self.coverage
                .execution
                .required_body_keys_observed(&self.coverage.mandatory_roots, phase)?,
        );
        phase.json(&mandatory, 1)?;
        let strict_roots = mandatory.clone();
        mandatory.extend(
            self.coverage
                .selected_completion_brief_ids
                .iter()
                .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Brief, id)),
        );
        let mandatory = self.completion_closure_observed(
            mandatory,
            strict_roots,
            &self.coverage.selected_completion_brief_ids,
            self.coverage.record_limit,
            phase,
        )?;
        for key in &mandatory {
            if !self.records.contains_key(key) {
                if key.family == InputEvidenceFamily::WorkExecution
                    && self
                        .coverage
                        .execution
                        .work_items
                        .get(&key.id)
                        .is_some_and(|x| x.presence == ExecutionWorkPresence::Absent)
                {
                    continue;
                }
                anyhow::bail!(
                    "mandatory evidence unavailable: {}:{}",
                    key.family.as_str(),
                    key.id
                );
            }
        }
        for proof in &self.coverage.completion_source_observations {
            phase.json(proof, 1)?;
            let head = self.records.get(&proof.head.key);
            let source = self.records.get(&proof.source.key);
            if let Some(source) = source {
                phase.completion_sources(head.map(Arc::as_ref), source)?;
            }
            ensure!(
                self.coverage
                    .selected_completion_brief_ids
                    .contains(&proof.head.key.id)
                    && head
                        .zip(source)
                        .is_some_and(|(head, source)| proof.matches(head, source))
                    && !self.records.contains_key(&proof.declared_target)
                    && !self.coverage.unavailable.contains(&proof.declared_target),
                "completion optional edge has no exact qualified head/source absence"
            );
        }
        for (id, proof) in &self.coverage.recovery_source_absences {
            phase.key(InputEvidenceFamily::Message, id, 1)?;
            if let Some(record) = self.records.values().find(|record| {
                record.locator.key.family == InputEvidenceFamily::Message
                    && record.locator.key.id == *id
            }) {
                if let PreparedInputEvidence::Message(message) = &record.record {
                    phase.continuity(message)?;
                }
            }
            let key = InputEvidenceKey::new(InputEvidenceFamily::Message, id);
            ensure!(
                self.records
                    .get(&key)
                    .is_some_and(|record| proof.matches(record)),
                "recovery source absence has no matching qualified message version"
            );
            ensure!(
                proof
                    .absent
                    .iter()
                    .all(|key| !self.records.contains_key(key)
                        && !self.coverage.unavailable.contains(key)),
                "recovery source absence overlaps prepared or pruned evidence"
            );
        }
        ensure!(
            selected
                .len()
                .saturating_add(self.coverage.recovery_source_absences.len())
                .saturating_add(self.coverage.completion_source_observations.len())
                .saturating_add(self.coverage.selected_completion_brief_ids.len())
                .saturating_add(self.coverage.recovery_lineage_messages.len())
                .saturating_add(self.coverage.execution.metadata_count())
                <= self.coverage.record_limit,
            "input evidence and execution metadata record limit exceeded"
        );
        phase.json(
            &(
                &self.coverage.turn_transcript_keys,
                &self.coverage.message_turn_keys,
                &self.coverage.recovery_source_absences,
                &self.coverage.recovery_lineage_messages,
                &self.coverage.completion_source_observations,
                &self.coverage.selected_completion_brief_ids,
            ),
            1,
        )?;
        let mut bytes = serde_json::to_vec(&self.coverage.turn_transcript_keys)?
            .len()
            .saturating_add(serde_json::to_vec(&self.coverage.message_turn_keys)?.len())
            .saturating_add(serde_json::to_vec(&self.coverage.recovery_source_absences)?.len())
            .saturating_add(serde_json::to_vec(&self.coverage.recovery_lineage_messages)?.len())
            .saturating_add(
                serde_json::to_vec(&self.coverage.completion_source_observations)?.len(),
            )
            .saturating_add(serde_json::to_vec(&self.coverage.selected_completion_brief_ids)?.len())
            .saturating_add(self.coverage.execution.metadata_bytes()?);
        for key in &selected {
            if let Some(record) = self.records.get(key) {
                if matches!(record.record, PreparedInputEvidence::Attempt(_)) {
                    ensure!(
                        record.attempt_turn_keys.is_some(),
                        "selected Attempt has no canonical Turn observation"
                    );
                }
                if let PreparedInputEvidence::Message(message) = &record.record {
                    ensure!(
                        self.coverage.message_turn_keys.contains_key(&message.id),
                        "selected Message has no canonical assignment observation"
                    );
                }
                if let PreparedInputEvidence::Turn(turn) = &record.record {
                    ensure!(
                        self.coverage
                            .turn_transcript_keys
                            .contains_key(&turn.turn_id),
                        "selected Turn has no qualified transcript coverage"
                    );
                }
                bytes = bytes.saturating_add(record.resident_bytes)
            } else {
                ensure!(
                    self.coverage.unavailable.contains(key)
                        && (!self.coverage.mandatory_roots.contains(key)
                            || (key.family == InputEvidenceFamily::WorkExecution
                                && self
                                    .coverage
                                    .execution
                                    .work_items
                                    .get(&key.id)
                                    .is_some_and(|x| x.presence == ExecutionWorkPresence::Absent))),
                    "mandatory or unproven evidence is unavailable: {}:{}",
                    key.family.as_str(),
                    key.id
                );
            }
        }
        ensure!(
            bytes <= self.coverage.byte_limit,
            "input evidence resident byte limit exceeded"
        );
        for window in &self.coverage.windows {
            ensure!(
                window.limit <= window.selector.maximum_candidates(),
                "input history policy exceeds qualified window"
            );
            ensure!(
                window.is_exhaustive || window.keys.len() == window.limit,
                "partial history coverage is not ready"
            );
        }
        Ok(())
    }
    fn trim_and_validate(&mut self) -> Result<()> {
        self.trim_and_validate_observed(&mut CapturePhase::unobserved())
    }
    fn trim_and_validate_observed(&mut self, phase: &mut CapturePhase<'_>) -> Result<()> {
        let checkpoint = phase.checkpoint()?;
        let result = self.trim_and_validate_observed_inner(phase)?;
        phase.collapse(checkpoint, (0, 0))?;
        Ok(result)
    }
    fn trim_and_validate_observed_inner(&mut self, phase: &mut CapturePhase<'_>) -> Result<()> {
        self.validate_observed(phase)?;
        let selected = self.required_keys_observed(phase)?;
        self.records.retain(|key, _| selected.contains(key));
        self.coverage.selected_completion_brief_ids.retain(|id| {
            selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Brief && key.id == *id)
        });
        self.coverage
            .completion_source_observations
            .retain(|proof| {
                self.coverage
                    .selected_completion_brief_ids
                    .contains(&proof.head.key.id)
            });
        self.coverage.recovery_lineage_messages.retain(|id| {
            selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Message && key.id == *id)
        });
        self.coverage.recovery_source_absences.retain(|id, _| {
            selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Message && key.id == *id)
        });
        for record in self.records.values() {
            match &record.record {
                PreparedInputEvidence::Message(message) => phase.json(&message.id, 1)?,
                PreparedInputEvidence::Attempt(attempt) => {
                    phase.json(&attempt.source_message_id, 1)?
                }
                _ => {}
            }
        }
        phase.json(&selected, 1)?;
        let source_ids = self
            .records
            .values()
            .filter_map(|evidence| match &evidence.record {
                PreparedInputEvidence::Message(message) => Some(message.id.clone()),
                PreparedInputEvidence::Attempt(attempt) => attempt.source_message_id.clone(),
                _ => None,
            })
            .collect();
        for record in self.records.values() {
            capture_execution_work_ids_observed(record, phase)?;
        }
        self.coverage.execution = self.coverage.execution.for_sources_observed(
            &source_ids,
            &selected_execution_work_ids(&self.records.values().collect::<Vec<_>>(), &selected),
            phase,
        )?;
        self.coverage.turn_transcript_keys.retain(|turn_id, _| {
            selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Turn && key.id == *turn_id)
        });
        self.coverage.message_turn_keys.retain(|message_id, _| {
            selected
                .iter()
                .any(|key| key.family == InputEvidenceFamily::Message && key.id == *message_id)
        });
        self.coverage
            .unavailable
            .retain(|key| selected.contains(key));
        Ok(())
    }
}
fn capture_execution_work_ids_observed(
    record: &QualifiedInputEvidence,
    phase: &mut CapturePhase<'_>,
) -> Result<()> {
    let id = match &record.record {
        PreparedInputEvidence::Message(message) => message.work_item_id.as_deref(),
        PreparedInputEvidence::Attempt(attempt) => match &attempt.binding {
            crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } => {
                Some(work_item_id.as_str())
            }
            _ => None,
        },
        PreparedInputEvidence::WorkExecution { work_item_id, .. } => Some(work_item_id.as_str()),
        _ => None,
    };
    if let Some(id) = id {
        phase.json(&id, 1)?;
    }
    Ok(())
}
fn selected_execution_work_ids(
    records: &[&Arc<QualifiedInputEvidence>],
    selected: &BTreeSet<InputEvidenceKey>,
) -> BTreeSet<String> {
    let mut ids = selected
        .iter()
        .filter(|key| key.family == InputEvidenceFamily::WorkExecution)
        .map(|key| key.id.clone())
        .collect::<BTreeSet<_>>();
    ids.extend(
        records
            .iter()
            .filter_map(|evidence| match &evidence.record {
                PreparedInputEvidence::Message(message) => message.work_item_id.clone(),
                PreparedInputEvidence::Attempt(attempt) => match &attempt.binding {
                    crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                        work_item_id,
                    } => Some(work_item_id.clone()),
                    _ => None,
                },
                PreparedInputEvidence::WorkExecution { work_item_id, .. } => {
                    Some(work_item_id.clone())
                }
                _ => None,
            }),
    );
    ids
}
fn matches_selector(record: &PreparedInputEvidence, selector: &InputWindowSelector) -> bool {
    match (record, selector) {
        (PreparedInputEvidence::Turn(_), InputWindowSelector::GlobalTurns) => true,
        (PreparedInputEvidence::Turn(x), InputWindowSelector::OwnerTurns { owner }) => {
            x.effective_owner() == *owner
        }
        (PreparedInputEvidence::Brief(_), InputWindowSelector::RecentBriefs)
        | (PreparedInputEvidence::ToolExecution(_), InputWindowSelector::RecentTools)
        | (PreparedInputEvidence::Transcript(_), InputWindowSelector::RecentTranscript) => true,
        (PreparedInputEvidence::Message(x), InputWindowSelector::TrustedOperator) => {
            x.message_seq.is_some()
                && x.authority_class == AuthorityClass::OperatorInstruction
                && matches!(x.origin, crate::types::MessageOrigin::Operator { .. })
        }
        _ => false,
    }
}
fn compare_record_order(
    left: &QualifiedInputEvidence,
    right: &QualifiedInputEvidence,
) -> std::cmp::Ordering {
    let sequence = |record: &QualifiedInputEvidence| {
        if record.locator.key.family == InputEvidenceFamily::Transcript {
            Some(record.order_sequence.unwrap_or(u64::MAX))
        } else {
            record.order_sequence
        }
    };
    left.turn_index
        .cmp(&right.turn_index)
        .then_with(|| sequence(left).cmp(&sequence(right)))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| {
            if matches!(
                left.locator.key.family,
                InputEvidenceFamily::Brief
                    | InputEvidenceFamily::ToolExecution
                    | InputEvidenceFamily::Message
            ) {
                left.locator.key.id.cmp(&right.locator.key.id)
            } else {
                right.locator.key.id.cmp(&left.locator.key.id)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "agent-a".into(),
            identity_incarnation: 1,
        }
    }
    fn turn(id: &str, index: u64) -> Arc<QualifiedInputEvidence> {
        let record = TurnRecord::new("agent-a", id, index);
        Arc::new(QualifiedInputEvidence {
            locator: EvidenceLocator {
                scope: scope(),
                key: InputEvidenceKey::new(InputEvidenceFamily::Turn, id),
                revision: 1,
                content_digest: format!("digest-{id}"),
            },
            record: PreparedInputEvidence::Turn(record),
            created_at: "2026-10-12T00:00:00Z".into(),
            order_sequence: None,
            turn_index: Some(index),
            owner: Some(TurnOwner::AgentLifecycle {
                agent_id: "agent-a".into(),
            }),
            resident_bytes: 128,
            attempt_turn_keys: None,
            tool_run_report: None,
        })
    }
    fn resident(exhaustive: bool) -> ResidentInputEvidence {
        let a = turn("a", 1);
        let b = turn("b", 2);
        ResidentInputEvidence::bootstrap(
            scope(),
            vec![a.clone(), b.clone()],
            InputEvidenceCoverage {
                query: Default::default(),
                selected_completion_brief_ids: Default::default(),
                completion_source_observations: Default::default(),
                recovery_source_absences: Default::default(),
                recovery_lineage_messages: Default::default(),
                execution: ExecutionInputCoverage::default(),
                windows: vec![InputWindowCoverage {
                    selector: InputWindowSelector::GlobalTurns,
                    limit: 2,
                    keys: vec![b.locator.key.clone(), a.locator.key.clone()],
                    is_exhaustive: exhaustive,
                }],
                mandatory_roots: BTreeSet::new(),
                turn_transcript_keys: BTreeMap::from([
                    ("a".into(), BTreeSet::new()),
                    ("b".into(), BTreeSet::new()),
                ]),
                message_turn_keys: BTreeMap::new(),
                unavailable: BTreeSet::new(),
                record_limit: 8,
                byte_limit: 4096,
            },
        )
        .unwrap()
    }
    #[test]
    fn request_capture_does_not_pin_other_resident_mandatory_inputs() {
        let mut original = resident(true);
        original
            .coverage
            .mandatory_roots
            .insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, "a"));
        let unselected = Arc::downgrade(
            original
                .records
                .get(&InputEvidenceKey::new(InputEvidenceFamily::Turn, "a"))
                .unwrap(),
        );
        let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: vec![
                crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                    selector: InputWindowSelector::GlobalTurns,
                    limit: 1,
                },
            ],
            mandatory_roots: BTreeSet::new(),
            record_limit: 8,
            byte_limit: 4096,
        };
        let captured = original.capture_for_request(&request).unwrap();
        assert_eq!(captured.records.len(), 1);
        assert_eq!(captured.records[0].locator.key.id, "b");
        assert_eq!(captured.turn_transcript_keys.len(), 1);
        drop(original);
        assert!(unselected.upgrade().is_none());
        assert_eq!(captured.records[0].locator.key.id, "b");
    }

    #[test]
    fn request_capture_checks_selected_coverage_and_bounds() {
        let original = resident(true);
        let mut request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: Vec::new(),
            mandatory_roots: BTreeSet::from([InputEvidenceKey::new(
                InputEvidenceFamily::Turn,
                "missing",
            )]),
            record_limit: 8,
            byte_limit: 4096,
        };
        assert!(original
            .capture_for_request(&request)
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
        request.mandatory_roots =
            BTreeSet::from([InputEvidenceKey::new(InputEvidenceFamily::Turn, "a")]);
        request.windows = vec![
            crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                selector: InputWindowSelector::GlobalTurns,
                limit: 1,
            },
        ];
        request.record_limit = 1;
        assert!(original.capture_for_request(&request).is_err());
        request.record_limit = 8;
        request.byte_limit = 1;
        assert!(original.capture_for_request(&request).is_err());
        request.byte_limit = 4096;
        request.windows[0].limit = 3;
        assert!(original
            .capture_for_request(&request)
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
    }

    #[test]
    fn rolling_window_shares_unchanged_records_and_capture_does_not_pin_map() {
        let original = Arc::new(resident(false));
        let captured = original.capture().unwrap();
        let old = original
            .records
            .get(&InputEvidenceKey::new(InputEvidenceFamily::Turn, "b"))
            .unwrap()
            .clone();
        let next = original
            .with_committed(
                &CommittedInputEvidenceDelta {
                    query_advance: None,
                    output_recall_invalidation: None,
                    execution: ExecutionInputDelta::default(),
                    records: vec![turn("c", 3)],
                    removals: Vec::new(),
                    turn_transcript_keys: BTreeMap::from([("c".into(), BTreeSet::new())]),
                    message_turn_keys: BTreeMap::new(),
                },
                BTreeSet::new(),
            )
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(
            &old,
            next.records.get(&old.locator.key).unwrap()
        ));
        assert_eq!(
            next.capture().unwrap().windows[0]
                .keys
                .iter()
                .map(|key| key.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "b"]
        );
        let weak = Arc::downgrade(&original);
        drop(original);
        assert!(weak.upgrade().is_none());
        assert_eq!(captured.windows[0].keys[0].id, "b");
    }
    #[test]
    fn removal_must_match_the_immediate_resident_predecessor() {
        let original = resident(true);
        let captured = original.capture().unwrap();
        let key = InputEvidenceKey::new(InputEvidenceFamily::Turn, "b");
        let mut delta = CommittedInputEvidenceDelta {
            removals: vec![CommittedEvidenceRemoval {
                scope: scope(),
                key: key.clone(),
                revision: 2,
                previous_content_digest: Some("another-body".into()),
            }],
            ..Default::default()
        };
        assert!(original
            .with_committed(&delta, BTreeSet::new())
            .unwrap_err()
            .to_string()
            .contains("immediate predecessor"));
        assert!(original.capture().unwrap().lookup(&key).unwrap().is_some());
        delta.removals[0].previous_content_digest = Some("digest-b".into());
        let next = original
            .with_committed(&delta, BTreeSet::new())
            .unwrap()
            .unwrap();
        assert!(!next.records.contains_key(&key));
        assert!(next
            .capture()
            .unwrap()
            .records
            .iter()
            .all(|record| record.locator.key != key));
        assert!(
            captured.lookup(&key).unwrap().is_some(),
            "already captured bytes keep their own version"
        );
        // A resident copy can lag multiple canonical revisions; the current
        // removal is authoritative without claiming a direct digest relation.
        delta.removals[0].revision = 3;
        delta.removals[0].previous_content_digest = Some("intermediate-body".into());
        assert!(!original
            .with_committed(&delta, BTreeSet::new())
            .unwrap()
            .unwrap()
            .records
            .contains_key(&key));
    }

    #[test]
    fn truncated_window_removal_invalidates_instead_of_faking_a_full_tail() {
        let original = resident(false);
        let removal = CommittedEvidenceRemoval {
            scope: scope(),
            key: InputEvidenceKey::new(InputEvidenceFamily::Turn, "b"),
            revision: 2,
            previous_content_digest: Some("digest-b".into()),
        };
        assert!(original
            .with_committed(
                &CommittedInputEvidenceDelta {
                    query_advance: None,
                    output_recall_invalidation: None,
                    execution: ExecutionInputDelta::default(),
                    records: Vec::new(),
                    removals: vec![removal.clone()],
                    turn_transcript_keys: BTreeMap::new(),
                    message_turn_keys: BTreeMap::new(),
                },
                BTreeSet::new()
            )
            .unwrap()
            .is_none());
        let exhaustive = resident(true);
        let next = exhaustive
            .with_committed(
                &CommittedInputEvidenceDelta {
                    query_advance: None,
                    output_recall_invalidation: None,
                    execution: ExecutionInputDelta::default(),
                    records: Vec::new(),
                    removals: vec![removal],
                    turn_transcript_keys: BTreeMap::new(),
                    message_turn_keys: BTreeMap::new(),
                },
                BTreeSet::new(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(next.capture().unwrap().windows[0].keys.len(), 1);
    }
    #[test]
    fn exhaustive_window_growth_then_removal_requires_hydration() {
        let original = resident(true);
        let grown = original
            .with_committed(
                &CommittedInputEvidenceDelta {
                    query_advance: None,
                    output_recall_invalidation: None,
                    execution: ExecutionInputDelta::default(),
                    records: vec![turn("c", 3)],
                    removals: Vec::new(),
                    turn_transcript_keys: BTreeMap::from([("c".into(), BTreeSet::new())]),
                    message_turn_keys: BTreeMap::new(),
                },
                BTreeSet::new(),
            )
            .unwrap()
            .unwrap();
        assert!(!grown.capture().unwrap().windows[0].is_exhaustive);
        assert!(!grown
            .records
            .contains_key(&InputEvidenceKey::new(InputEvidenceFamily::Turn, "a")));
        let pruned = grown
            .with_committed(
                &CommittedInputEvidenceDelta {
                    query_advance: None,
                    output_recall_invalidation: None,
                    execution: ExecutionInputDelta::default(),
                    records: Vec::new(),
                    removals: vec![CommittedEvidenceRemoval {
                        scope: scope(),
                        key: InputEvidenceKey::new(InputEvidenceFamily::Turn, "b"),
                        revision: 2,
                        previous_content_digest: Some("digest-b".into()),
                    }],
                    turn_transcript_keys: BTreeMap::new(),
                    message_turn_keys: BTreeMap::new(),
                },
                BTreeSet::new(),
            )
            .unwrap();
        assert!(pruned.is_none());
    }
    #[test]
    fn missing_mandatory_descendant_never_becomes_optional_unavailable() {
        let mut parent = turn("parent", 1).as_ref().clone();
        let PreparedInputEvidence::Turn(turn) = &mut parent.record else {
            unreachable!()
        };
        turn.input_message_ids.push("missing-input".into());
        let parent = Arc::new(parent);
        let key = parent.locator.key.clone();
        let missing = InputEvidenceKey::new(InputEvidenceFamily::Message, "missing-input");
        assert!(ResidentInputEvidence::bootstrap(
            scope(),
            vec![parent],
            InputEvidenceCoverage {
                query: Default::default(),
                selected_completion_brief_ids: Default::default(),
                completion_source_observations: Default::default(),
                recovery_source_absences: Default::default(),
                recovery_lineage_messages: Default::default(),
                execution: ExecutionInputCoverage::default(),
                windows: Vec::new(),
                mandatory_roots: BTreeSet::from([key]),
                turn_transcript_keys: BTreeMap::from([("parent".into(), BTreeSet::new())]),
                message_turn_keys: BTreeMap::new(),
                unavailable: BTreeSet::from([missing]),
                record_limit: 8,
                byte_limit: 4096
            }
        )
        .is_err());
    }
    #[test]
    fn committed_history_growth_checks_agent_and_shared_host_limits() {
        fn delta() -> CommittedInputEvidenceDelta {
            CommittedInputEvidenceDelta {
                query_advance: None,
                output_recall_invalidation: None,
                execution: ExecutionInputDelta::default(),
                records: vec![turn("c", 3)],
                removals: Vec::new(),
                turn_transcript_keys: BTreeMap::from([("c".into(), BTreeSet::new())]),
                message_turn_keys: BTreeMap::new(),
            }
        }
        fn history_bytes(value: &ResidentInputEvidence) -> u64 {
            value
                .records
                .values()
                .map(|record| {
                    u64::try_from(record.resident_material_estimate().unwrap().0).unwrap()
                })
                .sum()
        }
        let original = resident(true);
        let base = history_bytes(&original);
        let growth = u64::try_from(turn("c", 3).resident_material_estimate().unwrap().0).unwrap();
        assert!(growth > 1);
        let mut limits = crate::config::StateMemoryLimits::default();
        limits.history_per_agent_bytes = base + growth / 2;
        assert!(base < limits.history_per_agent_bytes);
        assert!(base + growth > limits.history_per_agent_bytes);
        let budget = InputEvidenceBudgetLedger::new(limits).unwrap();
        let original = original.with_budget(budget.clone()).unwrap();
        let error = original
            .with_committed(&delta(), BTreeSet::new())
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(
                super::super::input_evidence_budget::InputEvidenceBudgetError::ResourceExhausted {
                    resource:
                        super::super::input_evidence_budget::BudgetResource::HistoryAgentBytes,
                    ..
                }
            )
        ));
        assert_eq!(original.capture().unwrap().windows[0].keys[0].id, "b");
        drop(original);
        assert_eq!(budget.report().unwrap().observed_bytes, 0);

        let original = resident(true);
        let mut other = resident(true);
        other.scope.agent_id = "agent-b".into();
        other.records = other
            .records
            .into_iter()
            .map(|(key, record)| {
                let mut record = record.as_ref().clone();
                record.locator.scope.agent_id = "agent-b".into();
                let PreparedInputEvidence::Turn(turn) = &mut record.record else {
                    unreachable!()
                };
                turn.agent_id = "agent-b".into();
                turn.owner = Some(TurnOwner::AgentLifecycle {
                    agent_id: "agent-b".into(),
                });
                record.owner = turn.owner.clone();
                (key, Arc::new(record))
            })
            .collect();
        let base = history_bytes(&original);
        let other_base = history_bytes(&other);
        let mut limits = crate::config::StateMemoryLimits::default();
        limits.history_per_agent_bytes = base.max(other_base) + growth;
        limits.history_host_bytes = base + other_base + growth / 2;
        assert!(base + growth <= limits.history_per_agent_bytes);
        assert!(base + other_base < limits.history_host_bytes);
        assert!(base + other_base + growth > limits.history_host_bytes);
        let budget = InputEvidenceBudgetLedger::new(limits).unwrap();
        let original = original.with_budget(budget.clone()).unwrap();
        let other = other.with_budget(budget.clone()).unwrap();
        let error = original
            .with_committed(&delta(), BTreeSet::new())
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(
                super::super::input_evidence_budget::InputEvidenceBudgetError::ResourceExhausted {
                    resource: super::super::input_evidence_budget::BudgetResource::HistoryHostBytes,
                    ..
                }
            )
        ));
        assert_eq!(budget.report().unwrap().physical_records.allocations, 4);
        drop((original, other));
        assert_eq!(budget.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn budget_observes_selected_capture_and_releases_unselected_allocations() {
        let budget =
            InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
        let mut original = resident(true);
        let key = InputEvidenceKey::new(InputEvidenceFamily::Turn, "a");
        original.coverage.mandatory_roots.insert(key.clone());
        let weak = Arc::downgrade(original.records.get(&key).unwrap());
        let original = original.with_budget(budget.clone()).unwrap();
        let captured = original
            .capture_for_request(
                &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
                    selected_completion_brief_ids: Default::default(),
                    interrupted_sources: Default::default(),
                    windows: vec![
                        crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                            selector: InputWindowSelector::GlobalTurns,
                            limit: 1,
                        },
                    ],
                    mandatory_roots: BTreeSet::new(),
                    record_limit: 8,
                    byte_limit: 4096,
                },
            )
            .unwrap();
        assert_eq!(budget.report().unwrap().physical_records.allocations, 2);
        drop(original);
        assert!(
            weak.upgrade().is_none(),
            "the ledger and capture must not retain unrelated input"
        );
        let report = budget.report().unwrap();
        assert_eq!(report.physical_records.allocations, 1);
        assert_eq!(
            report.categories[&InputMemoryCategory::Pins].allocations,
            2,
            "one selected record plus its separately estimated metadata allocation"
        );
        assert!(report.owned.metadata_estimate_bytes > 0);
        assert!(!report
            .categories
            .contains_key(&InputMemoryCategory::History));
        drop(captured);
        let report = budget.report().unwrap();
        assert_eq!(report.physical_records.allocations, 0);
        assert_eq!(report.observed_bytes, 0);
    }
}

#[cfg(test)]
#[path = "resident_input_completion_tests.rs"]
mod completion_tests;
