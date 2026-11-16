use crate::model_values::{
    default_compaction_keep_recent_estimated_tokens, default_compaction_trigger_estimated_tokens,
    DEFAULT_PROMPT_BUDGET_ESTIMATED_TOKENS,
};

/// Explicit environment policy, captured at config load rather than during resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextBudgetOverrides {
    pub prompt_budget_estimated_tokens: Option<usize>,
    pub compaction_trigger_estimated_tokens: Option<usize>,
    pub compaction_keep_recent_estimated_tokens: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ContextConfig {
    pub recent_messages: usize,
    pub recent_briefs: usize,
    pub compaction_trigger_messages: usize,
    pub compaction_keep_recent_messages: usize,
    pub prompt_budget_estimated_tokens: usize,
    pub compaction_trigger_estimated_tokens: usize,
    pub compaction_keep_recent_estimated_tokens: usize,
    /// Explicit policy fields, separate from fallback/effective numeric values.
    pub budget_overrides: ContextBudgetOverrides,
    pub recent_episode_candidates: usize,
    pub max_relevant_episodes: usize,
    pub turn_projection_budget_ratio: f32,
    pub turn_projection_min_budget: usize,
    pub turn_projection_max_budget: usize,
    pub callback_base_url: String,
}

impl Default for ContextConfig {
    fn default() -> Self {
        let prompt_budget = DEFAULT_PROMPT_BUDGET_ESTIMATED_TOKENS;
        let compaction_trigger = default_compaction_trigger_estimated_tokens(prompt_budget);
        Self {
            recent_messages: 12,
            recent_briefs: 8,
            compaction_trigger_messages: 20,
            compaction_keep_recent_messages: 8,
            prompt_budget_estimated_tokens: prompt_budget,
            compaction_trigger_estimated_tokens: compaction_trigger,
            compaction_keep_recent_estimated_tokens:
                default_compaction_keep_recent_estimated_tokens(compaction_trigger),
            budget_overrides: ContextBudgetOverrides::default(),
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            turn_projection_budget_ratio: 0.30,
            turn_projection_min_budget: 4096,
            turn_projection_max_budget: 64000,
            callback_base_url: String::new(),
        }
    }
}
impl ContextConfig {
    /// Derive turn projection budget from resolved prompt budget.
    /// Applies ratio with floor and ceiling guards.
    pub fn turn_projection_budget(&self) -> usize {
        let budget = (self.prompt_budget_estimated_tokens as f32
            * self.turn_projection_budget_ratio) as usize;
        budget.clamp(
            self.turn_projection_min_budget,
            self.turn_projection_max_budget,
        )
    }
}
