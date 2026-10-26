//! Focused execution Work presence for original live/current sources only.
//! An absent observation never qualifies a body; a present observation requires
//! the existing same-scope body membership at materialization.
use super::*;
use crate::runtime::resident_input_evidence::{ExecutionWorkObservation, ExecutionWorkPresence};

fn decode(presence: &str) -> Result<ExecutionWorkPresence> {
    Ok(match presence {
        "present" => ExecutionWorkPresence::Present,
        "absent" => ExecutionWorkPresence::Absent,
        "pruned" => ExecutionWorkPresence::Pruned,
        _ => anyhow::bail!("invalid execution Work presence"),
    })
}
fn encode(presence: ExecutionWorkPresence) -> &'static str {
    match presence {
        ExecutionWorkPresence::Present => "present",
        ExecutionWorkPresence::Absent => "absent",
        ExecutionWorkPresence::Pruned => "pruned",
    }
}
fn actual_exists(tx: &Transaction<'_>, agent: &str, id: &str) -> Result<bool> {
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_protocol_work_items WHERE agent_id=?1 AND work_item_id=?2)",params![agent,id],|r|r.get(0))?)
}
fn current(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    id: &str,
) -> Result<Option<ExecutionWorkObservation>> {
    tx.query_row("SELECT revision,presence FROM input_evidence_execution_current_work WHERE agent_id=?1 AND identity_incarnation=?2 AND work_item_id=?3",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).optional()?.map(|(revision,presence)|Ok(ExecutionWorkObservation{revision:u64::try_from(revision)?,presence:decode(&presence)?})).transpose()
}
fn observed(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    id: &str,
) -> Result<Option<ExecutionWorkObservation>> {
    if let Some(value) = current(tx, scope, id)? {
        return Ok(Some(value));
    }
    let Some(seed) = binding.seed_id.as_deref() else {
        return Ok(None);
    };
    tx.query_row("SELECT presence FROM input_evidence_execution_seed_work WHERE seed_id=?1 AND work_item_id=?2",params![seed,id],|r|r.get::<_,String>(0)).optional()?.map(|presence|Ok(ExecutionWorkObservation{revision:0,presence:decode(&presence)?})).transpose()
}
fn save(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    id: &str,
    value: &ExecutionWorkObservation,
) -> Result<()> {
    tx.execute("INSERT INTO input_evidence_execution_current_work(agent_id,identity_incarnation,work_item_id,revision,presence) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(agent_id,identity_incarnation,work_item_id) DO UPDATE SET revision=excluded.revision,presence=excluded.presence",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,id,i64::try_from(value.revision)?,encode(value.presence)])?;
    ensure!(
        current(tx, scope, id)?.as_ref() == Some(value),
        "execution Work observation exact readback mismatch"
    );
    Ok(())
}
fn message_work_ids(
    tx: &Transaction<'_>,
    agent: &str,
    messages: impl Iterator<Item = String>,
    ids: &mut BTreeSet<String>,
) -> Result<()> {
    for id in messages {
        let work:Option<String>=tx.query_row("SELECT json_extract(payload_json,'$.work_item_id') FROM messages WHERE agent_id=?1 AND message_id=?2",params![agent,id],|r|r.get(0)).optional()?.flatten();
        if let Some(work) = work {
            ensure!(
                work.len() <= 4096,
                "execution Work ID exceeds metadata bound"
            );
            ids.insert(work);
        }
        ensure!(
            ids.len() <= MAX_RECORDS,
            "execution Work metadata count exceeded"
        );
    }
    Ok(())
}
/// Fixed capture only: roots_work are the original live Work IDs in the sealed
/// seed roots, rather than a new scan of historical WorkExecution rows.
pub(super) fn capture_seed_tx(
    tx: &Transaction<'_>,
    seed: &str,
    phase: &str,
    agent: &str,
    mandatory: &BTreeSet<Key>,
    roots_work: &BTreeSet<String>,
) -> Result<(BTreeSet<Key>, String, usize, usize)> {
    let mut ids = roots_work.clone();
    ids.extend(
        mandatory
            .iter()
            .filter(|x| x.family == Family::WorkExecution)
            .map(|x| x.id.clone()),
    );
    message_work_ids(
        tx,
        agent,
        mandatory
            .iter()
            .filter(|x| x.family == Family::Message)
            .map(|x| x.id.clone()),
        &mut ids,
    )?;
    let mut positive = BTreeSet::new();
    let mut manifest = Vec::new();
    let mut bytes = 0usize;
    for id in ids {
        ensure!(id.len() <= 4096, "execution Work ID exceeds metadata bound");
        let prior:Option<String>=tx.query_row("SELECT presence FROM input_evidence_execution_seed_work WHERE seed_id=?1 AND work_item_id=?2",params![seed,id],|r|r.get(0)).optional()?;
        let presence = match prior {
            Some(p) => decode(&p)?,
            None => {
                let p = if actual_exists(tx, agent, &id)? {
                    ExecutionWorkPresence::Present
                } else {
                    ExecutionWorkPresence::Absent
                };
                tx.execute("INSERT INTO input_evidence_execution_seed_work(seed_id,work_item_id,capture_phase,presence) VALUES(?1,?2,?3,?4)",params![seed,id,phase,encode(p)])?;
                p
            }
        };
        let stored: String = tx.query_row("SELECT presence FROM input_evidence_execution_seed_work WHERE seed_id=?1 AND work_item_id=?2", params![seed,id], |r| r.get(0))?;
        ensure!(
            decode(&stored)? == presence,
            "fixed Work observation exact readback mismatch"
        );
        if presence == ExecutionWorkPresence::Present {
            positive.insert(Key::new(Family::WorkExecution, &id));
        }
        bytes = bytes.saturating_add(id.len() + 128);
        ensure!(
            bytes <= MAX_METADATA_BYTES,
            "execution Work metadata bytes exceeded"
        );
        manifest.push((id, encode(presence)));
    }
    let (stored_count, stored_bytes): (i64, i64) = tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(work_item_id AS BLOB))+128),0) FROM input_evidence_execution_seed_work WHERE seed_id=?1", [seed], |r| Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        usize::try_from(stored_count)? <= MAX_RECORDS
            && usize::try_from(stored_bytes)? <= MAX_METADATA_BYTES,
        "fixed execution Work metadata budget exceeded"
    );
    Ok((
        positive,
        bytes_digest(&serde_json::to_vec(&manifest)?),
        usize::try_from(stored_count)?,
        usize::try_from(stored_bytes)?,
    ))
}
pub(super) fn load_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    mandatory: &BTreeSet<Key>,
    sources: &BTreeSet<String>,
) -> Result<BTreeMap<String, ExecutionWorkObservation>> {
    let mut ids = mandatory
        .iter()
        .filter(|x| x.family == Family::WorkExecution)
        .map(|x| x.id.clone())
        .collect::<BTreeSet<_>>();
    message_work_ids(tx, &scope.agent_id, sources.iter().cloned(), &mut ids)?;
    let mut result = BTreeMap::new();
    for id in ids {
        let Some(value) = observed(tx, scope, binding, &id)? else {
            continue;
        };
        let exists = actual_exists(tx, &scope.agent_id, &id)?;
        ensure!(
            exists == (value.presence == ExecutionWorkPresence::Present),
            "execution Work changed without qualified writer"
        );
        if value.presence == ExecutionWorkPresence::Present {
            ensure!(
                is_existing_member_on(tx, scope, binding, &Key::new(Family::WorkExecution, &id))?,
                "present execution Work has no original body qualification"
            );
        }
        result.insert(id, value);
    }
    Ok(result)
}
pub(super) fn register_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    records: &[CanonicalInputEvidence],
) -> Result<BTreeMap<String, ExecutionWorkObservation>> {
    let mut requested = BTreeMap::new();
    for record in records {
        match record {
            CanonicalInputEvidence::WorkExecution { work_item_id, .. } => {
                requested.insert(work_item_id.clone(), ExecutionWorkPresence::Present);
            }
            CanonicalInputEvidence::Message(message) => {
                if let Some(id) = &message.work_item_id {
                    if observed(tx, scope, binding, id)?.is_none() {
                        let exists = actual_exists(tx, &scope.agent_id, id)?;
                        if !exists {
                            requested
                                .entry(id.clone())
                                .or_insert(ExecutionWorkPresence::Absent);
                        } else if is_existing_member_on(
                            tx,
                            scope,
                            binding,
                            &Key::new(Family::WorkExecution, id),
                        )? {
                            requested
                                .entry(id.clone())
                                .or_insert(ExecutionWorkPresence::Present);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut result = BTreeMap::new();
    for (id, presence) in requested {
        let old = observed(tx, scope, binding, &id)?;
        ensure!(
            !old.as_ref()
                .is_some_and(|x| x.presence == ExecutionWorkPresence::Pruned),
            "cannot resurrect pruned execution Work"
        );
        let value = if old
            .as_ref()
            .is_some_and(|x| x.presence == presence && x.revision > 0)
        {
            old.unwrap()
        } else {
            ExecutionWorkObservation {
                revision: old.map_or(Ok(1), |x| {
                    x.revision
                        .checked_add(1)
                        .context("execution Work revision overflow")
                })?,
                presence,
            }
        };
        save(tx, scope, &id, &value)?;
        result.insert(id, value);
    }
    Ok(result)
}
pub(super) fn prune_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    keys: &[Key],
) -> Result<BTreeMap<String, ExecutionWorkObservation>> {
    let mut result = BTreeMap::new();
    for key in keys.iter().filter(|x| x.family == Family::WorkExecution) {
        ensure!(
            !actual_exists(tx, &scope.agent_id, &key.id)?,
            "execution Work prune before canonical deletion"
        );
        let old = observed(tx, scope, binding, &key.id)?
            .context("execution Work prune missing original observation")?;
        ensure!(
            old.presence != ExecutionWorkPresence::Absent,
            "never registered Work cannot be pruned"
        );
        let value = ExecutionWorkObservation {
            revision: old
                .revision
                .checked_add(1)
                .context("execution Work revision overflow")?,
            presence: ExecutionWorkPresence::Pruned,
        };
        save(tx, scope, &key.id, &value)?;
        result.insert(key.id.clone(), value);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::execution_protocol::{WorkItemExecutionRecord, WorkItemExecutionState};
    fn connection() -> Result<rusqlite::Connection> {
        let db = rusqlite::Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE episode_continuity_seeds(seed_id TEXT PRIMARY KEY); INSERT INTO episode_continuity_seeds VALUES('seed'); CREATE TABLE execution_protocol_attempts(agent_id TEXT,attempt_id TEXT,lifecycle_state TEXT,terminal_outcome_id TEXT,payload_json TEXT); CREATE TABLE execution_protocol_work_items(agent_id TEXT,work_item_id TEXT,payload_json TEXT,PRIMARY KEY(agent_id,work_item_id)); CREATE TABLE messages(agent_id TEXT,message_id TEXT,payload_json TEXT);")?;
        db.execute_batch(INPUT_EVIDENCE_SCHEMA)?;
        Ok(db)
    }
    fn scope(inc: u64) -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "agent".into(),
            identity_incarnation: inc,
        }
    }
    fn terminal() -> WorkItemExecutionRecord {
        WorkItemExecutionRecord {
            source_revision: 1,
            state: WorkItemExecutionState::Terminal {
                generation: 1,
                completion: "done".into(),
            },
        }
    }
    fn store(tx: &Transaction<'_>, record: &WorkItemExecutionRecord) -> Result<()> {
        tx.execute(
            "INSERT INTO execution_protocol_work_items VALUES('agent','work',?1)",
            [serde_json::to_string(record)?],
        )?;
        Ok(())
    }
    #[test]
    fn fixed_absence_does_not_adopt_a_late_terminal_work_row() -> Result<()> {
        let mut db = connection()?;
        let tx = db.transaction()?;
        let binding = Binding {
            seed_id: Some("seed".into()),
        };
        let roots = BTreeSet::from([Key::new(Family::WorkExecution, "work")]);
        let (positive, ..) =
            capture_seed_tx(&tx, "seed", "upgrade", "agent", &roots, &BTreeSet::new())?;
        assert!(positive.is_empty());
        let absent = load_tx(&tx, &scope(1), &binding, &roots, &BTreeSet::new())?;
        assert_eq!(absent["work"].presence, ExecutionWorkPresence::Absent);
        let record = terminal();
        store(&tx, &record)?;
        assert!(load_tx(&tx, &scope(1), &binding, &roots, &BTreeSet::new()).is_err());
        let actual = register_tx(
            &tx,
            &scope(1),
            &binding,
            &[CanonicalInputEvidence::WorkExecution {
                work_item_id: "work".into(),
                record,
            }],
        )?;
        assert_eq!(actual["work"].presence, ExecutionWorkPresence::Present);
        assert!(observed(&tx, &scope(2), &Binding { seed_id: None }, "work")?.is_none());
        Ok(())
    }
    #[test]
    fn pruning_registered_work_is_not_first_registration_permission() -> Result<()> {
        let mut db = connection()?;
        let tx = db.transaction()?;
        let record = terminal();
        store(&tx, &record)?;
        let binding = Binding { seed_id: None };
        let facts = [CanonicalInputEvidence::WorkExecution {
            work_item_id: "work".into(),
            record: record.clone(),
        }];
        register_tx(&tx, &scope(1), &binding, &facts)?;
        tx.execute(
            "DELETE FROM execution_protocol_work_items WHERE work_item_id='work'",
            [],
        )?;
        let delta = prune_tx(
            &tx,
            &scope(1),
            &binding,
            &[Key::new(Family::WorkExecution, "work")],
        )?;
        assert_eq!(delta["work"].presence, ExecutionWorkPresence::Pruned);
        store(&tx, &record)?;
        assert!(register_tx(&tx, &scope(1), &binding, &facts).is_err());
        Ok(())
    }
    #[test]
    fn canonical_work_and_presence_rollback_together() -> Result<()> {
        let mut db = connection()?;
        {
            let tx = db.transaction()?;
            let record = terminal();
            store(&tx, &record)?;
            register_tx(
                &tx,
                &scope(1),
                &Binding { seed_id: None },
                &[CanonicalInputEvidence::WorkExecution {
                    work_item_id: "work".into(),
                    record,
                }],
            )?;
        }
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM execution_protocol_work_items",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM input_evidence_execution_current_work",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    }
}
