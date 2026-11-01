//! Fixed source ordering metadata and exact writer updates. This is a child of
//! input_evidence_snapshot and uses the same transaction and qualification scope.
use super::*;
use crate::domain::execution_protocol::{ExecutionAttemptState, WorkItemExecutionState};
use crate::runtime::resident_input_evidence::{
    ExecutionInputCoverage, ExecutionInputDelta, ExecutionSourceCoverage, SourceAttemptObservation,
};

const MAX_ID_BYTES: usize = 4096;
const MAX_ORDER_BYTES: usize = 512;

fn source_error(source: &str) -> anyhow::Error {
    InputEvidenceSnapshotError::NeedsPreparation(format!(
        "complete execution source metadata:{source}"
    ))
    .into()
}
fn allocation_ordinal(source: &str, id: &str) -> Option<u64> {
    let base = format!("activation:message:{source}");
    if id == base {
        return Some(1);
    }
    id.strip_prefix(&format!("{base}:attempt:"))?
        .parse::<u64>()
        .ok()
        .filter(|n| *n >= 2)
}
fn metadata_digest(value: &ExecutionSourceCoverage) -> Result<String> {
    Ok(bytes_digest(serde_json::to_string(value)?.as_bytes()))
}
fn source_raw_metadata(
    tx: &Transaction<'_>,
    agent: &str,
    source: &str,
    limit: usize,
) -> Result<BTreeMap<String, SourceAttemptObservation>> {
    // Reject oversized scalar metadata before copying it into Rust. No attempt
    // payload, source identity JSON, or body bytes are loaded here.
    let invalid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts
        WHERE agent_id=?1 AND json_extract(payload_json,'$.source_message_id')=?2
          AND (length(CAST(attempt_id AS BLOB))>?3
            OR length(CAST(json_extract(payload_json,'$.admitted_at') AS BLOB))>?4
            OR length(CAST(json_extract(payload_json,'$.turn_id') AS BLOB))>?3
            OR length(CAST(terminal_outcome_id AS BLOB))>?3
            OR json_type(payload_json,'$.admitted_at')!='text'))",
        params![agent, source, MAX_ID_BYTES, MAX_ORDER_BYTES],
        |r| r.get(0),
    )?;
    ensure!(
        !invalid,
        "execution source scalar metadata exceeds preparation bounds"
    );
    let mut statement = tx.prepare(
        "SELECT attempt_id,json_extract(payload_json,'$.admitted_at'),lifecycle_state,
          json_extract(payload_json,'$.turn_id'),terminal_outcome_id
        FROM execution_protocol_attempts
        WHERE agent_id=?1 AND json_extract(payload_json,'$.source_message_id')=?2
        ORDER BY json_extract(payload_json,'$.admitted_at'),attempt_id LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![agent, source, i64::try_from(limit.saturating_add(1))?],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        },
    )?;
    let mut result = BTreeMap::new();
    for row in rows {
        let (attempt_id, admitted_at, state, turn_id_hint, terminal_outcome_id) = row?;
        if result.len() >= limit {
            return Err(InputEvidenceSnapshotError::ResourceExhausted(
                "execution source metadata count",
            )
            .into());
        }
        let state = match state.as_str() {
            "open" => ExecutionAttemptState::Open,
            "settled" => ExecutionAttemptState::Settled,
            "interrupted" => ExecutionAttemptState::Interrupted,
            "protocol_violation" => ExecutionAttemptState::ProtocolViolation,
            _ => anyhow::bail!("invalid normalized execution attempt lifecycle"),
        };
        result.insert(
            attempt_id.clone(),
            SourceAttemptObservation {
                attempt_id,
                allocation_ordinal: 0,
                admitted_at,
                state,
                turn_id_hint,
                terminal_outcome_id,
                pruned: false,
            },
        );
    }
    Ok(result)
}
fn freeze_ordinals(
    source: &str,
    records: &mut BTreeMap<String, SourceAttemptObservation>,
) -> Result<u64> {
    let mut highwater = records
        .keys()
        .filter_map(|id| allocation_ordinal(source, id))
        .max()
        .unwrap_or(0);
    let mut used = BTreeSet::new();
    for record in records.values_mut() {
        if let Some(ordinal) = allocation_ordinal(source, &record.attempt_id) {
            ensure!(
                used.insert(ordinal),
                "duplicate legacy execution allocation ordinal"
            );
            record.allocation_ordinal = ordinal;
        }
    }
    let mut arbitrary = records
        .values()
        .filter(|x| x.allocation_ordinal == 0)
        .map(|x| (x.admitted_at.clone(), x.attempt_id.clone()))
        .collect::<Vec<_>>();
    arbitrary.sort();
    for (_, id) in arbitrary {
        highwater = highwater
            .checked_add(1)
            .context("execution ordinal overflow")?;
        records
            .get_mut(&id)
            .expect("frozen metadata member")
            .allocation_ordinal = highwater;
    }
    Ok(highwater)
}
fn check_budget(count: usize, bytes: usize, record_limit: usize, byte_limit: usize) -> Result<()> {
    if count > record_limit.min(MAX_RECORDS) || bytes > byte_limit.min(MAX_METADATA_BYTES) {
        return Err(InputEvidenceSnapshotError::ResourceExhausted(
            "execution metadata count/bytes",
        )
        .into());
    }
    Ok(())
}
/// Only a fixed seed actor calls this; a second phase cannot replace a source
/// already frozen in that seed. Metadata grants no Attempt/Outcome body IDs.
pub(super) fn capture_sources_tx(
    tx: &Transaction<'_>,
    seed: &str,
    phase: &str,
    agent: &str,
    mandatory: &BTreeSet<Key>,
    base_count: usize,
    base_bytes: usize,
) -> Result<(String, usize, usize)> {
    let mut sources = BTreeSet::new();
    let mut source_id_bytes = 0usize;
    for key in mandatory {
        match key.family {
            Family::Message => {
                if sources.insert(key.id.clone()) {
                    source_id_bytes = source_id_bytes.saturating_add(key.id.len());
                }
            }
            Family::Attempt => {
                let source:Option<String>=tx.query_row("SELECT json_extract(payload_json,'$.source_message_id') FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",params![agent,key.id],|r|r.get(0)).optional()?.flatten();
                if let Some(source) = source {
                    if sources.insert(source.clone()) {
                        source_id_bytes = source_id_bytes.saturating_add(source.len());
                    }
                }
            }
            _ => {}
        }
        check_budget(
            base_count.saturating_add(sources.len()),
            base_bytes.saturating_add(source_id_bytes),
            MAX_RECORDS,
            MAX_METADATA_BYTES,
        )?;
    }
    let (stored_count,stored_bytes):(i64,i64)=tx.query_row("SELECT COALESCE(SUM(1+(SELECT count(*) FROM json_each(s.metadata_json,'$.attempts'))),0),COALESCE(SUM(length(CAST(s.metadata_json AS BLOB))),0) FROM input_evidence_execution_seed_sources s WHERE seed_id=?1",[seed],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let mut count = base_count.saturating_add(usize::try_from(stored_count)?);
    let mut bytes = base_bytes
        .saturating_add(usize::try_from(stored_bytes)?)
        .saturating_add(usize::try_from(stored_count)?.saturating_mul(128));
    check_budget(count, bytes, MAX_RECORDS, MAX_METADATA_BYTES)?;
    let mut manifest = Vec::new();
    for source in sources {
        ensure!(
            source.len() <= MAX_ID_BYTES,
            "execution source ID exceeds preparation bounds"
        );
        let existing =
            fixed_seed_source_on(tx, Some(seed), &source, MAX_RECORDS, MAX_METADATA_BYTES)?;
        let already_frozen = existing.is_some();
        let coverage = if let Some(existing) = existing {
            existing
        } else {
            let mut attempts =
                source_raw_metadata(tx, agent, &source, MAX_RECORDS.saturating_sub(count))?;
            let allocation_highwater = freeze_ordinals(&source, &mut attempts)?;
            let coverage = ExecutionSourceCoverage {
                source_message_id: source.clone(),
                revision: 0,
                allocation_highwater,
                attempts,
            };
            coverage.validate()?;
            let json = serde_json::to_string(&coverage)?;
            let digest = metadata_digest(&coverage)?;
            // The caller's transaction also seals its fixed capture checkpoint.
            tx.execute("INSERT INTO input_evidence_execution_seed_sources(seed_id,capture_phase,source_message_id,metadata_json,content_digest) VALUES(?1,?2,?3,?4,?5)",params![seed,phase,source,json,digest])?;
            coverage
        };
        if !already_frozen {
            count = count.saturating_add(1 + coverage.attempts.len());
            bytes = bytes.saturating_add(metadata_bytes(&coverage)?);
        }
        check_budget(count, bytes, MAX_RECORDS, MAX_METADATA_BYTES)?;
        manifest.push((source, metadata_digest(&coverage)?));
    }
    Ok((
        bytes_digest(serde_json::to_string(&manifest)?.as_bytes()),
        count,
        bytes,
    ))
}
fn metadata_bytes(source: &ExecutionSourceCoverage) -> Result<usize> {
    source.metadata_bytes()
}
fn decode_source(
    source: &str,
    json: String,
    digest: String,
    revision: u64,
    limit: usize,
    bytes: usize,
) -> Result<ExecutionSourceCoverage> {
    ensure!(
        json.len() <= bytes.min(MAX_METADATA_BYTES),
        "execution source metadata exceeds byte budget"
    );
    ensure!(
        bytes_digest(json.as_bytes()) == digest,
        "execution source metadata digest mismatch"
    );
    let record: ExecutionSourceCoverage = serde_json::from_str(&json)?;
    ensure!(
        record.source_message_id == source && record.revision == revision,
        "execution source header/payload mismatch"
    );
    record.validate()?;
    check_budget(
        1 + record.attempts.len(),
        metadata_bytes(&record)?,
        limit,
        bytes,
    )?;
    Ok(record)
}
fn fixed_seed_source_on(
    tx: &Transaction<'_>,
    seed: Option<&str>,
    source: &str,
    limit: usize,
    bytes: usize,
) -> Result<Option<ExecutionSourceCoverage>> {
    let Some(seed) = seed else { return Ok(None) };
    let header:Option<(i64,i64)>=tx.query_row("SELECT length(CAST(metadata_json AS BLOB)),(SELECT count(*) FROM json_each(metadata_json,'$.attempts')) FROM input_evidence_execution_seed_sources WHERE seed_id=?1 AND source_message_id=?2",params![seed,source],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((size, count)) = header else {
        return Ok(None);
    };
    check_budget(
        1 + usize::try_from(count)?,
        usize::try_from(size)?,
        limit,
        bytes,
    )?;
    ensure!(
        usize::try_from(size)? <= bytes.min(MAX_METADATA_BYTES),
        "execution seed source metadata exceeds byte budget"
    );
    let (json,digest):(String,String)=tx.query_row("SELECT metadata_json,content_digest FROM input_evidence_execution_seed_sources WHERE seed_id=?1 AND source_message_id=?2",params![seed,source],|r|Ok((r.get(0)?,r.get(1)?)))?;
    decode_source(source, json, digest, 0, limit, bytes).map(Some)
}
fn seed_source_on(
    tx: &Transaction<'_>,
    seed: Option<&str>,
    source: &str,
    limit: usize,
    bytes: usize,
) -> Result<Option<ExecutionSourceCoverage>> {
    let Some(seed_id) = seed else { return Ok(None) };
    let header:Option<(i64,i64,i64)>=tx.query_row("SELECT revision,length(CAST(metadata_json AS BLOB)),(SELECT count(*) FROM json_each(metadata_json,'$.attempts')) FROM input_evidence_execution_seed_source_recovery WHERE seed_id=?1 AND source_message_id=?2",params![seed_id,source],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let Some((revision, size, count)) = header else {
        return fixed_seed_source_on(tx, seed, source, limit, bytes);
    };
    check_budget(
        1 + usize::try_from(count)?,
        usize::try_from(size)?,
        limit,
        bytes,
    )?;
    ensure!(
        usize::try_from(size)? <= bytes.min(MAX_METADATA_BYTES),
        "execution recovery metadata exceeds byte budget"
    );
    let (json,digest):(String,String)=tx.query_row("SELECT metadata_json,content_digest FROM input_evidence_execution_seed_source_recovery WHERE seed_id=?1 AND source_message_id=?2",params![seed_id,source],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let record = decode_source(source, json, digest, u64::try_from(revision)?, limit, bytes)?;
    let fixed = match fixed_seed_source_on(tx, seed, source, limit, bytes)? {
        Some(fixed) => fixed,
        None => {
            // This source was an actual new fallback Message in the original
            // exclusive transaction, not a late inference about old history.
            legacy_recovery::validate_seed_overlay_on(
                tx,
                seed_id,
                &Key::new(Family::Message, source),
            )?;
            let origin: String = tx.query_row("SELECT origin_kind FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family='message' AND record_id=?2", params![seed_id, source], |r| r.get(0))?;
            ensure!(
                origin == "maintenance_created"
                    && record.allocation_highwater == 0
                    && record.attempts.is_empty(),
                "execution recovery has no fixed or actual-created empty source origin"
            );
            return Ok(Some(record));
        }
    };
    ensure!(
        record.allocation_highwater == fixed.allocation_highwater
            && record.attempts.len() == fixed.attempts.len(),
        "legacy recovery expanded fixed execution source"
    );
    for (id, observed) in &record.attempts {
        let original = fixed
            .attempts
            .get(id)
            .context("legacy recovery added an attempt ordinal")?;
        ensure!(
            observed.allocation_ordinal == original.allocation_ordinal
                && observed.admitted_at == original.admitted_at,
            "legacy recovery changed attempt ordering identity"
        );
        if observed != original {
            let eligible:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE seed_id=?1 AND family='attempt' AND record_id=?2) AND EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family='attempt' AND record_id=?2 AND origin_kind='fixed_member')",params![seed_id,id],|r|r.get(0))?;
            ensure!(
                eligible,
                "execution source recovery changed an unqualified member"
            );
            legacy_recovery::validate_seed_overlay_on(tx, seed_id, &Key::new(Family::Attempt, id))?;
        }
    }
    Ok(Some(record))
}
fn current_source_on(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    source: &str,
    limit: usize,
    bytes: usize,
) -> Result<Option<ExecutionSourceCoverage>> {
    let header:Option<(i64,i64,i64)>=tx.query_row("SELECT revision,length(CAST(metadata_json AS BLOB)),(SELECT count(*) FROM json_each(metadata_json,'$.attempts')) FROM input_evidence_execution_current_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND source_message_id=?3",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,source],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let Some((revision, size, count)) = header else {
        return Ok(None);
    };
    check_budget(
        1 + usize::try_from(count)?,
        usize::try_from(size)?,
        limit,
        bytes,
    )?;
    ensure!(
        usize::try_from(size)? <= bytes.min(MAX_METADATA_BYTES),
        "execution current source metadata exceeds byte budget"
    );
    let (json,digest):(String,String)=tx.query_row("SELECT metadata_json,content_digest FROM input_evidence_execution_current_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND source_message_id=?3",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,source],|r|Ok((r.get(0)?,r.get(1)?)))?;
    decode_source(source, json, digest, u64::try_from(revision)?, limit, bytes).map(Some)
}
fn save_current_source_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    source: &ExecutionSourceCoverage,
) -> Result<()> {
    source.validate()?;
    let json = serde_json::to_string(source)?;
    tx.execute("INSERT INTO input_evidence_execution_current_sources(agent_id,identity_incarnation,source_message_id,revision,metadata_json,content_digest) VALUES(?1,?2,?3,?4,?5,?6)
        ON CONFLICT(agent_id,identity_incarnation,source_message_id) DO UPDATE SET revision=excluded.revision,metadata_json=excluded.metadata_json,content_digest=excluded.content_digest",
        params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,source.source_message_id,i64::try_from(source.revision)?,json,metadata_digest(source)?])?;
    let actual = current_source_on(
        tx,
        scope,
        &source.source_message_id,
        MAX_RECORDS,
        MAX_METADATA_BYTES,
    )?
    .context("saved execution source missing")?;
    ensure!(
        &actual == source,
        "execution source exact SQL readback mismatch"
    );
    Ok(())
}
fn source_on(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
    source: &str,
    limit: usize,
    bytes: usize,
) -> Result<Option<ExecutionSourceCoverage>> {
    match current_source_on(tx, scope, source, limit, bytes)? {
        Some(value) => Ok(Some(value)),
        None => seed_source_on(tx, seed, source, limit, bytes),
    }
}
fn assert_actual_source(
    tx: &Transaction<'_>,
    agent: &str,
    source: &ExecutionSourceCoverage,
    limit: usize,
) -> Result<()> {
    let raw = source_raw_metadata(tx, agent, &source.source_message_id, limit)?;
    let available = source.attempts.values().filter(|x| !x.pruned).count();
    ensure!(
        raw.len() == available,
        "execution source changed without qualified writer"
    );
    for (id, record) in raw {
        let observed = source
            .attempts
            .get(&id)
            .ok_or_else(|| source_error(&source.source_message_id))?;
        ensure!(
            !observed.pruned
                && observed.admitted_at == record.admitted_at
                && observed.state == record.state
                && observed.turn_id_hint == record.turn_id_hint
                && observed.terminal_outcome_id == record.terminal_outcome_id,
            "execution source projection changed without qualified writer"
        );
    }
    Ok(())
}
/// Bootstrap/preparation reads only frozen source indexes plus actual aliases.
/// Comparing scalar canonical metadata detects uncoordinated writers; it does
/// not add late rows to either metadata or body qualification.
pub(super) fn load_coverage_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    sources: &BTreeSet<String>,
    limit: usize,
    bytes: usize,
) -> Result<ExecutionInputCoverage> {
    let initialized = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM execution_protocol_partitions WHERE agent_id=?1)",
        [&scope.agent_id],
        |r| r.get(0),
    )?;
    let mut result = ExecutionInputCoverage {
        initialized,
        ..Default::default()
    };
    let mut count = 0usize;
    let mut metadata_bytes_used = 0usize;
    for source in sources {
        let Some(record) = source_on(
            tx,
            scope,
            binding.seed_id.as_deref(),
            source,
            limit.saturating_sub(count),
            bytes.saturating_sub(metadata_bytes_used),
        )?
        else {
            // An optional history Message need not have source coverage. A
            // planner lookup of that source remains typed NeedsPreparation.
            continue;
        };
        assert_actual_source(tx, &scope.agent_id, &record, limit)?;
        count = count.saturating_add(1 + record.attempts.len());
        metadata_bytes_used = metadata_bytes_used.saturating_add(metadata_bytes(&record)?);
        check_budget(count, metadata_bytes_used, limit, bytes)?;
        result.sources.insert(source.clone(), record);
    }
    for key in qualified_live_protocol_keys(tx, scope, binding, limit)? {
        match key.family {
            Family::Attempt => {
                let open:bool=tx.query_row("SELECT lifecycle_state='open' FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",params![scope.agent_id,key.id],|r|r.get(0))?;
                if open {
                    result.open_attempt_ids.insert(key.id);
                }
            }
            Family::WorkExecution => {
                result.nonterminal_work_item_ids.insert(key.id);
            }
            _ => unreachable!(),
        }
    }
    check_budget(
        result.metadata_count(),
        result.metadata_bytes()?,
        limit,
        bytes,
    )?;
    ensure!(
        result.open_attempt_ids.len() <= 1,
        "multiple qualified Open attempts"
    );
    Ok(result)
}
/// Called only for the original transaction's actual canonical write set. A
/// newly written Message may establish empty source coverage by SQL absence;
/// existing untracked source history is never adopted by this writer helper.
pub(super) fn register_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    records: &[CanonicalInputEvidence],
) -> Result<ExecutionInputDelta> {
    let mut delta = ExecutionInputDelta {
        work_items: execution_work::register_tx(tx, scope, binding, records)?,
        ..Default::default()
    };
    let mut messages = BTreeSet::new();
    let mut attempts = BTreeMap::<String, Vec<&ExecutionAttempt>>::new();
    for record in records {
        match record {
            CanonicalInputEvidence::Message(x) => {
                messages.insert(x.id.clone());
            }
            CanonicalInputEvidence::Attempt(x) => {
                delta.initialized = Some(true);
                delta
                    .open_attempts
                    .insert(x.attempt_id.clone(), x.state == ExecutionAttemptState::Open);
                if let Some(source) = &x.source_message_id {
                    attempts.entry(source.clone()).or_default().push(x);
                }
            }
            CanonicalInputEvidence::Outcome(_) => delta.initialized = Some(true),
            CanonicalInputEvidence::WorkExecution {
                work_item_id,
                record,
            } => {
                delta.initialized = Some(true);
                delta.nonterminal_work_items.insert(
                    work_item_id.clone(),
                    !matches!(record.state, WorkItemExecutionState::Terminal { .. }),
                );
            }
            _ => {}
        }
    }
    for source in messages
        .into_iter()
        .chain(attempts.keys().cloned())
        .collect::<BTreeSet<_>>()
    {
        let previous = source_on(
            tx,
            scope,
            binding.seed_id.as_deref(),
            &source,
            MAX_RECORDS,
            MAX_METADATA_BYTES,
        )?;
        let supplied = attempts.get(&source).cloned().unwrap_or_default();
        let mut next = previous.clone().unwrap_or(ExecutionSourceCoverage {
            source_message_id: source.clone(),
            revision: 0,
            allocation_highwater: 0,
            attempts: BTreeMap::new(),
        });
        if previous.is_none() {
            let raw = source_raw_metadata(tx, &scope.agent_id, &source, MAX_RECORDS)?;
            let supplied_ids = supplied
                .iter()
                .map(|x| x.attempt_id.as_str())
                .collect::<BTreeSet<_>>();
            if raw.keys().any(|id| !supplied_ids.contains(id.as_str())) {
                if supplied.is_empty() {
                    continue;
                } // untracked historical Message
                return Err(source_error(&source));
            }
        }
        for attempt in supplied {
            ensure!(
                attempt.agent_id == scope.agent_id,
                "execution metadata writer agent mismatch"
            );
            if let Some(old) = next.attempts.get(&attempt.attempt_id) {
                ensure!(!old.pruned, "cannot resurrect pruned execution attempt ID");
                next.attempts.insert(
                    attempt.attempt_id.clone(),
                    SourceAttemptObservation::from_attempt(attempt, old.allocation_ordinal),
                );
            } else {
                ensure!(
                    allocation_ordinal(&source, &attempt.attempt_id)
                        .is_none_or(|n| n > next.allocation_highwater),
                    "execution attempt ID reuses a reserved allocation ordinal"
                );
                let ordinal = allocation_ordinal(&source, &attempt.attempt_id)
                    .filter(|x| *x > next.allocation_highwater)
                    .unwrap_or(
                        next.allocation_highwater
                            .checked_add(1)
                            .context("execution ordinal overflow")?,
                    );
                ensure!(
                    !next
                        .attempts
                        .values()
                        .any(|x| x.allocation_ordinal == ordinal),
                    "execution ordinal reused"
                );
                next.allocation_highwater = ordinal;
                next.attempts.insert(
                    attempt.attempt_id.clone(),
                    SourceAttemptObservation::from_attempt(attempt, ordinal),
                );
            }
        }
        check_budget(
            1 + next.attempts.len(),
            metadata_bytes(&next)?,
            MAX_RECORDS,
            MAX_METADATA_BYTES,
        )?;
        assert_actual_source(tx, &scope.agent_id, &next, MAX_RECORDS)?;
        if previous.as_ref() != Some(&next) || next.revision == 0 {
            next.revision = next
                .revision
                .checked_add(1)
                .context("execution source revision overflow")?;
            save_current_source_tx(tx, scope, &next)?;
        }
        delta.sources.insert(source, next);
    }
    if delta.initialized == Some(true) {
        let initialized: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_protocol_partitions WHERE agent_id=?1)",
            [&scope.agent_id],
            |r| r.get(0),
        )?;
        ensure!(initialized, "execution record writer omitted its partition");
    }
    Ok(delta)
}
pub(super) fn prune_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    keys: &[Key],
) -> Result<ExecutionInputDelta> {
    let mut delta = ExecutionInputDelta {
        work_items: execution_work::prune_tx(tx, scope, binding, keys)?,
        ..Default::default()
    };
    let targets = keys
        .iter()
        .filter(|x| x.family == Family::Attempt)
        .map(|x| x.id.as_str())
        .collect::<BTreeSet<_>>();
    for key in keys {
        if key.family == Family::Attempt {
            delta.open_attempts.insert(key.id.clone(), false);
        }
        if key.family == Family::WorkExecution {
            delta.nonterminal_work_items.insert(key.id.clone(), false);
        }
    }
    if targets.is_empty() {
        return Ok(delta);
    }
    // Complete finite metadata, not all canonical history. Tombstones remain in
    // each source so count/ordinal cannot decrease after pruning a body.
    let mut sources = BTreeSet::new();
    for id in &targets {
        let mut stmt=tx.prepare("SELECT c.source_message_id FROM input_evidence_execution_current_sources c, json_each(c.metadata_json,'$.attempts') a
            WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND a.key=?4
            UNION SELECT s.source_message_id FROM input_evidence_execution_seed_sources s, json_each(s.metadata_json,'$.attempts') a
            WHERE s.seed_id=?3 AND a.key=?4 AND NOT EXISTS(SELECT 1 FROM input_evidence_execution_current_sources c
              WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.source_message_id=s.source_message_id)
            ORDER BY source_message_id LIMIT 2")?;
        let rows = stmt.query_map(
            params![
                scope.agent_id,
                i64::try_from(scope.identity_incarnation)?,
                binding.seed_id,
                id
            ],
            |r| r.get::<_, String>(0),
        )?;
        let found = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            found.len() <= 1,
            "execution attempt metadata belongs to multiple sources"
        );
        sources.extend(found);
    }
    let mut count = 0usize;
    let mut bytes = 0usize;
    for source in sources {
        let mut record = source_on(
            tx,
            scope,
            binding.seed_id.as_deref(),
            &source,
            MAX_RECORDS.saturating_sub(count),
            MAX_METADATA_BYTES.saturating_sub(bytes),
        )?
        .context("execution prune source disappeared")?;
        count = count.saturating_add(1 + record.attempts.len());
        bytes = bytes.saturating_add(metadata_bytes(&record)?);
        check_budget(count, bytes, MAX_RECORDS, MAX_METADATA_BYTES)?;
        let mut changed = false;
        for id in &targets {
            if let Some(observed) = record.attempts.get_mut(*id) {
                let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2)",params![scope.agent_id,id],|r|r.get(0))?;
                ensure!(!exists, "execution ordinal prune still has canonical row");
                changed |= !observed.pruned;
                observed.pruned = true;
            }
        }
        if changed {
            record.revision = record
                .revision
                .checked_add(1)
                .context("execution source revision overflow")?;
            save_current_source_tx(tx, scope, &record)?;
            delta.sources.insert(source, record);
        }
    }
    Ok(delta)
}

/// Only the fallback's actual inserted Message and validated immutable origin
/// can establish a new empty unbound source. Existing sources never use this.
pub(super) fn register_maintenance_created_source_tx(
    tx: &Transaction<'_>,
    seed: &str,
    agent: &str,
    source: &str,
) -> Result<()> {
    maintenance::fallback::validate_origin_tx(tx, seed, &Key::new(Family::Message, source))?;
    let original_agent: String = tx.query_row(
        "SELECT agent_id FROM episode_continuity_seeds WHERE seed_id=?1",
        [seed],
        |r| r.get(0),
    )?;
    ensure!(
        original_agent == agent,
        "fallback source differs from its fixed seed namespace"
    );
    ensure!(
        source_raw_metadata(tx, agent, source, MAX_RECORDS)?.is_empty(),
        "new fallback source already has canonical execution attempts"
    );
    ensure!(
        fixed_seed_source_on(tx, Some(seed), source, MAX_RECORDS, MAX_METADATA_BYTES)?.is_none(),
        "new fallback source already has fixed metadata"
    );
    let record = ExecutionSourceCoverage {
        source_message_id: source.into(),
        revision: 1,
        allocation_highwater: 0,
        attempts: BTreeMap::new(),
    };
    let json = serde_json::to_string(&record)?;
    let inserted=tx.execute("INSERT INTO input_evidence_execution_seed_source_recovery(seed_id,source_message_id,revision,metadata_json,content_digest) VALUES(?1,?2,1,?3,?4)",params![seed,source,json,metadata_digest(&record)?])?;
    ensure!(
        inserted == 1
            && seed_source_on(tx, Some(seed), source, MAX_RECORDS, MAX_METADATA_BYTES)?.as_ref()
                == Some(&record),
        "new fallback empty source exact readback differs"
    );
    Ok(())
}

pub(super) fn register_seed_recovery_tx(
    tx: &Transaction<'_>,
    seed: &str,
    agent: &str,
    records: &[CanonicalInputEvidence],
) -> Result<()> {
    let mut grouped = BTreeMap::<String, Vec<&ExecutionAttempt>>::new();
    for record in records {
        if let CanonicalInputEvidence::Attempt(attempt) = record {
            ensure!(
                attempt.agent_id == agent,
                "unbound execution metadata agent mismatch"
            );
            if let Some(source) = &attempt.source_message_id {
                grouped.entry(source.clone()).or_default().push(attempt);
            }
        }
    }
    for (source, attempts) in grouped {
        let mut coverage =
            seed_source_on(tx, Some(seed), &source, MAX_RECORDS, MAX_METADATA_BYTES)?
                .ok_or_else(|| source_error(&source))?;
        let previous = coverage.clone();
        for attempt in attempts {
            let observed = coverage
                .attempts
                .get_mut(&attempt.attempt_id)
                .context("unbound execution recovery attempt was not frozen")?;
            ensure!(
                !observed.pruned && observed.admitted_at == attempt.admitted_at,
                "unbound recovery changed attempt identity"
            );
            let eligible:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_eligibility WHERE seed_id=?1 AND family='attempt' AND record_id=?2) AND EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family='attempt' AND record_id=?2 AND origin_kind='fixed_member')",params![seed,attempt.attempt_id],|r|r.get(0))?;
            ensure!(
                eligible,
                "unbound source update has no exact fixed recovery member"
            );
            *observed =
                SourceAttemptObservation::from_attempt(attempt, observed.allocation_ordinal);
            legacy_recovery::validate_seed_overlay_on(
                tx,
                seed,
                &Key::new(Family::Attempt, &attempt.attempt_id),
            )?;
        }
        if coverage == previous {
            continue;
        }
        coverage.revision = coverage
            .revision
            .checked_add(1)
            .context("unbound execution source revision overflow")?;
        let json = serde_json::to_string(&coverage)?;
        tx.execute("INSERT INTO input_evidence_execution_seed_source_recovery(seed_id,source_message_id,revision,metadata_json,content_digest) VALUES(?1,?2,?3,?4,?5)
          ON CONFLICT(seed_id,source_message_id) DO UPDATE SET revision=excluded.revision,metadata_json=excluded.metadata_json,content_digest=excluded.content_digest",
          params![seed,source,i64::try_from(coverage.revision)?,json,metadata_digest(&coverage)?])?;
        assert_actual_source(tx, agent, &coverage, MAX_RECORDS)?;
        ensure!(
            seed_source_on(tx, Some(seed), &source, MAX_RECORDS, MAX_METADATA_BYTES)?.as_ref()
                == Some(&coverage),
            "unbound execution source exact SQL readback mismatch"
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "input_evidence_execution_tests.rs"]
mod tests;
