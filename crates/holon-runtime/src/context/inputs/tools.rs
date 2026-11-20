use std::{borrow::Borrow, ops::Deref, sync::Arc};

use anyhow::{ensure, Result};
use serde::{Serialize, Serializer};

use super::{
    turns::HistoryTurnMaterial, PreparedInputEvidence, QualifiedInputEvidence,
    ToolContextProjection,
};
use crate::diagnostics::resource_io;

/// History windows and Turn references retain the same qualified projection.
#[derive(Debug, Clone)]
pub(crate) struct ContextHistoryTool(HistoryTool);

#[derive(Debug, Clone)]
enum HistoryTool {
    Qualified {
        evidence: Arc<QualifiedInputEvidence>,
        _material: Arc<HistoryTurnMaterial>,
    },
    Owned(Arc<ToolContextProjection>),
}

impl ContextHistoryTool {
    pub(super) fn qualified(
        evidence: Arc<QualifiedInputEvidence>,
        material: Arc<HistoryTurnMaterial>,
    ) -> Result<Self> {
        ensure!(
            matches!(evidence.record, PreparedInputEvidence::ToolExecution(_)),
            "resident history ToolExecution family mismatch"
        );
        resource_io::charge_owned_metadata(std::mem::size_of::<Self>() as u64, 1)?;
        Ok(Self(HistoryTool::Qualified {
            evidence,
            _material: material,
        }))
    }

    pub(crate) fn owned(record: ToolContextProjection) -> Result<Self> {
        resource_io::charge_owned_metadata(
            (std::mem::size_of::<ToolContextProjection>() + 2 * std::mem::size_of::<usize>())
                as u64,
            1,
        )?;
        Ok(Self(HistoryTool::Owned(Arc::new(record))))
    }
}

impl Deref for ContextHistoryTool {
    type Target = ToolContextProjection;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            HistoryTool::Qualified { evidence, .. } => match &evidence.record {
                PreparedInputEvidence::ToolExecution(record) => record,
                _ => unreachable!("qualified history ToolExecution has another family"),
            },
            HistoryTool::Owned(record) => record,
        }
    }
}

impl Borrow<ToolContextProjection> for ContextHistoryTool {
    fn borrow(&self) -> &ToolContextProjection {
        self
    }
}

impl PartialEq<ToolContextProjection> for ContextHistoryTool {
    fn eq(&self, other: &ToolContextProjection) -> bool {
        self.deref() == other
    }
}

impl Serialize for ContextHistoryTool {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}
