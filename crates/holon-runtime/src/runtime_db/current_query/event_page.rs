//! Explicit finite history, not an extension of the live Current frontier.
use super::*;
use crate::storage::EventLogPageOrder;

const MAX_PAGE_CANDIDATES: usize = 256;
const MAX_PAGE_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct EventPageCosts {
    pub scanned: usize,
    pub decoded: usize,
    pub matched: usize,
    pub returned: usize,
    pub payload_bytes: usize,
    pub hash: std::time::Duration,
    pub nodes: std::time::Duration,
    pub decode: std::time::Duration,
    pub filter: std::time::Duration,
    pub vm_steps: i32,
    pub sorts: i32,
}

#[derive(Clone, Copy)]
pub(crate) struct EventPageScan<'a> {
    pub kind: Option<&'a str>,
}

pub(crate) struct PreparedEventPage {
    events: Vec<Arc<OwnedCurrentRecord>>,
    pub cursor_seq: Option<u64>,
    pub recovery: crate::runtime_db::AgentEventRecoveryWindow,
    pub has_older: bool,
    pub has_newer: bool,
    pub next_before_seq: Option<u64>,
    pub next_after_seq: Option<u64>,
    #[cfg(test)]
    pub costs: EventPageCosts,
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

#[cfg(test)]
pub(crate) fn prepare_event_page(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    before_seq: Option<u64>,
    after_seq: Option<u64>,
    limit: usize,
    order: EventLogPageOrder,
    matches: impl FnMut(&crate::types::AuditEvent) -> bool,
) -> Result<PreparedEventPage> {
    prepare_event_page_with_scan(
        tx, scope, ledger, before_seq, after_seq, limit, order, None, matches,
    )
}

pub(crate) fn prepare_event_page_with_scan(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    before_seq: Option<u64>,
    after_seq: Option<u64>,
    limit: usize,
    order: EventLogPageOrder,
    scan: Option<EventPageScan<'_>>,
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
    let kind = scan.and_then(|scan| scan.kind);
    let sql = event_page_sql(descending, upper.is_some(), kind.is_some());
    let upper = upper.unwrap_or(i64::MAX);
    let mut statement = tx.prepare(&sql)?;
    let mut rows = if let Some(kind) = kind {
        statement.query(params![scope.agent_id, lower, upper, kind])?
    } else {
        statement.query(params![scope.agent_id, lower, upper])?
    };
    let mut has_more = false;
    let mut scanned_seq = None;
    let mut costs = EventPageCosts::default();
    if limit > 0 {
        while let Some(row) = rows.next()? {
            // Bounded history probes candidate existence, not another match.
            // This lookahead has not been consumed and cannot advance a cursor.
            if scan.is_some() && (events.len() == limit || costs.scanned == MAX_PAGE_CANDIDATES) {
                has_more = true;
                break;
            }
            let id = row.get_ref(0)?.as_str()?;
            let seq = u64::try_from(row.get::<_, i64>(1)?)
                .context("stored audit event sequence is negative")?;
            let payload = row.get_ref(2)?.as_str()?;
            // SQLite's borrowed UTF-8 cell is the canonical input, not a
            // reserialized value. Admit it before hashing or walking JSON.
            if scan.is_some() && payload.len() > MAX_PAGE_PAYLOAD_BYTES - costs.payload_bytes {
                if costs.scanned == 0 {
                    return Err(crate::runtime_error::RuntimeError::new(
                        crate::runtime_error::RuntimeErrorDomain::Runtime,
                        "event_page_resource_exhausted",
                        "ResourceExhausted: Event-page canonical JSON input exceeds the fixed 8 MiB budget",
                    )
                    .with_retryable(false)
                    .into());
                }
                has_more = true;
                break;
            }
            costs.scanned += 1;
            costs.payload_bytes += payload.len();
            let started = std::time::Instant::now();
            let hash = digest(payload);
            costs.hash += started.elapsed();
            let body = decode_body(
                tx,
                scope,
                CurrentFamily::Event,
                id,
                payload,
                hash,
                &mut material,
                Some(&mut costs),
            )?;
            costs.decoded += 1;
            let CurrentRecord::Event(event) = body.record() else {
                unreachable!("selected Event has another family")
            };
            ensure!(event.event_seq == seq, "Event query sequence differs");
            scanned_seq = Some(seq);
            let started = std::time::Instant::now();
            let matched = kind.is_none_or(|kind| event.kind == kind) && matches(event);
            costs.filter += started.elapsed();
            if !matched {
                continue;
            }
            costs.matched += 1;
            if events.len() == limit {
                has_more = true;
                break;
            }
            material
                .observe_native_footprint(std::mem::size_of::<Arc<OwnedCurrentRecord>>() + 64, 1)?;
            events.push(body);
        }
    }
    costs.returned = events.len();
    drop(rows);
    costs.vm_steps = statement.get_status(rusqlite::StatementStatus::VmStep);
    costs.sorts = statement.get_status(rusqlite::StatementStatus::Sort);
    tracing::debug!(?costs, "finite Event page costs");
    let guards = material.into_guards();
    Ok(PreparedEventPage {
        events,
        cursor_seq,
        recovery,
        has_older: descending && has_more,
        has_newer: !descending && has_more,
        next_before_seq: (scan.is_some() && descending && has_more)
            .then_some(scanned_seq)
            .flatten(),
        next_after_seq: (scan.is_some() && !descending && has_more)
            .then_some(scanned_seq)
            .flatten(),
        #[cfg(test)]
        costs,
        _material: guards,
    })
}

fn event_page_sql(descending: bool, has_upper: bool, filtered: bool) -> String {
    let direction = if descending { "DESC" } else { "ASC" };
    let upper_op = if has_upper { "<" } else { "<=" };
    let bounds = format!("agent_id=?1 AND event_seq>?2 AND event_seq{upper_op}?3");
    let columns = "audit_event_id,event_seq,data_json,kind,created_at";
    if filtered {
        let key = holon_storage::schema::EVENT_PAGE_KIND_KEY;
        // Two disjoint, ordered index ranges. The fallback cannot be dropped:
        // normalized kind alone says nothing about a legacy/corrupt body.
        format!(
            "SELECT {columns} FROM audit_events
             WHERE {bounds} AND ({key})=?4
             UNION ALL
             SELECT {columns} FROM audit_events
             WHERE {bounds} AND ({key}) IS NULL
             ORDER BY event_seq {direction},created_at {direction}"
        )
    } else {
        format!(
            "SELECT {columns} FROM audit_events WHERE {bounds}
             ORDER BY event_seq {direction},created_at {direction}"
        )
    }
}
