//! Metadata for an explicit Task output recall, never ordinary Input admission.
use super::*;
use crate::runtime_db::episode_snapshot::task_checkpoint::queries::FixedOutputInputCapture;

/// The opaque facet is minted only by the original fixed Task query actor.
/// Rows stream to the durable inventory; no all-history resident map exists.
pub(in crate::runtime_db) fn capture_fixed_task_output_inputs_on(
    connection: &rusqlite::Connection,
    authority: &FixedOutputInputCapture<'_>,
) -> Result<()> {
    authority.validate(connection)?;
    if let Some(original) = authority.original() {
        connection.execute("INSERT INTO task_query_fixed_input_members(inventory_id,family,record_id,related_turn_id,fingerprint_json) SELECT ?1,family,record_id,related_turn_id,fingerprint_json FROM task_query_fixed_input_members WHERE inventory_id=?2",params![authority.inventory(),original.id])?;
    }
    if let Some(original) = authority.original() {
        connection.execute("INSERT INTO task_output_fixed_input_prunes(inventory_id,family,record_id,fingerprint_json,content_digest) SELECT ?1,family,record_id,fingerprint_json,content_digest FROM task_output_fixed_input_prunes WHERE inventory_id=?2",params![authority.inventory(),original.id])?;
    }
    if !authority.upgrade() {
        return Ok(());
    }
    for family in [
        Family::Turn,
        Family::ToolExecution,
        Family::Brief,
        Family::Transcript,
        Family::Attempt,
        Family::Outcome,
        Family::WorkExecution,
    ] {
        let (table, id) = table(family);
        if !authority.table_available(table) {
            continue;
        }
        let mut query = connection.prepare(&format!(
            "SELECT length(CAST({id} AS BLOB)),{id} FROM {table} WHERE agent_id=?1 ORDER BY {id}"
        ))?;
        let mut rows = query.query([authority.agent()])?;
        while let Some(row) = rows.next()? {
            fixed_material(
                row.get::<_, usize>(0)?
                    .checked_add(256)
                    .context("focused Input key bytes overflow")?,
            )?;
            let key = Key::new(family, row.get::<_, String>(1)?);
            let (raw, related_turn) =
                freeze_input_fingerprint(connection, authority.agent(), &key)?;
            connection.execute("INSERT INTO task_query_fixed_input_members(inventory_id,family,record_id,related_turn_id,fingerprint_json) VALUES(?1,?2,?3,?4,?5)",params![authority.inventory(),family.as_str(),key.id,related_turn,raw])?;
        }
    }
    Ok(())
}
fn fixed_material(bytes: usize) -> Result<()> {
    ensure!(
        bytes
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .is_some_and(|n| n <= MAX_METADATA_BYTES),
        InputEvidenceSnapshotError::ResourceExhausted("fixed Task output Input material")
    );
    Ok(())
}
fn freeze_input_fingerprint(
    connection: &rusqlite::Connection,
    agent: &str,
    key: &Key,
) -> Result<(String, Option<String>)> {
    let (table, id) = table(key.family);
    let columns: &[&str] = match key.family {
        Family::Turn => &[
            "turn_id",
            "agent_id",
            "created_at",
            "owner_kind",
            "owner_id",
        ],
        Family::ToolExecution | Family::Brief => {
            &["evidence_id", "agent_id", "created_at", "content_hash"]
        }
        Family::Transcript => &[
            "evidence_id",
            "agent_id",
            "created_at",
            "content_hash",
            "turn_id",
        ],
        Family::Attempt => &[
            "attempt_id",
            "agent_id",
            "lifecycle_state",
            "terminal_outcome_id",
            "recovery_of_attempt_id",
        ],
        Family::Outcome => &["outcome_id", "agent_id", "attempt_id"],
        Family::WorkExecution => &["work_item_id", "agent_id", "lifecycle_state"],
        Family::Message => {
            unreachable!("Message fingerprints are already in the original fixed inventory")
        }
    };
    let strings = columns
        .iter()
        .map(|column| format!("COALESCE(length(CAST({column} AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let body = match key.family {
        Family::ToolExecution => "0",
        Family::Brief | Family::Transcript => {
            "CASE WHEN content_hash IS NULL THEN length(CAST(payload_json AS BLOB)) ELSE 0 END"
        }
        _ => "length(CAST(payload_json AS BLOB))",
    };
    let bytes: usize = connection.query_row(
        &format!("SELECT {strings}+{body} FROM {table} WHERE agent_id=?1 AND {id}=?2"),
        params![agent, key.id],
        |row| row.get(0),
    )?;
    fixed_material(bytes)?;
    let mut fingerprint =
        fingerprint_on(connection, agent, key)?.context("fixed Output Input disappeared")?;
    if requires_frozen_body_digest(key, &fingerprint) {
        let raw =
            payload_on(connection, agent, key)?.context("fixed Output Input body disappeared")?;
        fingerprint.frozen_payload_digest = Some(bytes_digest(raw.as_bytes()));
    }
    let encoded = serde_json::to_string(&fingerprint)?;
    fixed_material(encoded.len())?;
    Ok((encoded, fingerprint.related_turn_id))
}

use crate::runtime::input_evidence_budget::BudgetViewGuard;
use crate::runtime::resident_task::TaskFactLocator;
use crate::runtime_db::task_snapshot::{TaskInputReadAuthorization, TaskOutputReadAuthorization};

/// Only this read actor constructs a recall. Locators carry accepted read
/// versions; no corresponding generic D3 alias is created by this path.
pub(crate) struct TaskOutputInputRecall {
    domain: ResidentInputEvidence,
    task: TaskFactLocator,
    material: Option<TaskOutputRecallMaterial>,
}

/// One original read request covers SQL material and its overlapping resident
/// merge. This carrier owns only estimates/guards, never authority or records.
pub(crate) struct TaskOutputRecallMaterial {
    _views: Vec<Arc<BudgetViewGuard>>,
    ledger: crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
    byte_limit: usize,
    record_limit: usize,
}
impl TaskOutputRecallMaterial {
    pub(crate) fn admit_copy(
        &mut self,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        bytes: u64,
        records: usize,
    ) -> Result<()> {
        ensure!(
            self.ledger.same_ledger(ledger),
            "Output recall crossed its actual SQL ledger"
        );
        let bytes = self
            .bytes
            .checked_add(usize::try_from(bytes)?)
            .context("Output recall combined bytes overflow")?;
        let records = self
            .records
            .checked_add(records)
            .context("Output recall combined records overflow")?;
        ensure!(
            bytes <= self.byte_limit && records <= self.record_limit,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "Output SQL and recall merge combined material"
            )
        );
        self.bytes = bytes;
        self.records = records;
        Ok(())
    }
}
impl TaskOutputInputRecall {
    pub(in crate::runtime_db) fn with_material(
        mut self,
        views: Vec<Arc<BudgetViewGuard>>,
        ledger: crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        bytes: usize,
        records: usize,
        byte_limit: usize,
        record_limit: usize,
    ) -> Result<Self> {
        ensure!(
            !views.is_empty(),
            "Output recall lacks its actual SQL material guards"
        );
        self.material = Some(TaskOutputRecallMaterial {
            _views: views,
            ledger,
            bytes,
            records,
            byte_limit,
            record_limit,
        });
        Ok(self)
    }
    #[cfg(test)]
    pub(crate) fn admitted_material(&self) -> Result<(usize, usize)> {
        let material = self.material.as_ref().context("Output material missing")?;
        Ok((material.bytes, material.records))
    }
    pub(crate) fn into_parts(
        self,
    ) -> Result<(
        ResidentInputEvidence,
        TaskFactLocator,
        TaskOutputRecallMaterial,
    )> {
        Ok((
            self.domain,
            self.task,
            self.material
                .context("Output recall lacks actual material admission")?,
        ))
    }
}

/// Only central alias actual readback calls this hook. A v84-only DB has no
/// Task facet; after v88 a missing companion is corruption, never empty.
pub(super) fn register_current_input_relation_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
    revision: u64,
    related_turn: Option<&str>,
) -> Result<()> {
    let ready: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='task_output_current_input_relations')", [], |row| row.get(0))?;
    if !ready {
        let migrations: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')", [], |row|row.get(0))?;
        if migrations {
            let installed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=?1)",
                [super::super::task_snapshot::TASK_DOMAIN_VERSION],
                |row| row.get(0),
            )?;
            ensure!(
                !installed,
                "installed Task domain lost its Input relation companion"
            );
        }
        return Ok(());
    }
    tx.execute("INSERT INTO task_output_current_input_relations(agent_id,identity_incarnation,family,record_id,revision,related_turn_id) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,related_turn_id=excluded.related_turn_id",params![scope.agent_id,scope.identity_incarnation,key.family.as_str(),key.id,revision,related_turn])?;
    Ok(())
}

/// v88 can copy only already-qualified D4 metadata, never raw historical rows.
/// Streamed bounded fingerprints make this once-only migration independent of
/// the ordinary resident history window and the total history size.
pub(in crate::runtime_db) fn initialize_output_input_relations_tx(
    tx: &Transaction<'_>,
) -> Result<()> {
    let mut query = tx.prepare("SELECT length(CAST(agent_id AS BLOB))+length(CAST(family AS BLOB))+length(CAST(record_id AS BLOB))+length(CAST(fingerprint_json AS BLOB)),agent_id,identity_incarnation,family,record_id,revision,fingerprint_json FROM input_evidence_current ORDER BY agent_id,identity_incarnation,family,record_id")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        fixed_material(row.get(0)?)?;
        let scope = InputEvidenceScope {
            agent_id: row.get(1)?,
            identity_incarnation: row.get(2)?,
        };
        let key = Key::new(
            Family::from_str(&row.get::<_, String>(3)?)?,
            row.get::<_, String>(4)?,
        );
        let fingerprint: Fingerprint = serde_json::from_str(&row.get::<_, String>(6)?)?;
        register_current_input_relation_tx(
            tx,
            &scope,
            &key,
            row.get(5)?,
            fingerprint.related_turn_id.as_deref(),
        )?;
    }
    Ok(())
}

pub(in crate::runtime_db) fn prepare_authorized_task_output_inputs_tx(
    tx: &Transaction<'_>,
    authorization: &mut TaskOutputReadAuthorization<'_, '_, '_>,
    record_limit: usize,
    byte_limit: usize,
) -> Result<TaskOutputInputRecall> {
    authorization.read().validates_tx(tx)?;
    ensure!(
        record_limit > 0 && byte_limit > 0,
        "empty Task output recall budget"
    );
    let scope = authorization.scope().clone();
    let roots = authorization.roots().clone();
    let mut pending: VecDeque<_> = roots.iter().cloned().map(|key| (key, 0usize)).collect();
    let mut records = BTreeMap::<Key, Arc<QualifiedInputEvidence>>::new();
    let mut assignments = BTreeMap::new();
    let mut transcripts = BTreeMap::new();
    while let Some((key, depth)) = pending.pop_front() {
        if records.contains_key(&key) {
            continue;
        }
        ensure!(
            depth <= MAX_DEPTH && records.len() < record_limit.min(MAX_RECORDS),
            InputEvidenceSnapshotError::ResourceExhausted("Task output owning closure")
        );
        let (record, fingerprint) = read_authorized_task_input_with_fingerprint_tx(
            tx,
            authorization.read(),
            &key,
            false,
            false,
        )?
        .context("Output owning body disappeared")?;
        if let Some(source) = authorization.source().filter(|source| source.key == key) {
            ensure!(
                record.locator == *source,
                "Output Source locator differs from its original accepted version"
            );
        }
        let mut refs = record.references();
        if let PreparedInputEvidence::Message(message) = &record.record {
            if let Some(turn) = &fingerprint.related_turn_id {
                refs.insert(Key::new(Family::Turn, turn));
            }
            if let Some(work) = &message.work_item_id {
                let key = Key::new(Family::WorkExecution, work);
                if accepted_key_exists(tx, authorization.read(), &key)? {
                    refs.insert(key);
                }
            }
            assignments.insert(message.id.clone(), fingerprint.related_turn_id.clone());
        }
        if let PreparedInputEvidence::Turn(turn) = &record.record {
            let keys =
                output_turn_transcripts(tx, authorization.read(), &turn.turn_id, record_limit)?;
            refs.extend(keys.iter().cloned());
            transcripts.insert(turn.turn_id.clone(), keys);
        }
        for child in refs {
            pending.push_back((child, depth + 1));
        }
        records.insert(key, record);
    }
    for (turn, keys) in &transcripts {
        for key in keys {
            ensure!(records.get(key).is_some_and(|record| matches!(&record.record,PreparedInputEvidence::Transcript(entry) if entry.data.get("turn_id").and_then(serde_json::Value::as_str)==Some(turn.as_str()))), "Output Transcript relation differs from its canonical accepted version");
        }
    }
    let domain = ResidentInputEvidence::bootstrap(
        scope,
        records.into_values().collect(),
        InputEvidenceCoverage {
            selected_completion_brief_ids: BTreeSet::new(),
            completion_source_observations: Vec::new(),
            recovery_source_absences: BTreeMap::new(),
            recovery_lineage_messages: BTreeSet::new(),
            execution: Default::default(),
            windows: Vec::new(),
            mandatory_roots: roots,
            turn_transcript_keys: transcripts,
            message_turn_keys: assignments,
            unavailable: BTreeSet::new(),
            record_limit,
            byte_limit,
        },
    )?;
    Ok(TaskOutputInputRecall {
        domain,
        task: authorization.task().clone(),
        material: None,
    })
}

pub(in crate::runtime_db) fn read_authorized_task_input_tx(
    tx: &Transaction<'_>,
    authorization: &mut TaskInputReadAuthorization<'_, '_, '_>,
    key: &Key,
    allow_global_absent: bool,
) -> Result<Option<Arc<QualifiedInputEvidence>>> {
    read_authorized_task_input_with_fingerprint_tx(
        tx,
        authorization,
        key,
        allow_global_absent,
        true,
    )
    .map(|record| record.map(|(record, _)| record))
}

/// Finite read-only qualification shared by Output and the accepted TaskResult route.
/// The caller selects the closed owning edges; this helper grants no writer aliases.
fn read_authorized_task_input_with_fingerprint_tx(
    tx: &Transaction<'_>,
    authorization: &mut TaskInputReadAuthorization<'_, '_, '_>,
    key: &Key,
    allow_global_absent: bool,
    count_nodes: bool,
) -> Result<Option<(Arc<QualifiedInputEvidence>, Fingerprint)>> {
    authorization.validates_tx(tx)?;
    let scope = authorization.scope().clone();
    charge_input_key(tx, authorization, key)?;
    if count_nodes {
        let scope = authorization.scope();
        let nodes:usize=tx.query_row("SELECT COALESCE((SELECT COUNT(*) FROM input_evidence_current a,json_tree(a.attempt_turn_proof_json) WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family=?3 AND a.record_id=?4),0)",params![scope.agent_id,scope.identity_incarnation,key.family.as_str(),key.id],|row|row.get(0))?;
        authorization.charge(0, nodes)?;
    }

    let fingerprint = fingerprint_on(tx, &scope.agent_id, key)?;
    let Some(fingerprint) = fingerprint else {
        // A foreign, pruned or frozen-but-missing body is never global absence.
        let alias = alias_on(tx, &scope, key)?;
        let frozen = frozen_versions(tx, authorization, key)?;
        let (table, id) = table(key.family);
        let known:bool=tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {id}=?1) OR EXISTS(SELECT 1 FROM input_evidence_current WHERE family=?2 AND record_id=?1) OR EXISTS(SELECT 1 FROM input_evidence_seed_membership WHERE family=?2 AND record_id=?1) OR EXISTS(SELECT 1 FROM task_query_fixed_input_members WHERE family=?2 AND record_id=?1)"),params![key.id,key.family.as_str()],|row|row.get(0))?;
        ensure!(
            allow_global_absent && alias.is_none() && frozen.is_empty() && !known,
            InputEvidenceSnapshotError::NeedsPreparation(key.id.clone())
        );
        return Ok(None);
    };
    let alias = alias_on(tx, &scope, key)?;
    let frozen = if let Some(alias) = &alias {
        ensure!(
            alias.present,
            InputEvidenceSnapshotError::NeedsPreparation(format!(
                "pruned {}:{}",
                key.family.as_str(),
                key.id
            ))
        );
        ensure!(
            alias.fingerprint.same_metadata(&fingerprint),
            InputEvidenceSnapshotError::ProjectionChanged(key.id.clone())
        );
        Vec::new()
    } else {
        frozen_versions(tx, authorization, key)?
    };
    let payload = if key.family == Family::ToolExecution {
        None
    } else {
        Some(payload_on(tx, &scope.agent_id, key)?.context("Output owning body disappeared")?)
    };
    let digest = match &payload {
        Some(payload) => bytes_digest(payload.as_bytes()),
        None => normalized_payload_digest(
            fingerprint
                .content_hash
                .as_deref()
                .context("Output Tool needs its normalized canonical hash")?,
        )?,
    };
    validate_canonical_payload_digest(key, &fingerprint, &digest)?;
    if let Some(alias) = &alias {
        ensure!(
            alias.digest.as_deref() == Some(digest.as_str()),
            InputEvidenceSnapshotError::ProjectionChanged(key.id.clone())
        );
    } else {
        ensure!(
            frozen
                .iter()
                .any(|frozen| frozen.same_metadata(&fingerprint)
                    && (!requires_frozen_body_digest(key, &fingerprint)
                        || frozen.frozen_payload_digest.as_deref() == Some(digest.as_str()))),
            InputEvidenceSnapshotError::ProjectionChanged(key.id.clone())
        );
    }
    if count_nodes {
        let (table, id) = table(key.family);
        let nodes:usize=tx.query_row(&format!("SELECT (SELECT COUNT(*) FROM json_tree(payload_json)) FROM {table} WHERE agent_id=?1 AND {id}=?2"),params![scope.agent_id,key.id],|row|row.get(0))?;
        authorization.charge(0, nodes)?;
    }
    let canonical = payload
        .as_ref()
        .map(|payload| decode_canonical(key, payload))
        .transpose()?;
    let attempt_proof = match canonical.as_ref() {
        Some(CanonicalInputEvidence::Attempt(attempt)) => {
            if let Some(proof) = alias
                .as_ref()
                .and_then(|alias| alias.attempt_turn_proof.as_ref())
            {
                Some(proof.clone())
            } else {
                // A preallocated hint alone still grants no Turn. Only an
                // already accepted canonical Turn can become a read edge.
                let mut turns = BTreeSet::new();
                if let Some(turn) = attempt.turn_id.as_deref() {
                    let key = Key::new(Family::Turn, turn);
                    if accepted_key_exists(tx, authorization, &key)? {
                        turns.insert(key);
                    }
                }
                Some(AttemptTurnProof::for_output(attempt, turns))
            }
        }
        _ => None,
    };
    let record = prepare_record_on(tx, &scope.agent_id, key, canonical)?;
    let record = Arc::new(qualified(
        &scope,
        key.clone(),
        alias.as_ref().map_or(1, |alias| alias.revision),
        digest,
        fingerprint.clone(),
        record,
        attempt_proof,
    )?);
    Ok(Some((record, fingerprint)))
}

/// Pure SQL integers first, including damaged raw bodies, normalized fields,
/// fixed fingerprints and current alias/proof material. Guards cover all of the
/// subsequent Rust/native JSON materialization and remain with the recall.
fn charge_input_key(
    tx: &Transaction<'_>,
    authorization: &mut TaskInputReadAuthorization<'_, '_, '_>,
    key: &Key,
) -> Result<()> {
    let scope = authorization.scope();
    let (table, id) = table(key.family);
    let projection = match key.family {
        Family::Message => "COALESCE(length(CAST(content_hash AS BLOB)),0)+COALESCE(length(CAST(message_id AS BLOB)),0)+length(CAST(created_at AS BLOB))+COALESCE((SELECT length(CAST(a.turn_id AS BLOB))+length(CAST(a.agent_id AS BLOB)) FROM conversation_input_assignments a WHERE a.agent_id=?1 AND a.message_id=?2),0)",
        Family::Turn => "length(CAST(created_at AS BLOB))+COALESCE(length(CAST(owner_kind AS BLOB)),0)+COALESCE(length(CAST(owner_id AS BLOB)),0)",
        Family::ToolExecution => "length(CAST(created_at AS BLOB))+COALESCE(length(CAST(content_hash AS BLOB)),0)+COALESCE(length(CAST(work_item_id AS BLOB)),0)+COALESCE(length(CAST(turn_id AS BLOB)),0)",
        Family::Brief => "length(CAST(created_at AS BLOB))+COALESCE(length(CAST(content_hash AS BLOB)),0)",
        Family::Transcript => "length(CAST(created_at AS BLOB))+COALESCE(length(CAST(content_hash AS BLOB)),0)+COALESCE(length(CAST(turn_id AS BLOB)),0)",
        Family::Attempt => "length(CAST(lifecycle_state AS BLOB))+COALESCE(length(CAST(terminal_outcome_id AS BLOB)),0)+COALESCE(length(CAST(recovery_of_attempt_id AS BLOB)),0)",
        Family::Outcome => "length(CAST(attempt_id AS BLOB))",
        Family::WorkExecution => "length(CAST(lifecycle_state AS BLOB))",
    };
    let current: usize = tx.query_row(&format!("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST({id} AS BLOB))+{projection} FROM {table} WHERE agent_id=?1 AND {id}=?2),0)"),params![scope.agent_id,key.id],|row|row.get(0))?;
    let alias: usize = tx.query_row("SELECT COALESCE((SELECT length(CAST(fingerprint_json AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+length(CAST(availability AS BLOB))+COALESCE(length(CAST(attempt_turn_proof_json AS BLOB)),0) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4),0)",params![scope.agent_id,scope.identity_incarnation,key.family.as_str(),key.id],|row|row.get(0))?;
    authorization.charge_body(
        current
            .checked_add(alias)
            .and_then(|n| n.checked_add(key.id.len() + 512))
            .context("Output Input material overflow")?,
    )?;
    Ok(())
}
fn frozen_versions(
    tx: &Transaction<'_>,
    authorization: &mut TaskInputReadAuthorization<'_, '_, '_>,
    key: &Key,
) -> Result<Vec<Fingerprint>> {
    if authorization.seed().is_some() {
        let bytes: usize = tx.query_row("SELECT COALESCE((SELECT COALESCE(length(CAST(origin_kind AS BLOB)),0)+COALESCE(length(CAST(origin_attempt_id AS BLOB)),0)+COALESCE(length(CAST(command_kind AS BLOB)),0)+COALESCE(length(CAST(command_identity AS BLOB)),0)+COALESCE(length(CAST(command_hash AS BLOB)),0)+COALESCE(length(CAST(maintenance_proof_json AS BLOB)),0) FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3),0)+COALESCE((SELECT length(CAST(agent_id AS BLOB))+length(CAST(boundary_id AS BLOB))+length(CAST(source_kind AS BLOB)) FROM episode_continuity_seeds WHERE seed_id=?1),0)+COALESCE((SELECT SUM(length(CAST(c.payload_hash AS BLOB))) FROM execution_protocol_command_results c JOIN episode_continuity_seeds s ON s.agent_id=c.agent_id JOIN input_evidence_seed_recovery_current r ON r.seed_id=s.seed_id AND r.command_kind=c.command_kind AND r.command_identity=c.command_identity WHERE s.seed_id=?1 AND r.family=?2 AND r.record_id=?3),0)",params![authorization.seed(),key.family.as_str(),key.id],|row|row.get(0))?;
        authorization.charge_body(bytes)?;
        let nodes: usize = tx.query_row("SELECT COALESCE((SELECT COUNT(*) FROM input_evidence_seed_recovery_current r,json_tree(r.maintenance_proof_json) WHERE r.seed_id=?1 AND r.family=?2 AND r.record_id=?3),0)",params![authorization.seed(),key.family.as_str(),key.id],|row|row.get(0))?;
        authorization.charge(0, nodes)?;
        legacy_recovery::validate_seed_overlay_on(tx, authorization.seed().unwrap(), key)?;
    }
    let sql = "SELECT fingerprint_json FROM input_evidence_seed_membership WHERE seed_id=?1 AND family=?2 AND record_id=?3 UNION ALL SELECT fingerprint_json FROM task_query_fixed_input_members WHERE inventory_id=?4 AND family=?2 AND record_id=?3 AND NOT EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3) UNION ALL SELECT fingerprint_json FROM task_query_fixed_messages WHERE inventory_id=?4 AND ?2='message' AND evidence_id=?3 AND NOT EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3)";
    let (count, bytes): (usize, usize) = tx.query_row(
        &format!(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(fingerprint_json AS BLOB))),0) FROM ({sql})"
        ),
        params![
            authorization.seed(),
            key.family.as_str(),
            key.id,
            authorization.fixed_inventory()
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    authorization.charge_body(
        bytes
            .checked_add(
                count
                    .checked_mul(128)
                    .context("Output fixed nodes overflow")?,
            )
            .context("Output fixed material overflow")?,
    )?;
    authorization.charge(0, count)?;
    let (prune_count,prune_bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(fingerprint_json AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)),0) FROM task_output_fixed_input_prunes WHERE inventory_id=?1 AND family=?2 AND record_id=?3",params![authorization.fixed_inventory(),key.family.as_str(),key.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    authorization.charge_body(prune_bytes)?;
    authorization.charge(0, prune_count)?;
    let pruned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM task_output_fixed_input_prunes p JOIN task_query_fixed_input_members f ON f.inventory_id=p.inventory_id AND f.family=p.family AND f.record_id=p.record_id AND f.fingerprint_json=p.fingerprint_json WHERE p.inventory_id=?1 AND p.family=?2 AND p.record_id=?3 UNION ALL SELECT 1 FROM task_output_fixed_input_prunes p JOIN task_query_fixed_messages f ON f.inventory_id=p.inventory_id AND p.family='message' AND f.evidence_id=p.record_id AND f.content_digest=p.content_digest WHERE p.inventory_id=?1 AND p.family=?2 AND p.record_id=?3)",params![authorization.fixed_inventory(),key.family.as_str(),key.id],|row|row.get(0))?;
    ensure!(
        !pruned,
        InputEvidenceSnapshotError::NeedsPreparation(format!(
            "pruned focused {}:{}",
            key.family.as_str(),
            key.id
        ))
    );
    let mut query = tx.prepare(sql)?;
    let rows = query.query_map(
        params![
            authorization.seed(),
            key.family.as_str(),
            key.id,
            authorization.fixed_inventory()
        ],
        |row| row.get::<_, String>(0),
    )?;
    rows.map(|raw| serde_json::from_str(&raw?).map_err(Into::into))
        .collect()
}
fn output_turn_transcripts(
    tx: &Transaction<'_>,
    authorization: &mut TaskInputReadAuthorization<'_, '_, '_>,
    turn: &str,
    limit: usize,
) -> Result<BTreeSet<Key>> {
    let scope = authorization.scope();
    let sql = "SELECT r.record_id FROM task_output_current_input_relations r JOIN input_evidence_current a ON a.agent_id=r.agent_id AND a.identity_incarnation=r.identity_incarnation AND a.family=r.family AND a.record_id=r.record_id AND a.revision=r.revision WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family='transcript' AND r.related_turn_id=?3 UNION SELECT f.record_id FROM task_query_fixed_input_members f WHERE f.inventory_id=?4 AND f.family='transcript' AND f.related_turn_id=?3 AND NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family='transcript' AND a.record_id=f.record_id)";
    let (count, bytes): (usize, usize) = tx.query_row(
        &format!(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(record_id AS BLOB))+128),0) FROM ({sql})"
        ),
        params![
            scope.agent_id,
            scope.identity_incarnation,
            turn,
            authorization.fixed_inventory()
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        count <= limit,
        InputEvidenceSnapshotError::ResourceExhausted("Output Turn Transcript frontier")
    );
    authorization.charge(bytes, count)?;
    let scope = authorization.scope();
    let mut query = tx.prepare(&format!("{sql} ORDER BY record_id"))?;
    let rows = query.query_map(
        params![
            scope.agent_id,
            scope.identity_incarnation,
            turn,
            authorization.fixed_inventory()
        ],
        |row| row.get::<_, String>(0),
    )?;
    let keys = rows
        .map(|id| Ok(Key::new(Family::Transcript, id?)))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        keys.len() == count,
        "Output Transcript frontier changed within its original transaction"
    );
    Ok(keys)
}

fn accepted_key_exists(
    tx: &Transaction<'_>,
    authorization: &TaskInputReadAuthorization<'_, '_, '_>,
    key: &Key,
) -> Result<bool> {
    let scope = authorization.scope();
    tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 UNION ALL SELECT 1 FROM input_evidence_seed_membership WHERE seed_id=?5 AND family=?3 AND record_id=?4 UNION ALL SELECT 1 FROM task_query_fixed_input_members WHERE inventory_id=?6 AND family=?3 AND record_id=?4)",params![scope.agent_id,scope.identity_incarnation,key.family.as_str(),key.id,authorization.seed(),authorization.fixed_inventory()],|row|row.get(0)).map_err(Into::into)
}

/// Denial-only cache observation, minted by the original actual prune actor.
/// It carries neither a body locator nor mutation/Ready authority.
#[derive(Debug, Clone)]
pub(crate) struct OutputRecallInvalidation {
    scope: InputEvidenceScope,
}
impl OutputRecallInvalidation {
    pub(crate) fn belongs_to(&self, scope: &InputEvidenceScope) -> bool {
        &self.scope == scope
    }
    pub(crate) fn merge(&self, other: &Self) -> Result<()> {
        ensure!(
            self.scope == other.scope,
            "mixed Output recall invalidation scopes"
        );
        Ok(())
    }
}
pub(super) fn register_pruned_focused_input_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
) -> Result<Option<OutputRecallInvalidation>> {
    // This table exists only after the approved Task88 fixed actor. D4-only
    // schemas retain their original behavior, with no historical raw scan.
    let ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='task_output_fixed_input_prunes')",[],|row|row.get(0))?;
    if !ready {
        let migrations: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')", [], |row|row.get(0))?;
        if migrations {
            let installed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=?1)",
                [super::super::task_snapshot::TASK_DOMAIN_VERSION],
                |row| row.get(0),
            )?;
            ensure!(
                !installed,
                "installed Task domain lost its focused Input prune companion"
            );
        }
        return Ok(None);
    }
    let actual: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE agent_id=?1 AND {}=?2)",
            table(key.family).0,
            table(key.family).1
        ),
        params![scope.agent_id, key.id],
        |row| row.get(0),
    )?;
    ensure!(!actual, "focused prune still has its canonical body");
    tx.execute("INSERT INTO task_output_fixed_input_prunes(inventory_id,family,record_id,fingerprint_json,content_digest) SELECT f.inventory_id,f.family,f.record_id,f.fingerprint_json,NULL FROM task_query_fixed_input_members f JOIN task_query_fixed_manifests m ON m.inventory_id=f.inventory_id WHERE m.agent_id=?1 AND m.sealed=1 AND f.family=?2 AND f.record_id=?3 UNION ALL SELECT f.inventory_id,'message',f.evidence_id,f.fingerprint_json,f.content_digest FROM task_query_fixed_messages f JOIN task_query_fixed_manifests m ON m.inventory_id=f.inventory_id WHERE m.agent_id=?1 AND m.sealed=1 AND ?2='message' AND f.evidence_id=?3 ON CONFLICT(inventory_id,family,record_id) DO NOTHING",params![scope.agent_id,key.family.as_str(),key.id])?;
    let observed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_output_fixed_input_prunes p JOIN task_query_fixed_manifests m ON m.inventory_id=p.inventory_id WHERE m.agent_id=?1 AND p.family=?2 AND p.record_id=?3)",params![scope.agent_id,key.family.as_str(),key.id],|row|row.get(0))?;
    Ok(observed.then(|| OutputRecallInvalidation {
        scope: scope.clone(),
    }))
}
