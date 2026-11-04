#[cfg(holon_lib_test_shard = "a")]
mod agent_and_tools;
#[cfg(holon_lib_test_shard = "a")]
mod contracts;
#[cfg(holon_lib_test_shard = "a")]
mod input_consumption_baseline;
#[cfg(holon_lib_test_shard = "a")]
mod input_consumption_budget;
mod input_consumption_p1;
#[cfg(holon_lib_test_shard = "a")]
mod input_consumption_p2;
#[cfg(holon_lib_test_shard = "a")]
mod input_consumption_shared_reply;
#[cfg(holon_lib_test_shard = "a")]
mod message_dispatch;
// Shared startup fixtures stay visible even when their own tests are not built.
mod runtime_state;
#[cfg(holon_lib_test_shard = "a")]
mod runtime_time;
#[cfg(holon_lib_test_shard = "a")]
mod scheduler;
pub(crate) mod support;
#[cfg(holon_lib_test_shard = "a")]
mod task_recovery;
#[cfg(holon_lib_test_shard = "a")]
mod tasks;
#[cfg(holon_lib_test_shard = "a")]
mod timers;
#[cfg(holon_lib_test_shard = "a")]
mod tui_pipeline_e2e;
#[cfg(holon_lib_test_shard = "a")]
mod turns;
#[cfg(holon_lib_test_shard = "a")]
mod wake_hints;
#[cfg(holon_lib_test_shard = "a")]
mod work_items;
#[cfg(holon_lib_test_shard = "a")]
mod workspace;
#[cfg(holon_lib_test_shard = "a")]
mod workspace_control;

#[cfg(holon_lib_test_shard = "a")]
mod deletion_worktree;
#[cfg(holon_lib_test_shard = "a")]
mod prestart_terminal;
