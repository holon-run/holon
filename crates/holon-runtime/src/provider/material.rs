use crate::diagnostics::resource_io::ResourceReadMaterial;

use super::{ProviderImageData, ProviderRequestMaterial};
use holon_provider::provider::material::ProviderMaterialScope;

impl ProviderMaterialScope for ResourceReadMaterial {
    fn read(&self, action: &mut dyn FnMut() -> anyhow::Result<()>) -> anyhow::Result<()> {
        self.read(action)
    }

    fn shares_allowance(&self, other: &dyn ProviderMaterialScope) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.shares_allowance(other))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub(crate) fn encode_image(
    bytes: &[u8],
    material: &ResourceReadMaterial,
) -> anyhow::Result<ProviderImageData> {
    ProviderImageData::encode(bytes, &ProviderRequestMaterial::scoped(material.clone()))
}

pub(crate) fn material_matches_runtime(
    material: &ProviderRequestMaterial,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    agent_id: &str,
    incarnation: u64,
) -> bool {
    material
        .runtime_acquisition::<ResourceReadMaterial>()
        .is_some_and(|acquisition| {
            acquisition.matches_owner_identity(ledger, agent_id, incarnation)
        })
}

#[cfg(test)]
#[path = "material_tests.rs"]
pub(crate) mod tests;
