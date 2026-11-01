//! One final-release checkpoint for Work bodies and latest completion refs.
//! v86 consumes these exact members; it never scans a newer legacy tail.

use super::*;
use crate::types::{BriefContentSource, BriefKind, TranscriptEntry, TranscriptEntryKind};

#[path = "episode_snapshot_work_relations.rs"]
mod relations;

const NONOPEN_LIMIT: usize = 128;
const WORK_CHECKPOINT_POLICY: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::runtime_db) struct WorkLegacyWindow {
    pub(in crate::runtime_db) work_item_ids: Vec<String>,
    pub(in crate::runtime_db) is_exhaustive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::runtime_db) struct WorkResultCheckpoint {
    pub(in crate::runtime_db) brief_id: String,
    pub(in crate::runtime_db) transcript_entry_id: Option<String>,
    pub(in crate::runtime_db) source_message_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(in crate::runtime_db) enum CompletionSourceAvailability {
    Present,
    GlobalAbsent,
    HistoricalTableUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::runtime_db) struct CompletionSourceRef {
    pub(in crate::runtime_db) declared_id: String,
    pub(in crate::runtime_db) availability: CompletionSourceAvailability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::runtime_db) struct CompletionSourceObservation {
    pub(in crate::runtime_db) brief_id: String,
    pub(in crate::runtime_db) transcript: Option<CompletionSourceRef>,
    pub(in crate::runtime_db) incoming_message: Option<CompletionSourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::runtime_db) struct WorkContinuityCheckpoint {
    pub(in crate::runtime_db) policy_version: u32,
    /// False freezes an empty qualified namespace, not a global no-result proof.
    pub(in crate::runtime_db) result_table_observed: bool,
    /// None means this import phase does not extend the original optional tail.
    pub(in crate::runtime_db) nonopen: Option<WorkLegacyWindow>,
    /// Only Work/wait/frame owning closure, not D3 historical owner back-edges.
    pub(in crate::runtime_db) record_digests: BTreeMap<String, String>,
    pub(in crate::runtime_db) unavailable_refs: BTreeSet<String>,
    /// Missing entry is unprepared; Some(None) is the frozen no-result proof.
    pub(in crate::runtime_db) latest_results: BTreeMap<String, Option<WorkResultCheckpoint>>,
    /// Proof only for the optional source edges of these exact completion heads.
    pub(in crate::runtime_db) completion_sources: BTreeMap<String, CompletionSourceObservation>,
    /// Complete qualified Wait relation for only the original current Turn(s).
    pub(in crate::runtime_db) turn_wait_keys: BTreeMap<String, BTreeSet<String>>,
    /// Only original accepted/current TaskResult and trigger source IDs.
    pub(in crate::runtime_db) task_wait_keys: BTreeMap<String, BTreeSet<String>>,
    pub(in crate::runtime_db) trigger_wait_keys: BTreeMap<String, BTreeSet<String>>,
}

pub(in crate::runtime_db::episode_snapshot) fn capture(
    connection: &Connection,
    agent_id: &str,
    tables: Option<&BTreeSet<String>>,
    episode_roots: &EpisodeContinuityRoots,
    prior_payload_bytes: usize,
    include_nonopen: bool,
) -> Result<(WorkContinuityCheckpoint, BTreeMap<String, String>)> {
    let mut checkpoint = WorkContinuityCheckpoint {
        policy_version: WORK_CHECKPOINT_POLICY,
        result_table_observed: root_table_available(tables, "briefs"),
        nonopen: None,
        record_digests: BTreeMap::new(),
        unavailable_refs: BTreeSet::new(),
        latest_results: BTreeMap::new(),
        completion_sources: BTreeMap::new(),
        turn_wait_keys: BTreeMap::new(),
        task_wait_keys: BTreeMap::new(),
        trigger_wait_keys: BTreeMap::new(),
    };
    let mut inputs = RootCollector {
        payload_bytes: prior_payload_bytes,
        ..RootCollector::default()
    };
    // One aggregate body budget, deduplicated by canonical key on this read view.
    inputs.roots.record_digests = episode_roots.record_digests.clone();
    let mut pending = VecDeque::new();
    let mut selected = BTreeSet::new();
    let mut selected_bytes = 0usize;
    for key in episode_roots
        .record_digests
        .keys()
        .chain(episode_roots.unavailable_refs.iter())
    {
        if let Some((kind @ ("work_item" | "wait" | "continuation"), id)) = key.split_once(':') {
            enqueue(
                &mut pending,
                &mut selected,
                &mut selected_bytes,
                kind,
                id,
                0,
            )?;
        }
    }
    for id in &episode_roots.work_item_ids {
        enqueue(
            &mut pending,
            &mut selected,
            &mut selected_bytes,
            "work_item",
            id,
            0,
        )?;
    }
    relations::capture_relations(
        connection,
        agent_id,
        tables,
        &mut inputs,
        &mut checkpoint,
        &mut pending,
        &mut selected,
        &mut selected_bytes,
    )?;
    if include_nonopen {
        let mut ids = Vec::new();
        if root_table_available(tables, "work_items") {
            let mut query = connection.prepare(
                "SELECT work_item_id FROM work_items WHERE agent_id=?1 AND state!='open'
                 ORDER BY updated_at DESC,created_at DESC,work_item_id ASC LIMIT ?2",
            )?;
            for row in query.query_map(
                params![agent_id, i64::try_from(NONOPEN_LIMIT + 1)?],
                |row| row.get::<_, String>(0),
            )? {
                ids.push(row?);
            }
        }
        let is_exhaustive = ids.len() <= NONOPEN_LIMIT;
        ids.truncate(NONOPEN_LIMIT);
        for id in &ids {
            enqueue(
                &mut pending,
                &mut selected,
                &mut selected_bytes,
                "work_item",
                id,
                0,
            )?;
        }
        checkpoint.nonopen = Some(WorkLegacyWindow {
            work_item_ids: ids,
            is_exhaustive,
        });
    }
    while let Some((kind, id, depth)) = pending.pop_front() {
        inputs.current_depth = depth.saturating_add(1);
        let (table, id_column) = match kind.as_str() {
            "work_item" => ("work_items", "work_item_id"),
            "wait" => ("wait_conditions", "wait_condition_id"),
            "continuation" => ("work_item_continuations", "continuation_id"),
            _ => unreachable!("Work checkpoint only follows owning Work keys"),
        };
        let key = format!("{kind}:{id}");
        if !root_table_available(tables, table) {
            checkpoint.unavailable_refs.insert(key);
            continue;
        }
        let row: Option<(String, String)> = connection
            .query_row(
                &format!("SELECT agent_id,payload_json FROM {table} WHERE {id_column}=?1"),
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((stored_agent, payload)) = row else {
            checkpoint.unavailable_refs.insert(key);
            continue;
        };
        ensure!(
            stored_agent == agent_id,
            "Work checkpoint ID belongs to another agent"
        );
        inputs.digest(&kind, &id, &payload)?;
        checkpoint
            .record_digests
            .insert(key, bytes_digest(payload.as_bytes()));
        match kind.as_str() {
            "work_item" => {
                let work: WorkItemRecord = serde_json::from_str(&payload)?;
                ensure!(
                    work.agent_id == agent_id && work.id == id,
                    "Work checkpoint projection mismatch"
                );
                if let Some(wait) = work.blocked_by_wait_id.as_deref() {
                    enqueue(
                        &mut pending,
                        &mut selected,
                        &mut selected_bytes,
                        "wait",
                        wait,
                        depth + 1,
                    )?;
                }
                inputs.work_record(&work);
                let result = latest_result(
                    connection,
                    agent_id,
                    &id,
                    tables,
                    &mut inputs,
                    &mut checkpoint.unavailable_refs,
                    &mut checkpoint.completion_sources,
                )?;
                checkpoint.latest_results.insert(id, result);
            }
            "wait" => {
                let wait: WaitConditionRecord = serde_json::from_str(&payload)?;
                ensure!(
                    wait.agent_id == agent_id && wait.id == id,
                    "Wait checkpoint projection mismatch"
                );
                if let Some(work) = wait.work_item_id.as_deref() {
                    enqueue(
                        &mut pending,
                        &mut selected,
                        &mut selected_bytes,
                        "work_item",
                        work,
                        depth + 1,
                    )?;
                }
                inputs.wait_record(&wait);
            }
            "continuation" => {
                let frame: WorkItemContinuationFrame = serde_json::from_str(&payload)?;
                ensure!(
                    frame.agent_id == agent_id && frame.id == id,
                    "Frame checkpoint projection mismatch"
                );
                for work in [&frame.suspended_work_item_id, &frame.active_work_item_id] {
                    enqueue(
                        &mut pending,
                        &mut selected,
                        &mut selected_bytes,
                        "work_item",
                        work,
                        depth + 1,
                    )?;
                }
                inputs.optional_ref("turn", frame.turn_id.as_deref());
            }
            _ => unreachable!(),
        }
    }
    // Same bounded canonical edges as the original collector. Only its exact
    // digests are merged; its Episode owner/ref sets are deliberately discarded.
    expand_episode_root_refs(connection, agent_id, &mut inputs, tables)?;
    let aggregate_keys = inputs
        .roots
        .record_digests
        .keys()
        .chain(inputs.roots.unavailable_refs.iter())
        .chain(episode_roots.unavailable_refs.iter())
        .chain(checkpoint.unavailable_refs.iter())
        .collect::<BTreeSet<_>>();
    if aggregate_keys.len() > CONTINUITY_MAX_RECORDS {
        return Err(EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "Work checkpoint aggregate count",
        }
        .into());
    }
    if inputs
        .payload_bytes
        .saturating_add(serde_json::to_vec(&checkpoint)?.len())
        > CONTINUITY_MAX_PAYLOAD_BYTES
    {
        return Err(EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "Work checkpoint aggregate bytes",
        }
        .into());
    }
    Ok((checkpoint, inputs.roots.record_digests))
}

fn enqueue(
    pending: &mut VecDeque<(String, String, usize)>,
    selected: &mut BTreeSet<(String, String)>,
    bytes: &mut usize,
    kind: &str,
    id: &str,
    depth: usize,
) -> Result<()> {
    ensure!(!id.is_empty(), "Work checkpoint owning ID is empty");
    let key = (kind.to_owned(), id.to_owned());
    if selected.contains(&key) {
        return Ok(());
    }
    *bytes = bytes.saturating_add(kind.len()).saturating_add(id.len());
    if depth > CONTINUITY_MAX_REF_DEPTH
        || selected.len() >= CONTINUITY_MAX_RECORDS
        || *bytes > CONTINUITY_MAX_PAYLOAD_BYTES
    {
        return Err(EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "Work checkpoint metadata/depth",
        }
        .into());
    }
    selected.insert(key.clone());
    pending.push_back((key.0, key.1, depth));
    Ok(())
}

fn latest_result(
    connection: &Connection,
    agent_id: &str,
    work_item_id: &str,
    tables: Option<&BTreeSet<String>>,
    collector: &mut RootCollector,
    unavailable: &mut BTreeSet<String>,
    observations: &mut BTreeMap<String, CompletionSourceObservation>,
) -> Result<Option<WorkResultCheckpoint>> {
    if !root_table_available(tables, "briefs") {
        // A missing historical table freezes an empty fixed namespace. The
        // ordinary loader remains strict after migration and may not adopt later.
        return Ok(None);
    }
    let row: Option<(String, String)> =
        connection.query_row(
            "SELECT evidence_id,payload_json FROM briefs WHERE agent_id=?1 AND work_item_id=?2 AND kind='result'
             AND length(json_extract(payload_json,'$.text'))>0
             ORDER BY created_at DESC,evidence_id DESC LIMIT 1",
            params![agent_id, work_item_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?
    ;
    let Some((brief_id, payload)) = row else {
        return Ok(None);
    };
    let brief: BriefRecord = serde_json::from_str(&payload)?;
    ensure!(
        brief.agent_id == agent_id
            && brief.id == brief_id
            && brief.work_item_id.as_deref() == Some(work_item_id)
            && brief.kind == BriefKind::Result
            && !brief.text.is_empty(),
        "Work latest Result projection mismatch"
    );
    collector.digest("brief", &brief.id, &payload)?;
    collector.reference("brief", &brief.id);
    let mut observation = CompletionSourceObservation {
        brief_id: brief.id.clone(),
        transcript: None,
        incoming_message: None,
    };
    let mut result = WorkResultCheckpoint {
        brief_id: brief.id,
        transcript_entry_id: None,
        source_message_id: None,
    };
    if let BriefContentSource::TranscriptEntry { entry_id, .. } = brief.content_source {
        let row: Option<(String, String)> = if root_table_available(tables, "transcript_entries") {
            connection
                .query_row(
                    "SELECT agent_id,payload_json FROM transcript_entries WHERE evidence_id=?1",
                    [&entry_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
        } else {
            None
        };
        if let Some((stored_agent, payload)) = row {
            let entry: TranscriptEntry = serde_json::from_str(&payload)?;
            ensure!(
                stored_agent == agent_id && entry.agent_id == agent_id && entry.id == entry_id,
                "Work latest Result Transcript scope/key mismatch"
            );
            collector.digest("transcript", &entry_id, &payload)?;
            observation.transcript = Some(CompletionSourceRef {
                declared_id: entry_id.clone(),
                availability: CompletionSourceAvailability::Present,
            });
            result.transcript_entry_id = Some(entry_id);
            if entry.kind == TranscriptEntryKind::IncomingMessage {
                if let Some(message_id) = entry.related_message_id.as_deref() {
                    let source: Option<(String, String)> =
                        if root_table_available(tables, "messages") {
                            connection.query_row(
                            "SELECT agent_id,payload_json FROM messages WHERE evidence_id=?1",
                            [message_id], |row| Ok((row.get(0)?, row.get(1)?)),
                        ).optional()?
                        } else {
                            None
                        };
                    if let Some((stored_agent, payload)) = source {
                        let message: MessageEnvelope = serde_json::from_str(&payload)?;
                        ensure!(
                            stored_agent == agent_id
                                && message.agent_id == agent_id
                                && message.id == message_id,
                            "Work latest Result Message scope/key mismatch"
                        );
                        collector.digest("message", message_id, &payload)?;
                        collector.reference("message", message_id);
                        result.source_message_id = Some(message_id.to_owned());
                        observation.incoming_message = Some(CompletionSourceRef {
                            declared_id: message_id.to_owned(),
                            availability: CompletionSourceAvailability::Present,
                        });
                    } else {
                        unavailable.insert(format!("message:{message_id}"));
                        observation.incoming_message = Some(CompletionSourceRef {
                            declared_id: message_id.to_owned(),
                            availability: absent_source(
                                connection, tables, "messages", "message", message_id,
                            )?,
                        });
                    }
                }
            }
        } else {
            unavailable.insert(format!("transcript:{entry_id}"));
            observation.transcript = Some(CompletionSourceRef {
                declared_id: entry_id.clone(),
                availability: absent_source(
                    connection,
                    tables,
                    "transcript_entries",
                    "transcript",
                    &entry_id,
                )?,
            });
        }
    }
    observations.insert(result.brief_id.clone(), observation);
    Ok(Some(result))
}

fn absent_source(
    connection: &Connection,
    tables: Option<&BTreeSet<String>>,
    canonical_table: &str,
    family: &str,
    id: &str,
) -> Result<CompletionSourceAvailability> {
    for table in [
        "input_evidence_current",
        "input_evidence_eligibility",
        "input_evidence_seed_recovery_current",
    ] {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if exists {
            let known: bool = connection.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE family=?1 AND record_id=?2)"),
                params![family, id],
                |row| row.get(0),
            )?;
            ensure!(
                !known,
                "Work completion source is qualified or pruned but unavailable"
            );
        }
    }
    Ok(if root_table_available(tables, canonical_table) {
        CompletionSourceAvailability::GlobalAbsent
    } else {
        CompletionSourceAvailability::HistoricalTableUnavailable
    })
}

#[derive(Debug, thiserror::Error)]
pub(in crate::runtime_db) enum WorkCheckpointReadError {
    #[error("fixed Work checkpoint is not prepared for {seed_id}/{phase}")]
    Missing { seed_id: String, phase: String },
}

/// The caller already proved the exact immutable seed/binding. This helper
/// reads only that original phase and never discovers a newer canonical head.
pub(in crate::runtime_db) fn load(
    tx: &Transaction<'_>,
    seed_id: &str,
    phase: &str,
) -> Result<WorkContinuityCheckpoint> {
    ensure!(
        matches!(phase, "upgrade" | "legacy_import" | "first_import"),
        "invalid Work checkpoint phase"
    );
    let missing = || WorkCheckpointReadError::Missing {
        seed_id: seed_id.to_owned(),
        phase: phase.to_owned(),
    };
    let (table, joined, filter) = if phase == "first_import" {
        (
            "episode_continuity_imports i",
            "JOIN episode_continuity_seeds s USING(seed_id)",
            "i.seed_id=?1",
        )
    } else {
        ("episode_continuity_seeds s", "", "s.seed_id=?1")
    };
    let prefix = if phase == "first_import" { "i" } else { "s" };
    let metadata: Option<(usize, String)> = tx.query_row(
        &format!("SELECT length(CAST({prefix}.roots_json AS BLOB)),s.source_kind FROM {table} {joined} WHERE {filter}"),
        [seed_id], |row|Ok((row.get(0)?,row.get(1)?)),
    ).optional()?;
    let (bytes, source_kind) = metadata.ok_or_else(missing)?;
    ensure!(
        phase == "first_import" || (phase == "upgrade") == source_kind.starts_with("upgrade_"),
        "Work checkpoint source/phase mismatch"
    );
    if bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
        return Err(EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "Work checkpoint metadata bytes",
        }
        .into());
    }
    let payload: String = tx.query_row(
        &format!("SELECT {prefix}.roots_json FROM {table} {joined} WHERE {filter}"),
        [seed_id],
        |row| row.get(0),
    )?;
    let roots: EpisodeContinuityRoots = serde_json::from_str(&payload)?;
    let checkpoint = roots.work_domain_checkpoint.ok_or_else(missing)?;
    ensure!(
        checkpoint.policy_version == WORK_CHECKPOINT_POLICY,
        "Work checkpoint policy mismatch"
    );
    ensure!(
        (phase == "first_import") == checkpoint.nonopen.is_none(),
        "Work checkpoint optional-history phase mismatch"
    );
    validate_completion_sources(&checkpoint, &roots.record_digests)?;
    Ok(checkpoint)
}

fn validate_completion_sources(
    checkpoint: &WorkContinuityCheckpoint,
    input_digests: &BTreeMap<String, String>,
) -> Result<()> {
    let heads = checkpoint
        .latest_results
        .values()
        .flatten()
        .map(|head| head.brief_id.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        heads.len() == checkpoint.completion_sources.len()
            && checkpoint
                .completion_sources
                .keys()
                .all(|id| heads.contains(id.as_str())),
        "Work completion source observations do not match exact heads"
    );
    for head in checkpoint.latest_results.values().flatten() {
        ensure!(
            input_digests.contains_key(&format!("brief:{}", head.brief_id)),
            "Work completion head has no fixed input qualification"
        );
        let observation = checkpoint
            .completion_sources
            .get(&head.brief_id)
            .context("Work completion source observation is missing")?;
        ensure!(
            observation.brief_id == head.brief_id,
            "Work completion source observation head mismatch"
        );
        for (family, actual, declared) in [
            (
                "transcript",
                head.transcript_entry_id.as_deref(),
                observation.transcript.as_ref(),
            ),
            (
                "message",
                head.source_message_id.as_deref(),
                observation.incoming_message.as_ref(),
            ),
        ] {
            match (actual, declared) {
                (Some(actual), Some(declared)) => ensure!(
                    actual == declared.declared_id
                        && declared.availability == CompletionSourceAvailability::Present
                        && input_digests.contains_key(&format!("{family}:{actual}")),
                    "Work completion available source has no exact fixed proof"
                ),
                (None, Some(declared)) => ensure!(
                    !declared.declared_id.is_empty()
                        && declared.availability != CompletionSourceAvailability::Present
                        && checkpoint
                            .unavailable_refs
                            .contains(&format!("{family}:{}", declared.declared_id)),
                    "Work completion absent source has no exact fixed observation"
                ),
                (None, None) => {}
                (Some(_), None) => {
                    anyhow::bail!("Work completion available source has no declaration proof")
                }
            }
        }
        ensure!(
            observation.incoming_message.is_none()
                || observation.transcript.as_ref().is_some_and(
                    |source| source.availability == CompletionSourceAvailability::Present
                ),
            "Work completion Message source has no available incoming Transcript"
        );
    }
    Ok(())
}
