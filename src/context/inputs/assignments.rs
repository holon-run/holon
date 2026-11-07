//! One bounded history assignment collection, without per-row clone envelopes.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{ensure, Result};
use serde::{ser::SerializeMap, Serialize, Serializer};

use super::{Family, Key, PreparedInputEvidence, ResidentContextInputs};
use crate::diagnostics::resource_io;

#[derive(Debug, Clone)]
pub(crate) struct ContextHistoryAssignments(HistoryAssignments);

#[derive(Debug, Clone)]
enum HistoryAssignments {
    Owned(BTreeMap<String, Option<String>>),
    Qualified {
        assignments: Arc<BTreeMap<String, Option<String>>>,
        selected: Arc<BTreeMap<String, bool>>,
        _material: Arc<super::turns::HistoryTurnMaterial>,
        _allocation: Option<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    },
}

impl Default for ContextHistoryAssignments {
    fn default() -> Self {
        Self(HistoryAssignments::Owned(BTreeMap::new()))
    }
}

impl From<BTreeMap<String, Option<String>>> for ContextHistoryAssignments {
    fn from(value: BTreeMap<String, Option<String>>) -> Self {
        Self(HistoryAssignments::Owned(value))
    }
}

impl ContextHistoryAssignments {
    pub(crate) fn get(&self, id: &str) -> Option<&Option<String>> {
        match &self.0 {
            HistoryAssignments::Owned(assignments) => assignments.get(id),
            HistoryAssignments::Qualified {
                assignments,
                selected,
                ..
            } => selected.get(id).map(|present| {
                if *present {
                    assignments.get(id).expect("qualified assignment")
                } else {
                    &None
                }
            }),
        }
    }
}

impl Serialize for ContextHistoryAssignments {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match &self.0 {
            HistoryAssignments::Owned(assignments) => assignments.serialize(serializer),
            HistoryAssignments::Qualified { selected, .. } => {
                let mut map = serializer.serialize_map(Some(selected.len()))?;
                for id in selected.keys() {
                    map.serialize_entry(id, self.get(id).expect("selected assignment"))?;
                }
                map.end()
            }
        }
    }
}

pub(super) fn selection<'id>(
    ids: impl Iterator<Item = &'id str> + Clone,
) -> Result<BTreeSet<String>> {
    let mut bytes = std::mem::size_of::<BTreeSet<String>>() as u64;
    let mut records = 1usize;
    for id in ids.clone() {
        bytes = bytes
            .checked_add((id.len() + std::mem::size_of::<String>() + 128) as u64)
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: history assignment selection"))?;
        records = records
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: history assignment selection"))?;
    }
    resource_io::charge_owned_metadata(bytes, records)?;
    Ok(ids.map(str::to_owned).collect())
}

pub(super) fn capture_view<'id>(
    inputs: &ResidentContextInputs<'_, '_>,
    ids: impl Iterator<Item = &'id str> + Clone,
) -> Result<ContextHistoryAssignments> {
    // Check every finite source, including absence, before allocating selectors.
    for id in ids.clone() {
        borrowed(inputs, id)?;
    }
    let mut bytes = std::mem::size_of::<BTreeMap<String, bool>>() as u64;
    let mut records = 1usize;
    for id in ids.clone() {
        bytes = bytes
            .checked_add(
                (id.len() + std::mem::size_of::<String>() + std::mem::size_of::<bool>() + 128)
                    as u64,
            )
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: history assignment selection"))?;
        records = records
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: history assignment selection"))?;
    }
    resource_io::charge_owned_metadata(bytes, records)?;
    let selected = ids
        .map(|id| Ok((id.to_owned(), borrowed(inputs, id)?.is_some())))
        .collect::<Result<BTreeMap<_, _>>>()?;
    resource_io::charge_owned_metadata((2 * std::mem::size_of::<usize>()) as u64, 0)?;
    Ok(ContextHistoryAssignments(HistoryAssignments::Qualified {
        assignments: inputs.captured.message_turn_keys.clone(),
        selected: Arc::new(selected),
        _material: inputs.history_material()?,
        _allocation: resource_io::ResourceReadMaterial::active_guard()?,
    }))
}

pub(super) fn borrowed<'a>(
    inputs: &'a ResidentContextInputs<'_, '_>,
    id: &str,
) -> Result<Option<&'a Option<String>>> {
    let Some(evidence) = inputs.captured.lookup_borrowed(Family::Message, id)? else {
        return Ok(None);
    };
    ensure!(
        evidence.locator.scope == inputs.captured.scope
            && matches!(&evidence.record, PreparedInputEvidence::Message(message)
                if message.agent_id == inputs.captured.scope.agent_id && message.id == id),
        "resident Message assignment scope/family mismatch"
    );
    match inputs.captured.message_turn_keys.get(id) {
        Some(assignment) => Ok(Some(assignment)),
        None => {
            resource_io::admit_cloned_value(&(Family::Message, id))?;
            Err(
                crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(
                    Key::new(Family::Message, id),
                )
                .into(),
            )
        }
    }
}

pub(super) fn capture(
    inputs: &ResidentContextInputs<'_, '_>,
    ids: BTreeSet<String>,
) -> Result<BTreeMap<String, Option<String>>> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    // Validate the finite selection before admitting its single owning map.
    // No Message body or canonical assignment is copied during this preflight.
    for id in &ids {
        borrowed(inputs, id)?;
    }
    struct Batch<'read, 'capture, 'db> {
        inputs: &'read ResidentContextInputs<'capture, 'db>,
        ids: &'read BTreeSet<String>,
    }
    impl Serialize for Batch<'_, '_, '_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(self.ids.len()))?;
            for id in self.ids {
                let assignment = borrowed(self.inputs, id).map_err(serde::ser::Error::custom)?;
                map.serialize_entry(id, &assignment)?;
            }
            map.end()
        }
    }
    resource_io::admit_cloned_value(&Batch { inputs, ids: &ids })?;
    ids.into_iter()
        .map(|id| {
            let assignment = borrowed(inputs, &id)?.cloned().flatten();
            Ok((id, assignment))
        })
        .collect()
}
