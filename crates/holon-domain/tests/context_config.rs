use holon_domain::{ContextBudgetOverrides, ContextConfig};

#[test]
fn context_defaults_share_the_internal_prompt_budget_policy() {
    let config = ContextConfig::default();
    assert_eq!(config.prompt_budget_estimated_tokens, 128_000);
    assert_eq!(config.compaction_trigger_estimated_tokens, 115_200);
    assert_eq!(config.compaction_keep_recent_estimated_tokens, 43_776);
    assert_eq!(config.budget_overrides, ContextBudgetOverrides::default());
    assert_eq!(config.turn_projection_min_budget, 4_096);
    assert_eq!(config.turn_projection_max_budget, 64_000);
}

#[test]
fn turn_projection_budget_applies_ratio_floor_and_ceiling() {
    let ratio = ContextConfig {
        prompt_budget_estimated_tokens: 20_000,
        turn_projection_budget_ratio: 0.25,
        turn_projection_min_budget: 4_096,
        turn_projection_max_budget: 64_000,
        callback_base_url: String::new(),
        ..ContextConfig::default()
    };
    assert_eq!(ratio.turn_projection_budget(), 5_000);

    let floor = ContextConfig {
        prompt_budget_estimated_tokens: 2_000,
        turn_projection_budget_ratio: 0.30,
        turn_projection_min_budget: 4_096,
        turn_projection_max_budget: 64_000,
        callback_base_url: String::new(),
        ..ContextConfig::default()
    };
    assert_eq!(floor.turn_projection_budget(), 4_096);

    let ceiling = ContextConfig {
        prompt_budget_estimated_tokens: 258_400,
        turn_projection_budget_ratio: 0.30,
        turn_projection_min_budget: 4_096,
        turn_projection_max_budget: 64_000,
        callback_base_url: String::new(),
        ..ContextConfig::default()
    };
    assert_eq!(ceiling.turn_projection_budget(), 64_000);
}
