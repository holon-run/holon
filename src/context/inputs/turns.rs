use std::{borrow::Borrow, ops::Deref, sync::Arc};

use anyhow::{ensure, Result};
use serde::{Serialize, Serializer};

use crate::{
    diagnostics::resource_io,
    runtime::{
        input_evidence_budget::BudgetViewGuard,
        resident_input_evidence::{
            CapturedInputEvidence, PreparedInputEvidence, QualifiedInputEvidence,
        },
    },
    types::TurnRecord,
};

/// History retains the original qualified body, including its material guard.
/// Legacy adapters move their already-owned record into the same read facade.
#[derive(Debug, Clone)]
pub(crate) struct ContextHistoryTurn(HistoryTurn);

#[derive(Debug, Clone)]
enum HistoryTurn {
    Qualified {
        evidence: Arc<QualifiedInputEvidence>,
        _material: Arc<HistoryTurnMaterial>,
    },
    Owned(Arc<TurnRecord>),
}

#[derive(Debug)]
pub(super) struct HistoryTurnMaterial {
    _views: Vec<Arc<BudgetViewGuard>>,
    _acquisition: Option<resource_io::ResourceReadMaterial>,
    _allocation: Option<Arc<BudgetViewGuard>>,
}

impl HistoryTurnMaterial {
    pub(super) fn capture(input: &CapturedInputEvidence) -> Result<Arc<Self>> {
        resource_io::charge_owned_metadata(
            (std::mem::size_of::<Self>()
                + 2 * std::mem::size_of::<usize>()
                + std::mem::size_of_val(input.budget_views.as_slice())) as u64,
            input.budget_views.len().saturating_add(1),
        )?;
        Ok(Arc::new(Self {
            _views: input.budget_views.clone(),
            _acquisition: input.query_acquisition.clone(),
            _allocation: resource_io::ResourceReadMaterial::active_guard()?,
        }))
    }
}

impl ContextHistoryTurn {
    pub(super) fn qualified(
        evidence: Arc<QualifiedInputEvidence>,
        material: Arc<HistoryTurnMaterial>,
    ) -> Result<Self> {
        ensure!(
            matches!(evidence.record, PreparedInputEvidence::Turn(_)),
            "resident history evidence family mismatch"
        );
        resource_io::charge_owned_metadata(std::mem::size_of::<Self>() as u64, 1)?;
        Ok(Self(HistoryTurn::Qualified {
            evidence,
            _material: material,
        }))
    }

    pub(crate) fn owned(record: TurnRecord) -> Result<Self> {
        resource_io::charge_owned_metadata(
            (std::mem::size_of::<TurnRecord>() + 2 * std::mem::size_of::<usize>()) as u64,
            1,
        )?;
        Ok(Self(HistoryTurn::Owned(Arc::new(record))))
    }

    pub(crate) fn copy_owned(record: &TurnRecord) -> Result<Self> {
        resource_io::admit_cloned_value(record)?;
        Self::owned(record.clone())
    }
}

impl Deref for ContextHistoryTurn {
    type Target = TurnRecord;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            HistoryTurn::Qualified { evidence, .. } => match &evidence.record {
                PreparedInputEvidence::Turn(record) => record,
                _ => unreachable!("qualified history constructor checked the family"),
            },
            HistoryTurn::Owned(record) => record,
        }
    }
}

impl Borrow<TurnRecord> for ContextHistoryTurn {
    fn borrow(&self) -> &TurnRecord {
        self
    }
}

impl Serialize for ContextHistoryTurn {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}

impl PartialEq<TurnRecord> for ContextHistoryTurn {
    fn eq(&self, other: &TurnRecord) -> bool {
        self.deref() == other
    }
}
