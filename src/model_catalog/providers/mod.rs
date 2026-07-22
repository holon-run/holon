mod china;
pub(super) mod common;
mod core;
mod gateways;
mod hosted;
mod tencent_tokenhub;

pub(crate) use tencent_tokenhub::is_tencent_tokenhub_model_id;

pub(super) fn entries_for_registration(
    registration: crate::provider::ProviderCatalogRegistration,
) -> Vec<super::BuiltInModelMetadata> {
    use crate::provider::ProviderCatalogRegistration;

    match registration {
        ProviderCatalogRegistration::Core => core::entries(),
        ProviderCatalogRegistration::HostedEarly => hosted::early_entries(),
        ProviderCatalogRegistration::KiloCode => gateways::kilocode_entries(),
        ProviderCatalogRegistration::HostedMiddle => hosted::middle_entries(),
        ProviderCatalogRegistration::OpenCodeGo => gateways::opencode_entries(),
        ProviderCatalogRegistration::ChinaEarly => china::early_entries(),
        ProviderCatalogRegistration::HostedLate => hosted::late_entries(),
        ProviderCatalogRegistration::ChinaLate => china::late_entries(),
        ProviderCatalogRegistration::TencentTokenHub => tencent_tokenhub::entries(),
    }
}

pub(super) fn route_definitions() -> Vec<super::BuiltInModelRouteDefinition> {
    let mut definitions = china::route_definitions();
    definitions.extend(tencent_tokenhub::route_definitions());
    definitions
}
