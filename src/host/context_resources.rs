//! Named resource invalidation/refresh; it never activates or constructs Runtime.
pub(crate) use super::resource_writes::HostResourceWriteLease;
use super::*;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate},
    resource_observation::ResourceInvalidation,
};
pub(crate) struct HostResourceWriteLeases {
    leases: Vec<Arc<crate::runtime::resource_observation::ResourceWriteLease>>,
    _host_write: Arc<HostResourceWriteLease>,
    _material: Arc<BudgetViewGuard>,
}
impl HostResourceWriteLeases {
    pub(crate) fn share_physical(&self) -> Self {
        Self {
            leases: self.leases.clone(),
            _host_write: self._host_write.clone(),
            _material: self._material.clone(),
        }
    }
}
impl RuntimeHost {
    async fn resource_runtime_inventory(
        &self,
    ) -> Result<(Vec<RuntimeHandle>, Arc<BudgetViewGuard>)> {
        self.resource_runtime_inventory_with_material(0, 0).await
    }
    async fn resource_runtime_inventory_with_material(
        &self,
        reserved_bytes: u64,
        reserved_records: usize,
    ) -> Result<(Vec<RuntimeHandle>, Arc<BudgetViewGuard>)> {
        let registry = self.inner.runtimes.read().await;
        let count = registry
            .agents
            .len()
            .checked_add(registry.unloading_mutations.len())
            .and_then(|n| n.checked_add(registry.temporary_runtimes.len()))
            .context("resource runtime inventory overflow")?;
        let records = count
            .checked_mul(4)
            .context("resource runtime inventory count overflow")?;
        let bytes = u64::try_from(count)?
            .checked_mul(512)
            .context("resource runtime inventory bytes overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            records
                .checked_add(reserved_records)
                .context("resource fleet combined count overflow")?
                <= usize::try_from(limits.record_limit.min(65536))?
                && bytes
                    .checked_add(reserved_bytes)
                    .context("resource fleet combined bytes overflow")?
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("resource fleet byte limits overflow")?
                        .min(64 * 1024 * 1024),
            crate::runtime::resource_observation::ResourceObservationError::ResourceExhausted(
                "Host resource fleet"
            )
        );
        let guard = self.inner.input_evidence_budget.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let mut selected = BTreeMap::new();
        for runtime in registry
            .agents
            .values()
            .map(|entry| &entry.runtime)
            .chain(registry.unloading_mutations.values())
            .chain(
                registry
                    .temporary_runtimes
                    .values()
                    .map(|entry| &entry.runtime),
            )
        {
            selected
                .entry(runtime.original_process_registry_id())
                .or_insert_with(|| runtime.clone());
        }
        Ok((selected.into_values().collect(), guard))
    }
    pub(crate) async fn begin_loaded_context_resource_write(
        &self,
    ) -> Result<HostResourceWriteLeases> {
        // Fence new activations before waiting for the fleet snapshot.
        let host_write = Arc::new(self.begin_open_context_resource_write()?);
        let (reserved_bytes, reserved_records) = host_write.material_estimate();
        let (runtimes, material) = self
            .resource_runtime_inventory_with_material(reserved_bytes, reserved_records)
            .await?;
        let mut leases = Vec::with_capacity(runtimes.len());
        for runtime in runtimes {
            if let Some(lease) = runtime.begin_local_context_resource_write()? {
                leases.push(Arc::new(lease));
            }
        }
        Ok(HostResourceWriteLeases {
            leases,
            _host_write: host_write,
            _material: material,
        })
    }
    pub(crate) async fn invalidate_loaded_context_resources(
        &self,
        cause: ResourceInvalidation,
    ) -> Result<()> {
        let (runtimes, _guard) = self.resource_runtime_inventory().await?;
        for runtime in runtimes {
            runtime.invalidate_context_resources(cause)?;
        }
        Ok(())
    }
    /// Explicit operator/file workflow; no Host registry lock or owner survives I/O.
    pub(crate) async fn refresh_loaded_context_resources(&self) -> Result<()> {
        let (runtimes, _guard) = self.resource_runtime_inventory().await?;
        for runtime in runtimes {
            if runtime.accepts_execution_admission() {
                runtime.refresh_context_resources().await?;
            }
        }
        Ok(())
    }
}
