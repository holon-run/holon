//! Qualified, bounded input/evidence allocations. No storage capability lives here.

use super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, ExecutionOutcomeRecord, WorkItemExecutionRecord,
    },
    types::{
        AuthorityClass, BriefRecord, MessageEnvelope, ToolExecutionRecord, ToolExecutionStatus,
        TranscriptEntry, TurnOwner, TurnRecord,
    },
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::Arc,
};

#[path = "input_evidence_output_recall.rs"]
mod output_recall;

#[path = "input_evidence_queries.rs"]
mod queries;
pub(crate) use queries::{
    CapturedBirthInputs, InputQueryCoverage, InputQueryError, InputQueryRead, InputQuerySelection,
    OriginalInputQuerySource,
};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputEvidenceFamily {
    Message,
    Turn,
    ToolExecution,
    Brief,
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
            "transcript" => Self::Transcript,
            "attempt" => Self::Attempt,
            "outcome" => Self::Outcome,
            "work_execution" => Self::WorkExecution,
            _ => anyhow::bail!("unsupported input evidence family: {value}"),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
        let mut add = |family, id: &str| {
            refs.insert(InputEvidenceKey::new(family, id));
        };
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
                if let Some(id) = x
                    .idle_input_batch
                    .as_ref()
                    .and_then(|batch| batch.activation_id.as_deref())
                {
                    add(InputEvidenceFamily::Attempt, id)
                }
                if let Some(replay) = &x.replay {
                    // Actual assignment owns the body; the replay address is
                    // also observed (possibly absent) for exact lineage checks.
                    add(InputEvidenceFamily::Message, &replay.source_message_id);
                    add(InputEvidenceFamily::Turn, &replay.source_turn_id);
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
        refs
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
}
impl QualifiedInputEvidence {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
                key == &InputEvidenceKey::new(InputEvidenceFamily::Message, &source)
                    || key == &InputEvidenceKey::new(InputEvidenceFamily::Turn, &turn)
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReplaySourceAbsence {
    pub replay_turn: EvidenceLocator,
    pub source_message_id: String,
    pub source_turn_id: String,
}

impl ReplaySourceAbsence {
    pub(crate) fn matches(&self, record: &QualifiedInputEvidence) -> bool {
        let PreparedInputEvidence::Turn(turn) = &record.record else {
            return false;
        };
        self.replay_turn == record.locator
            && record.locator.key.family == InputEvidenceFamily::Turn
            && record.locator.key.id == turn.turn_id
            && record.locator.scope.agent_id == turn.agent_id
            && turn.replay.as_ref().is_some_and(|replay| {
                replay.reason == "interrupted_queue_claim_reentry"
                    && !replay.source_message_id.trim().is_empty()
                    && !replay.source_turn_id.trim().is_empty()
                    && replay.source_message_id == self.source_message_id
                    && replay.source_turn_id == self.source_turn_id
            })
    }

    pub(crate) fn observes_key(&self, key: &InputEvidenceKey) -> bool {
        key == &self.replay_turn.key
            || key == &InputEvidenceKey::new(InputEvidenceFamily::Turn, &self.source_turn_id)
    }
}

pub(crate) fn replay_references(
    record: &QualifiedInputEvidence,
    absences: &BTreeMap<String, ReplaySourceAbsence>,
) -> Result<BTreeSet<InputEvidenceKey>> {
    let mut refs = record.references();
    if let PreparedInputEvidence::Turn(turn) = &record.record {
        if let Some(proof) = absences.get(&turn.turn_id) {
            ensure!(proof.matches(record), "replay absence version differs");
            refs.remove(&InputEvidenceKey::new(
                InputEvidenceFamily::Turn,
                &proof.source_turn_id,
            ));
        }
    }
    Ok(refs)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InputEvidenceCoverage {
    pub selected_completion_brief_ids: BTreeSet<String>,
    pub completion_source_observations:
        Vec<super::completion_sources::CompletionOptionalSourceObservation>,
    pub recovery_source_absences: BTreeMap<String, RecoverySourceAbsence>,
    pub replay_source_absences: BTreeMap<String, ReplaySourceAbsence>,
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
    pub replay_source_absences: BTreeMap<String, ReplaySourceAbsence>,
    /// Exact qualified Transcript frontier for each Turn included in this commit.
    pub turn_transcript_keys: BTreeMap<String, BTreeSet<InputEvidenceKey>>,
    /// None is an actual same-view unassigned input, not a missing preparation.
    pub message_turn_keys: BTreeMap<String, Option<String>>,
}

impl CommittedInputEvidenceDelta {
    pub(super) fn is_empty(&self) -> bool {
        let Self {
            query_advance,
            output_recall_invalidation,
            execution,
            records,
            removals,
            replay_source_absences,
            turn_transcript_keys,
            message_turn_keys,
        } = self;
        let ExecutionInputDelta {
            initialized,
            sources,
            open_attempts,
            nonterminal_work_items,
            work_items,
        } = execution;
        query_advance.is_none()
            && output_recall_invalidation.is_none()
            && initialized.is_none()
            && sources.is_empty()
            && open_attempts.is_empty()
            && nonterminal_work_items.is_empty()
            && work_items.is_empty()
            && records.is_empty()
            && removals.is_empty()
            && replay_source_absences.is_empty()
            && turn_transcript_keys.is_empty()
            && message_turn_keys.is_empty()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResidentInputEvidence {
    query: Option<Arc<InputQueryCoverage>>,
    query_material: Vec<Arc<BudgetViewGuard>>,
    query_acquisition: Option<crate::diagnostics::resource_io::ResourceReadMaterial>,
    output_recall_roots: BTreeSet<InputEvidenceKey>,
    output_recall_task: Option<super::resident_task::TaskFactLocator>,
    scope: InputEvidenceScope,
    records: BTreeMap<InputEvidenceKey, Arc<QualifiedInputEvidence>>,
    coverage: InputEvidenceCoverage,
    /// Exact derived windows, not a validation proof. Record writes invalidate
    /// this snapshot; selector/limit (and output) changes fail the equality check.
    window_projection: Option<Arc<Vec<InputWindowCoverage>>>,
    budget: Option<InputEvidenceBudgetLedger>,
    budget_views: Vec<Arc<BudgetViewGuard>>,
    /// Key set proven by this instance's last full validation. A published
    /// instance is immutable until `with_committed` clones it, so the cache is
    /// exact evidence of prior validation, never a substitute for it.
    validated_keys: Option<Arc<BTreeSet<InputEvidenceKey>>>,
    validated_metadata_bytes: Option<u64>,
}
/// This object retains selected allocations, not the domain map or published root.
#[derive(Debug)]
pub(crate) struct CapturedInputEvidence {
    pub query: Option<Arc<InputQueryCoverage>>,
    pub query_acquisition: Option<crate::diagnostics::resource_io::ResourceReadMaterial>,
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
    pub message_turn_keys: Arc<BTreeMap<String, Option<String>>>,
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
        if let Some(material) = &self.query_acquisition {
            material.finish()?;
        }
        let mut bytes = serialized_bytes(&(
            &self.scope,
            &self.windows,
            &self.unavailable,
            &self.turn_transcript_keys,
            &self.message_turn_keys,
            &self.recovery_source_absences,
            &self.execution,
        ))?;
        if let Some(query) = &self.query {
            bytes = bytes
                .checked_add(serialized_bytes(query)?)
                .context("selected Input query material bytes overflow")?;
        }
        let mut count = self
            .records
            .len()
            .checked_add(self.windows.len())
            .and_then(|n| n.checked_add(self.unavailable.len()))
            .and_then(|n| n.checked_add(self.turn_transcript_keys.len()))
            .and_then(|n| n.checked_add(self.message_turn_keys.len()))
            .and_then(|n| n.checked_add(self.recovery_source_absences.len()))
            .and_then(|n| n.checked_add(self.execution.metadata_count()))
            .and_then(|n| {
                n.checked_add(
                    self.query
                        .as_ref()
                        .map_or(0, |q| q.required_keys.len() + q.born.len()),
                )
            })
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
        self.find_address(key.family, &key.id)
            .ok_or_else(|| InputEvidenceReadError::NeedsPreparation(key.clone()))
    }
    /// Success borrows a captured address; only a missing-address error owns a key.
    pub(crate) fn lookup_borrowed(
        &self,
        family: InputEvidenceFamily,
        id: &str,
    ) -> Result<Option<&Arc<QualifiedInputEvidence>>> {
        crate::diagnostics::resource_io::check_active()?;
        if let Some(record) = self.find_address(family, id) {
            return Ok(record);
        }
        crate::diagnostics::resource_io::admit_cloned_value(&(family, id))?;
        Err(InputEvidenceReadError::NeedsPreparation(InputEvidenceKey::new(family, id)).into())
    }
    fn find_address(
        &self,
        family: InputEvidenceFamily,
        id: &str,
    ) -> Option<Option<&Arc<QualifiedInputEvidence>>> {
        if let Some(record) = self
            .records
            .iter()
            .find(|record| record.locator.key.family == family && record.locator.key.id == id)
        {
            return Some(Some(record));
        }
        if self
            .unavailable
            .iter()
            .any(|key| key.family == family && key.id == id)
        {
            return Some(None);
        }
        None
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
        let mut state =
            crate::domain::execution_protocol::ExecutionProtocolState::empty(&self.scope.agent_id);
        for evidence in &self.records {
            match &evidence.record {
                PreparedInputEvidence::Attempt(x) => {
                    state.attempts.insert(x.attempt_id.clone(), x.clone());
                }
                PreparedInputEvidence::Outcome(x) => {
                    state.outcomes.insert(x.outcome_id.clone(), x.clone());
                }
                PreparedInputEvidence::WorkExecution {
                    work_item_id,
                    record,
                } => {
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
        Self::bootstrap_with_query(scope, records, coverage, None, Vec::new(), None)
    }
    pub(crate) fn bootstrap_with_query(
        scope: InputEvidenceScope,
        records: Vec<Arc<QualifiedInputEvidence>>,
        coverage: InputEvidenceCoverage,
        query: Option<InputQueryCoverage>,
        query_material: Vec<Arc<BudgetViewGuard>>,
        query_acquisition: Option<crate::diagnostics::resource_io::ResourceReadMaterial>,
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
            query: query.map(Arc::new),
            query_material,
            query_acquisition,
            output_recall_roots: BTreeSet::new(),
            output_recall_task: None,
            scope,
            records: map,
            coverage,
            window_projection: None,
            budget: None,
            budget_views: Vec::new(),
            validated_keys: None,
            validated_metadata_bytes: None,
        };
        result.trim_and_validate()?;
        Ok(result)
    }
    /// Observation guards own no evidence. The real domain/capture owns the
    /// allocations and the ledger keeps only Weak references to them.
    pub(crate) fn with_budget(mut self, budget: InputEvidenceBudgetLedger) -> Result<Self> {
        for guard in &self.query_material {
            guard.validate_owned_material_binding(&self.scope, &budget, 0)?;
        }
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
        if self.validated_keys.is_some() {
            if let Some(bytes) = self.validated_metadata_bytes {
                return Ok(bytes);
            }
        }
        self.compute_metadata_estimate_bytes()
    }

    fn compute_metadata_estimate_bytes(&self) -> Result<u64> {
        let nodes = self
            .records
            .len()
            .checked_mul(std::mem::size_of::<(
                InputEvidenceKey,
                Arc<QualifiedInputEvidence>,
            )>())
            .ok_or_else(|| anyhow::anyhow!("input metadata estimate overflow"))?;
        let recall_task_bytes = serialized_bytes(&self.output_recall_task)?;
        let validated_key_bytes = self
            .validated_keys
            .as_deref()
            .map(serialized_bytes)
            .transpose()?
            .unwrap_or(0);
        let query_bytes = self
            .query
            .as_deref()
            .map(serialized_bytes)
            .transpose()?
            .unwrap_or(0);
        let window_projection_bytes = self
            .window_projection
            .as_deref()
            .map(serialized_bytes)
            .transpose()?
            .unwrap_or(0);
        Ok(serialized_bytes(&self.coverage)?
            .checked_add(query_bytes)
            .context("Input query metadata estimate overflow")?
            .checked_add(serialized_bytes(&self.output_recall_roots)?)
            .and_then(|bytes| bytes.checked_add(recall_task_bytes))
            .and_then(|bytes| bytes.checked_add(validated_key_bytes))
            .and_then(|bytes| bytes.checked_add(window_projection_bytes))
            .and_then(|bytes| bytes.checked_add(nodes as u64))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>() as u64))
            .ok_or_else(|| anyhow::anyhow!("input metadata estimate overflow"))?)
    }

    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
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
    /// A scheduler needs the complete live protocol frontier, not every queued
    /// Message's source index. These IDs never qualify their record bodies.
    pub(crate) fn scheduler_execution_roots(
        &self,
    ) -> impl Iterator<Item = (InputEvidenceFamily, &str)> {
        self.coverage
            .execution
            .open_attempt_ids
            .iter()
            .map(|id| (InputEvidenceFamily::Attempt, id.as_str()))
            .chain(
                self.coverage
                    .execution
                    .nonterminal_work_item_ids
                    .iter()
                    .map(|id| (InputEvidenceFamily::WorkExecution, id.as_str())),
            )
    }
    pub(crate) fn nonterminal_execution_roots_after(
        &self,
        delta: &CommittedInputEvidenceDelta,
    ) -> Result<BTreeSet<InputEvidenceKey>> {
        use crate::domain::execution_protocol::{ExecutionAttemptState, WorkItemExecutionState};
        let selected = self.validated_required_keys()?;
        // Index this commit only. Later records win, including over removals,
        // without rescanning the delta for every selected record and suffix.
        let replacements = delta
            .records
            .iter()
            .map(|record| (&record.locator.key, record))
            .collect::<BTreeMap<_, _>>();
        let removals = delta
            .removals
            .iter()
            .map(|removed| &removed.key)
            .collect::<BTreeSet<_>>();
        let effective = selected
            .iter()
            .filter(|key| !removals.contains(key) && !replacements.contains_key(key))
            .filter_map(|key| self.records.get(key))
            .chain(replacements.values().copied());
        Ok(effective
            .filter(|record| match &record.record {
                PreparedInputEvidence::Attempt(attempt) => {
                    attempt.state != ExecutionAttemptState::Settled
                }
                PreparedInputEvidence::WorkExecution { record, .. } => {
                    !matches!(record.state, WorkItemExecutionState::Terminal { .. })
                }
                _ => false,
            })
            .map(|record| record.locator.key.clone())
            .collect())
    }
    pub(crate) fn capture(&self) -> Result<CapturedInputEvidence> {
        if let Some(material) = &self.query_acquisition {
            material.read(|| {
                crate::diagnostics::resource_io::admit_cloned_value(&(
                    &self.scope,
                    &self.coverage,
                ))?;
                self.capture_inner()
            })
        } else {
            self.capture_inner()
        }
    }
    pub(crate) fn capture_with_material(
        &self,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<CapturedInputEvidence> {
        self.validate_capture_material(material)?;
        material.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&(&self.scope, &self.coverage))?;
            self.capture_inner()
        })
    }
    fn validate_capture_material(
        &self,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<()> {
        ensure!(
            material.matches_scope(&self.scope)
                && self
                    .budget
                    .as_ref()
                    .is_none_or(|budget| material.matches_owner_identity(
                        budget,
                        &self.scope.agent_id,
                        self.scope.identity_incarnation,
                    )),
            "Input capture material owner differs"
        );
        Ok(())
    }
    fn capture_inner(&self) -> Result<CapturedInputEvidence> {
        let selected = self.validated_required_keys()?;
        crate::diagnostics::resource_io::charge_owned_metadata(
            (std::mem::size_of::<BTreeMap<String, Option<String>>>()
                + 2 * std::mem::size_of::<usize>()) as u64,
            1,
        )?;
        let mut captured = CapturedInputEvidence {
            query: self.query.clone(),
            query_acquisition: self.query_acquisition.clone(),
            #[cfg(test)]
            selected_completion_brief_ids: self.coverage.selected_completion_brief_ids.clone(),
            #[cfg(test)]
            completion_source_observations: self.coverage.completion_source_observations.clone(),
            recovery_source_absences: self.coverage.recovery_source_absences.clone(),
            execution: self.coverage.execution.clone(),
            scope: self.scope.clone(),
            budget: self.budget.clone(),
            budget_views: self.query_material.clone(),
            windows: self.coverage.windows.clone(),
            records: selected
                .iter()
                .filter_map(|key| self.records.get(key).cloned())
                .collect(),
            unavailable: self.coverage.unavailable.clone(),
            turn_transcript_keys: self.coverage.turn_transcript_keys.clone(),
            message_turn_keys: Arc::new(self.coverage.message_turn_keys.clone()),
        };
        captured.protocol_state()?;
        if let Some(budget) = &self.budget {
            captured.budget_views.push(budget.register_view(
                &self.scope,
                InputMemoryCategory::Pins,
                &captured.records,
            )?);
            captured.budget_views.push(budget.register_owned_bytes(
                &self.scope,
                InputMemoryCategory::Pins,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: self.metadata_estimate_bytes()?,
                },
            )?);
        }
        Ok(captured)
    }
    /// Request selection already owns a validated temporary domain. Move its
    /// metadata instead of cloning a second coverage before dropping the first.
    fn into_capture(self) -> Result<CapturedInputEvidence> {
        let selected = self.validated_required_keys()?;
        ensure!(
            self.records.keys().all(|key| selected.contains(key)),
            "input capture contains unselected records"
        );
        let metadata_estimate_bytes = self.metadata_estimate_bytes()?;
        let count = self.records.len();
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(
                count
                    .checked_mul(std::mem::size_of::<Arc<QualifiedInputEvidence>>())
                    .and_then(|bytes| {
                        bytes.checked_add(
                            std::mem::size_of::<Vec<Arc<QualifiedInputEvidence>>>()
                                + 2 * std::mem::size_of::<usize>(),
                        )
                    })
                    .context("input capture slots overflow")?,
            )?,
            count
                .checked_add(1)
                .context("input capture slots overflow")?,
        )?;
        let mut records = Vec::with_capacity(count);
        records.extend(self.records.into_values());
        let mut captured = CapturedInputEvidence {
            query: self.query,
            query_acquisition: self.query_acquisition,
            #[cfg(test)]
            selected_completion_brief_ids: self.coverage.selected_completion_brief_ids,
            #[cfg(test)]
            completion_source_observations: self.coverage.completion_source_observations,
            recovery_source_absences: self.coverage.recovery_source_absences,
            execution: self.coverage.execution,
            scope: self.scope,
            budget: self.budget,
            budget_views: self.query_material,
            windows: self.coverage.windows,
            records,
            unavailable: self.coverage.unavailable,
            turn_transcript_keys: self.coverage.turn_transcript_keys,
            message_turn_keys: Arc::new(self.coverage.message_turn_keys),
        };
        captured.protocol_state()?;
        if let Some(budget) = &captured.budget {
            captured.budget_views.push(budget.register_view(
                &captured.scope,
                InputMemoryCategory::Pins,
                &captured.records,
            )?);
            captured.budget_views.push(budget.register_owned_bytes(
                &captured.scope,
                InputMemoryCategory::Pins,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes,
                },
            )?);
        }
        Ok(captured)
    }
    /// A request owns only its selected closure. Resident mandatory roots also
    /// contain other accepted inputs and must not be pinned by every prompt.
    pub(crate) fn capture_for_request(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    ) -> Result<CapturedInputEvidence> {
        if request.query.is_some() {
            let material = self
                .query_acquisition
                .as_ref()
                .context("Input query omitted original acquisition")?;
            return self.capture_for_request_with_material(request, material);
        }
        self.capture_request_inner(request, None)
    }
    /// Explicit preparation owns its temporary capture, while the original
    /// query still supplies qualification and its failure latch.
    pub(crate) fn capture_for_request_with_material(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<CapturedInputEvidence> {
        self.validate_request_material(request, material)?;
        // A known selector miss is a preparation need, not an owning probe.
        self.validate_capture_selectors(request)?;
        material.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(request)?;
            self.capture_request_inner(request, Some(material))
        })
    }
    fn validate_capture_selectors(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    ) -> Result<()> {
        ensure!(
            request.record_limit > 0 && request.byte_limit > 0,
            "input request needs positive resource bounds"
        );
        for key in &request.mandatory_roots {
            if !self.records.contains_key(key) && !self.coverage.unavailable.contains(key) {
                return Err(InputEvidenceReadError::NeedsPreparation(key.clone()).into());
            }
        }
        for id in &request.selected_completion_brief_ids {
            // A committed, resident head can enter a new completion selection.
            // The capture below still proves its whole requested closure;
            // only exact existing observations can omit an optional source.
            if !self.coverage.selected_completion_brief_ids.contains(id)
                && !self
                    .records
                    .contains_key(&InputEvidenceKey::new(InputEvidenceFamily::Brief, id))
            {
                return Err(
                    InputEvidenceReadError::NeedsPreparation(InputEvidenceKey::new(
                        InputEvidenceFamily::Brief,
                        id,
                    ))
                    .into(),
                );
            }
        }
        for (index, wanted) in request.windows.iter().enumerate() {
            ensure!(
                !request.windows[..index]
                    .iter()
                    .any(|other| other.selector == wanted.selector),
                "duplicate input request selector"
            );
            if !self.coverage.windows.iter().any(|prepared| {
                prepared.selector == wanted.selector && wanted.limit <= prepared.limit
            }) {
                return Err(InputEvidenceReadError::WindowNotPrepared.into());
            }
        }
        Ok(())
    }
    fn validate_request_material(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<()> {
        self.validate_capture_material(material)?;
        material.finish()?;
        if let Some(query) = &request.query {
            ensure!(
                self.budget
                    .as_ref()
                    .is_none_or(|budget| query.ledger.same_ledger(budget)),
                "Input query ledger differs"
            );
            let accepted = self
                .query
                .as_ref()
                .context(InputQueryError::NeedsPreparation)?;
            accepted.validate_selection(&query.selection)?;
            for guard in &self.query_material {
                guard.validate_owned_material_binding(&self.scope, &query.ledger, 0)?;
            }
            self.query_acquisition
                .as_ref()
                .context("Input query omitted original acquisition")?
                .finish()?;
        }
        Ok(())
    }
    /// A subset of the already validated closure needs no throwaway owning
    /// capture. A false result is inconclusive, not permission to read SQL.
    pub(crate) fn preparation_request_is_covered(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<bool> {
        self.validate_request_material(request, material)?;
        ensure!(
            request.record_limit > 0 && request.byte_limit > 0,
            "input request needs positive resource bounds"
        );
        if request.record_limit < self.coverage.record_limit
            || request.byte_limit < self.coverage.byte_limit
            || !request
                .mandatory_roots
                .is_subset(&self.coverage.mandatory_roots)
        {
            return Ok(false);
        }
        // A new completion designation cannot strengthen an already strict
        // mandatory/window path in this validated publication. An arbitrary
        // resident candidate still needs the complete request capture below.
        for id in request
            .selected_completion_brief_ids
            .difference(&self.coverage.selected_completion_brief_ids)
        {
            let is_head =
                |key: &InputEvidenceKey| key.family == InputEvidenceFamily::Brief && key.id == *id;
            if !self.records.keys().any(is_head)
                || (!self.coverage.mandatory_roots.iter().any(is_head)
                    && !self
                        .coverage
                        .windows
                        .iter()
                        .any(|window| window.keys.iter().any(is_head)))
            {
                return Ok(false);
            }
        }
        // Dropping a completion-only head can strengthen an optional edge.
        // A separate mandatory/window path already proved the whole strict
        // closure, so removing that head needs no second owning projection.
        for id in self
            .coverage
            .selected_completion_brief_ids
            .difference(&request.selected_completion_brief_ids)
        {
            let is_head =
                |key: &InputEvidenceKey| key.family == InputEvidenceFamily::Brief && key.id == *id;
            if !self.coverage.mandatory_roots.iter().any(is_head)
                && !self
                    .coverage
                    .windows
                    .iter()
                    .any(|window| window.keys.iter().any(is_head))
            {
                return Ok(false);
            }
        }
        for source in &request.interrupted_sources {
            let Some(observed) = self.coverage.execution.sources.get(source) else {
                return Ok(false);
            };
            // The same-view index proves an empty candidate set without an
            // owning closure probe. Actual interrupted bodies still need it.
            if observed.attempts.values().any(|attempt| {
                !attempt.pruned && attempt.state == ExecutionAttemptState::Interrupted
            }) {
                return Ok(false);
            }
        }
        for (index, wanted) in request.windows.iter().enumerate() {
            ensure!(
                !request.windows[..index]
                    .iter()
                    .any(|other| other.selector == wanted.selector),
                "duplicate input request selector"
            );
            if !self.coverage.windows.iter().any(|prepared| {
                prepared.selector == wanted.selector && wanted.limit <= prepared.limit
            }) {
                return Ok(false);
            }
        }
        for key in &request.mandatory_roots {
            if let Some(record) = self.records.get(key) {
                if let PreparedInputEvidence::Message(message) = &record.record {
                    // Writer-supplied bodies alone do not prove recovery lineage.
                    if super::turn::TurnModelSelection::message_has_provider_recovery_provenance(
                        message,
                    ) && !self.coverage.recovery_lineage_messages.contains(&key.id)
                    {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }
    /// Borrow an explicit mandatory root only after the complete same-view
    /// preparation proof. None leaves the caller's exact capture path intact.
    pub(crate) fn lookup_prepared_request_root(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
        family: InputEvidenceFamily,
        id: &str,
    ) -> Result<Option<&Arc<QualifiedInputEvidence>>> {
        if !self.preparation_request_is_covered(request, material)? {
            return Ok(None);
        }
        Ok(request
            .mandatory_roots
            .iter()
            .find(|key| key.family == family && key.id == id)
            .and_then(|key| self.records.get(key)))
    }
    fn capture_request_inner(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        material: Option<&crate::diagnostics::resource_io::ResourceReadMaterial>,
    ) -> Result<CapturedInputEvidence> {
        use InputEvidenceReadError::WindowNotPrepared;
        self.validate_capture_selectors(request)?;
        // A writer may have supplied the incoming body without observing its
        // asserted lineage. Only an explicit request can require that SQL view.
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
        let mut mandatory_roots = request.mandatory_roots.clone();
        mandatory_roots.extend(
            self.coverage
                .execution
                .required_interrupted_body_keys(&request.interrupted_sources)?,
        );
        let mut selected = mandatory_roots.clone();
        let query = request
            .query
            .as_ref()
            .map(|query| {
                self.query
                    .as_ref()
                    .context(InputQueryError::NeedsPreparation)?
                    .for_selection(&query.selection)
            })
            .transpose()?;
        if let Some(query) = &query {
            selected.extend(query.observation_keys().cloned());
        }
        selected.extend(
            request
                .selected_completion_brief_ids
                .iter()
                .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Brief, id)),
        );
        selected.extend(
            self.coverage
                .execution
                .required_body_keys(&request.mandatory_roots),
        );
        for wanted in &request.windows {
            let prepared = self
                .coverage
                .windows
                .iter()
                .find(|window| window.selector == wanted.selector)
                .filter(|window| wanted.limit <= window.limit)
                .ok_or(WindowNotPrepared)?;
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
        let strict_roots = mandatory_roots
            .iter()
            .cloned()
            .chain(
                self.coverage
                    .execution
                    .required_body_keys(&request.mandatory_roots),
            )
            .chain(
                windows
                    .iter()
                    .flat_map(|window| window.keys.iter().cloned()),
            )
            .collect();
        selected = self.completion_closure(
            selected,
            strict_roots,
            &request.selected_completion_brief_ids,
            request.record_limit,
        )?;
        for key in &selected {
            if !self.records.contains_key(key) && !self.coverage.unavailable.contains(key) {
                return Err(InputEvidenceReadError::NeedsPreparation(key.clone()).into());
            }
        }
        let turns = self
            .coverage
            .turn_transcript_keys
            .iter()
            .filter(|(id, _)| {
                selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Turn, *id))
            })
            .map(|(id, keys)| (id.clone(), keys.clone()))
            .collect();
        let assignments = self
            .coverage
            .message_turn_keys
            .iter()
            .filter(|(id, _)| {
                selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Message, *id))
            })
            .map(|(id, turn)| (id.clone(), turn.clone()))
            .collect();
        let mut source_ids = selected
            .iter()
            .filter(|key| key.family == InputEvidenceFamily::Message)
            .map(|key| key.id.clone())
            .collect::<BTreeSet<_>>();
        source_ids.extend(request.interrupted_sources.iter().cloned());
        let coverage = InputEvidenceCoverage {
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
                    selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Message, *id))
                })
                .map(|(id, proof)| (id.clone(), proof.clone()))
                .collect(),
            recovery_lineage_messages: self
                .coverage
                .recovery_lineage_messages
                .iter()
                .filter(|id| {
                    selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Message, *id))
                })
                .cloned()
                .collect(),
            replay_source_absences: self
                .coverage
                .replay_source_absences
                .iter()
                .filter(|(id, _)| {
                    selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Turn, *id))
                })
                .map(|(id, proof)| (id.clone(), proof.clone()))
                .collect(),
            execution: self.coverage.execution.for_sources(
                &source_ids,
                &selected_execution_work_ids(
                    &selected
                        .iter()
                        .filter_map(|key| self.records.get(key))
                        .collect::<Vec<_>>(),
                    &selected,
                ),
            ),
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
        let has_query = query.is_some();
        let mut selected_domain = Self::bootstrap_with_query(
            self.scope.clone(),
            selected
                .iter()
                .filter_map(|key| self.records.get(key).cloned())
                .collect(),
            coverage,
            query,
            if has_query {
                self.query_material.clone()
            } else {
                Vec::new()
            },
            has_query.then(|| self.query_acquisition.clone()).flatten(),
        )?;
        selected_domain.budget = self.budget.clone();
        if let Some(material) = material {
            selected_domain.validate_capture_material(material)?;
        }
        let captured = selected_domain.into_capture()?;
        for source in &request.interrupted_sources {
            captured.execution_interrupted_attempts_for_source(source)?;
        }
        Ok(captured)
    }
    /// `mandatory_roots` is the actual committed queue/protocol/root selection,
    /// supplied by the owner installer, never inferred from command arguments.
    /// No Input write or selection change means the original publication and
    /// its allocation guards are still exact, not a new candidate to validate.
    pub(super) fn can_retain_after(
        &self,
        delta: &CommittedInputEvidenceDelta,
        mandatory_roots: &BTreeSet<InputEvidenceKey>,
    ) -> bool {
        self.validated_keys.is_some()
            && delta.is_empty()
            && *mandatory_roots == self.coverage.mandatory_roots
    }

    pub(crate) fn with_committed(
        &self,
        delta: &CommittedInputEvidenceDelta,
        mandatory_roots: BTreeSet<InputEvidenceKey>,
    ) -> Result<Option<Self>> {
        if let Some(advance) = &delta.query_advance {
            advance.validate_delta(&self.scope, &delta.records)?;
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
                            metadata_estimate_bytes: self
                                .metadata_estimate_bytes()?
                                .checked_add(serialized_bytes(&delta.replay_source_absences)?)
                                .context("replay absence candidate metadata overflow")?,
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
        let query_publication = delta
            .query_advance
            .as_ref()
            .map(|advance| advance.install_query(self.query.as_ref(), self.budget.as_ref()))
            .transpose()?
            .flatten();
        let mut next = self.clone();
        // The merged candidate is not the validated publication of `self`.
        next.validated_keys = None;
        next.validated_metadata_bytes = None;
        // Until the original writer supplies its complete query publication,
        // an acknowledged cut must never preserve stale query completeness.
        // Existing captures retain their original cut and guards.
        let mut removed_query_roots = false;
        if query_publication.is_none()
            && (delta.query_advance.is_some()
                || !delta.records.is_empty()
                || !delta.removals.is_empty()
                || delta.output_recall_invalidation.is_some())
        {
            removed_query_roots = next.query.take().is_some();
            next.query_material.clear();
            next.query_acquisition = None;
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
            let removed_recall_roots = !next.output_recall_roots.is_empty();
            next.output_recall_roots.clear();
            next.output_recall_task = None;
            // Only a changed selection needs an intermediate trim. Every
            // merged cut still receives full validation below.
            if removed_query_roots || removed_recall_roots {
                next.trim_and_validate()?;
                next.validated_keys = None;
                next.validated_metadata_bytes = None;
            }
        }
        let changed = delta
            .records
            .iter()
            .map(|record| &record.locator.key)
            .chain(delta.removals.iter().map(|removal| &removal.key))
            .collect::<BTreeSet<_>>();
        next.coverage
            .replay_source_absences
            .retain(|_, proof| !changed.iter().any(|key| proof.observes_key(key)));
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
            next.window_projection = None;
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
                    ensure!(
                        old.as_ref() == record.as_ref(),
                        "same revision has different evidence"
                    );
                    continue;
                }
                ensure!(
                    old.locator.revision < record.locator.revision,
                    "stale committed evidence"
                );
            }
            next.coverage.unavailable.remove(&record.locator.key);
            next.window_projection = None;
            next.records
                .insert(record.locator.key.clone(), record.clone());
        }
        for (message_id, turn) in &delta.message_turn_keys {
            next.coverage
                .message_turn_keys
                .insert(message_id.clone(), turn.clone());
        }
        for (id, proof) in &delta.replay_source_absences {
            ensure!(
                delta
                    .records
                    .iter()
                    .any(|record| { record.locator.key.id == *id && proof.matches(record) }),
                "committed replay absence has no exact final Turn"
            );
            next.coverage
                .replay_source_absences
                .insert(id.clone(), proof.clone());
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
        next.refresh_window_projection();
        // Keep the old accepted query roots through intermediate recall trim.
        // The new frontier becomes selectable only after every D3 body merges.
        if let Some((query, guards, acquisition)) = query_publication {
            next.query = Some(query);
            next.query_material = guards;
            next.query_acquisition = Some(acquisition);
        }
        if let Err(error) = next.trim_and_validate() {
            #[cfg(test)]
            eprintln!(
                "input commit requires preparation: agent={} validation={error:#}",
                self.scope.agent_id
            );
            #[cfg(not(test))]
            let _ = error;
            return Ok(None);
        }
        next.refresh_budget_views()?;
        if let Some(budget) = &next.budget {
            budget.check_history_budget()?;
        }
        Ok(Some(next))
    }

    fn refresh_window_projection(&mut self) {
        if self.window_projection.as_deref() == Some(&self.coverage.windows) {
            return;
        }
        self.window_projection = None;
        self.validated_metadata_bytes = None;
        for window in &mut self.coverage.windows {
            let mut candidates = self
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
        self.window_projection = Some(Arc::new(self.coverage.windows.clone()));
    }

    fn references(&self, record: &QualifiedInputEvidence) -> Result<BTreeSet<InputEvidenceKey>> {
        let mut refs = replay_references(record, &self.coverage.replay_source_absences)?;
        if let PreparedInputEvidence::Message(message) = &record.record {
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
                    refs.extend(self.coverage.execution.required_body_keys(&lineage_roots));
                    if let Some(source_record) = self.records.get(&source_key) {
                        if let PreparedInputEvidence::Message(source_message) =
                            &source_record.record
                        {
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
                refs.insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, turn_id));
            }
        }
        if let PreparedInputEvidence::Turn(turn) = &record.record {
            if let Some(keys) = self.coverage.turn_transcript_keys.get(&turn.turn_id) {
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
        use super::completion_sources::{completion_references, CompletionInputPath};
        let mut selected = roots.clone();
        let initial = roots.into_iter().map(|key| {
            let path = if key.family == InputEvidenceFamily::Brief && heads.contains(&key.id) {
                CompletionInputPath::Head(key.id.clone())
            } else {
                CompletionInputPath::Strict
            };
            (key, path)
        });
        let mut pending = VecDeque::new();
        let mut scheduled = HashMap::new();
        for candidate in initial.chain(
            strict_roots
                .into_iter()
                .map(|key| (key, CompletionInputPath::Strict)),
        ) {
            scheduled
                .entry(candidate)
                .or_insert_with_key(|candidate| pending.push_back(candidate.clone()));
        }
        let mut expanded = 0usize;
        while let Some((key, path)) = pending.pop_front() {
            selected.insert(key.clone());
            ensure!(
                selected.len() <= record_limit,
                "input evidence closure record limit exceeded"
            );
            expanded += 1;
            // There are at most the bounded strict graph plus two selected
            // completion edges per head, never a path cross-product.
            ensure!(
                expanded <= record_limit.saturating_mul(3),
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
            for candidate in completion_references(
                record,
                &path,
                head,
                &self.coverage.completion_source_observations,
                self.references(record)?,
            )? {
                // Membership is unordered; only the FIFO determines visit and
                // error order. Keep strict and optional paths independent.
                scheduled
                    .entry(candidate)
                    .or_insert_with_key(|candidate| pending.push_back(candidate.clone()));
            }
        }
        Ok(selected)
    }
    fn required_keys(&self) -> Result<BTreeSet<InputEvidenceKey>> {
        let mut roots = self.coverage.mandatory_roots.clone();
        if let Some(query) = &self.query {
            query.validate()?;
            roots.extend(query.observation_keys().cloned());
        }
        roots.extend(self.output_recall_roots.iter().cloned());
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
                    .required_body_keys(&self.coverage.mandatory_roots),
            )
            .chain(
                self.coverage
                    .windows
                    .iter()
                    .flat_map(|window| window.keys.iter().cloned()),
            )
            .collect::<BTreeSet<_>>();
        roots.extend(strict.iter().cloned());
        self.completion_closure(
            roots,
            strict,
            &self.coverage.selected_completion_brief_ids,
            self.coverage.record_limit,
        )
    }
    fn validated_required_keys(&self) -> Result<Arc<BTreeSet<InputEvidenceKey>>> {
        if let Some(validated) = &self.validated_keys {
            return Ok(Arc::clone(validated));
        }
        let selected = self.required_keys()?;
        let mut mandatory = self.coverage.mandatory_roots.clone();
        mandatory.extend(
            self.coverage
                .execution
                .required_body_keys(&self.coverage.mandatory_roots),
        );
        let strict_roots = mandatory.clone();
        mandatory.extend(
            self.coverage
                .selected_completion_brief_ids
                .iter()
                .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Brief, id)),
        );
        let mandatory = self.completion_closure(
            mandatory,
            strict_roots,
            &self.coverage.selected_completion_brief_ids,
            self.coverage.record_limit,
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
            let head = self.records.get(&proof.head.key);
            let source = self.records.get(&proof.source.key);
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
        for (id, proof) in &self.coverage.replay_source_absences {
            let key = InputEvidenceKey::new(InputEvidenceFamily::Turn, id);
            let source = InputEvidenceKey::new(InputEvidenceFamily::Turn, &proof.source_turn_id);
            ensure!(
                self.records
                    .get(&key)
                    .is_some_and(|record| proof.matches(record)),
                "replay source absence has no matching qualified Turn version"
            );
            ensure!(
                !self.records.contains_key(&source) && !self.coverage.unavailable.contains(&source),
                "replay source absence overlaps prepared or pruned evidence"
            );
        }
        ensure!(
            selected
                .len()
                .saturating_add(self.coverage.recovery_source_absences.len())
                .saturating_add(self.coverage.replay_source_absences.len())
                .saturating_add(self.coverage.completion_source_observations.len())
                .saturating_add(self.coverage.selected_completion_brief_ids.len())
                .saturating_add(self.coverage.recovery_lineage_messages.len())
                .saturating_add(self.coverage.execution.metadata_count())
                <= self.coverage.record_limit,
            "input evidence and execution metadata record limit exceeded"
        );
        let mut bytes = serde_json::to_vec(&self.coverage.turn_transcript_keys)?
            .len()
            .saturating_add(serde_json::to_vec(&self.coverage.message_turn_keys)?.len())
            .saturating_add(serde_json::to_vec(&self.coverage.recovery_source_absences)?.len())
            .saturating_add(serde_json::to_vec(&self.coverage.replay_source_absences)?.len())
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
        Ok(Arc::new(selected))
    }
    fn trim_and_validate(&mut self) -> Result<()> {
        // Mutable candidates must earn a fresh proof, even after a prior trim.
        self.validated_keys = None;
        self.validated_metadata_bytes = None;
        let selected = self.validated_required_keys()?;
        let mut removed = false;
        self.records.retain(|key, _| {
            let keep = selected.contains(key);
            removed |= !keep;
            keep
        });
        if removed {
            self.window_projection = None;
        }
        self.coverage
            .selected_completion_brief_ids
            .retain(|id| selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Brief, id)));
        self.coverage
            .completion_source_observations
            .retain(|proof| {
                self.coverage
                    .selected_completion_brief_ids
                    .contains(&proof.head.key.id)
            });
        self.coverage.recovery_lineage_messages.retain(|id| {
            selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Message, id))
        });
        self.coverage.recovery_source_absences.retain(|id, _| {
            selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Message, id))
        });
        self.coverage.replay_source_absences.retain(|id, _| {
            selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Turn, id))
        });
        let source_ids = self
            .records
            .values()
            .filter_map(|evidence| match &evidence.record {
                PreparedInputEvidence::Message(message) => Some(message.id.clone()),
                PreparedInputEvidence::Attempt(attempt) => attempt.source_message_id.clone(),
                _ => None,
            })
            .collect();
        self.coverage.execution = self.coverage.execution.for_sources(
            &source_ids,
            &selected_execution_work_ids(&self.records.values().collect::<Vec<_>>(), &selected),
        );
        self.coverage.turn_transcript_keys.retain(|turn_id, _| {
            selected.contains(&InputEvidenceKey::new(InputEvidenceFamily::Turn, turn_id))
        });
        self.coverage.message_turn_keys.retain(|message_id, _| {
            selected.contains(&InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                message_id,
            ))
        });
        self.coverage
            .unavailable
            .retain(|key| selected.contains(key));
        self.validated_keys = Some(selected);
        self.validated_metadata_bytes = Some(self.compute_metadata_estimate_bytes()?);
        Ok(())
    }
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

    mod capture_transfer_tests;
    mod required_keys_tests;

    #[test]
    fn captured_query_shares_only_immutable_metadata_until_last_holder() -> Result<()> {
        let mut original = resident(true);
        original.query = Some(Arc::new(InputQueryCoverage {
            required_keys: original.records.keys().cloned().collect(),
            birth_after: None,
            birth_cursor: 0,
            born: BTreeMap::new(),
        }));
        let query = original.query.as_ref().unwrap();
        let weak = Arc::downgrade(query);
        let captured = original.capture()?;
        assert!(Arc::ptr_eq(query, captured.query.as_ref().unwrap()));
        drop(original);
        assert!(weak.upgrade().is_some());
        drop(captured);
        assert!(weak.upgrade().is_none());
        Ok(())
    }

    #[test]
    fn borrowed_lookup_needs_no_material_and_preserves_exact_absence_and_family() -> Result<()> {
        use crate::diagnostics::resource_io::ResourceReadMaterial;
        let mut captured = resident(true).capture()?;
        captured
            .unavailable
            .insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, "gone"));
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 1_000_000, 64)?;
        let original = phase.finish()?;
        {
            let _active = phase.enter();
            let record = captured
                .lookup_borrowed(InputEvidenceFamily::Turn, "a")?
                .unwrap();
            assert!(Arc::ptr_eq(record, &captured.records[0]));
            assert!(captured
                .lookup_borrowed(InputEvidenceFamily::Turn, "gone")?
                .is_none());
        }
        assert_eq!(phase.finish()?, original);
        {
            let _active = phase.enter();
            assert!(crate::diagnostics::resource_io::charge_owned_metadata(0, 1).is_err());
            for id in ["a", "gone", "unknown"] {
                let error = captured
                    .lookup_borrowed(InputEvidenceFamily::Turn, id)
                    .unwrap_err();
                assert_eq!(
                    error
                        .downcast_ref::<crate::runtime_error::RuntimeError>()
                        .unwrap()
                        .descriptor()
                        .code,
                    "context_resource_exhausted"
                );
            }
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        for (family, id) in [
            (InputEvidenceFamily::Message, "a"),
            (InputEvidenceFamily::Message, "gone"),
            (InputEvidenceFamily::Turn, "unknown"),
        ] {
            let error = captured.lookup_borrowed(family, id).unwrap_err();
            assert_eq!(
                error.downcast_ref::<InputEvidenceReadError>(),
                Some(&InputEvidenceReadError::NeedsPreparation(
                    InputEvidenceKey::new(family, id)
                ))
            );
        }
        Ok(())
    }

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
        })
    }
    fn completion_head() -> Arc<QualifiedInputEvidence> {
        let mut brief = crate::types::BriefRecord::new(
            "agent-a",
            crate::types::BriefKind::Result,
            "completion body",
            None,
            None,
        );
        brief.id = "completion".into();
        let mut qualified = turn("completion", 3).as_ref().clone();
        qualified.record = PreparedInputEvidence::Brief(brief);
        qualified.locator.key = qualified.record.key();
        Arc::new(qualified)
    }
    #[test]
    fn turn_snapshot_references_exact_replay_and_activation_dependencies() -> Result<()> {
        let mut record = TurnRecord::new("agent-a", "replay", 2);
        record.replay = Some(crate::types::TurnReplayProvenance {
            source_message_id: "anchor".into(),
            source_turn_id: "previous".into(),
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: None,
        });
        let lineage = BTreeSet::from([
            InputEvidenceKey::new(InputEvidenceFamily::Message, "anchor"),
            InputEvidenceKey::new(InputEvidenceFamily::Turn, "previous"),
        ]);
        assert_eq!(
            PreparedInputEvidence::Turn(record.clone()).references(),
            lineage
        );
        record.idle_input_batch = Some(crate::types::IdleInputBatchRecord {
            anchor_message_id: "anchor".into(),
            activation_id: Some("activation-replay".into()),
            sender_agent_id: "operator".into(),
            cutoff: "2026-10-26T00:00:00Z".parse()?,
            max_items: 1,
            max_bytes: 1,
            rendered_bytes: 0,
            items: Vec::new(),
        });
        let activation = InputEvidenceKey::new(InputEvidenceFamily::Attempt, "activation-replay");
        let mut expected = lineage.clone();
        expected.insert(activation.clone());
        assert_eq!(
            PreparedInputEvidence::Turn(record.clone()).references(),
            expected
        );
        record.replay = None;
        assert_eq!(
            PreparedInputEvidence::Turn(record.clone()).references(),
            BTreeSet::from([activation])
        );
        record.idle_input_batch.as_mut().unwrap().activation_id = None;
        assert!(PreparedInputEvidence::Turn(record).references().is_empty());
        Ok(())
    }
    fn resident(exhaustive: bool) -> ResidentInputEvidence {
        let a = turn("a", 1);
        let b = turn("b", 2);
        ResidentInputEvidence::bootstrap(
            scope(),
            vec![a.clone(), b.clone()],
            InputEvidenceCoverage {
                selected_completion_brief_ids: Default::default(),
                completion_source_observations: Default::default(),
                recovery_source_absences: Default::default(),
                replay_source_absences: Default::default(),
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
    fn window_projection_starts_invalid_and_tracks_selector_limit_and_output_changes() -> Result<()>
    {
        let original = resident(true);
        assert!(original.window_projection.is_none());
        assert!(original.clone().window_projection.is_none());
        let mut warm = original
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .context("warm window projection")?;
        let cached = warm.window_projection.clone().unwrap();
        assert_eq!(*cached, original.coverage.windows);
        let unchanged = warm.clone();
        assert!(Arc::ptr_eq(
            &cached,
            unchanged.window_projection.as_ref().unwrap()
        ));
        let cached_bytes = warm.compute_metadata_estimate_bytes()?;
        let mut uncached = warm.clone();
        uncached.window_projection = None;
        assert_eq!(
            cached_bytes - uncached.compute_metadata_estimate_bytes()?,
            serialized_bytes(cached.as_ref())?
        );

        warm.coverage.windows[0].keys.reverse();
        warm.refresh_window_projection();
        assert_eq!(warm.coverage.windows, *cached);
        assert!(!Arc::ptr_eq(
            &cached,
            warm.window_projection.as_ref().unwrap()
        ));
        let cached = warm.window_projection.clone().unwrap();
        warm.coverage.windows[0].limit = 1;
        warm.refresh_window_projection();
        assert!(!Arc::ptr_eq(
            &cached,
            warm.window_projection.as_ref().unwrap()
        ));
        assert_eq!(
            warm.coverage.windows[0].keys,
            vec![turn("b", 2).locator.key.clone()]
        );
        assert!(!warm.coverage.windows[0].is_exhaustive);
        let cached = warm.window_projection.clone().unwrap();
        warm.coverage.windows[0].selector = InputWindowSelector::RecentTools;
        warm.refresh_window_projection();
        assert!(!Arc::ptr_eq(
            &cached,
            warm.window_projection.as_ref().unwrap()
        ));
        assert!(warm.coverage.windows[0].keys.is_empty());
        assert_eq!(unchanged.coverage.windows, original.coverage.windows);
        Ok(())
    }

    #[test]
    fn window_projection_invalidates_same_key_order_insert_remove_and_actual_trim() -> Result<()> {
        let warm = resident(true)
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .context("warm window projection")?;
        let cached = warm.window_projection.clone().unwrap();
        let mut replacement = turn("a", 3).as_ref().clone();
        replacement.locator.revision = 2;
        replacement.locator.content_digest = "a-reordered".into();
        let reordered = warm
            .with_committed(
                &CommittedInputEvidenceDelta {
                    records: vec![Arc::new(replacement)],
                    turn_transcript_keys: BTreeMap::from([("a".into(), BTreeSet::new())]),
                    ..Default::default()
                },
                BTreeSet::new(),
            )?
            .context("same-key replacement")?;
        assert!(!Arc::ptr_eq(
            &cached,
            reordered.window_projection.as_ref().unwrap()
        ));
        assert_eq!(
            reordered.coverage.windows[0].keys,
            vec![
                turn("a", 3).locator.key.clone(),
                turn("b", 2).locator.key.clone()
            ]
        );
        let cached = reordered.window_projection.clone().unwrap();
        let removed = reordered
            .with_committed(
                &CommittedInputEvidenceDelta {
                    removals: vec![CommittedEvidenceRemoval {
                        scope: scope(),
                        key: turn("b", 2).locator.key.clone(),
                        revision: 2,
                        previous_content_digest: Some("digest-b".into()),
                    }],
                    ..Default::default()
                },
                BTreeSet::new(),
            )?
            .context("exhaustive window removal")?;
        assert!(!Arc::ptr_eq(
            &cached,
            removed.window_projection.as_ref().unwrap()
        ));
        assert_eq!(
            removed.coverage.windows[0].keys,
            vec![turn("a", 3).locator.key.clone()]
        );

        let grown = warm
            .with_committed(
                &CommittedInputEvidenceDelta {
                    records: vec![turn("c", 3)],
                    turn_transcript_keys: BTreeMap::from([("c".into(), BTreeSet::new())]),
                    ..Default::default()
                },
                BTreeSet::new(),
            )?
            .context("window insertion")?;
        assert_eq!(
            grown.coverage.windows[0].keys,
            vec![
                turn("c", 3).locator.key.clone(),
                turn("b", 2).locator.key.clone()
            ]
        );
        assert!(!grown.records.contains_key(&turn("a", 1).locator.key));
        assert!(
            grown.window_projection.is_none(),
            "actual trim invalidates the new projection"
        );
        let rebuilt = grown
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .context("rebuild after trim")?;
        let cached = rebuilt.window_projection.clone().unwrap();
        let hit = rebuilt
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .context("unchanged dependencies")?;
        assert!(Arc::ptr_eq(
            &cached,
            hit.window_projection.as_ref().unwrap()
        ));
        assert_eq!(hit.coverage.windows, grown.coverage.windows);
        Ok(())
    }

    #[test]
    fn window_projection_recall_insert_and_actual_trim_invalidate_without_replacement() -> Result<()>
    {
        let mut warm = resident(true)
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .context("warm window projection")?;
        let cached = warm.window_projection.clone().unwrap();
        let a = warm.records[&InputEvidenceKey::new(InputEvidenceFamily::Turn, "a")].clone();
        warm.merge_output_recall_records(BTreeMap::from([(a.locator.key.clone(), a)]))?;
        assert!(Arc::ptr_eq(
            &cached,
            warm.window_projection.as_ref().unwrap()
        ));
        let changed = turn("a", 4);
        assert!(warm
            .merge_output_recall_records(BTreeMap::from([(changed.locator.key.clone(), changed)]))
            .unwrap_err()
            .to_string()
            .contains("cannot replace another published Input version"));
        assert!(Arc::ptr_eq(
            &cached,
            warm.window_projection.as_ref().unwrap()
        ));

        let recalled = turn("recalled", 0);
        let key = recalled.locator.key.clone();
        warm.merge_output_recall_records(BTreeMap::from([(key.clone(), recalled)]))?;
        assert!(warm.window_projection.is_none());
        warm.coverage
            .turn_transcript_keys
            .insert("recalled".into(), BTreeSet::new());
        warm.output_recall_roots.insert(key.clone());
        warm.refresh_window_projection();
        let cached = warm.window_projection.clone().unwrap();
        warm.trim_and_validate()?;
        assert!(warm.records.contains_key(&key));
        assert!(Arc::ptr_eq(
            &cached,
            warm.window_projection.as_ref().unwrap()
        ));
        warm.output_recall_roots.clear();
        warm.trim_and_validate()?;
        assert!(!warm.records.contains_key(&key));
        assert!(warm.window_projection.is_none());
        assert_eq!(warm.coverage.windows, *cached);
        Ok(())
    }

    #[test]
    fn window_projection_hit_does_not_skip_closure_policy_or_truncated_removal_rejection(
    ) -> Result<()> {
        let warm = resident(true)
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .context("warm window projection")?;
        let mut corrupt = warm.clone();
        corrupt.coverage.turn_transcript_keys.remove("a");
        assert!(corrupt
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .is_none());
        let mut policy = warm;
        policy.coverage.windows[0].limit = LEGACY_INPUT_WINDOW + 2;
        assert!(policy
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .is_none());

        let partial = resident(false)
            .with_committed(&CommittedInputEvidenceDelta::default(), BTreeSet::new())?
            .context("warm partial projection")?;
        assert!(partial.window_projection.is_some());
        assert!(partial
            .with_committed(
                &CommittedInputEvidenceDelta {
                    removals: vec![CommittedEvidenceRemoval {
                        scope: scope(),
                        key: turn("a", 1).locator.key.clone(),
                        revision: 2,
                        previous_content_digest: Some("digest-a".into()),
                    }],
                    ..Default::default()
                },
                BTreeSet::new(),
            )?
            .is_none());
        Ok(())
    }

    #[test]
    fn window_projection_real_zero_body_query_advances_hit_without_bypassing_original_fences(
    ) -> Result<()> {
        use crate::runtime_db::{
            input_evidence_snapshot::prepare_complete_query_for_fixture,
            prepared_input_evidence::PreparedInputEvidence as PreparedDbEvidence,
            task_snapshot::TaskDomainLoadRequest,
            transitions::{
                AgentPostureTransitionCommand, AgentStateMutation, InputMutationExecution,
                PreparedInputTransition,
            },
            work_snapshot::WorkDomainLoadRequest,
            RuntimeDb,
        };
        use crate::types::{
            AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
            AgentVisibility, TranscriptEntryKind,
        };

        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )?;
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            &scope().agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        db.agent_states()
            .upsert_for_fixture(&AgentState::new(&scope().agent_id))?;
        let at: DateTime<Utc> = "2026-10-13T00:00:00.123456789Z".parse()?;
        for (id, index) in [("a", 1), ("b", 2)] {
            let prepared = PreparedDbEvidence::turn(
                scope(),
                format!("window-seed-{id}"),
                at,
                TurnRecord::new(&scope().agent_id, id, index),
            )?;
            assert!(matches!(
                db.evidence().commit_prepared_input_evidence(&prepared)?,
                InputMutationExecution::Committed { .. }
            ));
        }
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let (mut owner, request) = prepare_complete_query_for_fixture(&db, &scope(), &ledger)?;
        owner.coverage.windows = resident(true).coverage.windows;
        owner.trim_and_validate()?;
        owner.refresh_budget_views()?;
        assert!(owner.window_projection.is_none());
        let windows = owner.coverage.windows.clone();
        let query = owner.query_coverage()?.clone();
        let work_request = WorkDomainLoadRequest {
            capture: super::super::resident_work::WorkCaptureRequest::current(),
            record_limit: 65_536,
            byte_limit: 64 * 1024 * 1024,
        };
        let task_request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: super::super::resident_task::TaskCaptureRequest::current(),
            record_limit: work_request.record_limit,
            byte_limit: work_request.byte_limit,
        };
        let commit = |owner: &ResidentInputEvidence, nonce: &str| -> Result<_> {
            let state = db
                .agent_states()
                .latest(&scope().agent_id)?
                .context("State")?;
            let prepared = PreparedInputTransition::posture(
                scope(),
                nonce.into(),
                at,
                AgentPostureTransitionCommand {
                    agent_id: scope().agent_id,
                    expected_incarnation: Some(1),
                    agent_state: AgentStateMutation {
                        expected: Some(Box::new(state.clone())),
                        record: Box::new(state),
                    },
                    audit_events: Vec::new(),
                    fault: None,
                },
                None,
            )?;
            let source = owner
                .original_query_source()?
                .context("original query source")?;
            let InputMutationExecution::Committed { value, receipt } = db
                .transitions_in_scope(&scope())
                .commit_prepared_transition_with_input_query(
                    &prepared,
                    &work_request,
                    &ledger,
                    &task_request,
                    Some(&source),
                    None,
                )?
            else {
                anyhow::bail!("new no-op nonce must commit its original query advance");
            };
            assert!(!value.applied);
            assert!(!receipt.outcome.applied);
            let delta = value.effects.input_evidence_delta;
            assert!(delta.records.is_empty(), "actual writer has zero body ACKs");
            assert!(delta.query_advance.is_some());
            assert!(!delta.is_empty(), "a query advance is not an empty delta");
            Ok(delta)
        };
        for index in 0..3 {
            let delta = commit(&owner, &format!("window-zero-body-{index}"))?;
            let next = owner
                .with_committed(&delta, BTreeSet::new())?
                .context("actual zero-body query publication")?;
            if let Some(cached) = &owner.window_projection {
                assert!(Arc::ptr_eq(
                    cached,
                    next.window_projection.as_ref().unwrap()
                ));
            } else {
                assert!(next.window_projection.is_some());
            }
            assert_eq!(next.coverage.windows, windows);
            assert_eq!(next.capture()?.windows, windows);
            assert_eq!(next.query_coverage()?, &query);
            for (key, record) in &owner.records {
                assert!(Arc::ptr_eq(record, &next.records[key]));
            }
            assert!(next.capture_for_request(&request)?.windows.is_empty());
            owner = next;
        }
        let delta = commit(&owner, "window-original-fences")?;
        let mut other_scope = owner.clone();
        other_scope.scope.identity_incarnation += 1;
        assert!(other_scope
            .with_committed(&delta, BTreeSet::new())
            .unwrap_err()
            .to_string()
            .contains("another scope"));
        let mut equal_base = owner.clone();
        equal_base.query = owner.query.as_deref().cloned().map(Arc::new);
        assert!(equal_base
            .with_committed(&delta, BTreeSet::new())
            .unwrap_err()
            .to_string()
            .contains("original accepted baseline"));
        let mut missing_ledger = owner.clone();
        missing_ledger.budget = None;
        assert!(missing_ledger
            .with_committed(&delta, BTreeSet::new())
            .unwrap_err()
            .to_string()
            .contains("no original ledger"));
        let mut foreign_ledger = owner.clone();
        foreign_ledger.budget = Some(InputEvidenceBudgetLedger::new(Default::default())?);
        assert!(foreign_ledger
            .with_committed(&delta, BTreeSet::new())
            .is_err());

        let source = owner
            .original_query_source()?
            .context("body writer source")?;
        let transcript = TranscriptEntry::new(
            &scope().agent_id,
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::json!({"body":"must remain qualified"}),
        );
        let prepared =
            PreparedDbEvidence::transcript(scope(), "window-missing-body".into(), at, transcript)?;
        let InputMutationExecution::Committed { value, .. } = db
            .evidence()
            .commit_prepared_input_evidence_with_task_budget(
                &prepared,
                &work_request,
                &task_request,
                &ledger,
                Some(&source),
                None,
            )?
        else {
            anyhow::bail!("body writer must commit");
        };
        let mut missing_body = value.input_delta;
        assert!(!missing_body.records.is_empty());
        missing_body.records.clear();
        assert!(owner
            .with_committed(&missing_body, BTreeSet::new())
            .unwrap_err()
            .to_string()
            .contains("exact actual D3 record"));

        let installed = owner
            .with_committed(&delta, BTreeSet::new())?
            .context("valid original publication still installs")?;
        assert!(Arc::ptr_eq(
            owner.window_projection.as_ref().unwrap(),
            installed.window_projection.as_ref().unwrap()
        ));
        let acquisition = installed.query_acquisition.as_ref().unwrap();
        assert!(acquisition
            .read(|| {
                crate::diagnostics::resource_io::charge_owned_metadata(u64::MAX, usize::MAX)?;
                Ok(())
            })
            .is_err());
        assert!(
            acquisition.finish().is_err(),
            "original failure latch remains set"
        );
        assert!(owner.with_committed(&delta, BTreeSet::new()).is_err());
        Ok(())
    }

    #[test]
    fn validated_selection_preserves_capture_trim_and_corruption_rejection() -> Result<()> {
        let mut original = resident(true);
        let selected = original.required_keys()?;
        assert_eq!(*original.validated_required_keys()?, selected);
        let empty = CommittedInputEvidenceDelta::default();
        assert!(original.can_retain_after(&empty, original.mandatory_roots()));
        let changed_roots = BTreeSet::from([turn("new-root", 3).locator.key.clone()]);
        assert!(!original.can_retain_after(&empty, &changed_roots));
        for delta in [
            CommittedInputEvidenceDelta {
                records: vec![turn("new-record", 3)],
                ..Default::default()
            },
            CommittedInputEvidenceDelta {
                turn_transcript_keys: BTreeMap::from([("a".into(), BTreeSet::new())]),
                ..Default::default()
            },
            CommittedInputEvidenceDelta {
                message_turn_keys: BTreeMap::from([("message".into(), None)]),
                ..Default::default()
            },
            CommittedInputEvidenceDelta {
                execution: ExecutionInputDelta {
                    initialized: Some(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            CommittedInputEvidenceDelta {
                execution: ExecutionInputDelta {
                    open_attempts: BTreeMap::from([("attempt".into(), false)]),
                    ..Default::default()
                },
                ..Default::default()
            },
        ] {
            assert!(!original.can_retain_after(&delta, original.mandatory_roots()));
        }
        let mut uncached = original.clone();
        uncached.validated_keys = None;
        assert!(!uncached.can_retain_after(&empty, uncached.mandatory_roots()));
        assert_eq!(*uncached.validated_required_keys()?, selected);
        assert!(original.metadata_estimate_bytes()? > uncached.metadata_estimate_bytes()?);
        let extra = turn("unselected", 3);
        let extra_key = extra.locator.key.clone();
        original.records.insert(extra_key.clone(), extra);
        original.trim_and_validate()?;
        assert!(!original.records.contains_key(&extra_key));
        assert_eq!(*original.validated_required_keys()?, selected);
        let captured = original.capture()?;
        for id in ["a", "b"] {
            assert!(captured
                .lookup_borrowed(InputEvidenceFamily::Turn, id)?
                .is_some());
        }
        assert!(captured
            .lookup_borrowed(InputEvidenceFamily::Turn, "unselected")
            .is_err());
        // Deliberately corrupt an unpublished candidate, not a cached view.
        original.validated_keys = None;
        original.coverage.turn_transcript_keys.remove("a");
        assert!(original.validated_required_keys().is_err());
        assert!(original.capture().is_err());
        assert!(original
            .nonterminal_execution_roots_after(&CommittedInputEvidenceDelta::default())
            .is_err());
        Ok(())
    }
    #[test]
    fn validated_publication_shares_keys_and_exact_metadata_until_candidate_changes() -> Result<()>
    {
        let original = resident(true);
        let selected = original.validated_required_keys()?;
        assert!(Arc::ptr_eq(&selected, &original.validated_required_keys()?));
        let bytes = original.metadata_estimate_bytes()?;
        assert_eq!(bytes, original.compute_metadata_estimate_bytes()?);
        assert_eq!(original.validated_metadata_bytes, Some(bytes));

        let next = original
            .with_committed(
                &CommittedInputEvidenceDelta::default(),
                BTreeSet::from([InputEvidenceKey::new(InputEvidenceFamily::Turn, "a")]),
            )?
            .context("changed mandatory roots must publish a fresh validated candidate")?;
        assert_eq!(*next.validated_required_keys()?, *selected);
        assert!(!Arc::ptr_eq(&selected, &next.validated_required_keys()?));
        assert_eq!(
            next.metadata_estimate_bytes()?,
            next.compute_metadata_estimate_bytes()?
        );
        assert!(next.metadata_estimate_bytes()? > bytes);
        assert_eq!(original.metadata_estimate_bytes()?, bytes);
        assert!(Arc::ptr_eq(&selected, &original.validated_required_keys()?));

        let mut invalid = next.clone();
        invalid.coverage.turn_transcript_keys.remove("a");
        assert!(invalid.trim_and_validate().is_err());
        assert!(invalid.validated_keys.is_none());
        assert!(invalid.validated_metadata_bytes.is_none());
        Ok(())
    }
    #[test]
    fn nonterminal_roots_apply_last_record_and_family_scoped_removals() -> Result<()> {
        use crate::domain::execution_protocol::{
            AdmittedFences, ExecutionBinding, ExecutionOrigin, ExecutionPriority,
            ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
            WorkItemExecutionRecord, WorkItemExecutionState,
        };

        fn evidence(
            id: &str,
            family: InputEvidenceFamily,
            terminal: bool,
        ) -> Arc<QualifiedInputEvidence> {
            let mut qualified = turn(id, 1).as_ref().clone();
            qualified.record = match family {
                InputEvidenceFamily::Attempt => PreparedInputEvidence::Attempt(ExecutionAttempt {
                    input_projection: None,
                    attempt_id: id.into(),
                    agent_id: "agent-a".into(),
                    source_message_id: None,
                    source: ExecutionSource {
                        identity: ExecutionSourceIdentity::RuntimeRecovery {
                            recovery_id: id.into(),
                        },
                        generation: 1,
                    },
                    binding: ExecutionBinding::AgentLifecycle {
                        agent_id: "agent-a".into(),
                    },
                    provenance: ExecutionProvenance {
                        origin: ExecutionOrigin::RuntimeRecovery,
                        trust: ExecutionTrust::RuntimeInstruction,
                        priority: ExecutionPriority::Normal,
                        correlation_id: None,
                        causation_id: None,
                    },
                    admitted_fences: AdmittedFences {
                        source_revision: 1,
                        work_item_source_revision: None,
                        work_item_generation: None,
                        rejoin: None,
                        agent_control_revision: 1,
                        host_registry_revision: 1,
                    },
                    state: if terminal {
                        ExecutionAttemptState::Settled
                    } else {
                        ExecutionAttemptState::Interrupted
                    },
                    run_id: None,
                    turn_id: None,
                    recovery_of_attempt_id: None,
                    terminal_outcome_id: None,
                    admitted_at: "2026-10-12T00:00:00Z".into(),
                    terminal_at: Some("2026-10-12T00:00:01Z".into()),
                }),
                InputEvidenceFamily::WorkExecution => PreparedInputEvidence::WorkExecution {
                    work_item_id: id.into(),
                    record: WorkItemExecutionRecord {
                        source_revision: 1,
                        state: if terminal {
                            WorkItemExecutionState::Terminal {
                                generation: 1,
                                completion: "done".into(),
                            }
                        } else {
                            WorkItemExecutionState::Runnable {
                                generation: 1,
                                recovery_ref: None,
                            }
                        },
                    },
                },
                _ => unreachable!(),
            };
            qualified.locator.key = qualified.record.key();
            qualified.attempt_turn_keys = Some(Default::default());
            Arc::new(qualified)
        }

        for family in [
            InputEvidenceFamily::Attempt,
            InputEvidenceFamily::WorkExecution,
        ] {
            let mut original = resident(true);
            original.validated_keys = None;
            original.coverage.record_limit = 16;
            for id in [
                "live",
                "removed",
                "untouched",
                "terminal",
                "both",
                "unselected",
            ] {
                let record = evidence(id, family, id == "terminal");
                let key = record.locator.key.clone();
                original.records.insert(key.clone(), record);
                if id != "unselected" {
                    original.coverage.mandatory_roots.insert(key);
                }
            }
            let key = |id| InputEvidenceKey::new(family, id);
            assert_eq!(
                original.nonterminal_execution_roots_after(&Default::default())?,
                ["live", "removed", "untouched", "both"]
                    .into_iter()
                    .map(key)
                    .collect()
            );
            let delta = CommittedInputEvidenceDelta {
                records: vec![
                    evidence("live", family, true),
                    evidence("revived", family, true),
                    evidence("revived", family, false),
                    evidence("terminal", family, false),
                    evidence("new-terminal", family, false),
                    evidence("new-terminal", family, true),
                    evidence("both", family, false),
                    // Another family with the same ID cannot replace a root.
                    turn("terminal", 3),
                ],
                removals: [
                    key("removed"),
                    key("both"),
                    InputEvidenceKey::new(InputEvidenceFamily::Turn, "untouched"),
                ]
                .into_iter()
                .map(|key| CommittedEvidenceRemoval {
                    scope: scope(),
                    key,
                    revision: 2,
                    previous_content_digest: None,
                })
                .collect(),
                ..Default::default()
            };
            assert_eq!(
                original.nonterminal_execution_roots_after(&delta)?,
                ["revived", "terminal", "untouched", "both"]
                    .into_iter()
                    .map(key)
                    .collect()
            );
            original.coverage.turn_transcript_keys.remove("a");
            assert!(original.nonterminal_execution_roots_after(&delta).is_err());
        }
        Ok(())
    }
    #[test]
    fn completion_closure_visits_each_key_path_once_on_cyclic_input() -> Result<()> {
        use crate::types::{MessageBody, MessageKind, MessageOrigin, Priority};
        let messages = ["m1", "m2", "m3", "m4", "m5"];
        let mut owning = turn("a", 1).as_ref().clone();
        let PreparedInputEvidence::Turn(record) = &mut owning.record else {
            unreachable!()
        };
        record.input_message_ids = messages.iter().map(|id| (*id).to_string()).collect();
        let owning = Arc::new(owning);
        let mut records = vec![owning.clone()];
        for id in messages {
            let mut envelope = MessageEnvelope::new(
                "agent-a",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("body {id}"),
                },
            );
            envelope.id = id.to_string();
            let mut qualified = turn("a", 1).as_ref().clone();
            qualified.record = PreparedInputEvidence::Message(envelope);
            qualified.locator.key = qualified.record.key();
            qualified.turn_index = None;
            qualified.owner = None;
            records.push(Arc::new(qualified));
        }
        let turn_key = owning.locator.key.clone();
        let mut resident = ResidentInputEvidence::bootstrap(
            scope(),
            records,
            InputEvidenceCoverage {
                selected_completion_brief_ids: Default::default(),
                completion_source_observations: Default::default(),
                recovery_source_absences: Default::default(),
                replay_source_absences: Default::default(),
                recovery_lineage_messages: Default::default(),
                execution: ExecutionInputCoverage::default(),
                windows: Vec::new(),
                mandatory_roots: BTreeSet::from([InputEvidenceKey::new(
                    InputEvidenceFamily::Message,
                    messages[0],
                )]),
                turn_transcript_keys: BTreeMap::from([("a".to_string(), BTreeSet::new())]),
                message_turn_keys: messages
                    .iter()
                    .map(|id| ((*id).to_string(), Some(turn_key.id.clone())))
                    .collect(),
                unavailable: BTreeSet::new(),
                record_limit: messages.len() + 1,
                byte_limit: 4096,
            },
        )?;
        let expected = std::iter::once(turn_key)
            .chain(
                messages
                    .iter()
                    .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Message, *id)),
            )
            .collect::<BTreeSet<_>>();
        assert_eq!(resident.required_keys()?, expected);
        assert_eq!(resident.validated_required_keys()?.as_ref(), &expected);
        // Every strict root is also an initial root. Repeated scheduling must
        // neither change the selected graph nor spend the path limit again.
        resident.coverage.mandatory_roots = expected.clone();
        assert_eq!(resident.required_keys()?, expected);
        assert_eq!(resident.validated_required_keys()?.as_ref(), &expected);
        // The owning Turn is reachable again through each of its input
        // messages. That (key, path) pair adds no key, and revisiting it made
        // the old traversal re-expand the same record for every message.
        let limit = resident.coverage.record_limit;
        resident.coverage.record_limit = expected.len() - 1;
        let error = resident.required_keys().unwrap_err();
        assert!(
            format!("{error:?}").contains("record limit exceeded"),
            "{error:?}"
        );
        resident.coverage.record_limit = limit;
        Ok(())
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
            query: None,
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
        let ledger =
            InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
        let material =
            crate::diagnostics::resource_io::ResourceReadMaterial::new(&ledger, &scope()).unwrap();
        let prepared = original
            .capture_for_request_with_material(&request, &material)
            .unwrap();
        assert_eq!(prepared.records.len(), 1);
        assert!(Arc::ptr_eq(&prepared.records[0], &captured.records[0]));
        assert_eq!(prepared.turn_transcript_keys.len(), 1);
        material.finish().unwrap();
        drop(original);
        assert!(unselected.upgrade().is_none());
        assert_eq!(captured.records[0].locator.key.id, "b");
    }

    #[test]
    fn request_capture_rejects_missing_roots_before_allocating_selectors() -> Result<()> {
        use crate::diagnostics::resource_io::ResourceReadMaterial;
        use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;

        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let original = resident(true).with_budget(ledger.clone())?;
        let header_bytes = (scope().agent_id.len() as u64 + 256) * 8;
        for missing in [
            InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, "a"),
            InputEvidenceKey::new(InputEvidenceFamily::Turn, "missing"),
        ] {
            let request = InputEvidenceLoadRequest {
                query: None,
                windows: Vec::new(),
                mandatory_roots: BTreeSet::from([
                    InputEvidenceKey::new(InputEvidenceFamily::Turn, "a"),
                    missing.clone(),
                ]),
                interrupted_sources: Default::default(),
                selected_completion_brief_ids: Default::default(),
                record_limit: 8,
                byte_limit: 4096,
            };
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), header_bytes, 64)?;
            let baseline = phase.finish()?;
            let error = original
                .capture_for_request_with_material(&request, &phase)
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<InputEvidenceReadError>(),
                Some(InputEvidenceReadError::NeedsPreparation(key)) if key == &missing
            ));
            assert_eq!(phase.finish()?, baseline);
        }
        Ok(())
    }

    #[test]
    fn request_capture_checks_completion_and_window_selectors_before_allocating() -> Result<()> {
        use crate::diagnostics::resource_io::ResourceReadMaterial;
        use crate::runtime_db::input_evidence_snapshot::{
            InputEvidenceLoadRequest, InputWindowRequest,
        };

        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let original = resident(true).with_budget(ledger.clone())?;
        let header_bytes = (scope().agent_id.len() as u64 + 256) * 8;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header_bytes, 64)?;
        let baseline = phase.finish()?;
        let mut request = InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: BTreeSet::from([InputEvidenceKey::new(
                InputEvidenceFamily::Turn,
                "a",
            )]),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: BTreeSet::from(["a".into()]),
            record_limit: 8,
            byte_limit: 4096,
        };
        // An existing same-ID Turn does not prepare the completion-head selector.
        for _ in 0..3 {
            let error = original
                .capture_for_request_with_material(&request, &phase)
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<InputEvidenceReadError>(),
                Some(InputEvidenceReadError::NeedsPreparation(key))
                    if key.family == InputEvidenceFamily::Brief && key.id == "a"
            ));
            assert_eq!(phase.finish()?, baseline);
        }
        request.selected_completion_brief_ids.clear();
        for (selector, limit) in [
            (InputWindowSelector::RecentBriefs, 1),
            (InputWindowSelector::GlobalTurns, 3),
        ] {
            request.windows = vec![InputWindowRequest { selector, limit }];
            let error = original
                .capture_for_request_with_material(&request, &phase)
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<InputEvidenceReadError>(),
                Some(InputEvidenceReadError::WindowNotPrepared)
            ));
            assert_eq!(phase.finish()?, baseline);
        }
        request.windows[0].limit = 1;
        request.windows.push(request.windows[0].clone());
        let error = original
            .capture_for_request_with_material(&request, &phase)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("duplicate input request selector"));
        assert_eq!(phase.finish()?, baseline);
        request.windows.pop();

        // Valid capture still pays the original allowance and latches exhaustion.
        let fresh = ResourceReadMaterial::new(&ledger, &scope())?;
        let before = fresh.finish()?;
        let captured = original.capture_for_request_with_material(&request, &fresh)?;
        assert_eq!(captured.records.len(), 2);
        assert_ne!(fresh.finish()?, before);
        assert!(original
            .capture_for_request_with_material(&request, &phase)
            .is_err());
        request
            .selected_completion_brief_ids
            .insert("missing".into());
        let error = original
            .capture_for_request_with_material(&request, &phase)
            .unwrap_err();
        assert!(error
            .downcast_ref::<crate::runtime_error::RuntimeError>()
            .is_some());
        assert!(phase.finish().is_err());
        Ok(())
    }

    #[test]
    fn scheduler_frontier_selects_each_family_without_qualifying_missing_bodies() -> Result<()> {
        use crate::diagnostics::resource_io::ResourceReadMaterial;
        use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;

        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let mut original = resident(true).with_budget(ledger.clone())?;
        // Deliberately incomplete protocol bodies: a same-ID Turn must not
        // satisfy either live frontier, nor grant a queue-only Message root.
        original.coverage.execution.initialized = true;
        original
            .coverage
            .execution
            .open_attempt_ids
            .insert("a".into());
        original
            .coverage
            .execution
            .nonterminal_work_item_ids
            .insert("a".into());
        original
            .coverage
            .mandatory_roots
            .insert(InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                "queued",
            ));
        let mut request = InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit: 8,
            byte_limit: 4096,
        };
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        phase.read(|| request.extend_admitted_roots(|| original.scheduler_execution_roots()))?;
        assert_eq!(
            request.mandatory_roots,
            BTreeSet::from([
                InputEvidenceKey::new(InputEvidenceFamily::Attempt, "a"),
                InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, "a"),
            ]),
        );
        let error = original
            .capture_for_request_with_material(&request, &phase)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<InputEvidenceReadError>(),
            Some(InputEvidenceReadError::NeedsPreparation(key))
                if key.id == "a"
                    && matches!(key.family, InputEvidenceFamily::Attempt | InputEvidenceFamily::WorkExecution)
        ));
        Ok(())
    }

    #[test]
    fn prepared_request_root_borrows_only_declared_covered_addresses() -> Result<()> {
        use crate::diagnostics::resource_io::ResourceReadMaterial;
        use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;
        use crate::types::{MessageBody, MessageKind, MessageOrigin, Priority};

        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let mut original = resident(true);
        let mut message = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "binding source".into(),
            },
        );
        message.id = "a".into();
        let mut qualified = turn("a", 1).as_ref().clone();
        qualified.record = PreparedInputEvidence::Message(message);
        qualified.locator.key = qualified.record.key();
        qualified.turn_index = None;
        qualified.owner = None;
        let message = Arc::new(qualified);
        let turn_key = InputEvidenceKey::new(InputEvidenceFamily::Turn, "a");
        let unavailable = InputEvidenceKey::new(InputEvidenceFamily::Message, "unavailable");
        original
            .records
            .insert(message.locator.key.clone(), message.clone());
        original.coverage.message_turn_keys.insert("a".into(), None);
        original.coverage.mandatory_roots = BTreeSet::from([message.locator.key.clone(), turn_key]);
        original.trim_and_validate()?;
        let original = original.with_budget(ledger.clone())?;
        let mut request = InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: original.coverage.mandatory_roots.clone(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit: 8,
            byte_limit: 4096,
        };
        let header_bytes = (scope().agent_id.len() as u64 + 256) * 8;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header_bytes, 64)?;
        let baseline = phase.finish()?;
        for _ in 0..3 {
            let borrowed = original
                .lookup_prepared_request_root(&request, &phase, InputEvidenceFamily::Message, "a")?
                .unwrap();
            assert!(Arc::ptr_eq(borrowed, &message));
            let borrowed_turn = original
                .lookup_prepared_request_root(&request, &phase, InputEvidenceFamily::Turn, "a")?
                .unwrap();
            assert!(matches!(
                borrowed_turn.record,
                PreparedInputEvidence::Turn(_)
            ));
        }
        for (family, id) in [
            (InputEvidenceFamily::Brief, "a"),
            (InputEvidenceFamily::Turn, "b"),
            (InputEvidenceFamily::Message, "unavailable"),
        ] {
            assert!(original
                .lookup_prepared_request_root(&request, &phase, family, id)?
                .is_none());
        }
        request
            .mandatory_roots
            .insert(InputEvidenceKey::new(InputEvidenceFamily::Brief, "a"));
        assert!(original
            .lookup_prepared_request_root(&request, &phase, InputEvidenceFamily::Message, "a")?
            .is_none());
        request
            .mandatory_roots
            .remove(&InputEvidenceKey::new(InputEvidenceFamily::Brief, "a"));
        request.byte_limit -= 1;
        assert!(original
            .lookup_prepared_request_root(&request, &phase, InputEvidenceFamily::Message, "a")?
            .is_none());
        request.byte_limit += 1;
        request.record_limit -= 1;
        assert!(original
            .lookup_prepared_request_root(&request, &phase, InputEvidenceFamily::Message, "a")?
            .is_none());
        assert_eq!(phase.finish()?, baseline);
        // An inconclusive proof is not a rejection: the original exact path
        // still owns its selected closure.
        let fresh = ResourceReadMaterial::new(&ledger, &scope())?;
        let captured = original.capture_for_request_with_material(&request, &fresh)?;
        assert!(Arc::ptr_eq(
            captured
                .lookup_borrowed(InputEvidenceFamily::Message, "a")?
                .unwrap(),
            &message,
        ));
        request.mandatory_roots.insert(unavailable);
        assert!(original
            .lookup_prepared_request_root(&request, &phase, InputEvidenceFamily::Message, "a")?
            .is_none());
        assert!(original
            .capture_for_request_with_material(&request, &fresh)
            .is_err());
        Ok(())
    }

    #[test]
    fn preparation_coverage_borrows_at_full_limits_and_keeps_both_failure_latches() -> Result<()> {
        use crate::diagnostics::resource_io::ResourceReadMaterial;
        use crate::runtime_db::input_evidence_snapshot::{
            InputEvidenceLoadRequest, InputWindowRequest,
        };
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let mut original = resident(true).with_budget(ledger.clone())?;
        let header_bytes = (scope().agent_id.len() as u64 + 256) * 8;
        let source = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header_bytes, 64)?;
        original.query = Some(Arc::new(InputQueryCoverage {
            required_keys: Default::default(),
            birth_after: None,
            birth_cursor: 0,
            born: Default::default(),
        }));
        original.query_material = vec![source.guard()?];
        original.query_acquisition = Some(source.clone());
        let mut request = InputEvidenceLoadRequest {
            query: Some(InputQueryRead {
                selection: Default::default(),
                ledger: ledger.clone(),
            }),
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: vec![InputWindowRequest {
                selector: InputWindowSelector::GlobalTurns,
                limit: 1,
            }],
            mandatory_roots: Default::default(),
            record_limit: 8,
            byte_limit: 4096,
        };
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header_bytes, 64)?;
        let guard = phase.guard()?;
        let last = Arc::downgrade(&guard);
        drop(guard);
        let baseline = phase.finish()?;
        for _ in 0..3 {
            assert!(original.preparation_request_is_covered(&request, &phase)?);
        }
        assert_eq!(phase.finish()?, baseline);
        assert_eq!(source.finish()?, baseline);
        request.record_limit -= 1;
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        request.record_limit += 1;
        request.byte_limit -= 1;
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        request.byte_limit += 1;
        request
            .mandatory_roots
            .insert(InputEvidenceKey::new(InputEvidenceFamily::Turn, "missing"));
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        request.mandatory_roots.clear();
        request.windows[0].limit = 3;
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        request.windows[0].limit = 1;
        request.windows.push(request.windows[0].clone());
        assert!(original
            .preparation_request_is_covered(&request, &phase)
            .is_err());
        request.windows.pop();
        request
            .selected_completion_brief_ids
            .insert("unknown".into());
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        request.selected_completion_brief_ids.clear();
        request.interrupted_sources.insert("unobserved".into());
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        original.coverage.execution.sources.insert(
            "unobserved".into(),
            ExecutionSourceCoverage {
                source_message_id: "unobserved".into(),
                revision: 1,
                allocation_highwater: 0,
                attempts: BTreeMap::new(),
            },
        );
        for _ in 0..3 {
            assert!(original.preparation_request_is_covered(&request, &phase)?);
        }
        assert_eq!(phase.finish()?, baseline);
        assert_eq!(source.finish()?, baseline);
        let observed = original
            .coverage
            .execution
            .sources
            .get_mut("unobserved")
            .unwrap();
        observed.allocation_highwater = 1;
        observed.attempts.insert(
            "interrupted-body-not-loaded".into(),
            SourceAttemptObservation {
                attempt_id: "interrupted-body-not-loaded".into(),
                allocation_ordinal: 1,
                admitted_at: Utc::now().to_rfc3339(),
                state: ExecutionAttemptState::Interrupted,
                turn_id_hint: None,
                terminal_outcome_id: None,
                pruned: false,
            },
        );
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        request.interrupted_sources.clear();
        request.byte_limit = 0;
        assert!(original
            .preparation_request_is_covered(&request, &phase)
            .is_err());
        request.byte_limit = 4096;
        let foreign_scope = InputEvidenceScope {
            agent_id: "foreign".into(),
            identity_incarnation: 1,
        };
        let foreign_scope_phase = ResourceReadMaterial::new(&ledger, &foreign_scope)?;
        assert!(original
            .preparation_request_is_covered(&request, &foreign_scope_phase)
            .is_err());
        assert!(original
            .lookup_prepared_request_root(
                &request,
                &foreign_scope_phase,
                InputEvidenceFamily::Turn,
                "a",
            )
            .is_err());
        let foreign = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let foreign_phase = ResourceReadMaterial::new(&foreign, &scope())?;
        assert!(original
            .preparation_request_is_covered(&request, &foreign_phase)
            .is_err());
        assert!(original
            .lookup_prepared_request_root(&request, &foreign_phase, InputEvidenceFamily::Turn, "a")
            .is_err());
        request.query.as_mut().unwrap().ledger = foreign;
        assert!(original
            .preparation_request_is_covered(&request, &phase)
            .is_err());
        request.query.as_mut().unwrap().ledger = ledger.clone();
        // Real consumption still allocates and must reject at the same full limit.
        assert!(original
            .capture_for_request_with_material(&request, &phase)
            .is_err());
        assert!(original
            .preparation_request_is_covered(&request, &phase)
            .is_err());
        assert!(original
            .lookup_prepared_request_root(&request, &phase, InputEvidenceFamily::Turn, "a")
            .is_err());
        assert!(last.upgrade().is_some());
        drop(phase);
        assert!(last.upgrade().is_none());
        let fresh = ResourceReadMaterial::new(&ledger, &scope())?;
        assert!(source
            .read(|| {
                crate::diagnostics::resource_io::charge_owned_metadata(0, 1)?;
                Ok(())
            })
            .is_err());
        assert!(original
            .preparation_request_is_covered(&request, &fresh)
            .is_err());
        assert!(original
            .lookup_prepared_request_root(&request, &fresh, InputEvidenceFamily::Turn, "a")
            .is_err());
        Ok(())
    }

    #[test]
    fn preparation_coverage_drops_only_independently_strict_completion_heads() -> Result<()> {
        use crate::diagnostics::resource_io::ResourceReadMaterial;
        use crate::runtime_db::input_evidence_snapshot::{
            InputEvidenceLoadRequest, InputWindowRequest,
        };
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let head = completion_head();
        let mut original = resident(true).with_budget(ledger.clone())?;
        original
            .records
            .insert(head.locator.key.clone(), head.clone());
        original
            .coverage
            .selected_completion_brief_ids
            .insert(head.locator.key.id.clone());
        original.coverage.windows.push(InputWindowCoverage {
            selector: InputWindowSelector::RecentBriefs,
            limit: 1,
            keys: vec![head.locator.key.clone()],
            is_exhaustive: true,
        });
        original.trim_and_validate()?;
        let mut request = InputEvidenceLoadRequest {
            query: None,
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: vec![
                InputWindowRequest {
                    selector: InputWindowSelector::GlobalTurns,
                    limit: 1,
                },
                InputWindowRequest {
                    selector: InputWindowSelector::RecentBriefs,
                    limit: 1,
                },
            ],
            mandatory_roots: Default::default(),
            record_limit: 8,
            byte_limit: 4096,
        };
        // Only the original header fits: the proof must not copy any metadata.
        let header_bytes = (scope().agent_id.len() as u64 + 256) * 8;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header_bytes, 64)?;
        let baseline = phase.finish()?;
        for _ in 0..5 {
            assert!(original.preparation_request_is_covered(&request, &phase)?);
        }
        assert_eq!(phase.finish()?, baseline);
        let captured = original.capture_for_request(&request)?;
        assert!(captured
            .records
            .iter()
            .any(|record| Arc::ptr_eq(record, &head)));
        request
            .selected_completion_brief_ids
            .insert("unknown".into());
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        request.selected_completion_brief_ids.clear();

        // The other independent strict-root family is equally sufficient.
        original.coverage.windows.pop();
        original
            .coverage
            .mandatory_roots
            .insert(head.locator.key.clone());
        request.windows.pop();
        request.mandatory_roots.insert(head.locator.key.clone());
        original.trim_and_validate()?;
        assert!(original.preparation_request_is_covered(&request, &phase)?);

        // An equal ID in another evidence family is not a Brief owning path.
        original.coverage.mandatory_roots.clear();
        request.mandatory_roots.clear();
        let same_id_turn = turn(&head.locator.key.id, 3);
        original
            .records
            .insert(same_id_turn.locator.key.clone(), same_id_turn.clone());
        original.coverage.windows[0]
            .keys
            .insert(0, same_id_turn.locator.key.clone());
        original.coverage.windows[0].limit = 3;
        original
            .coverage
            .turn_transcript_keys
            .insert(head.locator.key.id.clone(), Default::default());
        original.trim_and_validate()?;
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        assert_eq!(phase.finish()?, baseline);
        assert!(phase
            .read(|| {
                crate::diagnostics::resource_io::charge_owned_metadata(0, 1)?;
                Ok(())
            })
            .is_err());
        assert!(original
            .preparation_request_is_covered(&request, &phase)
            .is_err());
        Ok(())
    }

    #[test]
    fn preparation_coverage_cannot_drop_a_completion_only_optional_absence() -> Result<()> {
        use super::super::completion_sources::{
            CompletionOptionalAbsence, CompletionOptionalSourceObservation, CompletionSourceEdge,
        };
        use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let mut head = completion_head().as_ref().clone();
        let missing = InputEvidenceKey::new(InputEvidenceFamily::Transcript, "missing-transcript");
        let PreparedInputEvidence::Brief(brief) = &mut head.record else {
            unreachable!()
        };
        brief.content_source = crate::types::BriefContentSource::TranscriptEntry {
            entry_id: missing.id.clone(),
            relation: crate::types::BriefContentSourceRelation::Finalizes,
        };
        let head = Arc::new(head);
        let mut original = resident(true).with_budget(ledger.clone())?;
        original
            .records
            .insert(head.locator.key.clone(), head.clone());
        original
            .coverage
            .selected_completion_brief_ids
            .insert(head.locator.key.id.clone());
        original.coverage.completion_source_observations.push(
            CompletionOptionalSourceObservation {
                head: head.locator.clone(),
                source: head.locator.clone(),
                edge: CompletionSourceEdge::BriefTranscript,
                declared_target: missing.clone(),
                availability: CompletionOptionalAbsence::GlobalAbsent,
            },
        );
        original.trim_and_validate()?;
        let mut request = InputEvidenceLoadRequest {
            query: None,
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            record_limit: 8,
            byte_limit: 4096,
        };
        let phase = crate::diagnostics::resource_io::ResourceReadMaterial::new(&ledger, &scope())?;
        let baseline = phase.finish()?;
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        request.mandatory_roots.insert(head.locator.key.clone());
        assert!(!original.preparation_request_is_covered(&request, &phase)?);
        let error = original.capture_for_request(&request).unwrap_err();
        assert_eq!(
            error.downcast_ref::<InputEvidenceReadError>(),
            Some(&InputEvidenceReadError::NeedsPreparation(missing))
        );
        assert_eq!(phase.finish()?, baseline);
        Ok(())
    }

    #[test]
    fn request_capture_checks_selected_coverage_and_bounds() {
        let original = resident(true);
        let mut request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            query: None,
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
    fn committed_cut_without_removed_selectors_rejects_missing_descendants() -> Result<()> {
        let original = resident(false);
        assert!(original.validated_keys.is_some());
        assert!(original.query.is_none());
        assert!(original.output_recall_roots.is_empty());
        let mut record = turn("c", 3).as_ref().clone();
        let PreparedInputEvidence::Turn(owning) = &mut record.record else {
            unreachable!()
        };
        owning.input_message_ids.push("missing-message".into());
        let key = record.locator.key.clone();
        let delta = CommittedInputEvidenceDelta {
            records: vec![Arc::new(record)],
            turn_transcript_keys: BTreeMap::from([("c".into(), BTreeSet::new())]),
            ..Default::default()
        };
        assert!(original
            .with_committed(&delta, BTreeSet::from([key]))?
            .is_none());
        assert!(original
            .capture()?
            .lookup_borrowed(InputEvidenceFamily::Turn, "b")?
            .is_some());
        Ok(())
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
                    replay_source_absences: BTreeMap::new(),
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
                    replay_source_absences: BTreeMap::new(),
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
                    replay_source_absences: BTreeMap::new(),
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
                    replay_source_absences: BTreeMap::new(),
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
                    replay_source_absences: BTreeMap::new(),
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
                selected_completion_brief_ids: Default::default(),
                completion_source_observations: Default::default(),
                recovery_source_absences: Default::default(),
                replay_source_absences: Default::default(),
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
                replay_source_absences: BTreeMap::new(),
                turn_transcript_keys: BTreeMap::from([("c".into(), BTreeSet::new())]),
                message_turn_keys: BTreeMap::new(),
            }
        }
        let mut limits = crate::config::StateMemoryLimits::default();
        limits.history_per_agent_bytes = 300;
        let budget = InputEvidenceBudgetLedger::new(limits).unwrap();
        let original = resident(true).with_budget(budget.clone()).unwrap();
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

        let mut limits = crate::config::StateMemoryLimits::default();
        limits.history_per_agent_bytes = 400;
        limits.history_host_bytes = 600;
        let budget = InputEvidenceBudgetLedger::new(limits).unwrap();
        let original = resident(true).with_budget(budget.clone()).unwrap();
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
                    query: None,
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
