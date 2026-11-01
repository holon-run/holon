//! A finite first-Wake capability for the original cold normalization actor.
use super::*;
use crate::runtime_db::input_evidence_snapshot::validate_existing_input_evidence_tx;
use crate::types::{MessageEnvelope, MessageKind, MessageOrigin, QueueEntryStatus, TimerStatus};

pub(crate) struct BootstrapWakeMaterial<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    scope: InputEvidenceScope,
    request: SchedulingDomainLoadRequest,
    ledger: InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
}
pub(crate) struct BootstrapWakeCleanup<'tx, 'conn> {
    existing: ExistingSchedulingMaintenanceProof<'tx, 'conn>,
    new_wakes: BTreeMap<SchedulingKey, (SchedulingWakeFact, MessageEnvelope)>,
    new_aliases: BTreeMap<SchedulingKey, Option<Alias>>,
    request: SchedulingDomainLoadRequest,
}
/// JSON maps require string keys. Count the exact borrowed entries as a
/// sequence, without allocating a temporary Vec or copying their Message bodies.
struct NewWakeEntries<'a>(&'a BTreeMap<SchedulingKey, (SchedulingWakeFact, MessageEnvelope)>);
impl serde::Serialize for NewWakeEntries<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for entry in self.0 {
            sequence.serialize_element(&entry)?;
        }
        sequence.end()
    }
}
fn load_request(ledger: &InputEvidenceBudgetLedger) -> Result<SchedulingDomainLoadRequest> {
    let limits = ledger.limits();
    Ok(SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        record_limit: usize::try_from(limits.record_limit)?.min(MAX_RECORDS),
        byte_limit: usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("bootstrap Scheduling limit overflow")?,
        )?
        .min(MAX_BYTES),
    })
}
pub(crate) fn prepare_bootstrap_wake_normalization_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<BootstrapWakeMaterial<'tx, 'conn>> {
    let request = load_request(ledger)?;
    let mut material = Material::new(scope, &request, ledger);
    material.charge(usize::try_from(serialized_bytes(scope)?)?, 1)?;
    loader::scope_identity(tx, scope, &mut material, false)?
        .context(SchedulingSnapshotError::RetiredScope)?;
    let binding:usize=tx.query_row("SELECT COALESCE(SUM(length(CAST(b.mode AS BLOB))+COALESCE(length(CAST(b.seed_id AS BLOB)),0)+COALESCE(length(CAST(s.agent_id AS BLOB)),0)+COALESCE(length(CAST(s.boundary_id AS BLOB)),0)+COALESCE(length(CAST(s.source_kind AS BLOB)),0)),0) FROM agent_episode_continuity b LEFT JOIN episode_continuity_seeds s ON s.seed_id=b.seed_id WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    material.charge(binding, 1)?;
    let state:usize=tx.query_row("SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM agent_states WHERE agent_id=?1",[&scope.agent_id],|r|r.get(0))?;
    material.charge(state, 1)?;
    // pending_metadata_tx below already decodes State/WorkingMemory. Admit all
    // native collection nodes before that first typed State read.
    let state_nodes: usize = tx.query_row(
        "SELECT COUNT(*) FROM agent_states s,json_tree(s.payload_json) WHERE s.agent_id=?1",
        [&scope.agent_id],
        |r| r.get(0),
    )?;
    material.charge(
        state_nodes
            .checked_mul(std::mem::size_of::<serde_json::Value>())
            .context("bootstrap State node bytes overflow")?,
        state_nodes,
    )?;
    // Admit raw bytes before native origin selection or the original typed read.
    let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(q.payload_json AS BLOB))+COALESCE(length(CAST(m.payload_json AS BLOB)),0)+length(CAST(q.message_id AS BLOB))+length(CAST(q.agent_id AS BLOB))+length(CAST(q.status AS BLOB))+length(CAST(q.priority AS BLOB))+length(CAST(q.created_at AS BLOB))+length(CAST(q.updated_at AS BLOB))+COALESCE(length(CAST(m.agent_id AS BLOB)),0)+512),0) FROM queue_entries q LEFT JOIN messages m ON m.message_id=q.message_id WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted')",[&scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    material.charge(bytes, count)?;
    // Preserve the original logical pending gate before native JSON parsing.
    crate::runtime_db::bootstrap_inputs::pending_metadata_tx(tx, scope, ledger)?;
    let nodes:usize=tx.query_row("SELECT COUNT(*) FROM queue_entries q JOIN messages m ON m.message_id=q.message_id,json_tree(m.payload_json) WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted')",[&scope.agent_id],|r|r.get(0))?;
    // Native node count follows raw-byte and original pending admission. Charge
    // typed JSON allocations as bytes, not additional logical canonical rows.
    material.charge(
        nodes
            .checked_mul(std::mem::size_of::<serde_json::Value>())
            .context("bootstrap Message node bytes overflow")?,
        0,
    )?;
    prepare_pending_terminal_sources_tx(tx, scope, &mut material)?;
    let projection = stored::projection(SchedulingFamily::Timer);
    let sql=format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(c.payload_json AS BLOB))+length(CAST({projection} AS BLOB))+512),0) FROM timers c WHERE c.agent_id=?1 AND (EXISTS(SELECT 1 FROM timer_wakes w WHERE w.timer_id=c.timer_id AND w.status='pending') OR EXISTS(SELECT 1 FROM queue_entries q JOIN messages m ON m.message_id=q.message_id WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted') AND json_extract(m.payload_json,'$.origin.kind')='timer' AND json_extract(m.payload_json,'$.origin.timer_id')=c.timer_id))");
    let (count, bytes): (usize, usize) =
        tx.query_row(&sql, [&scope.agent_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    material.charge(bytes, count)?;
    let projection = stored::projection(SchedulingFamily::TimerWake);
    let sql=format!("SELECT COUNT(*),COALESCE(SUM(length(CAST({projection} AS BLOB))+512),0) FROM timer_wakes c JOIN timers t ON t.timer_id=c.timer_id WHERE t.agent_id=?1 AND (c.status='pending' OR EXISTS(SELECT 1 FROM queue_entries q WHERE q.agent_id=?1 AND q.message_id=c.message_id AND q.status IN ('queued','interrupted')))");
    let (count, bytes): (usize, usize) =
        tx.query_row(&sql, [&scope.agent_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    material.charge(bytes, count)?;
    Ok(BootstrapWakeMaterial {
        tx,
        scope: scope.clone(),
        request: request.clone(),
        ledger: ledger.clone(),
        bytes: material.bytes,
        records: material.records,
        guards: material.guards,
    })
}
fn prepare_pending_terminal_sources_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    material: &mut Material<'_, '_>,
) -> Result<()> {
    let sources = repositories::PENDING_TERMINAL_TIMER_WAKE_SOURCES;
    // These are history sources, not Accepted pending inputs. Admit their exact
    // raw footprint before reading IDs, decoding Queue, or validating Message.
    let sql = format!(
        "SELECT COUNT(*), COALESCE(SUM(
           length(CAST(q.payload_json AS BLOB)) + length(CAST(m.payload_json AS BLOB))
           + length(CAST(q.message_id AS BLOB)) + length(CAST(q.agent_id AS BLOB))
           + length(CAST(q.status AS BLOB)) + length(CAST(m.evidence_id AS BLOB))
           + length(CAST(m.agent_id AS BLOB)) + 512), 0)
         FROM {sources} AND t.agent_id = ?1"
    );
    let (count, bytes): (usize, usize) =
        tx.query_row(&sql, [&scope.agent_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if count == 0 {
        return Ok(());
    }
    material.charge(bytes, count)?;
    let sql = format!(
        "SELECT COALESCE(SUM(
           (SELECT COUNT(*) FROM json_tree(q.payload_json))
           + (SELECT COUNT(*) FROM json_tree(m.payload_json))), 0)
         FROM {sources} AND t.agent_id = ?1"
    );
    let nodes: usize = tx.query_row(&sql, [&scope.agent_id], |r| r.get(0))?;
    material.charge(
        nodes
            .checked_mul(std::mem::size_of::<serde_json::Value>())
            .context("bootstrap terminal source node bytes overflow")?,
        0,
    )?;
    let sql = format!(
        "SELECT q.message_id, m.evidence_id
         FROM {sources} AND t.agent_id = ?1
         ORDER BY w.timer_id, w.message_id"
    );
    let mut statement = tx.prepare(&sql)?;
    let mut rows = statement.query([&scope.agent_id])?;
    while let Some(row) = rows.next()? {
        let message_id = row.get_ref(0)?.as_str()?;
        ensure!(
            row.get_ref(1)?.as_str()? == message_id,
            "bootstrap terminal Message evidence ID differs"
        );
        message_source_material(tx, scope, message_id, 0, material)?;
        // Input qualification decodes these finite fingerprint/proof nodes.
        // Their encoded bytes were charged by message_source_material above.
        let nodes: usize = tx.query_row(
            "SELECT
               (SELECT COUNT(*) FROM input_evidence_current c, json_tree(c.fingerprint_json)
                WHERE c.agent_id=?1 AND c.identity_incarnation=?2
                  AND c.family='message' AND c.record_id=?3)
               + (SELECT COUNT(*) FROM input_evidence_current c,
                    json_tree(COALESCE(c.attempt_turn_proof_json,'null'))
                  WHERE c.agent_id=?1 AND c.identity_incarnation=?2
                    AND c.family='message' AND c.record_id=?3)
               + (SELECT COUNT(*) FROM input_evidence_seed_membership m
                  JOIN agent_episode_continuity b ON b.seed_id=m.seed_id,
                    json_tree(m.fingerprint_json)
                  WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1
                    AND m.family='message' AND m.record_id=?3)",
            params![scope.agent_id, scope.identity_incarnation, message_id],
            |r| r.get(0),
        )?;
        material.charge(
            nodes
                .checked_mul(std::mem::size_of::<serde_json::Value>())
                .context("bootstrap terminal proof node bytes overflow")?,
            0,
        )?;
        validate_existing_input_evidence_tx(
            tx,
            scope,
            &InputEvidenceKey::new(InputEvidenceFamily::Message, message_id),
        )?;
    }
    Ok(())
}
fn remaining(
    request: &SchedulingDomainLoadRequest,
    bytes: usize,
    records: usize,
) -> Result<SchedulingDomainLoadRequest> {
    let next = SchedulingDomainLoadRequest {
        capture: request.capture.clone(),
        byte_limit: request
            .byte_limit
            .checked_sub(bytes)
            .context("bootstrap Wake bytes exhausted")?,
        record_limit: request
            .record_limit
            .checked_sub(records)
            .context("bootstrap Wake records exhausted")?,
    };
    bound(0, 0, &next)?;
    Ok(next)
}
fn message_material(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message: &MessageEnvelope,
    material: &mut Material<'_, '_>,
) -> Result<()> {
    message_source_material(
        tx,
        scope,
        &message.id,
        usize::try_from(serialized_bytes(message)?)?,
        material,
    )
}
fn message_source_material(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message_id: &str,
    decoded_bytes: usize,
    material: &mut Material<'_, '_>,
) -> Result<()> {
    // Same finite D3 Message/alias/fixed-fingerprint tuple used by Task writers.
    let (bytes,count):(usize,usize)=tx.query_row("SELECT COALESCE((SELECT SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(content_hash AS BLOB)),0)+length(CAST(created_at AS BLOB))) FROM messages WHERE evidence_id=?1 OR message_id=?1),0)+COALESCE((SELECT length(CAST(turn_id AS BLOB)) FROM conversation_input_assignments WHERE agent_id=?2 AND message_id=?1),0)+COALESCE((SELECT SUM(length(CAST(fingerprint_json AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(attempt_turn_proof_json AS BLOB)),0)) FROM input_evidence_current WHERE agent_id=?2 AND identity_incarnation=?3 AND family='message' AND record_id=?1),0)+COALESCE((SELECT SUM(length(CAST(m.fingerprint_json AS BLOB))) FROM input_evidence_seed_membership m JOIN agent_episode_continuity b ON b.seed_id=m.seed_id WHERE b.agent_id=?2 AND b.identity_incarnation=?3 AND b.policy_version=1 AND m.family='message' AND m.record_id=?1),0),1+(SELECT COUNT(*) FROM messages WHERE evidence_id=?1 OR message_id=?1)+(SELECT COUNT(*) FROM conversation_input_assignments WHERE agent_id=?2 AND message_id=?1)+(SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?2 AND identity_incarnation=?3 AND family='message' AND record_id=?1)+(SELECT COUNT(*) FROM input_evidence_seed_membership m JOIN agent_episode_continuity b ON b.seed_id=m.seed_id WHERE b.agent_id=?2 AND b.identity_incarnation=?3 AND b.policy_version=1 AND m.family='message' AND m.record_id=?1)",params![message_id,scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
    material.charge(
        bytes
            .checked_add(decoded_bytes)
            .context("bootstrap source bytes overflow")?,
        count,
    )?;
    let (overlay_count,overlay_bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(o.origin_kind AS BLOB))+COALESCE(length(CAST(o.origin_attempt_id AS BLOB)),0)+COALESCE(length(CAST(o.command_kind AS BLOB)),0)+COALESCE(length(CAST(o.command_identity AS BLOB)),0)+COALESCE(length(CAST(o.command_hash AS BLOB)),0)+COALESCE(length(CAST(o.maintenance_proof_json AS BLOB)),0)+length(CAST(s.agent_id AS BLOB))+length(CAST(s.boundary_id AS BLOB))+length(CAST(s.source_kind AS BLOB))+128),0) FROM agent_episode_continuity b JOIN episode_continuity_seeds s ON s.seed_id=b.seed_id JOIN input_evidence_seed_recovery_current o ON o.seed_id=s.seed_id WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1 AND o.family='message' AND o.record_id=?3",params![scope.agent_id,scope.identity_incarnation,message_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    material.charge(overlay_bytes, overlay_count)?;
    // A damaged fallback origin is still bounded before its typed decoder.
    let nodes:usize=tx.query_row("SELECT COUNT(*) FROM agent_episode_continuity b JOIN input_evidence_seed_recovery_current o ON o.seed_id=b.seed_id,json_tree(CASE WHEN o.origin_kind='maintenance_created' THEN o.maintenance_proof_json ELSE 'null' END) WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1 AND o.family='message' AND o.record_id=?3",params![scope.agent_id,scope.identity_incarnation,message_id],|r|r.get(0))?;
    material.charge(
        nodes
            .checked_mul(std::mem::size_of::<serde_json::Value>())
            .context("bootstrap source native nodes overflow")?,
        nodes,
    )
}
fn validate_source(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message: &MessageEnvelope,
) -> Result<()> {
    ensure!(
        message.agent_id == scope.agent_id
            && message.kind == MessageKind::TimerTick
            && message.message_seq.is_some_and(|n| n > 0),
        "new bootstrap Wake lacks actual TimerTick source"
    );
    let (owner, seq, raw): (String, u64, String) = tx.query_row(
        "SELECT agent_id,message_seq,payload_json FROM messages WHERE message_id=?1",
        [&message.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let actual = repositories::decode_message_payload(&raw)?;
    ensure!(
        owner == scope.agent_id && Some(seq) == message.message_seq && actual == *message,
        "bootstrap Wake source differs from canonical Message"
    );
    validate_existing_input_evidence_tx(
        tx,
        scope,
        &InputEvidenceKey::new(InputEvidenceFamily::Message, &message.id),
    )
}
impl<'tx, 'conn> BootstrapWakeMaterial<'tx, 'conn> {
    pub(in crate::runtime_db) fn observe_inputs_tx(
        self,
        tx: &'tx Transaction<'conn>,
        input: &repositories::TimerWakeNormalizationInput,
    ) -> Result<BootstrapWakeCleanup<'tx, 'conn>> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "bootstrap Wake selection crossed transaction"
        );
        let mut material = Material::new(&self.scope, &self.request, &self.ledger);
        material.bytes = self.bytes;
        material.records = self.records;
        let mut existing_keys = BTreeSet::new();
        let mut possible_new = BTreeMap::new();
        let mut new_aliases = BTreeMap::new();
        for (timer, message, _) in &input.pending_wakes {
            existing_keys.insert(SchedulingKey::new(SchedulingFamily::Timer, timer));
            existing_keys.insert(SchedulingKey::wake(timer, message));
        }
        for (entry, message) in &input.live_messages {
            let MessageOrigin::Timer { timer_id } = &message.origin else {
                continue;
            };
            ensure!(
                entry.agent_id == self.scope.agent_id
                    && message.agent_id == self.scope.agent_id
                    && entry.message_id == message.id
                    && matches!(
                        entry.status,
                        QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                    ),
                "bootstrap Wake queue/source scope differs"
            );
            let key = SchedulingKey::new(SchedulingFamily::Timer, timer_id);
            let Some(timer) =
                stored::read_stored(tx, &key, &self.scope.agent_id, Some(&mut material))?
            else {
                continue;
            };
            existing_keys.insert(key);
            let wake_key = SchedulingKey::wake(timer_id, &message.id);
            if stored::exists_global(tx, &wake_key)? {
                existing_keys.insert(wake_key);
                continue;
            }
            let CanonicalSchedulingFact::Timer(timer) = timer.fact else {
                unreachable!()
            };
            if !matches!(timer.status, TimerStatus::Active | TimerStatus::Completed) {
                continue;
            }
            let prior_alias = read_alias(tx, &self.scope, &wake_key, &mut material)?;
            ensure!(
                !qualification::known_anywhere(tx, &wake_key)?
                    && prior_alias
                        .as_ref()
                        .is_none_or(|alias| alias.availability == SchedulingAvailability::Absent),
                "bootstrap Wake cannot reuse old/pruned membership"
            );
            new_aliases.insert(wake_key.clone(), prior_alias);
            message_material(tx, &self.scope, message, &mut material)?;
            validate_source(tx, &self.scope, message)?;
            let (owner,id,status,raw):(String,String,String,String)=tx.query_row("SELECT agent_id,message_id,status,payload_json FROM queue_entries WHERE message_id=?1",[&message.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            ensure!(
                owner == self.scope.agent_id
                    && id == message.id
                    && matches!(status.as_str(), "queued" | "interrupted")
                    && repositories::decode_queue_entry_payload(&raw)? == *entry,
                "bootstrap Wake queue changed before preparation"
            );
            let fire_count = message
                .metadata
                .as_ref()
                .and_then(|m| m.get("timer_fire_count"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_else(|| timer.fire_count.max(1));
            let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                entry.created_at.timestamp_millis(),
            )
            .context("bootstrap Wake timestamp outside canonical range")?;
            let expected = SchedulingWakeFact {
                agent_id: self.scope.agent_id.clone(),
                timer_id: timer_id.clone(),
                message_id: message.id.clone(),
                fire_count,
                status: SchedulingWakeStatus::Pending,
                created_at: at,
                updated_at: at,
                incorporated_at: None,
                cancelled_at: None,
            };
            possible_new.insert(wake_key, (expected, message.clone()));
        }
        material.charge(
            usize::try_from(serialized_bytes(&(
                &existing_keys,
                NewWakeEntries(&possible_new),
            ))?)?,
            existing_keys.len() + possible_new.len(),
        )?;
        let load = remaining(&self.request, material.bytes, material.records)?;
        let mut existing = prepare_existing_scheduling_maintenance_tx(
            tx,
            &self.scope,
            &existing_keys,
            &load,
            &self.ledger,
        )?;
        for key in possible_new.keys() {
            let (timer, _) = key.wake_pair()?;
            ensure!(
                existing
                    .inner
                    .before
                    .get(&SchedulingKey::new(SchedulingFamily::Timer, &timer))
                    .is_some_and(Option::is_some),
                SchedulingSnapshotError::NeedsPreparation(SchedulingKey::new(
                    SchedulingFamily::Timer,
                    timer
                ))
            );
        }
        existing.inner.material_bytes = existing
            .inner
            .material_bytes
            .checked_add(material.bytes)
            .context("bootstrap proof bytes overflow")?;
        existing.inner.material_records = existing
            .inner
            .material_records
            .checked_add(material.records)
            .context("bootstrap proof count overflow")?;
        existing.inner.guards.extend(self.guards);
        existing.inner.guards.extend(material.guards);
        bound(
            existing.inner.material_records,
            existing.inner.material_bytes,
            &self.request,
        )?;
        Ok(BootstrapWakeCleanup {
            existing,
            new_wakes: possible_new,
            new_aliases,
            request: self.request,
        })
    }
}
impl BootstrapWakeCleanup<'_, '_> {
    pub(crate) fn finish_tx(
        mut self,
        tx: &Transaction<'_>,
        actual_pairs: &BTreeSet<(String, String)>,
    ) -> Result<SchedulingMaintenanceAck> {
        let actual = actual_pairs
            .iter()
            .map(|(timer, message)| SchedulingKey::wake(timer, message))
            .collect::<BTreeSet<_>>();
        ensure!(
            actual
                .iter()
                .all(|key| self.existing.targets.contains_key(key)
                    || self.new_wakes.contains_key(key)),
            "bootstrap Wake actual footprint exceeds closed preparation"
        );
        let existing_actual = actual
            .iter()
            .filter(|key| self.existing.targets.contains_key(*key))
            .cloned()
            .collect();
        let (mut selected, mut changed) =
            validate_actual(tx, &mut self.existing, &existing_actual, &self.request)?;
        let mut material = Material::new(
            &self.existing.inner.scope,
            &self.request,
            &self.existing.inner.ledger,
        );
        material.bytes = self.existing.inner.material_bytes;
        material.records = self.existing.inner.material_records;
        let mut after = BTreeMap::new();
        for (key, before) in &self.existing.inner.before {
            let value = stored::read_stored(
                tx,
                key,
                &self.existing.inner.scope.agent_id,
                Some(&mut material),
            )?
            .context("bootstrap removed an original qualified row")?;
            let before = before
                .as_ref()
                .context("bootstrap original member missing")?;
            let differs = before.content != value.content || before.projection != value.projection;
            ensure!(
                !differs || selected.contains(key),
                "bootstrap changed undeclared qualified row"
            );
            if selected.contains(key) {
                changed |= differs;
                after.insert(key.clone(), value);
            }
        }
        for (key, (expected, source)) in &self.new_wakes {
            let value = stored::read_stored(
                tx,
                key,
                &self.existing.inner.scope.agent_id,
                Some(&mut material),
            )?;
            ensure!(
                value.is_none() || actual.contains(key),
                "bootstrap inserted an undeclared new Wake"
            );
            let Some(value) = value else {
                continue;
            };
            ensure!(
                matches!(&value.fact,CanonicalSchedulingFact::TimerWake(actual) if actual==expected),
                "bootstrap new Wake differs from its frozen Timer/Message source"
            );
            message_material(tx, &self.existing.inner.scope, source, &mut material)?;
            validate_source(tx, &self.existing.inner.scope, source)?;
            ensure!(
                &read_alias(tx, &self.existing.inner.scope, key, &mut material)?
                    == self
                        .new_aliases
                        .get(key)
                        .context("bootstrap new Wake original alias missing")?,
                "bootstrap Wake membership changed before ACK"
            );
            selected.insert(key.clone());
            after.insert(key.clone(), value);
            changed = true;
        }
        let revision = self
            .existing
            .inner
            .base_revision
            .checked_add(u64::from(changed))
            .context("bootstrap Wake revision overflow")?;
        for (key, value) in after {
            write_alias(
                tx,
                &self.existing.inner.scope,
                &key,
                revision,
                SchedulingAvailability::Present,
                Some(&value.content),
                Some(&value.projection),
                Some((&value.status, &value.sort_at, &value.created_at)),
            )?;
        }
        let invalidation = if changed {
            let mut coverage = SchedulingCoverage::empty(revision);
            coverage.active_timers_complete = false;
            coverage.pending_wakes_complete = false;
            coverage.active_triggers_complete = false;
            material.charge(usize::try_from(serialized_bytes(&coverage)?)?, 1)?;
            tx.execute("INSERT INTO scheduling_domain_current(agent_id,identity_incarnation,revision,coverage_json) VALUES(?1,?2,?3,?4) ON CONFLICT(agent_id,identity_incarnation) DO UPDATE SET revision=excluded.revision,coverage_json=excluded.coverage_json",params![self.existing.inner.scope.agent_id,self.existing.inner.scope.identity_incarnation,revision,serde_json::to_string(&coverage)?])?;
            Some(SchedulingScopeInvalidation {
                base_revision: self.existing.inner.base_revision,
                revision,
            })
        } else {
            None
        };
        let mut guards = self.existing.inner.guards.clone();
        guards.extend(material.guards);
        Ok(SchedulingMaintenanceAck {
            _scope: self.existing.inner.scope.clone(),
            qualified_actual_keys: selected,
            invalidation,
            guards,
        })
    }
}

#[cfg(test)]
#[path = "bootstrap_wake_tests.rs"]
mod tests;
