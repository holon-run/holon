//! Public roster products retain each original Agent acquisition through the wire.
use super::*;
use crate::{
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::agent_list_projection::{is_listed, AgentListUnavailable},
};
use std::ops::Deref;

#[derive(Debug)]
pub struct OwnedAgentListEntry {
    entry: AgentListEntry,
    material: ResourceReadMaterial,
}

impl Deref for OwnedAgentListEntry {
    type Target = AgentListEntry;

    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}

impl PartialEq for OwnedAgentListRead {
    fn eq(&self, other: &Self) -> bool {
        self.entries
            .iter()
            .map(|entry| &entry.entry)
            .eq(other.entries.iter().map(|entry| &entry.entry))
    }
}

impl PartialEq<Vec<AgentListEntry>> for OwnedAgentListRead {
    fn eq(&self, other: &Vec<AgentListEntry>) -> bool {
        self.entries
            .iter()
            .map(|entry| &entry.entry)
            .eq(other.iter())
    }
}

impl serde::Serialize for OwnedAgentListEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entry.serialize(serializer)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(transparent)]
pub struct OwnedAgentListRead {
    entries: Vec<OwnedAgentListEntry>,
}

impl Deref for OwnedAgentListRead {
    type Target = [OwnedAgentListEntry];

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl IntoIterator for OwnedAgentListRead {
    type Item = OwnedAgentListEntry;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

struct FleetWire {
    bytes: Vec<u8>,
    _materials: Vec<ResourceReadMaterial>,
}

impl AsRef<[u8]> for FleetWire {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl OwnedAgentListRead {
    #[cfg(test)]
    pub(super) fn from_observer_entries(
        entries: Vec<AgentListEntry>,
        material: ResourceReadMaterial,
    ) -> Result<Self> {
        material.read(|| {
            let mut owned = Vec::new();
            for entry in entries {
                crate::runtime_db::repositories::reserve_source_vec_slot(&mut owned)?;
                owned.push(OwnedAgentListEntry {
                    entry,
                    material: material.clone(),
                });
            }
            Ok(Self { entries: owned })
        })
    }

    pub(crate) fn into_json_bytes(self) -> Result<tokio_util::bytes::Bytes> {
        let Some(first) = self.entries.first() else {
            return Ok(tokio_util::bytes::Bytes::from_static(b"[]"));
        };
        let mut size = self
            .entries
            .len()
            .checked_add(1)
            .context("roster framing overflow")?;
        first.material.read(|| {
            resource_io::charge_owned_payload(u64::try_from(size)?)?;
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<FleetWire>() + 128)?,
                1,
            )?;
            Ok(())
        })?;
        let mut materials = Vec::new();
        for entry in &self.entries {
            entry.material.read(|| {
                let bytes = crate::runtime::input_evidence_budget::serialized_bytes(&entry.entry)?;
                resource_io::charge_owned_payload(bytes)?;
                size = size
                    .checked_add(usize::try_from(bytes)?)
                    .context("roster JSON size overflow")?;
                crate::runtime_db::repositories::reserve_source_vec_slot(&mut materials)?;
                materials.push(entry.material.clone());
                Ok(())
            })?;
        }
        let mut bytes = vec![0; size];
        let mut writer = std::io::Cursor::new(bytes.as_mut_slice());
        serde_json::to_writer(&mut writer, &self)?;
        anyhow::ensure!(
            usize::try_from(writer.position())? == size,
            "roster JSON size changed"
        );
        Ok(tokio_util::bytes::Bytes::from_owner(FleetWire {
            bytes,
            _materials: materials,
        }))
    }
}

impl RuntimeHost {
    pub async fn list_agent_entries(&self) -> Result<OwnedAgentListRead> {
        self.list_agent_entries_for_parent(None).await
    }

    pub async fn list_agent_entries_for_parent(
        &self,
        parent_agent_id: Option<&str>,
    ) -> Result<OwnedAgentListRead> {
        self.list_agent_entries_for_parent_in(parent_agent_id, self.runtime_db())
            .await
    }

    pub(crate) async fn list_agent_entries_for_parent_in(
        &self,
        parent_agent_id: Option<&str>,
        db: &RuntimeDb,
    ) -> Result<OwnedAgentListRead> {
        db.check_read_scope()?;
        let identities = self.inner.registry.agent_identity_reads(
            &self.inner.input_evidence_budget,
            |record| {
                is_listed(record)
                    && parent_agent_id.is_none_or(|parent| {
                        record
                            .lineage_parent_agent_id
                            .as_deref()
                            .or(record.parent_agent_id.as_deref())
                            == Some(parent)
                    })
            },
        )?;
        let mut entries = Vec::new();
        for (identity, material) in identities {
            db.check_read_scope()?;
            let (projection, runtime) =
                {
                    let registry = db.wait_for_memory_lock(self.inner.runtimes.read()).await?;
                    let runtime = registry
                        .agents
                        .get(&identity.agent_id)
                        .filter(|entry| !entry.task.is_finished())
                        .map(|entry| &entry.runtime);
                    if !self.inner.registry.matches_agent_identity(&identity)
                        || runtime.is_some_and(|runtime| {
                            runtime.instance_incarnation() != identity.incarnation
                        })
                    {
                        trace_unavailable_member(&identity, AgentListUnavailable::IdentityChanged);
                        return Err(anyhow::Error::new(AgentListUnavailable::IdentityChanged)
                            .context(crate::runtime_db::RuntimeDbRetryableError::new(
                                "observer publication",
                                db.path(),
                                "Agent list identity changed",
                            )));
                    }
                    let projection = match self.inner.mutation_owners.agent_list_projection(
                        &identity,
                        runtime.map(|runtime| runtime.instance_owner_epoch()),
                        db,
                    ) {
                        Ok(projection) => projection,
                        Err(error) => {
                            db.check_read_scope()?;
                            if let Some(reason) = error.downcast_ref::<AgentListUnavailable>() {
                                trace_unavailable_member(&identity, *reason);
                            }
                            return Err(error);
                        }
                    };
                    (projection, runtime.cloned())
                };
            let entry = material.read(|| {
                if let Some(runtime) = runtime {
                    runtime.agent_list_entry_from_projection(&identity, &projection)
                } else {
                    projection.to_entry(
                        &identity,
                        false,
                        self.inner.registry.model_catalog().as_ref(),
                        &self.config().default_agent_id,
                    )
                }
            })?;
            db.check_read_scope()?;
            material.read(|| {
                crate::runtime_db::repositories::reserve_source_vec_slot(&mut entries)?;
                entries.push(OwnedAgentListEntry {
                    entry,
                    material: material.clone(),
                });
                Ok(())
            })?;
        }
        // Inventory is already ordered by its unique canonical ID.
        db.check_read_scope()?;
        Ok(OwnedAgentListRead { entries })
    }
}

fn trace_unavailable_member(identity: &AgentIdentityRecord, reason: AgentListUnavailable) {
    tracing::warn!(
        route = "/agents/list",
        agent_id = %identity.agent_id,
        incarnation = identity.incarnation,
        identity_revision = identity.revision,
        reason = %reason,
        "Agent list member unavailable; refusing an incomplete list"
    );
}

mod tree;
pub use tree::OwnedAgentTreeRead;

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
#[path = "agent_list_read_tests.rs"]
mod tests;
