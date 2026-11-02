//! Explicit finite history, not an extension of the live Current frontier.
use super::*;
use crate::storage::EventLogPageOrder;

pub(crate) struct PreparedEventPage {
    events: Vec<Arc<OwnedCurrentRecord>>,
    pub cursor_seq: Option<u64>,
    pub recovery: crate::runtime_db::AgentEventRecoveryWindow,
    pub has_older: bool,
    pub has_newer: bool,
    _material: Vec<Arc<BudgetViewGuard>>,
}

#[cfg(test)]
pub(crate) fn append_qualified_event_for_fixture(
    db: &crate::runtime_db::RuntimeDb,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    event: &crate::types::AuditEvent,
) -> Result<crate::runtime_db::transitions::PostCommitEffects> {
    use crate::runtime_db::{
        current_read_writes::OriginalCurrentWrites,
        input_evidence_snapshot::native_birth::NativeInputWriteProof,
    };
    db.transaction(|tx| {
        let mut native = NativeInputWriteProof::prepare_tx(
            tx,
            scope,
            ledger,
            (4 * 1024 * 1024, 4096),
            None,
            |_| Ok(()),
        )?
        .context("Native Event source")?;
        let current = native
            .with_input_phase(tx, ledger, None, |material| {
                OriginalCurrentWrites::prepare(tx, scope, material, |visit| {
                    visit(CurrentFamily::Event, &event.id)
                })
            })?
            .context("Current source")?;
        crate::runtime_db::evidence::append_audit_event_tx(tx, Some(&scope.agent_id), event)?;
        let current = native.with_input_phase(tx, ledger, None, |material| {
            current.finish(material, |visit| visit(CurrentFamily::Event, &event.id))
        })?;
        let mut effects = crate::runtime_db::transitions::PostCommitEffects::default();
        for record in current.records() {
            if let CurrentRecord::Event(event) = record.record() {
                effects.audit_events.push(event.clone());
            }
        }
        effects.current_read_writes = Some(current);
        native.finish_delta_tx(tx, &mut effects.input_evidence_delta, ledger, None)?;
        Ok(effects)
    })
}

impl PreparedEventPage {
    pub(crate) fn events(
        &self,
    ) -> impl DoubleEndedIterator<Item = &crate::types::AuditEvent> + ExactSizeIterator {
        self.events.iter().map(|record| match record.record() {
            CurrentRecord::Event(event) => &**event,
            _ => unreachable!("finite Event query contains another family"),
        })
    }
}

pub(crate) fn prepare_event_page(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    before_seq: Option<u64>,
    after_seq: Option<u64>,
    limit: usize,
    order: EventLogPageOrder,
    mut matches: impl FnMut(&crate::types::AuditEvent) -> bool,
) -> Result<PreparedEventPage> {
    let lower = i64::try_from(after_seq.unwrap_or(0))
        .context("audit event lower cursor exceeds SQLite integer range")?;
    let upper = before_seq
        .map(|seq| {
            i64::try_from(seq).context("audit event upper cursor exceeds SQLite integer range")
        })
        .transpose()?;
    let limits = ledger.limits();
    let mut material = InputReadMaterial::new(
        scope,
        ledger,
        (
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Event query byte limit overflow")?,
            )?,
            usize::try_from(limits.record_limit)?,
        ),
        None,
    )?;
    let active = tx.query_row(
        "SELECT status='active' AND json_extract(payload_json,'$.agent_id')=?1 AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' THEN json_extract(payload_json,'$.incarnation') END=?2 AND json_extract(payload_json,'$.status')=status FROM agent_identities WHERE agent_id=?1",
        params![scope.agent_id, scope.identity_incarnation], |row| row.get::<_, bool>(0),
    ).optional()?.unwrap_or(false);
    ensure!(active, "Event query requires an active exact identity");
    let identity = super::super::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
        .identity_fact()?
        .context("Event query identity is absent")?;
    ensure!(
        identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation
            && identity.status == crate::types::AgentRegistryStatus::Active,
        "Event query requires an active exact identity"
    );
    let recovery = crate::runtime_db::observer_sync::agent_event_recovery_window_on(
        tx,
        Some(&scope.agent_id),
    )?;
    let cursor_seq = tx.query_row(
        "SELECT MAX(event_seq) FROM audit_events WHERE agent_id=?1",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    let mut events = Vec::new();
    let descending = order == EventLogPageOrder::Desc;
    let sql = if descending {
        "SELECT audit_event_id,event_seq FROM audit_events
         WHERE agent_id=?1 AND event_seq>?2 AND (?3 IS NULL OR event_seq<?3)
         ORDER BY event_seq DESC,created_at DESC"
    } else {
        "SELECT audit_event_id,event_seq FROM audit_events
         WHERE agent_id=?1 AND event_seq>?2 AND (?3 IS NULL OR event_seq<?3)
         ORDER BY event_seq ASC,created_at ASC"
    };
    let mut statement = tx.prepare(sql)?;
    let mut rows = statement.query(params![scope.agent_id, lower, upper])?;
    let mut has_more = false;
    if limit > 0 {
        while let Some(row) = rows.next()? {
            let id = row.get_ref(0)?.as_str()?;
            let seq = u64::try_from(row.get::<_, i64>(1)?)
                .context("stored audit event sequence is negative")?;
            let body = load_body(tx, scope, CurrentFamily::Event, id, &mut material, true)?
                .context("selected Event disappeared")?;
            let CurrentRecord::Event(event) = body.record() else {
                unreachable!("selected Event has another family")
            };
            ensure!(event.event_seq == seq, "Event query sequence differs");
            if !matches(event) {
                continue;
            }
            if events.len() == limit {
                has_more = true;
                break;
            }
            material
                .observe_native_footprint(std::mem::size_of::<Arc<OwnedCurrentRecord>>() + 64, 1)?;
            events.push(body);
        }
    }
    let guards = material.into_guards();
    Ok(PreparedEventPage {
        events,
        cursor_seq,
        recovery,
        has_older: descending && has_more,
        has_newer: !descending && has_more,
        _material: guards,
    })
}
