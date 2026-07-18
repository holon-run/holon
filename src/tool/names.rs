//! Centralized tool name constants.
//!
/// Every builtin tool exposes its name through a `pub(crate) const NAME` in its
/// own module. This module re-exports those constants as a single catalog so
/// that runtime / dispatch / presentation / prompt code references compile-time
/// constants instead of raw string literals.
///
/// Importing `tool::names::TOOL_*` lets the compiler catch typos and makes
/// renames a single-point edit.
pub mod tool_names {
    pub const AGENT_GET: &str = "AgentGet";
    pub const APPLY_PATCH: &str = "ApplyPatch";
    pub const ATTACH_WORKSPACE: &str = "AttachWorkspace";
    pub const CANCEL_EXTERNAL_TRIGGER: &str = "CancelExternalTrigger";
    pub const COMPLETE_WORK_ITEM: &str = "CompleteWorkItem";
    pub const CREATE_EXTERNAL_TRIGGER: &str = "CreateExternalTrigger";
    pub const CREATE_WORK_ITEM: &str = "CreateWorkItem";
    pub const CREATE_WORKTREE: &str = "CreateWorktree";
    pub const DETACH_WORKSPACE: &str = "DetachWorkspace";
    pub const ENQUEUE: &str = "Enqueue";
    pub const EXEC_COMMAND: &str = "ExecCommand";
    pub const EXEC_COMMAND_BATCH: &str = "ExecCommandBatch";
    pub const GET_WORK_ITEM: &str = "GetWorkItem";
    pub const GET_WORKSPACE_STATE: &str = "GetWorkspaceState";
    pub const GENERATE_IMAGE: &str = "GenerateImage";
    pub const LIST_MODEL_PROVIDERS: &str = "ListModelProviders";
    pub const LIST_PROVIDER_MODELS: &str = "ListProviderModels";
    pub const LIST_TASKS: &str = "ListTasks";
    pub const LIST_WORK_ITEMS: &str = "ListWorkItems";
    pub const MEMORY_GET: &str = "MemoryGet";
    pub const MEMORY_SEARCH: &str = "MemorySearch";
    pub const PICK_WORK_ITEM: &str = "PickWorkItem";
    pub const REMOVE_WORKTREE: &str = "RemoveWorktree";
    pub const SLEEP: &str = "Sleep";
    pub const SPAWN_AGENT: &str = "SpawnAgent";
    pub const TASK_INPUT: &str = "TaskInput";
    /// Legacy alias kept for backward-compatible dispatch.
    pub const TASK_LIST: &str = "TaskList";
    pub const TASK_OUTPUT: &str = "TaskOutput";
    pub const TASK_STATUS: &str = "TaskStatus";
    pub const TASK_STOP: &str = "TaskStop";
    pub const UPDATE_WORK_ITEM: &str = "UpdateWorkItem";
    pub const USE_WORKSPACE: &str = "UseWorkspace";
    pub const SWITCH_WORKSPACE: &str = "SwitchWorkspace";
    pub const VIEW_IMAGE: &str = "ViewImage";
    pub const WAIT_FOR: &str = "WaitFor";
    pub const WEB_FETCH: &str = "WebFetch";
    pub const WEB_SEARCH: &str = "WebSearch";
    pub const X_SEARCH: &str = "XSearch";
}

pub use tool_names::*;

/// All builtin tool names that participate in stability-level classification.
///
/// Keep alphabetically sorted for readability.
pub const STABLE_TOOL_NAMES: &[&str] = &[
    AGENT_GET,
    APPLY_PATCH,
    ATTACH_WORKSPACE,
    COMPLETE_WORK_ITEM,
    CREATE_WORK_ITEM,
    CREATE_WORKTREE,
    DETACH_WORKSPACE,
    ENQUEUE,
    EXEC_COMMAND,
    EXEC_COMMAND_BATCH,
    GET_WORK_ITEM,
    GET_WORKSPACE_STATE,
    GENERATE_IMAGE,
    LIST_MODEL_PROVIDERS,
    LIST_PROVIDER_MODELS,
    LIST_TASKS,
    LIST_WORK_ITEMS,
    MEMORY_GET,
    MEMORY_SEARCH,
    PICK_WORK_ITEM,
    REMOVE_WORKTREE,
    SLEEP,
    SPAWN_AGENT,
    SWITCH_WORKSPACE,
    TASK_INPUT,
    TASK_OUTPUT,
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
    TASK_OUTPUT,
    GENERATE_IMAGE,
    VIEW_IMAGE,
];

/// Tools that produce output needing the `function_json` envelope field.
pub const FUNCTION_JSON_ENVELOPE_TOOLS: &[&str] = &[EXEC_COMMAND, EXEC_COMMAND_BATCH, TASK_OUTPUT];

/// Tools that are **not** exposed to the model (hidden from tool specs).
pub const HIDDEN_FROM_MODEL_TOOLS: &[&str] = &[SLEEP, TASK_LIST, USE_WORKSPACE];

/// Tools considered "sleep-like" for the `only_sleep_tools` aggregation.
pub const SLEEP_LIKE_TOOLS: &[&str] = &[SLEEP, WAIT_FOR];

/// All builtin tool names, alphabetically sorted.
pub const ALL_TOOL_NAMES: &[&str] = &[
    AGENT_GET,
    APPLY_PATCH,
    ATTACH_WORKSPACE,
    CANCEL_EXTERNAL_TRIGGER,
    COMPLETE_WORK_ITEM,
    CREATE_EXTERNAL_TRIGGER,
    CREATE_WORK_ITEM,
    CREATE_WORKTREE,
    DETACH_WORKSPACE,
    ENQUEUE,
    EXEC_COMMAND,
    EXEC_COMMAND_BATCH,
    GET_WORK_ITEM,
    GET_WORKSPACE_STATE,
    GENERATE_IMAGE,
    LIST_MODEL_PROVIDERS,
    LIST_PROVIDER_MODELS,
    LIST_TASKS,
    LIST_WORK_ITEMS,
    MEMORY_GET,
    MEMORY_SEARCH,
    PICK_WORK_ITEM,
    REMOVE_WORKTREE,
    SLEEP,
    SPAWN_AGENT,
    SWITCH_WORKSPACE,
    TASK_INPUT,
    TASK_LIST,
    TASK_OUTPUT,
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
