use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::{
    runtime::resident_input_evidence::InputEvidenceScope,
    tool::names::EXEC_COMMAND,
    types::{ToolExecutionRecord, ToolExecutionStatus},
};

use super::evidence::committed_tool_tx;

pub(super) fn validate_write_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    record: &ToolExecutionRecord,
    settling: bool,
) -> Result<()> {
    let binding = record
        .call_binding
        .as_ref()
        .context("ExecCommand attempt has no call binding")?;
    let turn_id = record
        .turn_id
        .as_deref()
        .context("ExecCommand attempt has no Turn")?;
    ensure!(
        record.tool_name == EXEC_COMMAND
            && record.agent_id == scope.agent_id
            && binding.incarnation == scope.identity_incarnation
            && !binding.tool_call_id.is_empty()
            && !binding.assistant_round_id.is_empty(),
        "ExecCommand attempt differs from its identity scope"
    );
    let current: bool = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM agent_states s JOIN turn_records t
              ON t.agent_id=s.agent_id AND t.turn_id=?2 AND t.turn_index=s.turn_index
            WHERE s.agent_id=?1 AND s.turn_index=?3 AND t.terminal_kind IS NULL
              AND json_extract(s.payload_json,'$.current_turn_id')=?2
              AND s.current_run_id IS json_extract(t.payload_json,'$.run_id')
              AND (?4 OR COALESCE(json_extract(t.payload_json,'$.external_side_effect_possible'),0)=0)
        )",
        params![scope.agent_id, turn_id, record.turn_index, settling],
        |row| row.get(0),
    )?;
    ensure!(current, "ExecCommand attempt is no longer current");
    if settling {
        let prior = committed_tool_tx(tx, &scope.agent_id, &record.id)?;
        ensure!(
            prior.status == ToolExecutionStatus::Pending
                && matches!(
                    record.status,
                    ToolExecutionStatus::Success | ToolExecutionStatus::Error
                )
                && record.completed_at.is_some()
                && prior.call_binding == record.call_binding
                && prior.turn_id == record.turn_id
                && prior.turn_index == record.turn_index
                && prior.tool_name == record.tool_name
                && prior.work_item_id == record.work_item_id
                && prior.created_at == record.created_at
                && prior.authority_class == record.authority_class
                && prior.input == record.input
                && prior.invocation_surface == record.invocation_surface,
            "ExecCommand result differs from the pending attempt"
        );
    } else {
        ensure!(
            record.status == ToolExecutionStatus::Pending
                && record.completed_at.is_none()
                && record.output.is_null(),
            "ExecCommand attempt must start without a result"
        );
        let conflict: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_executions WHERE evidence_id=?1)
                OR EXISTS(SELECT 1 FROM tool_executions
                  WHERE agent_id=?2 AND turn_id=?3
                    AND json_extract(payload_json,'$.status')='pending')",
            params![record.id, scope.agent_id, turn_id],
            |row| row.get(0),
        )?;
        ensure!(
            !conflict,
            "ExecCommand attempt already exists or another command is pending"
        );
        assistant_round_tx(tx, scope, record)?;
    }
    Ok(())
}

fn assistant_round_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    record: &ToolExecutionRecord,
) -> Result<usize> {
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key,
    };
    let binding = record
        .call_binding
        .as_ref()
        .context("ExecCommand attempt has no call binding")?;
    let round = tx
        .query_row(
            "SELECT json_extract(t.payload_json,'$.round') FROM transcript_entries t
         WHERE t.evidence_id=?1 AND t.agent_id=?2 AND t.turn_id=?3 AND t.kind='assistant_round'
           AND EXISTS(SELECT 1 FROM json_each(t.payload_json,'$.data.blocks') b
              WHERE json_extract(b.value,'$.type')='tool_use'
                AND json_extract(b.value,'$.id')=?4
                AND json_extract(b.value,'$.name')=?5)",
            params![
                binding.assistant_round_id,
                scope.agent_id,
                record.turn_id,
                binding.tool_call_id,
                EXEC_COMMAND
            ],
            |row| row.get(0),
        )
        .optional()?
        .context("ExecCommand attempt has no matching assistant tool call")?;
    super::input_evidence_snapshot::validate_existing_input_evidence_tx(
        tx,
        scope,
        &Key::new(Family::Transcript, &binding.assistant_round_id),
    )?;
    Ok(round)
}

pub(super) fn pending_id_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    turn_id: &str,
) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT evidence_id FROM tool_executions
         WHERE agent_id=?1 AND turn_id=?2 AND kind=?3
           AND json_extract(payload_json,'$.status')='pending'
           AND json_extract(payload_json,'$.call_binding.incarnation')=?4
         ORDER BY created_at, evidence_id LIMIT 1",
            params![
                scope.agent_id,
                turn_id,
                EXEC_COMMAND,
                scope.identity_incarnation
            ],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn interrupt(record: &mut ToolExecutionRecord, at: chrono::DateTime<chrono::Utc>) {
    record.status = ToolExecutionStatus::Interrupted;
    record.completed_at = Some(at);
    record.duration_ms = at
        .signed_duration_since(record.created_at)
        .num_milliseconds()
        .max(0) as u64;
    record.summary = "Command interrupted. External outcome unknown.".into();
    record.output = serde_json::json!({
        "disposition": "interrupted",
        "reason": "external_outcome_unknown",
    });
}

pub(super) fn recover_missing_results_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    turn: &mut crate::types::TurnRecord,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<Vec<crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta>> {
    use super::input_evidence_snapshot::{
        native_birth::{original_material_limits, NativeInputWriteProof},
        register_committed_input_evidence_tx, validate_existing_input_evidence_tx,
        CanonicalInputEvidence,
    };
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key,
    };
    use crate::types::{ToolResultData, ToolResultRef, TranscriptEntry, TranscriptEntryKind};

    let turn_id = turn.turn_id.as_str();
    let mut statement = tx.prepare(
        "SELECT e.evidence_id FROM tool_executions e
         WHERE e.agent_id=?1 AND e.turn_id=?2 AND e.kind=?3
           AND json_extract(e.payload_json,'$.call_binding.incarnation')=?4
           AND json_extract(e.payload_json,'$.status') IN ('success','error')
           AND NOT EXISTS (
             SELECT 1 FROM transcript_entries t, json_each(t.payload_json,'$.data.refs') r
             WHERE t.agent_id=e.agent_id AND t.turn_id=e.turn_id AND t.kind='tool_results'
               AND json_extract(r.value,'$.tool_execution_id')=e.evidence_id
               AND json_extract(r.value,'$.tool_call_id')=json_extract(e.payload_json,'$.call_binding.tool_call_id')
           )
         ORDER BY e.created_at,e.evidence_id",
    )?;
    let ids = statement
        .query_map(
            params![
                scope.agent_id,
                turn_id,
                EXEC_COMMAND,
                scope.identity_incarnation
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut deltas = Vec::new();
    for id in ids {
        validate_existing_input_evidence_tx(tx, scope, &Key::new(Family::ToolExecution, &id))?;
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::new(ledger, scope)?;
        let entry = material.read(|| {
            let record = committed_tool_tx(tx, &scope.agent_id, &id)?;
            let round = assistant_round_tx(tx, scope, &record)?;
            let binding = record
                .call_binding
                .context("ExecCommand result has no call binding")?;
            crate::diagnostics::resource_io::admit_cloned_value(&record.output)?;
            let result: crate::tool::ToolResult = serde_json::from_value(record.output)?;
            let provider_visible_text = if result.is_error() {
                crate::tool::tools::render_tool_result_for_model(&result)?
            } else {
                crate::tool::tools::render_tool_result_for_model_with_context(
                    &result,
                    &crate::tool::tools::ToolModelRenderContext {
                        tool_execution_id: &id,
                        tool_output_budget_estimated_tokens: binding.model_output_budget,
                    },
                )?
            };
            let mut entry = TranscriptEntry::new(
                &scope.agent_id,
                TranscriptEntryKind::ToolResults,
                Some(round),
                None,
                serde_json::to_value(ToolResultData::RefsWithWrapper {
                    turn_id: Some(turn_id.to_owned()),
                    refs: vec![ToolResultRef {
                        tool_call_id: binding.tool_call_id,
                        tool_execution_id: Some(id.clone()),
                        provider_visible_text: Some(provider_visible_text),
                        content_truncated: false,
                        is_error: result.is_error(),
                    }],
                })?,
            );
            entry.data["recovered_exec_command_result"] = serde_json::Value::Bool(true);
            entry.id = format!("transcript:recovered-tool-result:{id}");
            entry.created_at = record
                .completed_at
                .context("settled ExecCommand has no completion time")?;
            Ok(entry)
        })?;
        validate_existing_input_evidence_tx(tx, scope, &Key::new(Family::Transcript, &entry.id))?;
        let native = NativeInputWriteProof::prepare_tx(
            tx,
            scope,
            ledger,
            original_material_limits(ledger)?,
            None,
            |visit| visit(Family::Transcript, &entry.id),
        )?;
        let (entry, inserted) = super::evidence::append_transcript_entry_tx(tx, &entry)?;
        ensure!(
            inserted,
            "recovered ExecCommand transcript already exists without its result"
        );
        let mut delta = register_committed_input_evidence_tx(
            tx,
            scope,
            &[CanonicalInputEvidence::Transcript(entry)],
        )?;
        if let Some(native) = native {
            native.finish_delta_tx(tx, &mut delta, ledger, None)?;
        }
        if !turn.tool_execution_ids.contains(&id) {
            turn.tool_execution_ids.push(id);
        }
        deltas.push(delta);
    }
    Ok(deltas)
}
