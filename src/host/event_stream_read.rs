//! Stream bodies retain qualified facts independently of a live Root or SQLite.
use super::*;
use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::current_read_writes::CommittedEventRead,
};
use std::io::Write;

pub(crate) struct EventStreamScope {
    scope: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    _material: ResourceReadMaterial,
}

#[cfg(test)]
#[path = "event_stream_read_tests.rs"]
mod tests;

#[derive(Clone)]
enum EventFrameSource {
    History(OwnedEventPageRead, usize),
    Live(CommittedEventRead),
}

#[derive(Clone)]
pub(crate) struct EventFrameRead {
    source: EventFrameSource,
    material: ResourceReadMaterial,
}

struct EventFrameWire {
    bytes: Vec<u8>,
    _read: EventFrameRead,
}

impl AsRef<[u8]> for EventFrameWire {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl RuntimeHost {
    pub(crate) fn event_stream_scope(&self, page: &OwnedEventPageRead) -> Result<EventStreamScope> {
        page.0.material.guard()?.validate_owned_material_binding(
            &page.0.scope,
            &self.inner.input_evidence_budget,
            0,
        )?;
        let material = ResourceReadMaterial::new(&self.inner.input_evidence_budget, &page.0.scope)?;
        let scope = material.read(|| {
            resource_io::charge_owned_metadata(
                u64::try_from(
                    std::mem::size_of::<EventStreamScope>() + page.0.scope.agent_id.len(),
                )?,
                1,
            )?;
            Ok(page.0.scope.clone())
        })?;
        Ok(EventStreamScope {
            scope,
            ledger: self.inner.input_evidence_budget.clone(),
            _material: material,
        })
    }
}

impl EventStreamScope {
    pub(crate) fn history_frame(
        &self,
        page: &OwnedEventPageRead,
        index: usize,
    ) -> Result<EventFrameRead> {
        anyhow::ensure!(
            page.0.scope == self.scope,
            "Event replay crossed stream scope"
        );
        page.0
            .material
            .guard()?
            .validate_owned_material_binding(&self.scope, &self.ledger, 0)?;
        anyhow::ensure!(index < page.events().len(), "Event replay index is absent");
        Ok(EventFrameRead {
            source: EventFrameSource::History(page.clone(), index),
            material: ResourceReadMaterial::new(&self.ledger, &self.scope)?,
        })
    }

    pub(crate) fn live_frame(&self, published: &PublishedAuditEvent) -> Result<EventFrameRead> {
        anyhow::ensure!(
            published.agent_id.as_deref() == Some(self.scope.agent_id.as_str()),
            "Event notification crossed stream Agent"
        );
        let original = published.qualified.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Event notification needs original Current qualification")
        })?;
        original.validate(&self.scope, &self.ledger)?;
        anyhow::ensure!(
            original.event() == &*published.event,
            "Event notification differs from qualified body"
        );
        Ok(EventFrameRead {
            source: EventFrameSource::Live(original.clone()),
            material: ResourceReadMaterial::new(&self.ledger, &self.scope)?,
        })
    }
}

impl EventFrameRead {
    pub(crate) fn event(&self) -> &AuditEvent {
        match &self.source {
            EventFrameSource::History(page, index) => {
                page.events().nth(*index).expect("checked Event index")
            }
            EventFrameSource::Live(original) => original.event(),
        }
    }

    /// JSON and SSE framing are both written once into the final admitted buffer.
    pub(crate) fn sse_bytes(
        &self,
        value: &impl serde::Serialize,
    ) -> Result<tokio_util::bytes::Bytes> {
        self.material.read(|| {
            let event = self.event();
            anyhow::ensure!(
                !event.kind.contains(['\r', '\n']),
                "invalid SSE event field"
            );
            let mut seq = std::io::Cursor::new([0u8; 20]);
            write!(&mut seq, "{}", event.event_seq)?;
            let seq_len = usize::try_from(seq.position())?;
            let json_len = usize::try_from(
                crate::runtime::input_evidence_budget::serialized_bytes(value)?,
            )?;
            let size = json_len
                .checked_add(seq_len)
                .and_then(|n| n.checked_add(event.kind.len()))
                .and_then(|n| n.checked_add(b"id: \nevent: \ndata: \n\n".len()))
                .ok_or_else(|| anyhow::anyhow!("SSE frame size overflow"))?;
            resource_io::charge_owned_buffer(u64::try_from(size)?)?;
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<EventFrameWire>() + 128)?,
                1,
            )?;
            let mut bytes = vec![0; size];
            let mut writer = std::io::Cursor::new(bytes.as_mut_slice());
            writer.write_all(b"id: ")?;
            writer.write_all(&seq.get_ref()[..seq_len])?;
            writer.write_all(b"\nevent: ")?;
            writer.write_all(event.kind.as_bytes())?;
            writer.write_all(b"\ndata: ")?;
            serde_json::to_writer(&mut writer, value)?;
            writer.write_all(b"\n\n")?;
            anyhow::ensure!(
                usize::try_from(writer.position())? == size,
                "SSE frame size changed"
            );
            Ok(tokio_util::bytes::Bytes::from_owner(EventFrameWire {
                bytes,
                _read: self.clone(),
            }))
        })
    }
}
