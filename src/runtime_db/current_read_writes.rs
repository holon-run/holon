//! Original Queue/Event observations. A later canonical read is not authority.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

use super::input_evidence_snapshot::read_material::InputReadMaterial;
use crate::runtime::{
    input_evidence_budget::BudgetViewGuard, resident_input_evidence::InputEvidenceScope,
};

pub(super) const VERSION: i64 = 103;
pub(super) const SCHEMA: &str = r#"
CREATE TABLE current_read_revisions (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 revision INTEGER NOT NULL CHECK(revision>=0),
 PRIMARY KEY(agent_id,identity_incarnation));
CREATE TABLE current_read_qualification (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 family TEXT NOT NULL CHECK(family IN ('queue','event')),
 record_id TEXT NOT NULL CHECK(length(CAST(record_id AS BLOB))>0),
 revision INTEGER NOT NULL CHECK(revision>0),
 qualification_state TEXT NOT NULL CHECK(qualification_state IN ('qualified','unborn','absent','unprepared')),
 content_digest TEXT,
 CHECK((qualification_state='qualified' AND content_digest IS NOT NULL AND length(content_digest)=64)
    OR (qualification_state IN ('unborn','absent','unprepared') AND content_digest IS NULL)),
 PRIMARY KEY(agent_id,identity_incarnation,family,record_id),
 FOREIGN KEY(agent_id,identity_incarnation) REFERENCES current_read_revisions(agent_id,identity_incarnation));
CREATE INDEX current_read_known_address ON current_read_qualification(family,record_id);
CREATE TABLE event_history_acks (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 audit_event_id TEXT NOT NULL CHECK(length(CAST(audit_event_id AS BLOB))>0),
 event_seq INTEGER NOT NULL CHECK(event_seq>0),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 PRIMARY KEY(agent_id,identity_incarnation,audit_event_id));
CREATE INDEX event_history_ack_address ON event_history_acks(audit_event_id);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CurrentFamily {
    Queue,
    Event,
}

/// Unborn preserves first-birth eligibility; deletion and refusal fence an ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CurrentAddressFact {
    Qualified(String),
    Unborn,
    Absent,
    Unprepared,
}

/// Canonical body decoded in the acknowledging transaction, never a later loan.
#[derive(Debug)]
pub(crate) enum CurrentRecord {
    Queue(crate::types::QueueEntryRecord),
    Event(super::transitions::AuditEventAck),
}

#[derive(Debug)]
pub(crate) struct OwnedCurrentRecord {
    pub(super) record: CurrentRecord,
    pub(super) content_digest: String,
    pub(super) _material: Arc<BudgetViewGuard>,
    pub(super) _address_material: Arc<BudgetViewGuard>,
}

enum CanonicalEvents<'a> {
    #[cfg(test)]
    Owned(&'a [crate::types::AuditEvent]),
    Shared(&'a [super::transitions::AuditEventAck]),
}

impl CanonicalEvents<'_> {
    fn find(
        &self,
        id: &str,
    ) -> Option<(
        &crate::types::AuditEvent,
        Option<&super::transitions::AuditEventAck>,
    )> {
        match self {
            #[cfg(test)]
            Self::Owned(events) => events
                .iter()
                .find(|event| event.id == id)
                .map(|event| (event, None)),
            Self::Shared(events) => events
                .iter()
                .find(|event| event.id == id)
                .map(|event| (&**event, Some(event))),
        }
    }
}

impl OwnedCurrentRecord {
    pub(crate) fn record(&self) -> &CurrentRecord {
        &self.record
    }

    #[cfg(test)]
    pub(crate) fn weak_material(&self) -> std::sync::Weak<BudgetViewGuard> {
        Arc::downgrade(&self._material)
    }
}

/// Only an original qualified Current ACK can supply a live notification body.
#[derive(Debug, Clone)]
pub(crate) struct CommittedEventRead(Arc<OwnedCurrentRecord>);

impl CommittedEventRead {
    pub(crate) fn event(&self) -> &crate::types::AuditEvent {
        match self.0.record() {
            CurrentRecord::Event(event) => event,
            _ => unreachable!("committed Event read has another family"),
        }
    }

    pub(crate) fn validate(
        &self,
        scope: &InputEvidenceScope,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<()> {
        self.0
            ._material
            .validate_owned_material_binding(scope, ledger, 0)?;
        self.0
            ._address_material
            .validate_owned_material_binding(scope, ledger, 0)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum OriginalAddress {
    MayWrite,
    MayBirth,
    AbsentOnly,
    Unprepared,
}

/// One pre-admitted address slot, completed in place by the original TX.
#[derive(Debug, Clone)]
struct CurrentAddressSlot {
    original: OriginalAddress,
    fact: Option<CurrentAddressFact>,
    record: Option<Arc<OwnedCurrentRecord>>,
}

impl CurrentFamily {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Event => "event",
        }
    }
    pub(super) fn sql(self) -> &'static str {
        match self {
            Self::Queue => "SELECT agent_id,payload_json FROM queue_entries WHERE message_id=?1",
            Self::Event => "SELECT agent_id,data_json FROM audit_events WHERE audit_event_id=?1",
        }
    }
}

/// Accounting and original-address authority are separate. This proof is not
/// Clone/Serde and cannot be rebound to another transaction.
pub(in crate::runtime_db) struct OriginalCurrentWrites<'tx, 'conn> {
    tx: &'tx Transaction<'conn>,
    scope: InputEvidenceScope,
    base_revision: i64,
    active: bool,
    observed: BTreeMap<(CurrentFamily, String), CurrentAddressSlot>,
    guards: Vec<Arc<BudgetViewGuard>>,
    query_source: Option<Arc<super::current_query::CurrentQuerySource>>,
}

#[derive(Debug, Clone)]
pub(crate) struct CommittedCurrentWrites {
    pub scope: InputEvidenceScope,
    pub base_revision: u64,
    pub revision: u64,
    /// Actual outcomes in original address slots, not complete-domain proof.
    addresses: BTreeMap<(CurrentFamily, String), CurrentAddressSlot>,
    _material: Vec<Arc<BudgetViewGuard>>,
    pub(crate) query: Option<Arc<super::current_query::CurrentQuery>>,
    pub(crate) query_publication: Option<Arc<super::current_query::CurrentQueryPublication>>,
}

impl<'tx, 'conn> OriginalCurrentWrites<'tx, 'conn> {
    pub(in crate::runtime_db) fn bind_query_source(
        &mut self,
        source: Option<&Arc<super::current_query::CurrentQuerySource>>,
        material: &InputReadMaterial<'_>,
    ) -> Result<()> {
        if let Some(source) = source {
            source.validate_tx(
                self.tx,
                &self.scope,
                u64::try_from(self.base_revision)?,
                material,
            )?;
            self.query_source = Some(source.clone());
        }
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::runtime_db) fn slot_identity_for_fixture(
        &self,
        key: &(CurrentFamily, String),
    ) -> (usize, usize) {
        let (key, slot) = self.observed.get_key_value(key).expect("original address");
        (
            key.1.as_ptr() as usize,
            slot as *const CurrentAddressSlot as usize,
        )
    }

    pub(in crate::runtime_db) fn prepare(
        tx: &'tx Transaction<'conn>,
        scope: &InputEvidenceScope,
        material: &mut InputReadMaterial<'_>,
        mut visit: impl FnMut(&mut dyn FnMut(CurrentFamily, &str) -> Result<()>) -> Result<()>,
    ) -> Result<Option<Self>> {
        material.validate_binding(scope)?;
        let tables: usize = tx.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('current_read_revisions','current_read_qualification')",
            [], |row| row.get(0),
        )?;
        if tables == 0 {
            let migrations: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
                [], |row| row.get(0),
            )?;
            let installed = migrations
                && tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version>=?1)",
                    [VERSION],
                    |row| row.get::<_, bool>(0),
                )?;
            ensure!(!installed, "installed Current source is incomplete");
            return Ok(None);
        }
        ensure!(tables == 2, "installed Current source is incomplete");
        // Scope and address map share one owner and one material reservation.
        let mut bytes = scope
            .agent_id
            .len()
            .checked_add(1024)
            .context("Current scope overflow")?;
        let mut nodes = 8usize;
        visit(&mut |_, id| {
            bytes = bytes
                .checked_add(id.len())
                .and_then(|n| n.checked_add(1024))
                .context("Current address overflow")?;
            nodes = nodes
                .checked_add(8)
                .context("Current address nodes overflow")?;
            Ok(())
        })?;
        let guard = material.observe_native_footprint(bytes, nodes)?;
        let active: bool = tx.query_row(
            "SELECT status='active' FROM agent_identities WHERE agent_id=?1 AND status IN ('active','deleting') AND json_extract(payload_json,'$.agent_id')=?1 AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' THEN json_extract(payload_json,'$.incarnation') END=?2 AND json_extract(payload_json,'$.status')=status",
            params![scope.agent_id, scope.identity_incarnation], |row| row.get(0),
        ).optional()?.ok_or(super::input_evidence_snapshot::InputEvidenceSnapshotError::RetiredScope)?;
        tx.execute(
            "INSERT INTO current_read_revisions VALUES(?1,?2,0) ON CONFLICT DO NOTHING",
            params![scope.agent_id, scope.identity_incarnation],
        )?;
        let base_revision = tx.query_row(
            "SELECT revision FROM current_read_revisions WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope.agent_id, scope.identity_incarnation], |row| row.get(0),
        )?;
        let mut proof = Self {
            tx,
            scope: scope.clone(),
            base_revision,
            active,
            observed: BTreeMap::new(),
            guards: vec![guard],
            query_source: None,
        };
        let mut copied_bytes = scope
            .agent_id
            .len()
            .checked_add(1024)
            .context("Current scope overflow")?;
        let mut copied_nodes = 8usize;
        visit(&mut |family, id| {
            copied_bytes = copied_bytes
                .checked_add(id.len())
                .and_then(|n| n.checked_add(1024))
                .context("Current address overflow")?;
            copied_nodes = copied_nodes
                .checked_add(8)
                .context("Current address nodes overflow")?;
            ensure!(
                copied_bytes <= bytes && copied_nodes <= nodes,
                "Current original footprint changed"
            );
            proof.observe(family, id.to_owned())
        })?;
        ensure!(
            copied_bytes == bytes && copied_nodes == nodes,
            "Current original footprint changed"
        );
        Ok(Some(proof))
    }

    fn observe(&mut self, family: CurrentFamily, id: String) -> Result<()> {
        ensure!(!id.is_empty(), "Current original address is empty");
        let tx = self.tx;
        let scope = &self.scope;
        let mut statement = tx.prepare(family.sql())?;
        let mut rows = statement.query([&id])?;
        let original = if let Some(row) = rows.next()? {
            let owner = row.get_ref(0)?;
            let payload = row.get_ref(1)?.as_str()?;
            let qualified = owner.as_str().ok() == Some(scope.agent_id.as_str()) &&
                    tx.query_row(
                        "SELECT qualification_state,content_digest FROM current_read_qualification WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
                        params![scope.agent_id, scope.identity_incarnation, family.name(), id],
                        |row| Ok(row.get_ref(0)?.as_str()? == "qualified"
                            && row.get_ref(1)?.as_str()? == digest(payload)),
                    ).optional()?.unwrap_or(false);
            if qualified {
                OriginalAddress::MayWrite
            } else {
                OriginalAddress::Unprepared
            }
        } else {
            let state = tx.query_row(
                    "SELECT qualification_state FROM current_read_qualification WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
                    params![scope.agent_id, scope.identity_incarnation, family.name(), id],
                    |row| row.get::<_, String>(0),
                ).optional()?;
            if self.active && state.as_deref() == Some("unborn") {
                    OriginalAddress::MayBirth
                } else if matches!(state.as_deref(), Some("unborn" | "absent")) {
                    OriginalAddress::AbsentOnly
                } else if self.active && !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM current_read_qualification WHERE family=?1 AND record_id=?2)",
                    params![family.name(), id], |row| row.get::<_, bool>(0),
                )? {
                    OriginalAddress::MayBirth
                } else {
                    OriginalAddress::Unprepared
                }
        };
        self.observed
            .entry((family, id))
            .or_insert(CurrentAddressSlot {
                original,
                fact: None,
                record: None,
            });
        Ok(())
    }

    /// Freeze possible helper addresses before business SQL. Wait identifiers
    /// are only address inputs: this does not qualify Wait or Event bodies.
    pub(in crate::runtime_db) fn prepare_trigger_events<'a>(
        &mut self,
        message_id: &str,
        new_wait_ids: impl IntoIterator<Item = &'a str> + Clone,
        material: &mut InputReadMaterial<'_>,
    ) -> Result<()> {
        material.validate_binding(&self.scope)?;
        for guard in &self.guards {
            material.validate_guard_binding(guard)?;
        }
        let tx = self.tx;
        let mut bytes = 0usize;
        let mut nodes = 0usize;
        {
            let mut count = |wait: Option<&str>, stale: bool| -> Result<()> {
                bytes = bytes
                    .checked_add(Self::trigger_address_bytes(message_id, wait, stale)?)
                    .and_then(|n| n.checked_add(1024))
                    .context("Current generated address overflow")?;
                nodes = nodes
                    .checked_add(8)
                    .context("Current generated address nodes overflow")?;
                Ok(())
            };
            count(None, false)?;
            count(None, true)?;
            let mut statement = tx.prepare(
                "SELECT wait_condition_id FROM wait_conditions
                 WHERE agent_id=?1 AND status IN ('active','triggered')
                 ORDER BY updated_at DESC,created_at DESC,wait_condition_id DESC",
            )?;
            let mut rows = statement.query([&self.scope.agent_id])?;
            while let Some(row) = rows.next()? {
                count(Some(row.get_ref(0)?.as_str()?), false)?;
            }
            for id in new_wait_ids.clone() {
                count(Some(id), false)?;
            }
        }
        self.guards
            .push(material.observe_native_footprint(bytes, nodes)?);
        let mut remaining = (bytes, nodes);
        for stale in [false, true] {
            self.observe_trigger_event(message_id, None, stale, &mut remaining)?;
        }
        let mut statement = tx.prepare(
            "SELECT wait_condition_id FROM wait_conditions
             WHERE agent_id=?1 AND status IN ('active','triggered')
             ORDER BY updated_at DESC,created_at DESC,wait_condition_id DESC",
        )?;
        let mut rows = statement.query([&self.scope.agent_id])?;
        while let Some(row) = rows.next()? {
            let id = row.get_ref(0)?.as_str()?;
            self.observe_trigger_event(message_id, Some(id), false, &mut remaining)?;
        }
        for id in new_wait_ids {
            self.observe_trigger_event(message_id, Some(id), false, &mut remaining)?;
        }
        ensure!(remaining == (0, 0), "Current generated footprint changed");
        Ok(())
    }

    pub(in crate::runtime_db) fn prepare_timer_cancellation_events(
        &mut self,
        command: &super::TimerCancel,
        material: &mut InputReadMaterial<'_>,
    ) -> Result<()> {
        material.validate_binding(&self.scope)?;
        for guard in &self.guards {
            material.validate_guard_binding(guard)?;
        }
        let tx = self.tx;
        let visit_addresses = |visit: &mut dyn FnMut(&str, &str) -> Result<()>| -> Result<()> {
            if command.expected.status == crate::types::TimerStatus::Active {
                visit("audit:timer-cancelled:", &command.record.id)?;
            }
            super::repositories::visit_timer_cancellation_wait_ids_tx(tx, &command.record, |id| {
                visit("audit:timer-wait-cancelled:", id)
            })
        };
        let address_bytes = |prefix: &str, id: &str| {
            prefix
                .len()
                .checked_add(id.len())
                .and_then(|n| n.checked_add(1024))
                .context("Current generated address overflow")
        };
        let mut bytes = 0usize;
        let mut nodes = 0usize;
        visit_addresses(&mut |prefix, id| {
            bytes = bytes
                .checked_add(address_bytes(prefix, id)?)
                .context("Current generated address overflow")?;
            nodes = nodes
                .checked_add(8)
                .context("Current generated address nodes overflow")?;
            Ok(())
        })?;
        self.guards
            .push(material.observe_native_footprint(bytes, nodes)?);
        let mut remaining = (bytes, nodes);
        visit_addresses(&mut |prefix, id| {
            remaining.0 = remaining
                .0
                .checked_sub(address_bytes(prefix, id)?)
                .context("Current generated footprint changed")?;
            remaining.1 = remaining
                .1
                .checked_sub(8)
                .context("Current generated footprint changed")?;
            self.observe(CurrentFamily::Event, format!("{prefix}{id}"))
        })?;
        ensure!(remaining == (0, 0), "Current generated footprint changed");
        Ok(())
    }

    fn observe_trigger_event(
        &mut self,
        message_id: &str,
        wait_id: Option<&str>,
        stale: bool,
        remaining: &mut (usize, usize),
    ) -> Result<()> {
        let bytes = Self::trigger_address_bytes(message_id, wait_id, stale)?
            .checked_add(1024)
            .context("Current generated address overflow")?;
        remaining.0 = remaining
            .0
            .checked_sub(bytes)
            .context("Current generated footprint changed")?;
        remaining.1 = remaining
            .1
            .checked_sub(8)
            .context("Current generated footprint changed")?;
        // Move the admitted identifier into its original map slot.
        let id = super::transitions::wait_trigger_audit_id(message_id, wait_id, stale);
        self.observe(CurrentFamily::Event, id)
    }

    fn trigger_address_bytes(
        message_id: &str,
        wait_id: Option<&str>,
        stale: bool,
    ) -> Result<usize> {
        let suffix = if stale {
            Some(0)
        } else {
            wait_id.map_or(Some(0), |id| id.len().checked_add(1))
        }
        .context("Current generated suffix overflow")?;
        let prefix = if stale {
            super::transitions::WAIT_STALE_AUDIT_PREFIX
        } else {
            super::transitions::WAIT_TRIGGERED_AUDIT_PREFIX
        };
        message_id
            .len()
            .checked_add(suffix)
            .and_then(|size| size.checked_add(prefix.len()))
            .context("Current generated address overflow")
    }

    #[cfg(test)]
    pub(in crate::runtime_db) fn finish(
        self,
        material: &mut InputReadMaterial<'_>,
        visit: impl FnOnce(&mut dyn FnMut(CurrentFamily, &str) -> Result<()>) -> Result<()>,
    ) -> Result<CommittedCurrentWrites> {
        self.finish_with_events(material, &[], visit)
    }

    #[cfg(test)]
    pub(in crate::runtime_db) fn finish_with_events(
        self,
        material: &mut InputReadMaterial<'_>,
        canonical_events: &[crate::types::AuditEvent],
        visit: impl FnOnce(&mut dyn FnMut(CurrentFamily, &str) -> Result<()>) -> Result<()>,
    ) -> Result<CommittedCurrentWrites> {
        self.finish_with_records(material, canonical_events, &[], visit)
    }

    #[cfg(test)]
    pub(in crate::runtime_db) fn finish_with_records(
        self,
        material: &mut InputReadMaterial<'_>,
        canonical_events: &[crate::types::AuditEvent],
        canonical_queue: &[crate::types::QueueEntryRecord],
        visit: impl FnOnce(&mut dyn FnMut(CurrentFamily, &str) -> Result<()>) -> Result<()>,
    ) -> Result<CommittedCurrentWrites> {
        self.finish_canonical_records(
            material,
            CanonicalEvents::Owned(canonical_events),
            canonical_queue,
            visit,
        )
    }

    pub(in crate::runtime_db) fn finish_with_audit_acks(
        self,
        material: &mut InputReadMaterial<'_>,
        canonical_events: &[super::transitions::AuditEventAck],
        visit: impl FnOnce(&mut dyn FnMut(CurrentFamily, &str) -> Result<()>) -> Result<()>,
    ) -> Result<CommittedCurrentWrites> {
        self.finish_with_shared_records(material, canonical_events, &[], visit)
    }

    pub(in crate::runtime_db) fn finish_with_shared_records(
        self,
        material: &mut InputReadMaterial<'_>,
        canonical_events: &[super::transitions::AuditEventAck],
        canonical_queue: &[crate::types::QueueEntryRecord],
        visit: impl FnOnce(&mut dyn FnMut(CurrentFamily, &str) -> Result<()>) -> Result<()>,
    ) -> Result<CommittedCurrentWrites> {
        self.finish_canonical_records(
            material,
            CanonicalEvents::Shared(canonical_events),
            canonical_queue,
            visit,
        )
    }

    fn finish_canonical_records(
        mut self,
        material: &mut InputReadMaterial<'_>,
        canonical_events: CanonicalEvents<'_>,
        canonical_queue: &[crate::types::QueueEntryRecord],
        visit: impl FnOnce(&mut dyn FnMut(CurrentFamily, &str) -> Result<()>) -> Result<()>,
    ) -> Result<CommittedCurrentWrites> {
        material.validate_binding(&self.scope)?;
        for guard in &self.guards {
            material.validate_guard_binding(guard)?;
        }
        let revision = self
            .base_revision
            .checked_add(1)
            .context("Current revision overflow")?;
        let mut touched = false;
        visit(&mut |family, id| {
            touched = true;
            // Prepared keys/tree slots already cover this in-place outcome.
            // A late address still needs admission and remains Unprepared.
            if !self
                .observed
                .keys()
                .any(|(f, key)| *f == family && key == id)
            {
                self.guards.push(
                    material.observe_native_footprint(
                        id.len()
                            .checked_mul(2)
                            .and_then(|bytes| bytes.checked_add(2048))
                            .context("Current ACK overflow")?,
                        16,
                    )?,
                );
                self.observed.insert(
                    (family, id.to_owned()),
                    CurrentAddressSlot {
                        original: OriginalAddress::Unprepared,
                        fact: None,
                        record: None,
                    },
                );
            }
            let slot = self
                .observed
                .iter_mut()
                .find(|((f, key), _)| *f == family && key == id)
                .map(|(_, slot)| slot)
                .expect("Current actual address slot");
            let mut statement = self.tx.prepare(family.sql())?;
            let mut rows = statement.query([id])?;
            let mut body = None;
            let fact = match (slot.original, rows.next()?) {
                (OriginalAddress::MayWrite | OriginalAddress::MayBirth, Some(row)) => {
                    ensure!(
                        row.get_ref(0)?.as_str()? == self.scope.agent_id,
                        "Current actual owner differs"
                    );
                    let payload = row.get_ref(1)?.as_str()?;
                    let canonical_event = (family == CurrentFamily::Event)
                        .then(|| canonical_events.find(id))
                        .flatten();
                    let canonical_queue = (family == CurrentFamily::Queue)
                        .then(|| canonical_queue.iter().find(|queue| queue.message_id == id))
                        .flatten();
                    let shared_guard = if let Some((event, ack)) = canonical_event {
                        validate_canonical_payload(event, payload)?;
                        ack.and_then(|ack| ack.original_material())
                            .filter(|guard| material.validate_guard_binding(guard).is_ok())
                            .cloned()
                    } else {
                        None
                    };
                    let guard = if let Some(guard) = shared_guard {
                        guard
                    } else {
                        let (bytes, nodes) = if let Some((event, _)) = canonical_event {
                            crate::diagnostics::resource_io::cloned_value_material(event)?
                        } else if let Some(queue) = canonical_queue {
                            validate_canonical_payload(queue, payload)?;
                            crate::diagnostics::resource_io::cloned_value_material(queue)?
                        } else {
                            crate::diagnostics::resource_io::json_payload_material(payload)?
                        };
                        material.observe_native_footprint(bytes, nodes)?
                    };
                    let record = match family {
                        CurrentFamily::Queue => {
                            let record: crate::types::QueueEntryRecord = match canonical_queue {
                                Some(queue) => queue.clone(),
                                None => serde_json::from_str(payload)?,
                            };
                            ensure!(
                                record.message_id == id && record.agent_id == self.scope.agent_id,
                                "Current Queue body differs from its original address"
                            );
                            CurrentRecord::Queue(record)
                        }
                        CurrentFamily::Event => {
                            let record = match canonical_event {
                                Some((_, Some(ack))) => ack.body_ack(),
                                Some((event, None)) => {
                                    let event = event.clone();
                                    ensure!(
                                        event.id == id,
                                        "Current Event body differs from its address"
                                    );
                                    super::transitions::AuditEventAck::new(event)?
                                }
                                None => {
                                    let event: crate::types::AuditEvent =
                                        serde_json::from_str(payload)?;
                                    ensure!(
                                        event.id == id,
                                        "Current Event body differs from its address"
                                    );
                                    super::transitions::AuditEventAck::new(event)?
                                }
                            };
                            ensure!(
                                record.id == id,
                                "Current Event body differs from its address"
                            );
                            CurrentRecord::Event(record)
                        }
                    };
                    body = Some(Arc::new(OwnedCurrentRecord {
                        record,
                        content_digest: digest(payload),
                        _material: guard,
                        _address_material: self
                            .guards
                            .first()
                            .context("Current address material missing")?
                            .clone(),
                    }));
                    CurrentAddressFact::Qualified(digest(payload))
                }
                (OriginalAddress::MayBirth, None) => CurrentAddressFact::Unborn,
                (OriginalAddress::MayWrite | OriginalAddress::AbsentOnly, None) => {
                    CurrentAddressFact::Absent
                }
                _ => CurrentAddressFact::Unprepared,
            };
            let (state, hash) = match &fact {
                CurrentAddressFact::Qualified(hash) => ("qualified", Some(hash.as_str())),
                CurrentAddressFact::Unborn => ("unborn", None),
                CurrentAddressFact::Absent => ("absent", None),
                CurrentAddressFact::Unprepared => ("unprepared", None),
            };
            self.tx.execute(
                "INSERT INTO current_read_qualification VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,qualification_state=excluded.qualification_state,content_digest=excluded.content_digest",
                params![self.scope.agent_id, self.scope.identity_incarnation, family.name(), id, revision, state, hash],
            )?;
            slot.fact = Some(fact);
            slot.record = body;
            Ok(())
        })?;
        self.observed.retain(|_, slot| slot.fact.is_some());
        let revision = if touched {
            ensure!(self.tx.execute(
                "UPDATE current_read_revisions SET revision=?3 WHERE agent_id=?1 AND identity_incarnation=?2 AND revision=?4",
                params![self.scope.agent_id, self.scope.identity_incarnation, revision, self.base_revision],
            )? == 1, "Current original revision changed");
            revision
        } else {
            self.base_revision
        };
        let query_source = self.query_source.take();
        let mut writes = CommittedCurrentWrites {
            scope: self.scope,
            base_revision: u64::try_from(self.base_revision)?,
            revision: u64::try_from(revision)?,
            addresses: self.observed,
            _material: self.guards,
            query: None,
            query_publication: None,
        };
        if let Some(source) = query_source {
            let publication = source.finish(self.tx, &writes, material)?;
            writes.query = Some(publication.next.clone());
            writes.query_publication = Some(Arc::new(publication));
        }
        Ok(writes)
    }
}

pub(super) fn digest(payload: &str) -> String {
    format!("{:x}", Sha256::digest(payload.as_bytes()))
}

/// Archive proof attached by original Event writers inside their own append
/// transaction, and frozen once by the legacy upgrade cut. It never grants
/// live Current authority; only the finite history page consumes it, against
/// the exact identity, sequence and body digest.
pub(crate) fn append_event_history_ack_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    event_id: &str,
) -> Result<()> {
    // Offline recovery also writes original Events before this domain exists.
    // Its later original upgrade cut adopts those Events. Gate on the committed
    // schema version, not table existence: a missing installed table must fail.
    let installed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version>=?1)",
        [VERSION],
        |row| row.get(0),
    )?;
    if !installed {
        return Ok(());
    }
    let (seq, payload): (i64, String) = tx
        .query_row(
            "SELECT event_seq,data_json FROM audit_events
             WHERE audit_event_id=?1 AND agent_id=?2",
            params![event_id, scope.agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("history ACK Event row is absent")?;
    ensure!(seq > 0, "history ACK Event sequence is not allocated");
    tx.execute(
        "INSERT INTO event_history_acks(agent_id,identity_incarnation,audit_event_id,event_seq,content_digest)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(agent_id,identity_incarnation,audit_event_id)
         DO UPDATE SET event_seq=excluded.event_seq,content_digest=excluded.content_digest",
        params![
            scope.agent_id,
            scope.identity_incarnation,
            event_id,
            seq,
            digest(&payload)
        ],
    )?;
    Ok(())
}

fn validate_canonical_payload(value: &impl serde::Serialize, payload: &str) -> Result<()> {
    struct MatchPayload<'a>(&'a [u8]);
    impl std::io::Write for MatchPayload<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.strip_prefix(bytes).ok_or_else(|| {
                std::io::Error::other("Current canonical ACK differs from SQL payload")
            })?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // Compare the original helper ACK without allocating or decoding a body.
    let mut matched = MatchPayload(payload.as_bytes());
    serde_json::to_writer(&mut matched, value)?;
    ensure!(
        matched.0.is_empty(),
        "Current canonical ACK differs from SQL payload"
    );
    Ok(())
}

impl CommittedCurrentWrites {
    pub(crate) fn qualify_notification(
        &self,
        event: &mut super::transitions::AuditEventAck,
    ) -> Result<()> {
        let record = self.event_notification(event)?;
        event.set_current_record(record);
        Ok(())
    }

    pub(crate) fn event_notification(
        &self,
        event: &crate::types::AuditEvent,
    ) -> Result<Option<CommittedEventRead>> {
        let Some((CurrentAddressFact::Qualified(_), Some(record))) =
            self.outcome(CurrentFamily::Event, &event.id)
        else {
            return Ok(None);
        };
        let CurrentRecord::Event(original) = record.record() else {
            anyhow::bail!("Current Event notification has another family");
        };
        ensure!(
            &**original == event,
            "Current Event notification differs from original ACK"
        );
        Ok(Some(CommittedEventRead(record.clone())))
    }

    pub(crate) fn prepared_query(query: Arc<super::current_query::CurrentQuery>) -> Self {
        Self {
            scope: query.scope.clone(),
            base_revision: query.revision,
            revision: query.revision,
            addresses: BTreeMap::new(),
            _material: Vec::new(),
            query: Some(query),
            query_publication: None,
        }
    }

    pub(super) fn records(&self) -> impl Iterator<Item = &Arc<OwnedCurrentRecord>> {
        self.addresses
            .values()
            .filter_map(|slot| slot.record.as_ref())
    }

    pub(super) fn outcome(
        &self,
        family: CurrentFamily,
        id: &str,
    ) -> Option<(&CurrentAddressFact, Option<&Arc<OwnedCurrentRecord>>)> {
        self.addresses
            .iter()
            .find(|((f, key), _)| *f == family && key == id)
            .and_then(|(_, slot)| slot.fact.as_ref().map(|fact| (fact, slot.record.as_ref())))
    }
    #[cfg(test)]
    pub(in crate::runtime_db) fn slot_identity_for_fixture(
        &self,
        key: &(CurrentFamily, String),
    ) -> (usize, usize) {
        let (key, slot) = self
            .addresses
            .get_key_value(key)
            .expect("acknowledged address");
        (
            key.1.as_ptr() as usize,
            slot as *const CurrentAddressSlot as usize,
        )
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.addresses.is_empty()
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.addresses.values().all(|slot| {
            slot.fact
                .as_ref()
                .is_some_and(|fact| !matches!(fact, CurrentAddressFact::Unprepared))
        })
    }

    #[cfg(test)]
    pub(crate) fn record(&self, key: &(CurrentFamily, String)) -> Option<&Arc<OwnedCurrentRecord>> {
        self.addresses.get(key)?.record.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn facts(&self) -> BTreeMap<(CurrentFamily, String), CurrentAddressFact> {
        self.addresses
            .iter()
            .map(|(key, slot)| {
                (
                    key.clone(),
                    slot.fact.clone().expect("Current acknowledged slot"),
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub(in crate::runtime_db) fn replace_record_for_fixture(
        &mut self,
        key: &(CurrentFamily, String),
        record: Arc<OwnedCurrentRecord>,
    ) {
        self.addresses
            .get_mut(key)
            .expect("Current fixture address")
            .record = Some(record);
    }

    #[cfg(test)]
    pub(in crate::runtime_db) fn qualifies(&self, key: &(CurrentFamily, String)) -> bool {
        matches!(
            self.addresses.get(key).and_then(|slot| slot.fact.as_ref()),
            Some(CurrentAddressFact::Qualified(_))
        )
    }

    #[cfg(test)]
    pub(in crate::runtime_db) fn weak_guard_for_fixture(&self) -> std::sync::Weak<BudgetViewGuard> {
        Arc::downgrade(self._material.last().expect("Current ACK material"))
    }

    pub(crate) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope
                && (self.revision == self.base_revision
                    || self.base_revision.checked_add(1) == Some(self.revision)),
            "Current ACK differs from original scope/revision"
        );
        for guard in &self._material {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        if let Some(query) = &self.query {
            query.validate_binding(scope, ledger)?;
        }
        if let Some(publication) = &self.query_publication {
            publication.baseline.validate_binding(scope, ledger)?;
            ensure!(
                self.query
                    .as_ref()
                    .is_some_and(|query| Arc::ptr_eq(query, &publication.next)),
                "Current publication differs from its acknowledged query"
            );
        }
        for (key, slot) in &self.addresses {
            let fact = slot
                .fact
                .as_ref()
                .context("Current ACK omitted an outcome")?;
            let Some(record) = &slot.record else {
                ensure!(
                    !matches!(fact, CurrentAddressFact::Qualified(_)),
                    "Current ACK omitted an owning qualified body"
                );
                continue;
            };
            let (family, id) = key;
            record
                ._material
                .validate_owned_material_binding(scope, ledger, 0)?;
            ensure!(
                matches!(fact, CurrentAddressFact::Qualified(hash)
                    if hash == &record.content_digest)
                    && match record.record() {
                        CurrentRecord::Queue(value) =>
                            *family == CurrentFamily::Queue
                                && value.message_id == *id
                                && value.agent_id == scope.agent_id,
                        CurrentRecord::Event(value) =>
                            *family == CurrentFamily::Event && value.id == *id,
                    },
                "Current owning body differs from its ACK"
            );
        }
        ensure!(
            self.addresses.values().all(|slot| match &slot.fact {
                Some(CurrentAddressFact::Qualified(hash)) =>
                    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
                Some(
                    CurrentAddressFact::Unborn
                    | CurrentAddressFact::Absent
                    | CurrentAddressFact::Unprepared,
                ) => true,
                None => false,
            }),
            "Current ACK omitted a qualified digest"
        );
        // This flag is deliberately not a complete-domain attestation.
        if !self.is_complete() {
            tracing::debug!(agent_id=%scope.agent_id, "Current writer has unprepared addresses");
        }
        Ok(())
    }
}

pub(in crate::runtime_db) fn visit_effects(
    effects: &super::transitions::PostCommitEffects,
    visit: &mut dyn FnMut(CurrentFamily, &str) -> Result<()>,
) -> Result<()> {
    for event in &effects.audit_events {
        visit(CurrentFamily::Event, &event.id)?;
    }
    for message in &effects.queued_messages {
        visit(CurrentFamily::Queue, &message.id)?;
    }
    for id in &effects.removed_queue_message_ids {
        visit(CurrentFamily::Queue, id)?;
    }
    for id in &effects.current_queue_write_ids {
        visit(CurrentFamily::Queue, id)?;
    }
    Ok(())
}
