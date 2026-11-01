//! Loaded prompts retain the immutable observation, not a second private body.
use std::{ops::Deref, sync::Arc};

use serde::{Serialize, Serializer};

use crate::runtime::resource_observation::{ContextResourceBody, ResourceObservation};

#[derive(Debug)]
pub struct PromptResource<T>(ResourceStorage<T>);

#[derive(Debug)]
enum ResourceStorage<T> {
    Owned(T),
    Observed {
        observation: Arc<ResourceObservation>,
        project: fn(&ContextResourceBody) -> &T,
    },
}

impl<T> PromptResource<T> {
    pub(crate) fn observed(
        observation: Arc<ResourceObservation>,
        project: fn(&ContextResourceBody) -> &T,
    ) -> Self {
        Self(ResourceStorage::Observed {
            observation,
            project,
        })
    }
}

impl<T> From<T> for PromptResource<T> {
    fn from(value: T) -> Self {
        Self(ResourceStorage::Owned(value))
    }
}

impl<T: Default> Default for PromptResource<T> {
    fn default() -> Self {
        T::default().into()
    }
}

impl<T> Deref for PromptResource<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match &self.0 {
            ResourceStorage::Owned(value) => value,
            ResourceStorage::Observed {
                observation,
                project,
            } => project(observation.body()),
        }
    }
}

impl<T: Clone> Clone for PromptResource<T> {
    fn clone(&self) -> Self {
        match &self.0 {
            ResourceStorage::Owned(value) => value.clone().into(),
            ResourceStorage::Observed {
                observation,
                project,
            } => Self::observed(observation.clone(), *project),
        }
    }
}

impl<T: Serialize> Serialize for PromptResource<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}
