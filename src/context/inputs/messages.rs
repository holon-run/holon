use std::{borrow::Borrow, ops::Deref, sync::Arc};

use anyhow::{ensure, Result};
use serde::{Serialize, Serializer};

use super::{
    turns::HistoryTurnMaterial, MessageEnvelope, PreparedInputEvidence, QualifiedInputEvidence,
};
use crate::diagnostics::resource_io;

/// Read-only history body retaining its original qualification and allocation.
#[derive(Debug, Clone)]
pub(crate) struct ContextHistoryMessage(HistoryMessage);

#[derive(Debug, Clone)]
enum HistoryMessage {
    Qualified {
        evidence: Arc<QualifiedInputEvidence>,
        _material: Arc<HistoryTurnMaterial>,
    },
    Owned(Arc<MessageEnvelope>),
}

impl ContextHistoryMessage {
    pub(super) fn qualified(
        evidence: Arc<QualifiedInputEvidence>,
        material: Arc<HistoryTurnMaterial>,
    ) -> Result<Self> {
        ensure!(
            matches!(evidence.record, PreparedInputEvidence::Message(_)),
            "resident history Message family mismatch"
        );
        resource_io::charge_owned_metadata(std::mem::size_of::<Self>() as u64, 1)?;
        Ok(Self(HistoryMessage::Qualified {
            evidence,
            _material: material,
        }))
    }

    pub(crate) fn owned(record: MessageEnvelope) -> Result<Self> {
        resource_io::charge_owned_metadata(
            (std::mem::size_of::<MessageEnvelope>() + 2 * std::mem::size_of::<usize>()) as u64,
            1,
        )?;
        Ok(Self(HistoryMessage::Owned(Arc::new(record))))
    }

    pub(in crate::context) fn budget_payload_bytes(&self) -> Result<u64> {
        match &self.0 {
            // Qualification already measured this immutable body, including
            // the evidence/relation envelope. Retain that conservative charge.
            HistoryMessage::Qualified { evidence, .. } => {
                Ok(u64::try_from(evidence.resident_bytes)?)
            }
            HistoryMessage::Owned(record) => Ok(
                crate::runtime::input_evidence_budget::serialized_bytes(record.as_ref())?,
            ),
        }
    }
}

impl Deref for ContextHistoryMessage {
    type Target = MessageEnvelope;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            HistoryMessage::Qualified { evidence, .. } => match &evidence.record {
                PreparedInputEvidence::Message(record) => record,
                _ => unreachable!("qualified history Message has another family"),
            },
            HistoryMessage::Owned(record) => record,
        }
    }
}

impl Borrow<MessageEnvelope> for ContextHistoryMessage {
    fn borrow(&self) -> &MessageEnvelope {
        self
    }
}

impl PartialEq<MessageEnvelope> for ContextHistoryMessage {
    fn eq(&self, other: &MessageEnvelope) -> bool {
        self.deref() == other
    }
}

impl Serialize for ContextHistoryMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        MessageEnvelope::serialize(self, serializer)
    }
}
