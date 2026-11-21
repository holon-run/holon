use std::{borrow::Borrow, ops::Deref, sync::Arc};

use anyhow::{ensure, Result};
use serde::{Serialize, Serializer};

use super::{
    turns::HistoryTurnMaterial, PreparedInputEvidence, QualifiedInputEvidence, TranscriptEntry,
};
use crate::diagnostics::resource_io;

/// Read-only history retaining its qualified transcript and original material.
#[derive(Debug, Clone)]
pub(crate) struct ContextHistoryTranscript(HistoryTranscript);

#[derive(Debug, Clone)]
enum HistoryTranscript {
    Qualified {
        evidence: Arc<QualifiedInputEvidence>,
        _material: Arc<HistoryTurnMaterial>,
    },
    Owned(Arc<TranscriptEntry>),
}

impl ContextHistoryTranscript {
    pub(super) fn qualified(
        evidence: Arc<QualifiedInputEvidence>,
        material: Arc<HistoryTurnMaterial>,
    ) -> Result<Self> {
        ensure!(
            matches!(evidence.record, PreparedInputEvidence::Transcript(_)),
            "resident history Transcript family mismatch"
        );
        resource_io::charge_owned_metadata(std::mem::size_of::<Self>() as u64, 1)?;
        Ok(Self(HistoryTranscript::Qualified {
            evidence,
            _material: material,
        }))
    }

    pub(crate) fn owned(record: TranscriptEntry) -> Result<Self> {
        resource_io::charge_owned_metadata(
            (std::mem::size_of::<TranscriptEntry>() + 2 * std::mem::size_of::<usize>()) as u64,
            1,
        )?;
        Ok(Self(HistoryTranscript::Owned(Arc::new(record))))
    }
}

impl Deref for ContextHistoryTranscript {
    type Target = TranscriptEntry;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            HistoryTranscript::Qualified { evidence, .. } => match &evidence.record {
                PreparedInputEvidence::Transcript(record) => record,
                _ => unreachable!("qualified history Transcript has another family"),
            },
            HistoryTranscript::Owned(record) => record,
        }
    }
}

impl Borrow<TranscriptEntry> for ContextHistoryTranscript {
    fn borrow(&self) -> &TranscriptEntry {
        self
    }
}

impl PartialEq for ContextHistoryTranscript {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}

impl PartialEq<TranscriptEntry> for ContextHistoryTranscript {
    fn eq(&self, other: &TranscriptEntry) -> bool {
        self.deref() == other
    }
}

impl Serialize for ContextHistoryTranscript {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}
