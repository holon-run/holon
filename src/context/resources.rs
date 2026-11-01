//! Resource reads keep the stamped observation through rendering.
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::{
    runtime::resource_observation::ResourceObservation,
    types::{WorkItemPlanArtifact, WorkItemRecord},
    work_item_plan::PlanArtifactQuery,
};

use super::snapshot::ContextResources;

pub(super) trait PlanArtifactLookup {
    fn plan(&self, id: &str) -> Option<&WorkItemPlanArtifact>;
}

impl PlanArtifactLookup for BTreeMap<String, WorkItemPlanArtifact> {
    fn plan(&self, id: &str) -> Option<&WorkItemPlanArtifact> {
        self.get(id)
    }
}

pub(super) enum ContextResourceInput<'a> {
    Filesystem(&'a dyn ContextResources),
    Loaded(Arc<ResourceObservation>),
}

impl ContextResourceInput<'_> {
    pub(super) fn observes_recorded_plans(&self) -> bool {
        match self {
            Self::Filesystem(resources) => resources.observes_recorded_plans(),
            Self::Loaded(_) => true,
        }
    }

    pub(super) fn plan(
        &self,
        item: &WorkItemRecord,
    ) -> Result<Option<Cow<'_, WorkItemPlanArtifact>>> {
        if matches!(self, Self::Loaded(_)) {
            crate::diagnostics::resource_io::check_active_budget()?;
        }
        match self {
            Self::Filesystem(resources) => Ok(resources.plan(item)?.map(Cow::Owned)),
            Self::Loaded(observation) => match observation.body().plans.get(&item.id) {
                Some(PlanArtifactQuery::Available(artifact)) => Ok(Some(Cow::Borrowed(artifact))),
                Some(PlanArtifactQuery::Missing | PlanArtifactQuery::NotRecorded) => Ok(None),
                Some(PlanArtifactQuery::Unreadable(error)) => {
                    anyhow::bail!("plan observation failed: {error}")
                }
                None => anyhow::bail!(
                    "plan {} was not prepared in this resource observation",
                    item.id
                ),
            },
        }
    }

    pub(super) fn notes(&self) -> Cow<'_, Option<String>> {
        match self {
            Self::Filesystem(resources) => Cow::Owned(resources.notes()),
            Self::Loaded(observation) => Cow::Borrowed(&observation.body().notes),
        }
    }
}

#[derive(Debug, Default)]
pub(super) enum ContextResourceViews {
    #[default]
    Empty,
    Owned {
        plans: BTreeMap<String, WorkItemPlanArtifact>,
        notes: Option<String>,
    },
    Loaded {
        observation: Arc<ResourceObservation>,
        selected_plans: BTreeSet<String>,
    },
}

impl ContextResourceViews {
    pub(super) fn new(input: &ContextResourceInput<'_>) -> Self {
        match input {
            ContextResourceInput::Filesystem(_) => Self::Owned {
                plans: BTreeMap::new(),
                notes: None,
            },
            ContextResourceInput::Loaded(observation) => Self::Loaded {
                observation: observation.clone(),
                selected_plans: BTreeSet::new(),
            },
        }
    }

    pub(super) fn insert_plan(
        &mut self,
        id: &str,
        artifact: Cow<'_, WorkItemPlanArtifact>,
    ) -> Result<()> {
        match self {
            Self::Owned { plans, .. } => {
                plans.insert(id.to_owned(), artifact.into_owned());
            }
            Self::Loaded { selected_plans, .. } => {
                // Only the selector is new; the original guard owns the body.
                crate::diagnostics::resource_io::charge_owned_metadata(
                    (id.len() + std::mem::size_of::<String>() + 128) as u64,
                    1,
                )?;
                selected_plans.insert(id.to_owned());
            }
            Self::Empty => anyhow::bail!("resource views were not prepared"),
        }
        Ok(())
    }

    pub(super) fn set_notes(&mut self, notes: Cow<'_, Option<String>>) {
        if let Self::Owned { notes: target, .. } = self {
            *target = notes.into_owned();
        }
    }

    pub(super) fn notes(&self) -> Option<&String> {
        match self {
            Self::Empty => None,
            Self::Owned { notes, .. } => notes.as_ref(),
            Self::Loaded { observation, .. } => observation.body().notes.as_ref(),
        }
    }

    pub(super) fn digest(&self) -> Result<String> {
        if matches!(self, Self::Loaded { .. }) {
            crate::diagnostics::resource_io::check_active_budget()?;
        }
        let digest = match self {
            Self::Loaded { observation, .. } => Sha256::digest(observation.digest()),
            Self::Owned { plans, notes } => Sha256::digest(serde_json::to_vec(&(plans, notes))?),
            Self::Empty => anyhow::bail!("resource views were not prepared"),
        };
        if matches!(self, Self::Loaded { .. }) {
            crate::diagnostics::resource_io::charge_owned_metadata(
                (std::mem::size_of::<String>() + 64) as u64,
                1,
            )?;
        }
        use std::fmt::Write;
        let mut encoded = String::with_capacity(64);
        write!(&mut encoded, "{digest:x}")?;
        Ok(encoded)
    }

    #[cfg(test)]
    pub(super) fn owned_plans_mut(&mut self) -> &mut BTreeMap<String, WorkItemPlanArtifact> {
        match self {
            Self::Owned { plans, .. } => plans,
            _ => panic!("expected cold resource views"),
        }
    }
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;

impl PlanArtifactLookup for ContextResourceViews {
    fn plan(&self, id: &str) -> Option<&WorkItemPlanArtifact> {
        match self {
            Self::Owned { plans, .. } => plans.get(id),
            Self::Loaded {
                observation,
                selected_plans,
            } if selected_plans.contains(id) => match observation.body().plans.get(id) {
                Some(PlanArtifactQuery::Available(artifact)) => Some(artifact),
                _ => None,
            },
            _ => None,
        }
    }
}
