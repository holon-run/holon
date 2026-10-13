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
        AuthorityClass, BriefRecord, MessageEnvelope, ToolExecutionRecord, ToolExecutionStatus,
        TranscriptEntry, TurnOwner, TurnRecord,
    },
};
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
                key == &InputEvidenceKey::new(InputEvidenceFamily::Message, &source)
                    || key == &InputEvidenceKey::new(InputEvidenceFamily::Turn, &turn)
            })
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InputEvidenceCoverage {
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
    pub execution: ExecutionInputDelta,
    pub records: Vec<Arc<QualifiedInputEvidence>>,
    pub removals: Vec<CommittedEvidenceRemoval>,
    /// Exact qualified Transcript frontier for each Turn included in this commit.
    pub turn_transcript_keys: BTreeMap<String, BTreeSet<InputEvidenceKey>>,
    /// None is an actual same-view unassigned input, not a missing preparation.
    pub message_turn_keys: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResidentInputEvidence {
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
            scope,
            records: map,
            coverage,
            budget: None,
            budget_views: Vec::new(),
        };
        result.trim_and_validate()?;
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
        Ok(serialized_bytes(&self.coverage)?
            .checked_add(nodes as u64)
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
    pub(crate) fn capture(&self) -> Result<CapturedInputEvidence> {
        self.validate()?;
        let selected = self.required_keys()?;
        let mut captured = CapturedInputEvidence {
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
    /// A request owns only its selected closure. Resident mandatory roots also
    /// contain other accepted inputs and must not be pinned by every prompt.
    pub(crate) fn capture_for_request(
        &self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    ) -> Result<CapturedInputEvidence> {
        use InputEvidenceReadError::WindowNotPrepared;
        ensure!(
            request.record_limit > 0 && request.byte_limit > 0,
            "input request needs positive resource bounds"
        );
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
        for id in &request.selected_completion_brief_ids {
            let key = InputEvidenceKey::new(InputEvidenceFamily::Brief, id);
            if !self.coverage.selected_completion_brief_ids.contains(id) {
                return Err(InputEvidenceReadError::NeedsPreparation(key).into());
            }
        }
        mandatory_roots.extend(
            self.coverage
                .execution
                .required_interrupted_body_keys(&request.interrupted_sources)?,
        );
        let mut selected = mandatory_roots.clone();
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
            ensure!(
                !windows
                    .iter()
                    .any(|window: &InputWindowCoverage| { window.selector == wanted.selector }),
                "duplicate input request selector"
            );
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
        let mut selected_domain = Self::bootstrap(
            self.scope.clone(),
            selected
                .iter()
                .filter_map(|key| self.records.get(key).cloned())
                .collect(),
            coverage,
        )?;
        selected_domain.budget = self.budget.clone();
        let captured = selected_domain.capture()?;
        for source in &request.interrupted_sources {
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
        let mut refs = record.references();
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
            pending.extend(completion_references(
                record,
                &path,
                head,
                &self.coverage.completion_source_observations,
                self.references(record)?,
            )?);
        }
        Ok(selected)
    }
    fn required_keys(&self) -> Result<BTreeSet<InputEvidenceKey>> {
        let mut roots = self.coverage.mandatory_roots.clone();
        roots.extend(
            self.coverage
                .execution
                .required_body_keys(&self.coverage.mandatory_roots),
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
    fn validate(&self) -> Result<()> {
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
        self.validate()?;
        let selected = self.required_keys()?;
        self.records.retain(|key, _| selected.contains(key));
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
                execution: ExecutionInputDelta::default(),
                records: vec![turn("c", 3)],
                removals: Vec::new(),
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
