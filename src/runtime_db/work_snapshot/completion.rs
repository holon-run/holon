use super::*;
use crate::runtime::{
    completion_sources::{
        CompletionOptionalAbsence, CompletionOptionalSourceObservation, CompletionSourceEdge,
    },
    resident_input_evidence::EvidenceLocator,
};

/// Called only after this owner key was qualified in the caller's same input
/// transaction. No global by-ID exception is returned, only the exact edge.
#[cfg(test)]
pub(crate) fn completion_source_observations_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected_heads: &BTreeSet<String>,
    source_key: &InputEvidenceKey,
) -> Result<Vec<CompletionOptionalSourceObservation>> {
    completion_source_observations_with_scope_tx(tx, scope, selected_heads, source_key, None)
}

pub(crate) fn completion_source_observations_with_scope_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected_heads: &BTreeSet<String>,
    source_key: &InputEvidenceKey,
    read: Option<&super::super::context_inputs::ContextScopeRead<'_>>,
) -> Result<Vec<CompletionOptionalSourceObservation>> {
    if let Some(read) = read {
        read.validate(tx, scope)?;
        ensure!(
            scope_is_current(tx, scope, read.facts(), true)?,
            "completion source scope retired"
        );
    } else {
        ensure!(
            scope_facts(tx, scope, true)?.is_some(),
            "completion source scope retired"
        );
    }
    bound_metadata(
        selected_heads.len(),
        selected_heads.iter().map(String::len).sum(),
    )?;
    let coverage = loader::current_coverage(tx, scope)?
        .context("completion Work index requires preparation")?;
    let indexed = coverage
        .latest_results
        .values()
        .flatten()
        .map(|head| head.brief_id.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        selected_heads.is_subset(&indexed),
        "selected completion Brief is not a current qualified Work head"
    );
    let mut observations = Vec::new();
    for head_id in selected_heads {
        // A source key can only authorize its own concrete canonical edge.
        if source_key.family == InputEvidenceFamily::Brief && source_key.id != *head_id {
            continue;
        }
        if !matches!(
            source_key.family,
            InputEvidenceFamily::Brief | InputEvidenceFamily::Transcript
        ) {
            continue;
        }
        let head_key = InputEvidenceKey::new(InputEvidenceFamily::Brief, head_id);
        let (head, head_payload) = qualified_payload(tx, scope, &head_key)?;
        let brief: BriefRecord = serde_json::from_str(&head_payload)?;
        ensure!(
            brief.id == *head_id
                && brief.agent_id == scope.agent_id
                && brief.kind == BriefKind::Result
                && !brief.text.is_empty(),
            "completion head projection mismatch"
        );
        ensure!(
            coverage
                .latest_results
                .get(
                    brief
                        .work_item_id
                        .as_deref()
                        .context("completion head has no Work")?
                )
                .and_then(Option::as_ref)
                .is_some_and(|current| current.brief_id == *head_id),
            "completion Brief owner/index mismatch"
        );
        let Some(transcript_id) = brief.content_source.transcript_entry_id() else {
            continue;
        };
        match source_key.family {
            InputEvidenceFamily::Brief => {
                let target = InputEvidenceKey::new(InputEvidenceFamily::Transcript, transcript_id);
                if let Some(availability) = source_absence_with_scope(tx, scope, &target, read)? {
                    observations.push(CompletionOptionalSourceObservation {
                        head: head.clone(),
                        source: head,
                        edge: CompletionSourceEdge::BriefTranscript,
                        declared_target: target,
                        availability,
                    });
                }
            }
            InputEvidenceFamily::Transcript if source_key.id == transcript_id => {
                let (source, payload) = qualified_payload(tx, scope, source_key)?;
                let transcript: TranscriptEntry = serde_json::from_str(&payload)?;
                ensure!(
                    transcript.id == transcript_id && transcript.agent_id == scope.agent_id,
                    "completion Transcript projection mismatch"
                );
                if transcript.kind == TranscriptEntryKind::IncomingMessage {
                    if let Some(id) = transcript.related_message_id {
                        let target = InputEvidenceKey::new(InputEvidenceFamily::Message, id);
                        if let Some(availability) =
                            source_absence_with_scope(tx, scope, &target, read)?
                        {
                            observations.push(CompletionOptionalSourceObservation {
                                head,
                                source,
                                edge: CompletionSourceEdge::IncomingTranscriptMessage,
                                declared_target: target,
                                availability,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    bound_metadata(observations.len(), serde_json::to_vec(&observations)?.len())?;
    Ok(observations)
}

pub(super) fn completion_head_refs_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    id: &str,
) -> Result<WorkCompletionRef> {
    let key = InputEvidenceKey::new(InputEvidenceFamily::Brief, id);
    let payload = proven_payload(tx, scope, &key)?;
    let brief: BriefRecord = serde_json::from_str(&payload)?;
    ensure!(
        brief.agent_id == scope.agent_id
            && brief.id == id
            && brief.kind == BriefKind::Result
            && !brief.text.is_empty(),
        "Work head is not a canonical Result"
    );
    let mut result = WorkCompletionRef {
        brief_id: id.to_owned(),
        transcript_entry_id: None,
        source_message_id: None,
    };
    if let Some(id) = brief.content_source.transcript_entry_id() {
        let key = InputEvidenceKey::new(InputEvidenceFamily::Transcript, id);
        if source_absence(tx, scope, &key)?.is_none() {
            let payload = proven_payload(tx, scope, &key)?;
            let transcript: TranscriptEntry = serde_json::from_str(&payload)?;
            ensure!(
                transcript.agent_id == scope.agent_id && transcript.id == id,
                "Work head Transcript scope mismatch"
            );
            result.transcript_entry_id = Some(id.to_owned());
            if transcript.kind == TranscriptEntryKind::IncomingMessage {
                if let Some(id) = transcript.related_message_id {
                    let key = InputEvidenceKey::new(InputEvidenceFamily::Message, &id);
                    if source_absence(tx, scope, &key)?.is_none() {
                        result.source_message_id = Some(id);
                    }
                }
            }
        }
    }
    Ok(result)
}

fn qualified_payload(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &InputEvidenceKey,
) -> Result<(EvidenceLocator, String)> {
    let (table, id) = match key.family {
        InputEvidenceFamily::Brief => ("briefs", "evidence_id"),
        InputEvidenceFamily::Transcript => ("transcript_entries", "evidence_id"),
        InputEvidenceFamily::Message => ("messages", "evidence_id"),
        _ => anyhow::bail!("invalid completion source family"),
    };
    let alias:Option<(u64,Option<String>,String)>=tx.query_row("SELECT revision,content_digest,availability FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",params![scope.agent_id,scope.identity_incarnation,key.family.as_str(),key.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
    let (revision, expected, status) =
        alias.context("completion owner was not qualified before observing its edge")?;
    ensure!(
        status == "present" && revision > 0,
        "completion source is pruned or unprepared"
    );
    let expected = expected.context("completion source digest absent")?;
    let (agent, bytes): (String, usize) = tx.query_row(
        &format!("SELECT agent_id,length(CAST(payload_json AS BLOB)) FROM {table} WHERE {id}=?1"),
        [&key.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        agent == scope.agent_id,
        "completion source belongs to another agent"
    );
    bound_metadata(1, bytes)?;
    let (payload, hash): (String, Option<String>) = tx.query_row(
        &format!("SELECT payload_json,content_hash FROM {table} WHERE {id}=?1"),
        [&key.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        digest(payload.as_bytes()) == expected,
        "completion source raw body changed without a qualified writer"
    );
    if let Some(hash) = hash {
        ensure!(
            hash == super::super::evidence::content_hash(&payload),
            "completion source canonical hash differs from its body"
        );
    }
    Ok((
        EvidenceLocator {
            scope: scope.clone(),
            key: key.clone(),
            revision,
            content_digest: expected,
        },
        payload,
    ))
}

fn source_absence(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &InputEvidenceKey,
) -> Result<Option<CompletionOptionalAbsence>> {
    source_absence_with_scope(tx, scope, key, None)
}

fn source_absence_with_scope(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &InputEvidenceKey,
    read: Option<&super::super::context_inputs::ContextScopeRead<'_>>,
) -> Result<Option<CompletionOptionalAbsence>> {
    let table = match key.family {
        InputEvidenceFamily::Transcript => "transcript_entries",
        InputEvidenceFamily::Message => "messages",
        _ => anyhow::bail!("invalid optional completion edge target"),
    };
    let exists: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE evidence_id=?1)"),
        [&key.id],
        |row| row.get(0),
    )?;
    if exists {
        let _ = proven_payload_with_scope(tx, scope, key, read)?;
        return Ok(None);
    }
    let known:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE family=?1 AND record_id=?2 UNION ALL SELECT 1 FROM input_evidence_eligibility WHERE family=?1 AND record_id=?2 UNION ALL SELECT 1 FROM input_evidence_seed_recovery_current WHERE family=?1 AND record_id=?2)",params![key.family.as_str(),key.id],|row|row.get(0))?;
    ensure!(
        !known,
        "completion source is unavailable or pruned, not globally absent"
    );
    Ok(Some(CompletionOptionalAbsence::GlobalAbsent))
}

/// Existing immutable/current membership, not a grant. This allows computing a
/// finite owning-root request before the input sidecar materializes its alias.
pub(super) fn proven_payload(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &InputEvidenceKey,
) -> Result<String> {
    proven_payload_with_scope(tx, scope, key, None)
}

fn proven_payload_with_scope(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &InputEvidenceKey,
    read: Option<&super::super::context_inputs::ContextScopeRead<'_>>,
) -> Result<String> {
    let (table, id) = match key.family {
        InputEvidenceFamily::Brief => ("briefs", "evidence_id"),
        InputEvidenceFamily::Transcript => ("transcript_entries", "evidence_id"),
        InputEvidenceFamily::Message => ("messages", "evidence_id"),
        _ => anyhow::bail!("invalid completion source family"),
    };
    let bytes: usize = tx.query_row(
        &format!("SELECT length(CAST(payload_json AS BLOB)) FROM {table} WHERE {id}=?1"),
        [&key.id],
        |row| row.get(0),
    )?;
    bound_metadata(1, bytes)?;
    use super::super::input_evidence_snapshot::ExistingAdmissionInputProof;
    let roots = BTreeSet::from([key.clone()]);
    let _proof = match read {
        Some(read) => ExistingAdmissionInputProof::prepare_with_scope_read_tx(
            tx,
            scope,
            roots,
            BTreeSet::new(),
            read,
        )?,
        None => ExistingAdmissionInputProof::prepare_tx(tx, scope, roots, BTreeSet::new())?,
    };
    let payload: String = tx.query_row(
        &format!("SELECT payload_json FROM {table} WHERE {id}=?1"),
        [&key.id],
        |row| row.get(0),
    )?;
    Ok(payload)
}
