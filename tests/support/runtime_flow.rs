#![allow(dead_code)]

// Legacy compatibility shim: keep old `runtime_flow` entrypoint available by
// re-exporting the focused runtime test implementations.
//
// The concrete behavior lives in domain-owned support modules:
// - runtime_waiting
// - runtime_tasks
// - runtime_compaction
// - runtime_subagents

pub use crate::support::runtime_tasks::*;
pub use crate::support::runtime_waiting::*;
pub use crate::support::runtime_subagents::*;

// Keep compaction ownership explicit in the shim to avoid duplicate symbol re-exports
// with overlapping historical names in runtime_waiting.
pub use crate::support::runtime_compaction::{
    contentful_wake_hint_after_compaction_keeps_active_work_truth,
    max_output_recovery_followed_by_turn_local_compaction_preserves_progress_signal,
    preview_prompt_after_compaction_keeps_work_item_plan_and_pending_work_visible,
    queued_activation_after_compaction_promotes_the_correct_next_step,
    repeated_turn_local_compaction_evolves_checkpoint_mode_and_keeps_latest_exact_tail,
    runtime_compaction_multi_pass_recovery_preserves_progress_and_artifacts,
    task_result_rejoin_after_compaction_preserves_current_work_truth,
};
