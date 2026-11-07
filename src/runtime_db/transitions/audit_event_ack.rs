use std::{ops::Deref, sync::Arc};

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::input_evidence_budget::BudgetViewGuard,
    types::AuditEvent,
};

/// One immutable original writer ACK shared by notifications and Current.
#[derive(Debug, Clone)]
pub(crate) struct AuditEventAck(
    Arc<AuditEventMaterial>,
    Option<crate::runtime_db::current_read_writes::CommittedEventRead>,
);

#[derive(Debug)]
struct AuditEventMaterial {
    event: AuditEvent,
    _material: Option<Arc<BudgetViewGuard>>,
    _dispatch_material: Option<Arc<crate::runtime_db::dispatch_memory::DispatchAuditMaterial>>,
    original_writer: bool,
}

impl PartialEq<AuditEventAck> for AuditEvent {
    fn eq(&self, other: &AuditEventAck) -> bool {
        self == other.deref()
    }
}

impl AuditEventAck {
    /// Current records hold the body only, never a notification back-reference.
    pub(in crate::runtime_db) fn body_ack(&self) -> Self {
        Self(self.0.clone(), None)
    }

    pub(crate) fn current_record(
        &self,
    ) -> Option<&crate::runtime_db::current_read_writes::CommittedEventRead> {
        self.1.as_ref()
    }

    pub(in crate::runtime_db) fn set_current_record(
        &mut self,
        record: Option<crate::runtime_db::current_read_writes::CommittedEventRead>,
    ) {
        self.1 = record;
    }

    /// An owned/replayed value is not proof that this phase admitted its body.
    pub(crate) fn new(event: AuditEvent) -> Result<Self> {
        Self::build(event, ResourceReadMaterial::active_guard()?, false)
    }

    pub(in crate::runtime_db) fn with_original_material(
        event: AuditEvent,
        material: Option<Arc<BudgetViewGuard>>,
    ) -> Result<Self> {
        Self::build(event, material, true)
    }

    pub(in crate::runtime_db) fn with_dispatch_material(
        event: AuditEvent,
        material: Arc<crate::runtime_db::dispatch_memory::DispatchAuditMaterial>,
    ) -> Result<Self> {
        let mut ack = Self::build(event, Some(material.guard().clone()), true)?;
        Arc::get_mut(&mut ack.0)
            .expect("new Audit ACK")
            ._dispatch_material = Some(material);
        Ok(ack)
    }

    fn build(
        event: AuditEvent,
        material: Option<Arc<BudgetViewGuard>>,
        original_writer: bool,
    ) -> Result<Self> {
        resource_io::charge_owned_metadata(
            u64::try_from(
                std::mem::size_of::<AuditEventMaterial>() + 2 * std::mem::size_of::<usize>(),
            )?,
            1,
        )?;
        Ok(Self(
            Arc::new(AuditEventMaterial {
                event,
                _material: material,
                _dispatch_material: None,
                original_writer,
            }),
            None,
        ))
    }

    pub(in crate::runtime_db) fn original_material(&self) -> Option<&Arc<BudgetViewGuard>> {
        self.0
            .original_writer
            .then_some(self.0._material.as_ref())
            .flatten()
    }

    pub(in crate::runtime_db) fn from_events(events: Vec<AuditEvent>) -> Result<Vec<Self>> {
        Self::admit_slots(events.len())?;
        events.into_iter().map(Self::new).collect()
    }

    pub(in crate::runtime_db) fn clone_batch(events: &[Self]) -> Result<Vec<Self>> {
        Self::admit_slots(events.len())?;
        Ok(events.to_vec())
    }

    pub(super) fn admit_slots(len: usize) -> Result<()> {
        resource_io::charge_owned_metadata(
            u64::try_from(
                len.checked_mul(std::mem::size_of::<Self>())
                    .ok_or_else(|| anyhow::anyhow!("Audit ACK slots overflow"))?,
            )?,
            len,
        )?;
        Ok(())
    }
}

impl Deref for AuditEventAck {
    type Target = AuditEvent;

    fn deref(&self) -> &Self::Target {
        &self.0.event
    }
}

impl PartialEq for AuditEventAck {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}

impl PartialEq<AuditEvent> for AuditEventAck {
    fn eq(&self, other: &AuditEvent) -> bool {
        self.deref() == other
    }
}

impl Serialize for AuditEventAck {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AuditEventAck {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(AuditEvent::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
