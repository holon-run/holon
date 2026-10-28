use std::{borrow::Borrow, ops::Deref, sync::Arc};

use anyhow::{ensure, Result};
use serde::{Serialize, Serializer};

use super::{
    turns::HistoryTurnMaterial, BriefRecord, PreparedInputEvidence, QualifiedInputEvidence,
};
use crate::diagnostics::resource_io;

/// Both history windows retain the same qualified Brief and its material.
#[derive(Debug, Clone)]
pub(crate) struct ContextHistoryBrief(HistoryBrief);

#[derive(Debug, Clone)]
enum HistoryBrief {
    Qualified {
        evidence: Arc<QualifiedInputEvidence>,
        _material: Arc<HistoryTurnMaterial>,
    },
    Owned(Arc<BriefRecord>),
}

impl ContextHistoryBrief {
    pub(super) fn qualified(
        evidence: Arc<QualifiedInputEvidence>,
        material: Arc<HistoryTurnMaterial>,
    ) -> Result<Self> {
        ensure!(
            matches!(evidence.record, PreparedInputEvidence::Brief(_)),
            "resident history Brief family mismatch"
        );
        resource_io::charge_owned_metadata(std::mem::size_of::<Self>() as u64, 1)?;
        Ok(Self(HistoryBrief::Qualified {
            evidence,
            _material: material,
        }))
    }

    pub(crate) fn owned(record: BriefRecord) -> Result<Self> {
        resource_io::charge_owned_metadata(
            (std::mem::size_of::<BriefRecord>() + 2 * std::mem::size_of::<usize>()) as u64,
            1,
        )?;
        Ok(Self(HistoryBrief::Owned(Arc::new(record))))
    }
}

impl Deref for ContextHistoryBrief {
    type Target = BriefRecord;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            HistoryBrief::Qualified { evidence, .. } => match &evidence.record {
                PreparedInputEvidence::Brief(record) => record,
                _ => unreachable!("qualified history Brief has another family"),
            },
            HistoryBrief::Owned(record) => record,
        }
    }
}

impl Borrow<BriefRecord> for ContextHistoryBrief {
    fn borrow(&self) -> &BriefRecord {
        self
    }
}

impl PartialEq<BriefRecord> for ContextHistoryBrief {
    fn eq(&self, other: &BriefRecord) -> bool {
        self.deref() == other
    }
}

impl Serialize for ContextHistoryBrief {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}
