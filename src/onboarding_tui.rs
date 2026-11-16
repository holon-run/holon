//! Application options for the terminal onboarding wizard.
use anyhow::Result;
use holon_tui::onboarding::OnboardingOptions;

use crate::{
    config::AppConfig,
    onboarding::{
        onboarding_model_choices, onboarding_provider_choices, onboarding_search_choices,
        OnboardingSearchSelection, OnboardingWizardSubmission,
    },
};

pub fn run_onboarding_tui(config: AppConfig) -> Result<OnboardingWizardSubmission> {
    let providers = onboarding_provider_choices(&config);
    let models_by_provider = providers
        .iter()
        .map(|provider| {
            (
                provider.id.clone(),
                onboarding_model_choices(&config, &provider.id),
            )
        })
        .collect();
    let search_choices = onboarding_search_choices(&config);
    let search = &config.web_config.search;
    let selected_search = if !search.enabled {
        OnboardingSearchSelection::Disabled
    } else if search.builtin_provider_enabled {
        OnboardingSearchSelection::Auto
    } else if search.provider == "duckduckgo" {
        OnboardingSearchSelection::ManagedDuckDuckGo
    } else {
        OnboardingSearchSelection::Auto
    };
    holon_tui::onboarding::run_onboarding_tui(OnboardingOptions {
        providers,
        models_by_provider,
        search_choices,
        default_model: config.default_model,
        selected_search,
    })
}
