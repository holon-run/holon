//! Finite Tool report material. The original Input actor owns qualification.
use super::*;
use crate::runtime::run_report_projection::{
    project_tool_run_report_fields, ToolRunReportProjection,
};

#[path = "input_tool_report_stream.rs"]
mod stream;

/// Original callers supply qualification and this exact actor's remaining
/// material. The incremental reader grants no alias, birth or new ledger.
pub(in crate::runtime_db) fn prepare_tool_run_report_on(
    tx: &Transaction<'_>, agent: &str, id: &str, tool: &str,
    material: &mut read_material::InputReadMaterial<'_>,
) -> Result<ToolRunReportProjection> {
    material.validate_tool_agent(agent)?;
    let mark = material.tool_scratch_mark();
    let mut retained = None;
    let result = (|| {
        let source = stream::selected_report_on(tx, agent, id, tool,
            |bytes, records| material.observe_projected(bytes, records))?;
        let null = serde_json::Value::Null;
        project_tool_run_report_fields(tool, source.get("input").unwrap_or(&null), source.get("output").unwrap_or(&null),
            |bytes, count| {
                retained = Some(material.tool_scratch_mark());
                material.observe_projected(bytes.checked_mul(8).context("Tool report copy bytes overflow")?,
                    count.checked_mul(8).context("Tool report copy count overflow")?)
            })
    })();
    match result {
        Ok(report) => { material.retire_tool_scratch(mark, retained)?; Ok(report) }
        Err(error) => { material.retire_tool_scratch(mark, None)?; Err(error) }
    }
}

/// Named hydration after original Input qualification. Keep this phase's guards
/// through the actual handoff. Ordinary same-locator Some reads are pure.
pub(in crate::runtime_db) fn prepare_qualified_tool_run_report_on(
    tx: &Transaction<'_>,
    record: &crate::runtime::resident_input_evidence::QualifiedInputEvidence,
    material: &mut read_material::InputReadMaterial<'_>,
) -> Result<Box<crate::runtime::resident_input_evidence::QualifiedToolRunReport>> {
    let mark = material.tool_scratch_mark();
    let mut retained = None;
    let result = (|| {
        let locator = &record.locator;
        let key = &locator.key;
        material.validate_binding(&locator.scope)?;
        let tool = match &record.record {
            PreparedInputEvidence::ToolExecution(tool) if key.family == Family::ToolExecution
                && tool.id == key.id && tool.agent_id == locator.scope.agent_id => tool,
            _ => return Err(InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()).into()),
        };
        let facts = material.scope_facts_tx(tx)?;
        ensure!(facts.identity.as_ref().is_some_and(|identity|
            identity.agent_id == locator.scope.agent_id
                && identity.incarnation == locator.scope.identity_incarnation
                && identity.status == AgentRegistryStatus::Active),
            InputEvidenceSnapshotError::RetiredScope);
        ensure!(facts.state.is_some(), InputEvidenceSnapshotError::NeedsPreparation("Tool report owner has no State".into()));
        material.binding_metadata_tx(tx)?;
        let binding = binding_on(tx, &locator.scope)?;
        material.record_metadata_tx(tx, &binding, key)?;
        let alias = alias_on(tx, &locator.scope, key)?.context(InputEvidenceSnapshotError::NeedsPreparation(key.id.clone()))?;
        ensure!(alias.present && alias.revision == locator.revision
            && alias.digest.as_deref() == Some(locator.content_digest.as_str()),
            InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()));
        validate_tool_report_blob_on(tx, &locator.scope.agent_id, key, &alias.fingerprint,
            &locator.content_digest, |bytes, count| material.observe_projected(bytes, count))?;
        let projection = prepare_tool_run_report_on(tx, &locator.scope.agent_id, &key.id, &tool.tool_name, material)?;
        // The locator and final Box have their own actual borrowed lower bound
        // before the first locator clone. Temporary decoders can now retire.
        let (encoded, nodes) = crate::runtime::input_evidence_budget::borrowed_json_material(&(locator, &projection))?;
        let bytes = encoded.checked_add(u64::try_from(nodes)?.checked_mul(256).context("Tool report retained nodes overflow")?)
            .and_then(|n|n.checked_add(4096)).and_then(|n|n.checked_mul(8)).context("Tool report retained bytes overflow")?;
        retained = Some(material.tool_scratch_mark());
        material.observe_projected(bytes, nodes.checked_mul(8).context("Tool report retained count overflow")?)?;
        Ok(Box::new(crate::runtime::resident_input_evidence::QualifiedToolRunReport::from_qualified_source(locator.clone(), projection)))
    })();
    match result {
        Ok(report) => { material.retire_tool_scratch(mark, retained)?; Ok(report) }
        Err(error) => { material.retire_tool_scratch(mark, None)?; Err(error) }
    }
}

const TOOL_REPORT_HASH_CHUNK: usize = 64 * 1024;
/// rowid only locates this already-qualified same-TX cell for incremental I/O;
/// it grants no body membership, native birth, State or mutation authority.
fn validate_tool_report_blob_on(
    tx: &Transaction<'_>, agent: &str, key: &Key, expected: &Fingerprint, expected_digest: &str,
    mut admit: impl FnMut(u64, usize) -> Result<()>,
) -> Result<()> {
    let digest = original_tool_blob_digest_on(tx, agent, key, expected, &mut admit)?;
    ensure!(normalized_payload_digest(expected.content_hash.as_deref().context("Tool report canonical hash missing")?)? == expected_digest
        && digest == expected_digest, InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()));
    Ok(())
}

/// Used only after the original scoped writer qualified this exact pre-business
/// row. Its digest seals the existing bytes; it grants no alias or new source.
pub(super) fn original_tool_blob_digest_on(
    tx: &Transaction<'_>, agent: &str, key: &Key, expected: &Fingerprint,
    mut admit: impl FnMut(u64, usize) -> Result<()>,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    // Buffer, Blob handle/page scratch, hash state and locator copies are held
    // before blob_open/first Vec. One buffer is reused for the entire source.
    admit(u64::try_from((TOOL_REPORT_HASH_CHUNK + 4096) * 8)?, 32)?;
    let (row_id, text, created_at, content_hash): (i64, bool, String, Option<String>) = tx.query_row(
        "SELECT rowid,typeof(payload_json)='text',created_at,content_hash FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2",
        params![agent,key.id], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional()?.context(InputEvidenceSnapshotError::NeedsPreparation(key.id.clone()))?;
    ensure!(text, InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()));
    let blob = tx.blob_open(rusqlite::DatabaseName::Main, "tool_executions", "payload_json", row_id, true)?;
    // Blob length avoids materializing the entire TEXT cell merely to learn
    // its byte length. This is the original Tool fingerprint, field for field.
    let actual = Fingerprint {
        content_hash, sequence: None, turn_index: None, created_at,
        owner_kind: None, owner_id: None, revision_marker: "canonical_body".into(),
        payload_bytes: blob.len(), related_turn_id: None, frozen_payload_digest: None,
    };
    ensure!(actual.same_metadata(expected),
        InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()));
    ensure!(blob.len().checked_mul(2).and_then(|n|n.checked_add(4096)).is_some_and(|n|n <= MAX_METADATA_BYTES),
        InputEvidenceSnapshotError::ResourceExhausted("Tool report raw"));
    let expected_len = expected.payload_bytes;
    let mut buffer = vec![0u8; TOOL_REPORT_HASH_CHUNK.min(expected_len)];
    let mut hash = Sha256::new();
    let mut offset = 0usize;
    while offset < expected_len {
        let length = buffer.len().min(expected_len - offset);
        blob.read_at_exact(&mut buffer[..length], offset)?;
        hash.update(&buffer[..length]);
        offset = offset.checked_add(length).context("Tool report hash offset overflow")?;
    }
    blob.close()?;
    let digest = format!("{:x}", hash.finalize());
    validate_canonical_payload_digest(key, expected, &digest)?;
    Ok(digest)
}

#[cfg(test)]
#[path = "input_tool_run_report_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "input_qualified_tool_report_tests.rs"]
mod qualified_tests;

/// The original selected Tool's prompt role. Use only after the same Input
/// qualification; unknown output/command bodies never become a Value here.
pub(in crate::runtime_db::input_evidence_snapshot) fn prepare_tool_prompt_on(
    tx: &Transaction<'_>, agent: &str, id: &str,
    material: &mut read_material::InputReadMaterial<'_>,
) -> Result<ToolContextProjection> {
    use crate::runtime::input_evidence_budget::borrowed_json_material;
    use crate::runtime::resident_input_evidence::ToolCommandProjection;
    use serde_json::Value;
    material.validate_tool_agent(agent)?;
    let mark = material.tool_scratch_mark();
    let mut retained = None;
    let result = (|| {
        // Every normalized String is measured before row.get. These are the
        // original projection columns, distinct from the borrowed body fields.
        let normalized: usize = tx.query_row(
            "SELECT length(CAST(agent_id AS BLOB))+length(CAST(evidence_id AS BLOB))+COALESCE(length(CAST(work_item_id AS BLOB)),0)+COALESCE(length(CAST(turn_id AS BLOB)),0)+length(CAST(created_at AS BLOB)) FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2",
            params![agent,id], |r|r.get(0),
        ).optional()?.context(InputEvidenceSnapshotError::NeedsPreparation(id.into()))?;
        material.observe_projected(u64::try_from(normalized)?.checked_add(4096).and_then(|n|n.checked_mul(8)).context("Tool prompt metadata overflow")?, 40)?;
        let (work_item_id, turn_id, created_at): (Option<String>, Option<String>, String) = tx.query_row(
            "SELECT work_item_id,turn_id,created_at FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2",
            params![agent,id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        )?;
        let source = stream::selected_prompt_on(tx, agent, id, |bytes, nodes|material.observe_projected(bytes,nodes))?;
        ensure!(source.get("work_item_id").and_then(Value::as_str) == work_item_id.as_deref()
            && source.get("turn_id").and_then(Value::as_str) == turn_id.as_deref(),
            InputEvidenceSnapshotError::ProjectionChanged(id.into()));
        let null = Value::Null;
        let raw_output = source.get("output").unwrap_or(&null);
        let output = raw_output.get("result")
            .or_else(||raw_output.get("envelope").and_then(|v|v.get("result")))
            .unwrap_or(raw_output);
        let input = source.get("input").unwrap_or(&null);
        // This guard precedes every finite DTO/header/safe-preview clone.
        let (bytes,nodes)=borrowed_json_material(&(&source,&work_item_id,&turn_id,&created_at,agent,id))?;
        material.observe_projected(bytes.checked_add(u64::try_from(nodes)?.checked_mul(256).context("Tool prompt copy nodes overflow")?)
            .and_then(|n|n.checked_add(4096)).and_then(|n|n.checked_mul(8)).context("Tool prompt copy bytes overflow")?,
            nodes.checked_mul(8).context("Tool prompt copy count overflow")?)?;
        let tool_name = source.get("tool_name").and_then(Value::as_str).context("Tool name missing")?;
        let disposition = output.get("disposition").and_then(Value::as_str);
        let task_id = output.get("task_handle").and_then(|v|v.get("task_id")).and_then(Value::as_str);
        ensure!(id.len()<=4096 && agent.len()<=4096 && tool_name.len()<=512
            && work_item_id.as_ref().is_none_or(|v|v.len()<=4096)
            && turn_id.as_ref().is_none_or(|v|v.len()<=4096)
            && disposition.is_none_or(|v|v.len()<=512) && task_id.is_none_or(|v|v.len()<=4096),
            "tool context metadata exceeds bounded projection");
        let exit = output.get("exit_status").or_else(||output.get("exit_code"));
        let command = ToolCommandProjection {
            command_preview: input.get("cmd").and_then(Value::as_str).map(safe_command_preview),
            disposition: disposition.map(str::to_owned), task_id: task_id.map(str::to_owned),
            exit_status: exit.and_then(Value::as_i64),
            has_output_evidence: ["stdout_preview","stderr_preview","initial_output_preview","stdout_artifact","stderr_artifact"].iter().any(|key|output.get(key).is_some()),
            truncated: ["truncated","initial_output_truncated","stdout_truncated","stderr_truncated","output_truncated"].iter().any(|key|output.get(key).and_then(Value::as_bool).unwrap_or(false)),
            has_artifact: output.get("artifacts").and_then(Value::as_u64).is_some_and(|n|n>0)
                || ["artifact","stdout_artifact","stderr_artifact","output_artifact"].iter().any(|key|output.get(key).is_some()),
        };
        let total = usize::try_from(input.get("items").and_then(Value::as_u64).unwrap_or(0))?;
        let batch = output.get("items");
        let count = |name:&str| -> Result<usize> { Ok(usize::try_from(batch.and_then(|v|v.get(name)).and_then(Value::as_u64).unwrap_or(0))?) };
        let mut record = ToolContextProjection {
            id: id.into(), agent_id: agent.into(), work_item_id, turn_id,
            turn_index: source.get("turn_index").filter(|v|!v.is_null()).map(|v|serde_json::from_value(v.clone())).transpose()?.unwrap_or(0),
            tool_name: tool_name.into(), created_at: created_at.parse()?,
            authority_class: serde_json::from_value(source.get("authority_class").filter(|v|!v.is_null()).or_else(||source.get("trust")).unwrap_or(&null).clone())?,
            status: serde_json::from_value(source.get("status").unwrap_or(&null).clone())?,
            summary: source.get("summary").and_then(Value::as_str).unwrap_or("").into(), command,
            batch_total: total, batch_succeeded: 0, batch_failed: 0, batch_promoted: 0, batch_unknown: 0,
        };
        if tool_name=="ExecCommandBatch" {
            let seen = count("seen")?;
            record.batch_succeeded=count("succeeded")?;
            record.batch_failed=count("failed")?;
            record.batch_promoted=count("promoted")?;
            record.batch_unknown=count("unknown")?.checked_add(total.saturating_sub(seen)).context("Tool batch count overflow")?;
        }
        // Only the actual final DTO/native floor survives decoder retirement.
        let (bytes,nodes)=borrowed_json_material(&record)?;
        retained=Some(material.tool_scratch_mark());
        material.observe_projected(bytes.checked_add(u64::try_from(nodes)?.checked_mul(256).context("Tool prompt retained nodes overflow")?)
            .and_then(|n|n.checked_add(4096)).and_then(|n|n.checked_mul(8)).context("Tool prompt retained bytes overflow")?,
            nodes.checked_mul(8).context("Tool prompt retained count overflow")?)?;
        Ok(record)
    })();
    match result {
        Ok(record)=>{material.retire_tool_scratch(mark,retained)?;Ok(record)}
        Err(error)=>{material.retire_tool_scratch(mark,None)?;Err(error)}
    }
}

#[cfg(test)]
#[path = "input_tool_prompt_stream_tests.rs"]
mod prompt_tests;

/// The caller has the original Task/Input read authorization and fingerprint.
/// Fixed members remain read-only; this helper neither requires nor mints aliases.
pub(in crate::runtime_db::input_evidence_snapshot) fn prepare_authorized_tool_prompt_on(
    tx: &Transaction<'_>, scope: &InputEvidenceScope, key: &Key,
    fingerprint: &Fingerprint, digest: &str,
    material: &mut read_material::InputReadMaterial<'_>,
) -> Result<ToolContextProjection> {
    material.validate_binding(scope)?;
    ensure!(key.family == Family::ToolExecution, "readonly Tool prompt family differs");
    let scratch = material.tool_scratch_mark();
    let result = (|| {
        validate_tool_report_blob_on(tx, &scope.agent_id, key, fingerprint, digest,
            |bytes, count| material.observe_projected(bytes, count))?;
        let retained = material.tool_scratch_mark();
        let prompt = prepare_tool_prompt_on(tx, &scope.agent_id, &key.id, material)?;
        Ok((prompt, retained))
    })();
    match result {
        Ok((prompt, retained)) => {
            material.retire_tool_scratch(scratch, Some(retained))?;
            Ok(prompt)
        }
        Err(error) => {
            material.retire_tool_scratch(scratch, None)?;
            Err(error)
        }
    }
}


/// A Focus kernel no-op can reuse only the source sealed before business SQL.
/// Unlike ordinary hydration, old qualified rows may lack a normalized hash.
pub(super) fn prepare_original_noop_tool_prompt_on(
    tx: &Transaction<'_>, scope: &InputEvidenceScope, key: &Key,
    source: &tool_writer::OriginalToolSource,
    material: &mut read_material::InputReadMaterial<'_>,
) -> Result<ToolContextProjection> {
    material.validate_binding(scope)?;
    ensure!(key.family == Family::ToolExecution, "original no-op Tool family differs");
    let scratch = material.tool_scratch_mark();
    let result = (|| {
        let digest = original_tool_blob_digest_on(tx, &scope.agent_id, key, source.fingerprint(),
            |bytes, count| material.observe_projected(bytes, count))?;
        ensure!(digest == source.digest(), InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()));
        let retained = material.tool_scratch_mark();
        let prompt = prepare_tool_prompt_on(tx, &scope.agent_id, &key.id, material)?;
        Ok((prompt, retained))
    })();
    match result {
        Ok((prompt, retained)) => { material.retire_tool_scratch(scratch, Some(retained))?; Ok(prompt) }
        Err(error) => { material.retire_tool_scratch(scratch, None)?; Err(error) }
    }
}


pub(super) fn original_tool_body_nodes_on(
    tx: &Transaction<'_>, agent: &str, id: &str,
    admit: impl FnMut(u64,usize)->Result<()>,
) -> Result<usize> {
    stream::original_body_nodes_on(tx,agent,id,admit)
}
