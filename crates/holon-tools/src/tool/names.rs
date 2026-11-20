//! Centralized tool name constants.
//!
/// Every builtin tool exposes its name through a `pub const NAME` in its
/// own module. This module re-exports those constants as a single catalog so
/// that runtime / dispatch / presentation / prompt code references compile-time
/// constants instead of raw string literals.
///
/// Importing `tool::names::TOOL_*` lets the compiler catch typos and makes
/// renames a single-point edit.
pub use holon_domain::tool_names;

pub use tool_names::*;

/// All builtin tool names that participate in stability-level classification.
///
/// Keep alphabetically sorted for readability.
pub const STABLE_TOOL_NAMES: &[&str] = &[
    GET_AGENT,
    APPLY_PATCH,
    ATTACH_WORKSPACE,
    CANCEL_TIMER,
    COMPLETE_WORK_ITEM,
    CREATE_AGENT,
    CREATE_TIMER,
    CREATE_WORK_ITEM,
    CREATE_WORKTREE,
    DETACH_WORKSPACE,
    DELETE_AGENT,
    ENQUEUE,
    EXEC_COMMAND,
    EXEC_COMMAND_BATCH,
    GET_TIMER,
    GET_WORK_ITEM,
    GET_WORKSPACE_STATE,
    GENERATE_IMAGE,
    INVOKE_AGENT,
    LIST_MODEL_PROVIDERS,
    LIST_PROVIDER_MODELS,
    LIST_SUBAGENTS,
    LIST_SUBAGENT_CLEANUP,
    LIST_TASKS,
    LIST_TIMERS,
    LIST_WORK_ITEMS,
    MEMORY_GET,
    MEMORY_SEARCH,
    PICK_WORK_ITEM,
    REMOVE_WORKTREE,
    SEND_AGENT_MESSAGE,
    SLEEP,
    SWITCH_WORKSPACE,
    TASK_INPUT,
    TASK_STATUS,
    TASK_STOP,
    UPDATE_WORK_ITEM,
    WAIT_FOR,
];

pub const DEPRECATED_TOOL_NAMES: &[&str] = &[
    CREATE_EXTERNAL_TRIGGER,
    CANCEL_EXTERNAL_TRIGGER,
    USE_WORKSPACE,
];

/// All tool names whose result is rendered with `custom_text_receipt` rather
/// than the canonical JSON envelope.
pub const CUSTOM_TEXT_RECEIPT_TOOLS: &[&str] = &[
    APPLY_PATCH,
    EXEC_COMMAND,
    EXEC_COMMAND_BATCH,
    GET_WORKSPACE_STATE,
    GENERATE_IMAGE,
    VIEW_IMAGE,
];

/// Tools that produce output needing the `function_json` envelope field.
pub const FUNCTION_JSON_ENVELOPE_TOOLS: &[&str] = &[EXEC_COMMAND, EXEC_COMMAND_BATCH];

/// Tools that are **not** exposed to the model (hidden from tool specs).
pub const HIDDEN_FROM_MODEL_TOOLS: &[&str] =
    &[SLEEP, TASK_LIST, USE_WORKSPACE, LIST_SUBAGENT_CLEANUP];

/// Tools considered "sleep-like" for the `only_sleep_tools` aggregation.
pub const SLEEP_LIKE_TOOLS: &[&str] = &[SLEEP, WAIT_FOR];

/// All builtin tool names, alphabetically sorted.
pub const ALL_TOOL_NAMES: &[&str] = &[
    ADVISORY_DECISION,
    GET_AGENT,
    APPLY_PATCH,
    ATTACH_WORKSPACE,
    CANCEL_EXTERNAL_TRIGGER,
    CANCEL_TIMER,
    COMPLETE_WORK_ITEM,
    CREATE_AGENT,
    CREATE_EXTERNAL_TRIGGER,
    CREATE_TIMER,
    CREATE_WORK_ITEM,
    CREATE_WORKTREE,
    DETACH_WORKSPACE,
    ENQUEUE,
    EXEC_COMMAND,
    EXEC_COMMAND_BATCH,
    GET_TIMER,
    GET_WORK_ITEM,
    GET_WORKSPACE_STATE,
    GENERATE_IMAGE,
    INVOKE_AGENT,
    LIST_MODEL_PROVIDERS,
    LIST_PROVIDER_MODELS,
    LIST_SUBAGENTS,
    LIST_SUBAGENT_CLEANUP,
    LIST_TASKS,
    LIST_TIMERS,
    LIST_WEB_SEARCH_PROVIDERS,
    LIST_WORK_ITEMS,
    MEMORY_GET,
    MEMORY_SEARCH,
    PICK_WORK_ITEM,
    REMOVE_WORKTREE,
    SEND_AGENT_MESSAGE,
    SLEEP,
    SWITCH_WORKSPACE,
    TASK_INPUT,
    TASK_LIST,
    TASK_STATUS,
    TASK_STOP,
    UPDATE_WORK_ITEM,
    USE_WORKSPACE,
    VIEW_IMAGE,
    WAIT_FOR,
    WEB_FETCH,
    WEB_SEARCH,
    X_SEARCH,
];

pub const EXTERNAL_COMMAND_TOOLS: [&str; 3] = [
    EXEC_COMMAND,
    EXEC_COMMAND_BATCH,
    SCHEDULE_DESTRUCTIVE_OPERATION,
];
