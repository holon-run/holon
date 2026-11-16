#[cfg(test)]
mod china;
#[cfg(test)]
pub(super) mod common;
#[cfg(test)]
mod core;
#[cfg(test)]
mod gateways;
#[cfg(test)]
mod hosted;
#[cfg(test)]
mod tencent_tokenhub;
#[cfg(test)]
mod typesafe;

#[cfg(test)]
pub(super) fn entries_for_registration(
    registration: holon_domain::model_values::ProviderCatalogRegistration,
) -> Vec<super::BuiltInModelMetadata> {
    use holon_domain::model_values::ProviderCatalogRegistration;

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
        ProviderCatalogRegistration::Typesafe => typesafe::entries(),
    }
}

#[cfg(test)]
pub(super) fn route_definitions() -> Vec<super::BuiltInModelRouteDefinition> {
    let mut definitions = china::route_definitions();
    definitions.extend(hosted::route_definitions());
    definitions.extend(typesafe::route_definitions());
    definitions.extend(tencent_tokenhub::route_definitions());
    definitions.extend(
        ["deepseek-v4-flash", "deepseek-v4-pro"]
            .into_iter()
            .map(|model| super::BuiltInModelRouteDefinition {
                legacy_provider: super::provider_id("deepseek"),
                model_ref: super::ModelRef::new(super::provider_id("deepseek"), model),
                endpoint: super::ProviderEndpointId::parse("responses")
                    .expect("valid built-in endpoint"),
                policy: super::BuiltInModelRoutePolicy::default(),
            }),
    );
    definitions
}
