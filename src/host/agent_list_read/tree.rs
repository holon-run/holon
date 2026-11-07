//! Operator visibility differs from the public list; acquisition does not.
use super::*;

#[derive(Debug, serde::Serialize)]
#[serde(transparent)]
pub struct OwnedAgentTreeRead {
    tree: AgentTreeProjection,
    #[serde(skip)]
    materials: Vec<(String, ResourceReadMaterial)>,
}

impl Deref for OwnedAgentTreeRead {
    type Target = AgentTreeProjection;

    fn deref(&self) -> &Self::Target {
        &self.tree
    }
}

struct TreeWire {
    bytes: Vec<u8>,
    _materials: Vec<(String, ResourceReadMaterial)>,
}

impl AsRef<[u8]> for TreeWire {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl OwnedAgentTreeRead {
    pub(crate) fn into_json_bytes(self) -> Result<tokio_util::bytes::Bytes> {
        let Some((_, first)) = self.materials.first() else {
            return Ok(tokio_util::bytes::Bytes::from_static(b"{\"roots\":[]}"));
        };
        fn node_size(
            node: &AgentTreeNode,
            materials: &[(String, ResourceReadMaterial)],
        ) -> Result<usize> {
            let material = &materials
                .iter()
                .find(|(id, _)| id == &node.agent.identity.agent_id)
                .context("tree node lacks its acquisition")?
                .1;
            let mut size = material.read(|| {
                let fields = crate::runtime::input_evidence_budget::serialized_bytes(&node.agent)?
                    .checked_add(crate::runtime::input_evidence_budget::serialized_bytes(
                        &node.canonical_relations,
                    )?)
                    .context("tree fields size overflow")?;
                let framing = b"{\"agent\":,\"canonical_relations\":}".len()
                    + if node.children.is_empty() {
                        0
                    } else {
                        b",\"children\":[]".len() + node.children.len() - 1
                    };
                let size = fields
                    .checked_add(u64::try_from(framing)?)
                    .context("tree node size overflow")?;
                resource_io::charge_owned_payload(size)?;
                Ok(usize::try_from(size)?)
            })?;
            for child in &node.children {
                size = size
                    .checked_add(node_size(child, materials)?)
                    .context("tree child size overflow")?;
            }
            Ok(size)
        }
        let mut size = b"{\"roots\":[]}".len() + self.tree.roots.len().saturating_sub(1);
        first.read(|| {
            resource_io::charge_owned_payload(u64::try_from(size)?)?;
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<TreeWire>() + 128)?,
                1,
            )?;
            Ok(())
        })?;
        for root in &self.tree.roots {
            size = size
                .checked_add(node_size(root, &self.materials)?)
                .context("tree response size overflow")?;
        }
        let mut bytes = vec![0; size];
        let mut writer = std::io::Cursor::new(bytes.as_mut_slice());
        serde_json::to_writer(&mut writer, &self.tree)?;
        anyhow::ensure!(
            usize::try_from(writer.position())? == size,
            "tree JSON size changed"
        );
        Ok(tokio_util::bytes::Bytes::from_owner(TreeWire {
            bytes,
            _materials: self.materials,
        }))
    }

    #[cfg(test)]
    pub(crate) fn into_agent_entries(self) -> Vec<AgentListEntry> {
        self.tree.into_agent_entries()
    }
}

impl RuntimeHost {
    pub(in crate::host) async fn read_agent_tree(&self) -> Result<OwnedAgentTreeRead> {
        let identities = self
            .inner
            .registry
            .agent_identity_reads(&self.inner.input_evidence_budget, |identity| {
                identity.status == AgentRegistryStatus::Active
            })?;
        let mut active_agent_ids = HashSet::new();
        for (identity, material) in &identities {
            material.read(|| {
                // All structural copies, hash buckets, path markers and final
                // root/child slots are admitted before building the graph.
                resource_io::charge_owned_payload(u64::try_from(identity.agent_id.len())? * 8)?;
                let slots = std::mem::size_of::<AgentTreeNode>() * 2
                    + std::mem::size_of::<(String, AgentTreeNode)>() * 4
                    + std::mem::size_of::<(String, Vec<String>)>() * 4
                    + std::mem::size_of::<String>() * 16
                    + std::mem::size_of::<(String, ResourceReadMaterial)>();
                resource_io::charge_owned_metadata(u64::try_from(slots)?, 1)?;
                active_agent_ids.try_reserve(1)?;
                active_agent_ids.insert(identity.agent_id.clone());
                Ok(())
            })?;
        }
        let mut nodes = HashMap::new();
        let mut children_by_parent = HashMap::<String, Vec<String>>::new();
        let mut root_ids = Vec::new();
        let mut materials = Vec::new();
        for (identity, material) in identities {
            let runtime = self.try_get_loaded_runtime(&identity.agent_id).await;
            let agent = self
                .agent_list_entry_with_material(&identity, &material, runtime)
                .await?;
            let canonical_relations = material.read(|| {
                self.runtime_db()
                    .agent_canonical_relations()
                    .latest(&identity.agent_id)?
                    .with_context(|| {
                        format!(
                            "missing canonical relations for active agent {}",
                            identity.agent_id
                        )
                    })
            })?;
            material.read(|| {
                let parent = canonical_relations
                    .lineage
                    .as_ref()
                    .map(|lineage| &lineage.parent_agent_id)
                    .filter(|parent| active_agent_ids.contains(*parent));
                if let Some(parent) = parent {
                    resource_io::admit_cloned_value(parent)?;
                    children_by_parent
                        .entry(parent.clone())
                        .or_default()
                        .push(identity.agent_id.clone());
                } else {
                    root_ids.push(identity.agent_id.clone());
                }
                nodes.insert(
                    identity.agent_id.clone(),
                    AgentTreeNode {
                        agent,
                        canonical_relations,
                        children: Vec::new(),
                    },
                );
                materials.push((identity.agent_id, material.clone()));
                Ok(())
            })?;
        }
        root_ids.sort();
        for children in children_by_parent.values_mut() {
            children.sort();
        }
        let mut roots = Vec::new();
        for id in root_ids {
            if let Some(root) =
                build_agent_tree_node(&id, &mut nodes, &children_by_parent, &mut HashSet::new())
            {
                roots.push(root);
            }
        }
        let mut remaining = nodes.keys().cloned().collect::<Vec<_>>();
        remaining.sort();
        for id in remaining {
            if let Some(root) =
                build_agent_tree_node(&id, &mut nodes, &children_by_parent, &mut HashSet::new())
            {
                roots.push(root);
            }
        }
        Ok(OwnedAgentTreeRead {
            tree: AgentTreeProjection { roots },
            materials,
        })
    }
}
