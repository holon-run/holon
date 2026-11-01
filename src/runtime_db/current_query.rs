//! Complete selected Current membership, distinct from a sparse writer ACK.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};

use super::current_read_writes::{
    digest, CurrentAddressFact, CurrentFamily, CurrentRecord, OwnedCurrentRecord,
};
use super::input_evidence_snapshot::read_material::InputReadMaterial;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_input_evidence::InputEvidenceScope,
};

mod event_page;
#[cfg(test)]
pub(crate) use event_page::append_qualified_event_for_fixture;
#[cfg(test)]
mod source_fixture;
pub(crate) use event_page::{prepare_event_page, PreparedEventPage};
#[cfg(test)]
pub(crate) use source_fixture::remove_qualified_terminal_queue_for_fixture;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct CurrentSelection {
    pub queue_ids: std::collections::BTreeSet<String>,
    pub events_after: Option<u64>,
}

/// Counts are bounded metadata, never body qualification or JSON material.
pub(crate) fn summary_counts(
    connection: &rusqlite::Connection,
    agent_id: &str,
) -> Result<(usize, usize)> {
    let briefs = connection.query_row(
        "SELECT COUNT(*) FROM (SELECT 1 FROM briefs WHERE agent_id=?1 LIMIT 50)",
        [agent_id],
        |row| row.get(0),
    )?;
    let events = connection.query_row(
        "SELECT COUNT(*) FROM (SELECT 1 FROM audit_events WHERE agent_id=?1 LIMIT 100)",
        [agent_id],
        |row| row.get(0),
    )?;
    Ok((briefs, events))
}

/// Metadata only: these facts never grant Queue or Event body capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CurrentSchedulerMetadata {
    pub has_interrupted_replay: bool,
    pub has_queued_input: bool,
    pub latest_runtime_error: Option<chrono::DateTime<chrono::Utc>>,
    pub recent_brief_count: usize,
    pub recent_event_count: usize,
}

pub(crate) fn scheduler_metadata(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<CurrentSchedulerMetadata> {
    let has_interrupted_replay = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND status='interrupted')",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    let has_queued_input = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND status='queued')",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    // Keep the original window and ordering, including non-error events.
    let mut query = tx.prepare(
        "SELECT kind,created_at FROM audit_events WHERE agent_id=?1
         ORDER BY event_seq DESC,created_at DESC LIMIT 64",
    )?;
    let mut rows = query.query([&scope.agent_id])?;
    let mut latest_runtime_error = None;
    while let Some(row) = rows.next()? {
        let kind = super::repositories::borrowed_sql_text(row, 0)?;
        let at =
            super::repositories::parse_timestamp(super::repositories::borrowed_sql_text(row, 1)?)?;
        if kind == "runtime_error" {
            latest_runtime_error = Some(
                latest_runtime_error.map_or(at, |previous: chrono::DateTime<chrono::Utc>| {
                    previous.max(at)
                }),
            );
        }
    }
    let (recent_brief_count, recent_event_count) = summary_counts(tx, &scope.agent_id)?;
    Ok(CurrentSchedulerMetadata {
        has_interrupted_replay,
        has_queued_input,
        latest_runtime_error,
        recent_brief_count,
        recent_event_count,
    })
}

impl CurrentSelection {
    pub(crate) fn covers(&self, other: &Self) -> bool {
        other.queue_ids.is_subset(&self.queue_ids)
            && other
                .events_after
                .is_none_or(|after| self.events_after.is_some_and(|base| base <= after))
    }
}

#[derive(Debug)]
pub(crate) struct CurrentQuery {
    pub scope: InputEvidenceScope,
    pub revision: u64,
    pub event_cursor: u64,
    pub selection: CurrentSelection,
    pub identity: Arc<crate::types::AgentIdentityRecord>,
    pub scheduler: CurrentSchedulerMetadata,
    queue: BTreeMap<String, Option<Arc<OwnedCurrentRecord>>>,
    events: BTreeMap<u64, Arc<OwnedCurrentRecord>>,
    ledger: InputEvidenceBudgetLedger,
    _material: Arc<BudgetViewGuard>,
    _scope_material: Option<Arc<BudgetViewGuard>>,
    _identity_material: Option<Arc<BudgetViewGuard>>,
}

/// Frozen under the mutation owner before business SQL. It owns no Root.
#[derive(Debug)]
pub(crate) struct CurrentQuerySource {
    pub baseline: Arc<CurrentQuery>,
    pub selection: Arc<CurrentSelection>,
    pub owner_epoch: u64,
    pub roles: crate::runtime::current_queries::CurrentRoles,
}

#[derive(Debug)]
pub(crate) struct CurrentQueryPublication {
    pub baseline: Arc<CurrentQuery>,
    pub next: Arc<CurrentQuery>,
}

#[derive(Debug)]
pub(crate) struct CapturedCurrent {
    pub event_cursor: u64,
    pub events: Vec<Arc<OwnedCurrentRecord>>,
    _queue: Vec<Option<Arc<OwnedCurrentRecord>>>,
    _material: crate::diagnostics::resource_io::ResourceReadMaterial,
}

impl CapturedCurrent {
    pub(crate) fn events(&self) -> impl DoubleEndedIterator<Item = &crate::types::AuditEvent> {
        self.events.iter().map(|body| match body.record() {
            CurrentRecord::Event(event) => &**event,
            _ => unreachable!("Current Event membership has a different family"),
        })
    }
}

impl CurrentQuery {
    pub(crate) fn queue_record(&self, id: &str) -> Result<Option<&Arc<OwnedCurrentRecord>>> {
        self.queue
            .get(id)
            .map(Option::as_ref)
            .context("Current Queue membership needs preparation")
    }

    pub(crate) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.ledger.same_ledger(ledger),
            "Current query scope or ledger differs"
        );
        self._material
            .validate_owned_material_binding(scope, ledger, 0)?;
        Ok(())
    }

    pub(crate) fn capture(
        &self,
        selection: &CurrentSelection,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<CapturedCurrent> {
        self.validate_binding(&self.scope, ledger)?;
        ensure!(
            self.selection.covers(selection)
                && selection
                    .events_after
                    .is_none_or(|after| after <= self.event_cursor),
            "Current query needs preparation"
        );
        let material =
            crate::diagnostics::resource_io::ResourceReadMaterial::new(ledger, &self.scope)?;
        let (events, queue) = material.read(|| {
            let count = self
                .events
                .iter()
                .filter(|(seq, _)| selection.events_after.is_some_and(|after| **seq > after))
                .count()
                .checked_add(selection.queue_ids.len())
                .context("Current capture count overflow")?;
            crate::diagnostics::resource_io::charge_owned_metadata(
                u64::try_from(
                    count
                        .checked_mul(64)
                        .context("Current capture bytes overflow")?,
                )?,
                count,
            )?;
            let events = self
                .events
                .iter()
                .filter(|(seq, _)| selection.events_after.is_some_and(|after| **seq > after))
                .map(|(_, body)| body.clone())
                .collect();
            let queue = selection
                .queue_ids
                .iter()
                .map(|id| {
                    self.queue
                        .get(id)
                        .cloned()
                        .context("Current Queue membership needs preparation")
                })
                .collect::<Result<_>>()?;
            Ok((events, queue))
        })?;
        Ok(CapturedCurrent {
            event_cursor: self.event_cursor,
            events,
            _queue: queue,
            _material: material,
        })
    }
}

impl CurrentQuerySource {
    pub(in crate::runtime_db) fn validate_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        revision: u64,
        material: &InputReadMaterial<'_>,
    ) -> Result<()> {
        self.roles
            .validate(scope, self.owner_epoch, &self.selection)?;
        self.baseline.validate_binding(scope, self.roles.ledger())?;
        material.validate_binding(scope)?;
        material.validate_guard_binding(&self.baseline._material)?;
        ensure!(
            revision == self.baseline.revision
                && self.baseline.selection.covers(&self.selection)
                && event_cursor(tx, scope)? == self.baseline.event_cursor
                && scheduler_metadata(tx, scope)? == self.baseline.scheduler,
            "Current writer query baseline needs preparation"
        );
        // No body decode or new authority. Detect raw additions, removals and
        // changes to the exact accepted selection before business SQL.
        if let Some(after) = self.selection.events_after {
            let count: usize = tx.query_row(
                "SELECT COUNT(*) FROM audit_events WHERE agent_id=?1 AND event_seq>?2 AND event_seq<=?3",
                params![scope.agent_id, after, self.baseline.event_cursor], |row| row.get(0),
            )?;
            ensure!(
                count
                    == self
                        .baseline
                        .events
                        .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
                        .count(),
                "Current Event interval changed outside its original writers"
            );
            for body in self.baseline.events.values().filter(|body| {
                matches!(body.record(), CurrentRecord::Event(event) if event.event_seq > after)
            }) {
                validate_body(tx, scope, CurrentFamily::Event, body)?;
            }
        }
        for id in &self.selection.queue_ids {
            match self
                .baseline
                .queue
                .get(id)
                .context("Current Queue source needs preparation")?
            {
                Some(body) => validate_body(tx, scope, CurrentFamily::Queue, body)?,
                None => ensure!(
                    !tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1)",
                        [id],
                        |row| row.get::<_, bool>(0)
                    )?,
                    "Current absent Queue address changed"
                ),
            }
        }
        Ok(())
    }

    pub(in crate::runtime_db) fn finish(
        &self,
        tx: &Transaction<'_>,
        writes: &super::current_read_writes::CommittedCurrentWrites,
        material: &mut InputReadMaterial<'_>,
    ) -> Result<CurrentQueryPublication> {
        let mut count = self.selection.queue_ids.len();
        let mut bytes = 1024usize;
        for id in &self.selection.queue_ids {
            bytes = bytes
                .checked_add(id.len() + 256)
                .context("Current query bytes overflow")?;
        }
        for body in self.baseline.events.values().chain(writes.records()) {
            if matches!(body.record(), CurrentRecord::Event(event)
                if self.selection.events_after.is_some_and(|after| event.event_seq > after))
            {
                count = count
                    .checked_add(1)
                    .context("Current query count overflow")?;
            }
        }
        bytes = bytes
            .checked_add(
                count
                    .checked_mul(256)
                    .context("Current query bytes overflow")?,
            )
            .context("Current query bytes overflow")?;
        let guard = material.observe_native_footprint(
            bytes,
            count
                .checked_add(8)
                .context("Current query nodes overflow")?,
        )?;
        let mut queue = BTreeMap::new();
        for id in &self.selection.queue_ids {
            let body = match writes.outcome(CurrentFamily::Queue, id) {
                Some((CurrentAddressFact::Qualified(_), Some(body))) => Some(body.clone()),
                Some((CurrentAddressFact::Unborn | CurrentAddressFact::Absent, None)) => None,
                Some(_) => anyhow::bail!("Current Queue writer needs preparation"),
                None => self
                    .baseline
                    .queue
                    .get(id)
                    .cloned()
                    .context("Current Queue source needs preparation")?,
            };
            queue.insert(id.clone(), body);
        }
        let cursor = event_cursor(tx, &writes.scope)?;
        let mut events = BTreeMap::new();
        if let Some(after) = self.selection.events_after {
            for (seq, body) in &self.baseline.events {
                if *seq > after {
                    events.insert(*seq, body.clone());
                }
            }
            for body in writes.records() {
                if let CurrentRecord::Event(event) = body.record() {
                    if event.event_seq > after {
                        events.insert(event.event_seq, body.clone());
                    }
                }
            }
            // Sparse ACKs do not prove that all inserted rows were returned.
            let actual: usize = tx.query_row(
                "SELECT COUNT(*) FROM audit_events WHERE agent_id=?1 AND event_seq>?2 AND event_seq<=?3",
                params![writes.scope.agent_id, after, cursor], |row| row.get(0),
            )?;
            ensure!(
                actual == events.len(),
                "Current Event ACK omitted selected membership: canonical={actual}, selected={}, after={after}, cursor={cursor}",
                events.len()
            );
            for body in events.values() {
                let CurrentRecord::Event(event) = body.record() else {
                    unreachable!()
                };
                ensure!(
                    !writes
                        .outcome(CurrentFamily::Event, &event.id)
                        .is_some_and(|(fact, _)| {
                            !matches!(fact, CurrentAddressFact::Qualified(_))
                        }),
                    "Current Event writer invalidated selected membership"
                );
            }
        }
        let next = Arc::new(CurrentQuery {
            scope: self.baseline.scope.clone(),
            revision: writes.revision,
            event_cursor: cursor,
            selection: self.selection.as_ref().clone(),
            identity: self.baseline.identity.clone(),
            scheduler: scheduler_metadata(tx, &writes.scope)?,
            queue,
            events,
            ledger: self.roles.ledger().clone(),
            _material: guard,
            _scope_material: None,
            _identity_material: self.baseline._identity_material.clone(),
        });
        Ok(CurrentQueryPublication {
            baseline: self.baseline.clone(),
            next,
        })
    }
}

fn validate_body(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    family: CurrentFamily,
    body: &OwnedCurrentRecord,
) -> Result<()> {
    let id = match body.record() {
        CurrentRecord::Queue(queue) => &queue.message_id,
        CurrentRecord::Event(event) => &event.id,
    };
    let mut statement = tx.prepare(family.sql())?;
    let mut rows = statement.query([id])?;
    let row = rows.next()?.context("Current accepted body was removed")?;
    ensure!(
        row.get_ref(0)?.as_str()? == scope.agent_id
            && digest(row.get_ref(1)?.as_str()?) == body.content_digest,
        "Current accepted body changed outside its original writer"
    );
    Ok(())
}

pub(crate) fn event_cursor(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<u64> {
    Ok(tx.query_row(
        "SELECT COALESCE(MAX(event_seq),0) FROM audit_events WHERE agent_id=?1",
        [&scope.agent_id],
        |row| row.get(0),
    )?)
}

/// Explicit read-TX preparation. Never called from a pure capture.
pub(crate) fn prepare(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selection: Option<&CurrentSelection>,
    ledger: &InputEvidenceBudgetLedger,
    limits: (usize, usize),
) -> Result<Arc<CurrentQuery>> {
    prepare_inner(tx, scope, selection, ledger, limits, None)
}

pub(crate) fn prepare_with_scope_read(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selection: Option<&CurrentSelection>,
    ledger: &InputEvidenceBudgetLedger,
    limits: (usize, usize),
    scope_read: &super::context_inputs::ContextScopeRead<'_>,
) -> Result<Arc<CurrentQuery>> {
    prepare_inner(tx, scope, selection, ledger, limits, Some(scope_read))
}

fn prepare_inner(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selection: Option<&CurrentSelection>,
    ledger: &InputEvidenceBudgetLedger,
    limits: (usize, usize),
    scope_read: Option<&super::context_inputs::ContextScopeRead<'_>>,
) -> Result<Arc<CurrentQuery>> {
    let mut material = InputReadMaterial::new(scope, ledger, limits, None)?;
    let active = tx.query_row(
        "SELECT status='active' AND json_extract(payload_json,'$.agent_id')=?1 AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' THEN json_extract(payload_json,'$.incarnation') END=?2 AND json_extract(payload_json,'$.status')=status FROM agent_identities WHERE agent_id=?1",
        params![scope.agent_id, scope.identity_incarnation], |row| row.get::<_, bool>(0),
    ).optional()?.unwrap_or(false);
    ensure!(active, "Current query requires an active exact identity");
    let identity = if let Some(scope_read) = scope_read {
        scope_read.validate(tx, scope)?;
        let identity = scope_read
            .facts()
            .identity
            .as_ref()
            .context("Current identity is absent")?;
        crate::diagnostics::resource_io::admit_cloned_value(identity)?;
        Arc::new(identity.clone())
    } else {
        Arc::new(
            super::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
                .identity_fact()?
                .context("Current identity is absent")?,
        )
    };
    ensure!(
        identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation
            && identity.status == crate::types::AgentRegistryStatus::Active,
        "Current identity differs"
    );
    let cursor = event_cursor(tx, scope)?;
    let baseline = CurrentSelection {
        events_after: Some(cursor),
        ..Default::default()
    };
    let selection = selection.unwrap_or(&baseline);
    ensure!(
        selection.events_after.is_none_or(|after| after <= cursor),
        "Current query frontier is not prepared"
    );
    let revision = tx.query_row(
        "SELECT revision FROM current_read_revisions WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id, scope.identity_incarnation], |row| row.get::<_, u64>(0),
    ).optional()?.unwrap_or(0);
    let (count, raw): (usize, usize) = match selection.events_after {
        Some(after) => tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(audit_event_id AS BLOB))),0) FROM audit_events WHERE agent_id=?1 AND event_seq>?2 AND event_seq<=?3",
            params![scope.agent_id, after, cursor], |row| Ok((row.get(0)?,row.get(1)?)),
        )?,
        None => (0, 0),
    };
    let nodes = count
        .checked_add(selection.queue_ids.len())
        .and_then(|n| n.checked_add(8))
        .context("Current source nodes overflow")?;
    let selected_bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
        selection,
    )?)?;
    let bytes = nodes
        .checked_mul(256)
        .and_then(|n| n.checked_add(raw))
        .and_then(|n| n.checked_add(selected_bytes))
        .context("Current source bytes overflow")?;
    let guard = material.observe_native_footprint(bytes, nodes)?;
    let mut queue = BTreeMap::new();
    for id in &selection.queue_ids {
        queue.insert(
            id.clone(),
            load_body(tx, scope, CurrentFamily::Queue, id, &mut material)?,
        );
    }
    let mut events = BTreeMap::new();
    if let Some(after) = selection.events_after {
        let mut statement = tx.prepare(
            "SELECT audit_event_id,event_seq FROM audit_events WHERE agent_id=?1 AND event_seq>?2 AND event_seq<=?3 ORDER BY event_seq",
        )?;
        let mut rows = statement.query(params![scope.agent_id, after, cursor])?;
        while let Some(row) = rows.next()? {
            ensure!(events.len() < count, "Current source interval changed");
            let id = row.get_ref(0)?.as_str()?;
            let seq: u64 = row.get(1)?;
            let body = load_body(tx, scope, CurrentFamily::Event, id, &mut material)?
                .context("Current Event body is absent")?;
            ensure!(
                matches!(body.record(), CurrentRecord::Event(event) if event.event_seq == seq),
                "Current Event sequence differs from its original body"
            );
            events.insert(seq, body);
        }
        ensure!(
            count == events.len(),
            "Current source interval is incomplete"
        );
    }
    Ok(Arc::new(CurrentQuery {
        scope: scope.clone(),
        revision,
        event_cursor: cursor,
        selection: CurrentSelection {
            queue_ids: selection.queue_ids.clone(),
            events_after: Some(selection.events_after.unwrap_or(cursor)),
        },
        identity,
        scheduler: scheduler_metadata(tx, scope)?,
        queue,
        events,
        ledger: ledger.clone(),
        _material: guard,
        _scope_material: Some(material.scope_guard()?),
        _identity_material: crate::diagnostics::resource_io::ResourceReadMaterial::active_guard()?,
    }))
}

fn load_body(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    family: CurrentFamily,
    id: &str,
    material: &mut InputReadMaterial<'_>,
) -> Result<Option<Arc<OwnedCurrentRecord>>> {
    let mut statement = tx.prepare(family.sql())?;
    let mut rows = statement.query([id])?;
    let Some(row) = rows.next()? else {
        let state = tx.query_row(
            "SELECT qualification_state FROM current_read_qualification WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
            params![scope.agent_id, scope.identity_incarnation, family.name(), id],
            |row| row.get::<_, String>(0),
        ).optional()?;
        ensure!(
            matches!(state.as_deref(), Some("unborn" | "absent")),
            "Current absent address needs preparation"
        );
        return Ok(None);
    };
    let payload = row.get_ref(1)?.as_str()?;
    ensure!(
        row.get_ref(0)?.as_str()? == scope.agent_id,
        "Current body owner differs"
    );
    let hash = digest(payload);
    let qualified = tx.query_row(
        "SELECT qualification_state='qualified' AND content_digest=?5 FROM current_read_qualification WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
        params![scope.agent_id, scope.identity_incarnation, family.name(), id, hash],
        |row| row.get::<_, bool>(0),
    ).optional()?.unwrap_or(false);
    ensure!(qualified, "Current body needs original writer preparation");
    let (address_material, guard) = if family == CurrentFamily::Event {
        // One typed Event, not a completion/resolver aggregate. Gate SQLite
        // traversal before counting nodes, and owning cells before decoding.
        let raw = material.observe_native_footprint(
            payload
                .len()
                .checked_add(256)
                .context("Event raw bytes overflow")?,
            1,
        )?;
        let nodes: usize =
            match tx.query_row("SELECT COUNT(*) FROM json_tree(?1)", [payload], |row| {
                row.get(0)
            }) {
                Ok(nodes) => nodes,
                Err(error) => {
                    let (bytes, nodes) =
                        crate::diagnostics::resource_io::json_payload_material(payload)?;
                    material.observe_native_footprint(bytes, nodes)?;
                    // Preserve the canonical decoder's field/syntax error order.
                    serde_json::from_str::<crate::types::AuditEvent>(payload)?;
                    return Err(error.into());
                }
            };
        let decoded = material.observe_native_footprint(
            nodes
                .checked_mul(std::mem::size_of::<serde_json::Value>())
                .and_then(|bytes| bytes.checked_add(std::mem::size_of::<OwnedCurrentRecord>()))
                .context("Event node bytes overflow")?,
            nodes,
        )?;
        (raw, decoded)
    } else {
        let (bytes, nodes) = crate::diagnostics::resource_io::json_payload_material(payload)?;
        let guard = material.observe_native_footprint(bytes, nodes)?;
        (guard.clone(), guard)
    };
    let record = match family {
        CurrentFamily::Queue => {
            let queue: crate::types::QueueEntryRecord = serde_json::from_str(payload)?;
            ensure!(
                queue.message_id == id && queue.agent_id == scope.agent_id,
                "Current Queue body address differs"
            );
            CurrentRecord::Queue(queue)
        }
        CurrentFamily::Event => {
            let event: crate::types::AuditEvent = serde_json::from_str(payload)?;
            ensure!(event.id == id, "Current Event body address differs");
            CurrentRecord::Event(super::transitions::AuditEventAck::new(event)?)
        }
    };
    Ok(Some(Arc::new(OwnedCurrentRecord {
        record,
        content_digest: hash,
        _address_material: address_material,
        _material: guard,
    })))
}
