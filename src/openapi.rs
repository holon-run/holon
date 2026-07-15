use aide::{
    axum::{routing::ApiMethodDocs, ApiRouter},
    openapi::{OpenApi, Operation},
};
use schemars::{generate::SchemaSettings, JsonSchema};
use serde_json::{json, Value};

use crate::{
    diagnostics::PerformanceDiagnosticsSnapshot,
    http::{
        BatchGetMessagesRequest, CancelTimerRequest, CompleteWorkItemRequest, CreateTimerRequest,
        MemoryGetRequest, ModelConfigMigrationRequest, PickWorkItemRequest, PickWorkItemResponse,
        RuntimeConfigReadResponse, RuntimeConfigUpdateRequest, RuntimeConfigUpdateResponse,
        SearchRequest, SearchResponse, UpdateWorkItemRequest,
    },
    memory::MemoryGetResult,
    model_config_migration::ModelConfigMigrationReport,
    types::{
        AddSkillRequest, BriefRecord, TaskInputResult, TaskOutputResult, TaskStatusSnapshot,
        TaskStopResult, TimerRecord, ToolExecutionRecord, WorkItemRecord,
    },
};

const API_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy)]
struct RouteSpec {
    method: &'static str,
    path: &'static str,
    operation_id: &'static str,
    tag: &'static str,
    summary: &'static str,
    description: &'static str,
    request_schema: Option<&'static str>,
    response_schema: Option<&'static str>,
    response_kind: ResponseKind,
    auth: AuthKind,
    metadata_source: MetadataSource,
}

#[derive(Clone, Copy)]
enum ResponseKind {
    Json,
    EventStream,
}

#[derive(Clone, Copy)]
enum AuthKind {
    RemoteAccess,
    Control,
    Capability,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataSource {
    Manual,
    Aide,
}

// Migration boundary for #1438: public agent read routes and their default
// compatibility aliases are registered through aide route metadata first. The
// remaining control, mutation, callback, event stream, and placeholder request
// schema routes stay on the conservative manual baseline until their DTO
// contracts are tightened in follow-up work.
const ROUTES: &[RouteSpec] = &[
    route("get", "/", "root", "discovery", "Root discovery", "Return the default agent id.", None, AuthKind::RemoteAccess),
    route("get", "/handshake", "handshake", "discovery", "Protocol handshake", "Return auth mode, protocol version, capabilities, and runtime hints.", None, AuthKind::RemoteAccess),
    route("get", "/models", "models", "discovery", "List available models", "Return model catalog entries and runtime availability.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/list", "listAgents", "agents", "List agents", "Return lightweight public agent entries.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/status", "agentStatus", "agents", "Agent status", "Return the public AgentSummary read model.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/briefs", "agentBriefs", "agents", "Recent briefs", "Return recent user-facing delivery briefs. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/briefs/{brief_id}", "agentBrief", "agents", "Brief detail", "Return a persisted user-facing delivery brief by id.", None, "BriefRecord", AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/state", "agentState", "agents", "Agent state snapshot", "Return the lightweight bootstrap snapshot for an agent. Heavy task, work-item, operator notification, and execution details are available through dedicated routes and events.", None, AuthKind::RemoteAccess),
    event_stream_route("get", "/events/stream", "eventsStream", "events", "Global event stream", "Return Server-Sent Events carrying raw StreamEventEnvelope JSON data for all public agents. This live stream uses the in-memory event watcher and does not provide historical replay or a global cursor. If the receiver lags, the server closes the stream; clients must backfill each agent from its last contiguous event_seq before reconnecting.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/events", "agentEvents", "events", "Agent event page", "Return a bounded page of runtime event envelopes. Query parameters: before_seq, after_seq, limit, order, max_level. Event payloads are included in full; max_level filters event inclusion only. Breaking change: the projection query parameter and StreamEventEnvelope.projection field have been removed.", None, AuthKind::RemoteAccess),
    event_stream_route("get", "/agents/{agent_id}/events/stream", "agentEventsStream", "events", "Agent event stream", "Return Server-Sent Events carrying raw StreamEventEnvelope JSON data. Query parameters: after_seq, limit. SSE id is event_seq; SSE event is the audit event kind; missing replay cursors return cursor_not_found before the stream opens. If the receiver lags, the server closes the stream so clients can backfill after the last contiguous SSE id before reconnecting. Breaking change: the projection query parameter and StreamEventEnvelope.projection field have been removed.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/messages/{message_id}", "agentMessage", "messages", "Message detail", "Return a persisted message envelope by id for the selected agent.", None, AuthKind::RemoteAccess),
    route_with_response("post", "/agents/{agent_id}/messages:batchGet", "agentMessagesBatchGet", "messages", "Batch get messages", "Return persisted message envelopes for the selected agent. Missing or cross-agent ids are reported in missing_message_ids.", Some("BatchGetMessagesRequest"), "BatchGetMessagesResponse", AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/transcript", "agentTranscript", "agents", "Recent transcript", "Return recent transcript entries. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/transcript/{entry_id}", "agentTranscriptEntry", "agents", "Transcript entry detail", "Return a persisted transcript entry by id for the selected agent.", None, AuthKind::RemoteAccess),
    route_with_response("post", "/agents/{agent_id}/transcript:batchGet", "agentTranscriptBatchGet", "agents", "Batch get transcript entries", "Return persisted transcript entries for the selected agent. Missing or cross-agent ids are reported in missing_entry_ids.", Some("BatchGetTranscriptEntriesRequest"), "BatchGetTranscriptEntriesResponse", AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/tasks", "agentTasks", "tasks", "List active tasks", "Return active task records. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tasks/{task_id}", "agentTaskStatus", "tasks", "Task status", "Return a task lifecycle snapshot by id.", None, "TaskStatusSnapshot", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tasks/{task_id}/output", "agentTaskOutput", "tasks", "Task output", "Return a task output snapshot. Query parameters: block, timeout_ms.", None, "TaskOutputResult", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tool-executions/{tool_execution_id}", "agentToolExecution", "tools", "Tool execution detail", "Return a persisted tool execution record by id.", None, "ToolExecutionRecord", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tool-executions/{tool_execution_id}/artifacts/{artifact_index}", "agentToolExecutionArtifact", "tools", "Tool execution artifact", "Return UTF-8 content for an artifact referenced by the selected tool execution. Artifact paths are resolved server-side and confined to the agent runtime data directory.", None, "ToolExecutionArtifactContent", AuthKind::RemoteAccess),
    route_with_response("post", "/control/agents/{agent_id}/tasks/{task_id}/input", "taskInput", "control", "Task input", "Deliver text input to a managed task.", Some("TaskInputRequest"), "TaskInputResult", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/tasks/{task_id}/stop", "taskStop", "control", "Task stop", "Request cancellation for a managed task.", Some("TaskStopRequest"), "TaskStopResult", AuthKind::Control),
    route("get", "/agents/{agent_id}/work-items", "agentWorkItems", "work-items", "List work items", "Return latest work item records for the agent. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/work-items/{work_item_id}", "agentWorkItem", "work-items", "Work item detail", "Return a work item record by id.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/worktree-summary", "agentWorktreeSummary", "agents", "Worktree summary", "Return managed worktree summary for an agent.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/timers", "agentTimers", "timers", "List timers", "Return recent timer records. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/timers/{timer_id}", "agentTimer", "timers", "Timer detail", "Return a timer record by id.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/skills", "agentSkills", "skills", "List agent skills", "Return skills enabled/effective for an agent.", None, AuthKind::RemoteAccess),
    route("get", "/skills/catalog", "skillsCatalog", "skills", "Skills catalog", "Return the global user Skill Library catalog. Query parameter: scope.", None, AuthKind::RemoteAccess),
    route("get", "/skills/catalog/{skill_id}", "skillDetail", "skills", "Skill detail", "Return catalog metadata and SKILL.md content for a Global Skill Library skill.", None, AuthKind::RemoteAccess),
    route("get", "/workspaces/{workspace_id}/files", "workspaceFilesRoot", "workspaces", "Browse workspace root", "List directory entries at the workspace root. Query parameters: execution_root_id.", None, AuthKind::RemoteAccess),
    route("get", "/workspaces/{workspace_id}/files/{path}", "workspaceFiles", "workspaces", "Browse workspace files", "List a directory or read a file by path. Supports content negotiation: Accept: application/json returns structured metadata + content, other Accept values return raw body. Query parameters: execution_root_id, download, meta.", None, AuthKind::RemoteAccess),
    route_with_response("post", "/jobs", "createJob", "jobs", "Create job", "Create an asynchronous job. Currently supports kind=skill.install for Global Skill Library installation.", Some("CreateJobRequest"), "JobResponse", AuthKind::Control),
    route_with_response("get", "/jobs/{job_id}", "jobStatus", "jobs", "Job status", "Return a generic asynchronous job snapshot by id.", None, "JobResponse", AuthKind::RemoteAccess),
    route("post", "/skills/catalog/add", "addSkillToCatalog", "skills", "Add skill to library", "Add or import a skill into the local Skill Library.", Some("AddSkillRequest"), AuthKind::Control),
    route("post", "/skills/catalog/remove", "removeSkillFromCatalog", "skills", "Remove skill from library", "Remove a skill from the local Skill Library.", Some("RemoveSkillRequest"), AuthKind::Control),
    route("post", "/skills/catalog/reconcile", "reconcileSkillCatalog", "skills", "Reconcile skill library lock", "Reconcile local Skill Library contents with .skill-lock.json, then check consistency. This does not fetch remote updates.", Some("ReconcileSkillRequest"), AuthKind::Control),
    route("post", "/skills/catalog/refresh", "refreshSkillCatalog", "skills", "Refresh runtime catalog", "Refresh runtime Skill Library catalog by rescanning local skill roots. Does not reconcile with lock file or fetch remote updates.", Some("RefreshCatalogRequest"), AuthKind::Control),
    route_with_response("post", "/skills/catalog/update", "updateSkillCatalog", "skills", "Update skill library", "Queue an asynchronous update of supported remote Skill Library entries described by .skill-lock.json. Progress and per-skill results are available through the returned job.", Some("UpdateSkillRequest"), "JobResponse", AuthKind::Control),
    route("post", "/skills/catalog/check", "checkSkillCatalog", "skills", "Check skill library", "Check Skill Library and lock-file consistency.", Some("CheckSkillRequest"), AuthKind::Control),
    route("get", "/templates/catalog", "templatesCatalog", "templates", "Template catalog", "Return the global AgentTemplate catalog (user global library + synced remote sources).", None, AuthKind::RemoteAccess),
    route("get", "/templates/catalog/{catalog_id}", "templateDetail", "templates", "Template detail", "Return template detail with full AGENTS.md content, manifest, and skill dependencies.", None, AuthKind::RemoteAccess),
    route("post", "/templates/catalog/check", "checkTemplate", "templates", "Check template", "Validate a local template directory without applying it.", Some("CheckTemplateRequest"), AuthKind::RemoteAccess),
    route("post", "/templates/remote-sources/sync", "syncTemplateRemoteSources", "templates", "Sync remote template sources", "Queue a daemon job that synchronizes configured AgentTemplate remote sources.", Some("SyncTemplateRemoteSourcesRequest"), AuthKind::Control),
    route("post", "/control/templates/install", "installTemplate", "templates", "Install template", "Install a template package from a GitHub tree URL into the user global library.", Some("InstallTemplateRequest"), AuthKind::Control),
    route("post", "/control/templates/remove", "removeTemplate", "templates", "Remove template", "Remove a template from the user global library.", Some("RemoveTemplateRequest"), AuthKind::Control),
    route_with_response("post", "/search", "runtimeSearch", "search", "Search runtime memory", "Search the same memory v2 index used by the agent MemorySearch tool.", Some("SearchRequest"), "SearchResponse", AuthKind::RemoteAccess),
    route_with_response("post", "/memory/get", "runtimeMemoryGet", "search", "Fetch runtime memory source", "Fetch exact bounded memory content by source_ref, matching the agent MemoryGet tool contract.", Some("MemoryGetRequest"), "MemoryGetResult", AuthKind::RemoteAccess),
    route("post", "/enqueue", "enqueueDefault", "ingress", "Enqueue default agent message", "Enqueue a public channel/webhook message for the default agent.", Some("EnqueueRequest"), AuthKind::RemoteAccess),
    route("post", "/agents/{agent_id}/enqueue", "enqueueAgent", "ingress", "Enqueue agent message", "Enqueue a public channel/webhook message for the named agent.", Some("EnqueueRequest"), AuthKind::RemoteAccess),
    route("post", "/webhooks/generic/{agent_id}", "genericWebhook", "ingress", "Generic webhook", "Convert an arbitrary JSON webhook body into a trusted integration message.", Some("GenericJsonPayload"), AuthKind::None),
    route("post", "/control/agents/{agent_id}/tasks", "createCommandTask", "control", "Create command task", "Schedule a command task for an agent.", Some("CreateCommandTaskRequest"), AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/work-items", "createWorkItem", "control", "Create work item", "Create or enqueue a public work item objective.", Some("CreateWorkItemRequest"), "WorkItemRecord", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/work-items/{work_item_id}/pick", "pickWorkItem", "control", "Pick work item", "Make an existing open work item the current focus for the agent.", Some("PickWorkItemRequest"), "PickWorkItemResponse", AuthKind::Control),
    route_with_response("patch", "/control/agents/{agent_id}/work-items/{work_item_id}", "updateWorkItem", "control", "Update work item", "Mutate work item objective, plan status, todo list, or blocker fields.", Some("UpdateWorkItemRequest"), "WorkItemRecord", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/work-items/{work_item_id}/complete", "completeWorkItem", "control", "Complete work item", "Mark an open work item completed.", Some("CompleteWorkItemRequest"), "WorkItemRecord", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/timers", "createTimer", "control", "Create timer", "Schedule a timer for an agent.", Some("CreateTimerRequest"), "TimerRecord", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/timers/{timer_id}/cancel", "cancelTimer", "control", "Cancel timer", "Cancel an active timer. Cancellation is idempotent for already-cancelled timers; completed or missing timers return a shared error envelope.", Some("CancelTimerRequest"), "TimerRecord", AuthKind::Control),
    route("post", "/control/agents/{agent_id}/create", "createAgent", "control", "Create named agent", "Create a public named agent, optionally from a template.", Some("CreateAgentRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/reset-callback", "resetCallback", "control", "Reset external trigger callback", "Revoke the current external trigger and provision a fresh one with a new token.", None, AuthKind::Control),
    route("post", "/control/agents/{agent_id}/workspace/attach", "attachWorkspace", "control", "Attach workspace", "Attach a workspace path to an agent.", Some("AttachWorkspaceRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/workspace/exit", "exitWorkspace", "control", "Exit workspace", "Return an agent to its default AgentHome workspace.", Some("ExitWorkspaceRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/workspace/detach", "detachWorkspace", "control", "Detach workspace", "Detach a workspace binding by workspace id.", Some("DetachWorkspaceRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/model", "setAgentModel", "control", "Set agent model override", "Set an agent model override and optional reasoning effort.", Some("SetAgentModelRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/model/clear", "clearAgentModel", "control", "Clear agent model override", "Clear an agent model override.", Some("ClearAgentModelRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/control", "controlAgent", "control", "Control agent lifecycle", "Submit a lifecycle control action.", Some("ControlRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/current-run/abort", "abortCurrentRun", "control", "Abort current run", "Request abort for the current agent run.", Some("AbortCurrentRunRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/prompt", "controlPrompt", "control", "Submit operator prompt", "Submit a trusted operator prompt through the control plane.", Some("ControlPromptRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/operator-bindings", "createOperatorTransportBinding", "control", "Create operator binding", "Create or update a remote operator transport binding.", Some("OperatorTransportBindingRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/operator-ingress", "operatorIngress", "control", "Operator ingress", "Deliver an authenticated remote operator prompt.", Some("OperatorIngressRequest"), AuthKind::Control),
    route("get", "/control/runtime/readiness", "runtimeReadiness", "runtime", "Runtime readiness", "Return daemon readiness metadata.", None, AuthKind::Control),
    route("get", "/control/runtime/status", "runtimeStatus", "runtime", "Runtime status", "Return daemon status and runtime activity metadata.", None, AuthKind::Control),
    route_with_response("get", "/control/runtime/performance", "runtimePerformance", "runtime", "Runtime performance diagnostics", "Return bounded in-process performance diagnostics for HTTP, projections, DB, and scheduler activity.", None, "PerformanceDiagnosticsSnapshot", AuthKind::Control),
    route_with_response("get", "/control/runtime/config", "runtimeConfig", "runtime", "Runtime config", "Return the daemon effective runtime configuration surface.", None, "RuntimeConfigReadResponse", AuthKind::Control),
    route_with_response("patch", "/control/runtime/config", "runtimeConfigUpdate", "runtime", "Update runtime config", "Persist runtime-mutable config updates and classify their effect as restart/reload-required or rejected.", Some("RuntimeConfigUpdateRequest"), "RuntimeConfigUpdateResponse", AuthKind::Control),
    route_with_response("post", "/control/runtime/config/migrate-model-routes", "migrateModelConfigRoutes", "runtime", "Migrate model config routes", "Inspect legacy model route references or persist a complete canonical migration across config.json and agent state.", Some("ModelConfigMigrationRequest"), "ModelConfigMigrationReport", AuthKind::Control),
    route("get", "/control/runtime/credentials", "runtimeCredentials", "runtime", "Runtime credential profiles", "List credential profiles stored in the runtime credential store.", None, AuthKind::Control),
    route("put", "/control/runtime/credentials/{profile}", "setRuntimeCredential", "runtime", "Set runtime credential", "Set an API key credential profile in the runtime credential store.", Some("SetCredentialRequest"), AuthKind::Control),
    route("delete", "/control/runtime/credentials/{profile}", "deleteRuntimeCredential", "runtime", "Delete runtime credential", "Remove a credential profile from the runtime credential store.", None, AuthKind::Control),
    route("post", "/auth/codex/device/start", "startCodexDeviceLogin", "auth", "Start Codex device login", "Request an OpenAI Codex device code and start a background job that persists the OAuth credential profile after user authorization.", None, AuthKind::Control),
    route("post", "/auth/{provider}/device/start", "startOAuthDeviceLogin", "auth", "Start OAuth device login", "Request a provider OAuth device code and start a background job that persists the OAuth credential profile after user authorization. Supported providers include openai-codex and xai.", None, AuthKind::Control),
    route("post", "/control/runtime/shutdown", "runtimeShutdown", "runtime", "Runtime shutdown", "Request graceful runtime shutdown.", None, AuthKind::Control),
    route("post", "/control/agents/{agent_id}/debug-prompt", "debugPrompt", "control", "Debug prompt", "Render a diagnostic prompt preview.", Some("DebugPromptRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/wake", "controlWake", "control", "Wake agent", "Submit a trusted wake hint.", Some("ControlWakeRequest"), AuthKind::Control),
    route("post", "/callbacks/enqueue/{callback_token}", "callbackEnqueue", "callbacks", "Callback enqueue ingress", "Capability-token callback ingress for enqueue delivery. The token is a secret path segment and examples intentionally use a placeholder.", Some("CallbackBody"), AuthKind::Capability),
    route("post", "/callbacks/wake/{callback_token}", "callbackWake", "callbacks", "Callback wake ingress", "Capability-token callback ingress for wake delivery. The token is a secret path segment and examples intentionally use a placeholder.", Some("CallbackBody"), AuthKind::Capability),
    route("post", "/control/agents/{agent_id}/skills/enable", "enableSkill", "skills", "Enable agent skill", "Enable a locally known skill for an agent.", Some("EnableSkillRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/skills/disable", "disableSkill", "skills", "Disable agent skill", "Disable a skill for an agent.", Some("DisableSkillRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/skills/install", "installSkill", "skills", "Install skill compatibility alias", "Compatibility alias for older agent skill install behavior.", Some("InstallSkillRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/skills/uninstall", "uninstallSkill", "skills", "Uninstall skill compatibility alias", "Compatibility alias for disabling an agent skill.", Some("UninstallSkillRequest"), AuthKind::Control),
    aide_route("get", "/status", "defaultStatus", "compat", "Default agent status alias", "Compatibility alias for the default agent status route.", None, AuthKind::RemoteAccess),
    aide_route("get", "/briefs", "defaultBriefs", "compat", "Default agent briefs alias", "Compatibility alias for the default agent briefs route. Query parameter: limit.", None, AuthKind::RemoteAccess),
    aide_route("get", "/state", "defaultState", "compat", "Default agent state alias", "Compatibility alias for the default agent state route.", None, AuthKind::RemoteAccess),
    aide_route("get", "/transcript", "defaultTranscript", "compat", "Default agent transcript alias", "Compatibility alias for the default agent transcript route. Query parameter: limit.", None, AuthKind::RemoteAccess),
    aide_route("get", "/worktree-summary", "defaultWorktreeSummary", "compat", "Default agent worktree summary alias", "Compatibility alias for the default agent worktree summary route.", None, AuthKind::RemoteAccess),
];

const fn route(
    method: &'static str,
    path: &'static str,
    operation_id: &'static str,
    tag: &'static str,
    summary: &'static str,
    description: &'static str,
    request_schema: Option<&'static str>,
    auth: AuthKind,
) -> RouteSpec {
    RouteSpec {
        method,
        path,
        operation_id,
        tag,
        summary,
        description,
        request_schema,
        response_schema: None,
        response_kind: ResponseKind::Json,
        auth,
        metadata_source: MetadataSource::Manual,
    }
}

const fn route_with_response(
    method: &'static str,
    path: &'static str,
    operation_id: &'static str,
    tag: &'static str,
    summary: &'static str,
    description: &'static str,
    request_schema: Option<&'static str>,
    response_schema: &'static str,
    auth: AuthKind,
) -> RouteSpec {
    RouteSpec {
        method,
        path,
        operation_id,
        tag,
        summary,
        description,
        request_schema,
        response_schema: Some(response_schema),
        response_kind: ResponseKind::Json,
        auth,
        metadata_source: MetadataSource::Manual,
    }
}

const fn aide_route(
    method: &'static str,
    path: &'static str,
    operation_id: &'static str,
    tag: &'static str,
    summary: &'static str,
    description: &'static str,
    request_schema: Option<&'static str>,
    auth: AuthKind,
) -> RouteSpec {
    RouteSpec {
        method,
        path,
        operation_id,
        tag,
        summary,
        description,
        request_schema,
        response_schema: None,
        response_kind: ResponseKind::Json,
        auth,
        metadata_source: MetadataSource::Aide,
    }
}

const fn event_stream_route(
    method: &'static str,
    path: &'static str,
    operation_id: &'static str,
    tag: &'static str,
    summary: &'static str,
    description: &'static str,
    request_schema: Option<&'static str>,
    auth: AuthKind,
) -> RouteSpec {
    RouteSpec {
        method,
        path,
        operation_id,
        tag,
        summary,
        description,
        request_schema,
        response_schema: None,
        response_kind: ResponseKind::EventStream,
        auth,
        metadata_source: MetadataSource::Manual,
    }
}

pub fn generate_openapi_json() -> Value {
    openapi_value()
}

fn openapi_value() -> Value {
    let mut paths = serde_json::Map::new();
    for spec in ROUTES
        .iter()
        .filter(|spec| spec.metadata_source == MetadataSource::Manual)
    {
        let entry = paths
            .entry(format!("/api{}", spec.path))
            .or_insert_with(|| json!({}));
        entry[spec.method] = operation(spec);
    }
    merge_path_items(&mut paths, aide_route_paths());

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Holon HTTP control-plane API",
            "version": API_VERSION,
            "description": "Generated baseline for Holon's current HTTP control-plane surface. It is intentionally conservative: many response bodies remain JSON placeholders until the success/error envelope contracts stabilize."
        },
        "servers": [{ "url": "http://127.0.0.1:7878", "description": "Local runtime; port is configurable." }],
        "tags": [
            { "name": "discovery" },
            { "name": "agents" },
            { "name": "events" },
            { "name": "messages" },
            { "name": "ingress" },
            { "name": "control" },
            { "name": "runtime" },
            { "name": "tasks" },
            { "name": "work-items" },
            { "name": "timers" },
            { "name": "skills" },
            { "name": "templates" },
            { "name": "jobs" },
            { "name": "search" },
            { "name": "callbacks", "description": "Capability-token callback ingress. Never publish real callback_token values." },
            { "name": "compat" }
        ],
        "components": {
            "securitySchemes": {
                "BearerAuth": { "type": "http", "scheme": "bearer" },
                "CallbackToken": {
                    "type": "apiKey",
                    "in": "path",
                    "name": "callback_token",
                    "description": "Opaque capability secret path segment. Use placeholders in documentation and tests."
                }
            },
            "schemas": component_schemas()
        },
        "paths": paths
    })
}

fn merge_path_items(
    paths: &mut serde_json::Map<String, Value>,
    incoming: serde_json::Map<String, Value>,
) {
    for (path, path_item) in incoming {
        match (
            paths.get_mut(&path).and_then(Value::as_object_mut),
            path_item,
        ) {
            (Some(existing), Value::Object(incoming_methods)) => {
                existing.extend(incoming_methods);
            }
            (_, value) => {
                paths.insert(path, value);
            }
        }
    }
}

fn aide_route_paths() -> serde_json::Map<String, Value> {
    let mut router: ApiRouter<()> = ApiRouter::new();
    for spec in ROUTES
        .iter()
        .filter(|spec| spec.metadata_source == MetadataSource::Aide)
    {
        router = router.api_route_docs(
            &format!("/api{}", spec.path),
            ApiMethodDocs::new(spec.method, aide_operation(spec)),
        );
    }

    let mut api = OpenApi::default();
    let _ = router.finish_api(&mut api);
    serde_json::to_value(api)
        .expect("serialize aide OpenAPI route metadata")
        .get("paths")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn aide_operation(spec: &RouteSpec) -> Operation {
    let mut operation: Operation =
        serde_json::from_value(operation(spec)).expect("convert baseline operation to aide");
    operation.extensions.insert(
        "x-holon-openapi-source".into(),
        json!("aide::axum::ApiRouter::api_route_docs"),
    );
    operation
}

fn operation(spec: &RouteSpec) -> Value {
    let mut op = json!({
        "operationId": spec.operation_id,
        "tags": [spec.tag],
        "summary": spec.summary,
        "description": spec.description,
        "parameters": path_parameters(spec.path),
        "responses": responses(spec.response_kind, spec.response_schema),
    });
    if let Some(schema) = spec.request_schema {
        op["requestBody"] = json!({
            "required": true,
            "content": {
                "application/json": {
                    "schema": { "$ref": format!("#/components/schemas/{schema}") }
                }
            }
        });
    }
    match spec.auth {
        AuthKind::RemoteAccess | AuthKind::Control => {
            op["security"] = json!([{ "BearerAuth": [] }])
        }
        AuthKind::Capability => op["security"] = json!([{ "CallbackToken": [] }]),
        AuthKind::None => {}
    }
    op
}

fn path_parameters(path: &str) -> Vec<Value> {
    let mut params = Vec::new();
    if path.contains("{agent_id}") {
        params.push(path_param("agent_id", "Agent id."));
    }
    if path.contains("{task_id}") {
        params.push(path_param("task_id", "Task id."));
    }
    if path.contains("{work_item_id}") {
        params.push(path_param("work_item_id", "Work item id."));
    }
    if path.contains("{brief_id}") {
        params.push(path_param("brief_id", "Brief id."));
    }
    if path.contains("{timer_id}") {
        params.push(path_param("timer_id", "Timer id."));
    }
    if path.contains("{tool_execution_id}") {
        params.push(path_param("tool_execution_id", "Tool execution id."));
    }
    if path.contains("{artifact_index}") {
        params.push(path_integer_param(
            "artifact_index",
            "Zero-based artifact index within the tool execution result.",
        ));
    }
    if path.contains("{message_id}") {
        params.push(path_param("message_id", "Message id."));
    }
    if path.contains("{skill_id}") {
        params.push(path_param("skill_id", "Root-qualified skill id."));
    }
    if path.contains("{callback_token}") {
        params.push(path_param(
            "callback_token",
            "Opaque callback capability token. This is a secret and must not be exposed in examples.",
        ));
    }
    params
}

fn path_param(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "in": "path",
        "required": true,
        "description": description,
        "schema": { "type": "string" }
    })
}

fn path_integer_param(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "in": "path",
        "required": true,
        "description": description,
        "schema": { "type": "integer", "minimum": 0 }
    })
}

fn responses(kind: ResponseKind, response_schema: Option<&str>) -> Value {
    match kind {
        ResponseKind::Json => {
            let success_description = if response_schema.is_some() {
                "Successful JSON response using a stable DTO schema."
            } else {
                "Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized."
            };
            let success_schema = response_schema
                .map(|schema| json!({ "$ref": format!("#/components/schemas/{schema}") }))
                .unwrap_or_else(|| json!({ "$ref": "#/components/schemas/JsonValue" }));

            json!({
                "200": {
                    "description": success_description,
                    "content": { "application/json": { "schema": success_schema } }
                },
                "4XX": {
                    "description": "Client error JSON response.",
                    "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } }
                },
                "5XX": {
                    "description": "Server error JSON response.",
                    "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } }
                }
            })
        }
        ResponseKind::EventStream => json!({
            "200": {
                "description": "Server-Sent Events stream. Each data frame contains a StreamEventEnvelope JSON object.",
                "content": { "text/event-stream": { "schema": { "type": "string" } } }
            },
            "4XX": {
                "description": "Client error before stream establishment.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } }
            }
        }),
    }
}

fn component_schemas() -> Value {
    let request_schema = json!({
        "type": "object",
        "description": "Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize.",
        "additionalProperties": true
    });
    let mut schemas = serde_json::Map::new();
    schemas.insert("JsonValue".into(), json!({
        "description": "Arbitrary JSON value. Used as a conservative baseline for routes whose DTO is not yet stabilized."
    }));
    schemas.insert(
        "ErrorResponse".into(),
        json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean", "const": false },
                "error": { "type": "string" },
                "code": { "type": "string" },
                "agent_id": { "type": "string" },
                "hint": { "type": "string" },
                "after_seq": { "type": "integer", "minimum": 0 },
                "event_seq": { "type": "integer", "minimum": 0 }
            },
            "required": ["error"],
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "GenericJsonPayload".into(),
        json!({ "$ref": "#/components/schemas/JsonValue" }),
    );
    schemas.insert("CallbackBody".into(), json!({
        "description": "Raw callback request body. JSON and text bodies are parsed; other content types are represented internally as base64 JSON."
    }));
    schemas.insert(
        "CreateJobRequest".into(),
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["skill.install", "skill.update"] },
                "params": { "$ref": "#/components/schemas/AddSkillRequest" }
            },
            "required": ["kind", "params"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "JobResponse".into(),
        json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean" },
                "job": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "kind": { "type": "string" },
                        "status": { "type": "string", "enum": ["queued", "running", "completed", "failed"] },
                        "phase": { "type": "string" },
                        "progress": { "type": "object", "additionalProperties": true },
                        "summary": { "type": "string" },
                        "items": { "type": "array", "items": { "type": "object", "additionalProperties": true } },
                        "result": { "type": "object", "additionalProperties": true },
                        "error": { "type": "string" },
                        "created_at": { "type": "string", "format": "date-time" },
                        "updated_at": { "type": "string", "format": "date-time" }
                    },
                    "required": ["id", "kind", "status", "phase", "progress", "summary", "items", "created_at", "updated_at"],
                    "additionalProperties": true
                }
            },
            "required": ["ok", "job"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "TaskStatusSnapshot".into(),
        component_schema::<TaskStatusSnapshot>(),
    );
    schemas.insert(
        "TaskOutputResult".into(),
        component_schema::<TaskOutputResult>(),
    );
    schemas.insert(
        "ToolExecutionRecord".into(),
        component_schema::<ToolExecutionRecord>(),
    );
    schemas.insert(
        "ToolExecutionArtifactContent".into(),
        component_schema::<crate::http::ToolExecutionArtifactContent>(),
    );
    schemas.insert(
        "TaskInputResult".into(),
        component_schema::<TaskInputResult>(),
    );
    schemas.insert(
        "TaskStopResult".into(),
        component_schema::<TaskStopResult>(),
    );
    schemas.insert(
        "WorkItemRecord".into(),
        component_schema::<WorkItemRecord>(),
    );
    schemas.insert("BriefRecord".into(), component_schema::<BriefRecord>());
    schemas.insert("TimerRecord".into(), component_schema::<TimerRecord>());
    schemas.insert(
        "PickWorkItemResponse".into(),
        component_schema::<PickWorkItemResponse>(),
    );
    schemas.insert(
        "RuntimeConfigReadResponse".into(),
        component_schema::<RuntimeConfigReadResponse>(),
    );
    schemas.insert(
        "PerformanceDiagnosticsSnapshot".into(),
        component_schema::<PerformanceDiagnosticsSnapshot>(),
    );
    schemas.insert(
        "RuntimeConfigUpdateRequest".into(),
        component_schema::<RuntimeConfigUpdateRequest>(),
    );
    schemas.insert(
        "RuntimeConfigUpdateResponse".into(),
        component_schema::<RuntimeConfigUpdateResponse>(),
    );
    schemas.insert(
        "ModelConfigMigrationRequest".into(),
        component_schema::<ModelConfigMigrationRequest>(),
    );
    schemas.insert(
        "ModelConfigMigrationReport".into(),
        component_schema::<ModelConfigMigrationReport>(),
    );
    schemas.insert(
        "PickWorkItemRequest".into(),
        component_schema::<PickWorkItemRequest>(),
    );
    schemas.insert(
        "UpdateWorkItemRequest".into(),
        component_schema::<UpdateWorkItemRequest>(),
    );
    schemas.insert(
        "CompleteWorkItemRequest".into(),
        component_schema::<CompleteWorkItemRequest>(),
    );
    schemas.insert(
        "CreateTimerRequest".into(),
        component_schema::<CreateTimerRequest>(),
    );
    schemas.insert(
        "CancelTimerRequest".into(),
        component_schema::<CancelTimerRequest>(),
    );
    schemas.insert("SearchRequest".into(), component_schema::<SearchRequest>());
    schemas.insert(
        "SearchResponse".into(),
        component_schema::<SearchResponse>(),
    );
    schemas.insert(
        "MemoryGetRequest".into(),
        component_schema::<MemoryGetRequest>(),
    );
    schemas.insert(
        "MemoryGetResult".into(),
        component_schema::<MemoryGetResult>(),
    );
    schemas.insert(
        "AddSkillRequest".into(),
        component_schema::<AddSkillRequest>(),
    );
    schemas.insert(
        "BatchGetMessagesRequest".into(),
        component_schema::<BatchGetMessagesRequest>(),
    );
    schemas.insert(
        "BatchGetMessagesResponse".into(),
        json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": { "$ref": "#/components/schemas/JsonValue" }
                },
                "missing_message_ids": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["messages"],
            "additionalProperties": false
        }),
    );
    for name in [
        "EnqueueRequest",
        "IncomingOrigin",
        "CreateCommandTaskRequest",
        "TaskInputRequest",
        "TaskStopRequest",
        "CreateWorkItemRequest",
        "CreateAgentRequest",
        "AttachWorkspaceRequest",
        "ExitWorkspaceRequest",
        "DetachWorkspaceRequest",
        "SetAgentModelRequest",
        "ClearAgentModelRequest",
        "ControlRequest",
        "AbortCurrentRunRequest",
        "ControlPromptRequest",
        "OperatorTransportBindingRequest",
        "OperatorIngressRequest",
        "SetCredentialRequest",
        "DebugPromptRequest",
        "ControlWakeRequest",
        "RemoveSkillRequest",
        "EnableSkillRequest",
        "DisableSkillRequest",
        "InstallSkillRequest",
        "UninstallSkillRequest",
        "CheckTemplateRequest",
        "InstallTemplateRequest",
        "RemoveTemplateRequest",
    ] {
        schemas.insert(name.into(), request_schema.clone());
    }
    Value::Object(schemas)
}

fn component_schema<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft07()
        .with(|settings| {
            settings.inline_subschemas = true;
        })
        .into_generator()
        .into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema).expect("serialize OpenAPI component schema");
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_openapi_includes_current_http_surface() {
        let api = generate_openapi_json();
        let paths = api["paths"].as_object().expect("paths object");
        let operation_count = paths
            .values()
            .flat_map(|path| path.as_object().into_iter().flat_map(|ops| ops.keys()))
            .count();
        assert!(operation_count >= 40, "expected baseline coverage");
        assert!(paths["/api/events/stream"]["get"].is_object());
        assert!(paths["/api/agents/{agent_id}/events/stream"]["get"].is_object());
        assert!(paths["/api/callbacks/wake/{callback_token}"]["post"].is_object());
        assert_eq!(
            paths["/api/agents/{agent_id}/status"]["get"]["x-holon-openapi-source"],
            "aide::axum::ApiRouter::api_route_docs"
        );
        assert!(
            paths["/api/control/agents/{agent_id}/tasks"]["post"]["x-holon-openapi-source"]
                .is_null()
        );
    }

    #[test]
    fn aide_paths_merge_with_existing_path_items() {
        let mut paths = serde_json::Map::new();
        paths.insert(
            "/shared".into(),
            json!({
                "get": { "operationId": "manualGet" },
                "parameters": [{ "name": "shared" }]
            }),
        );

        let mut incoming = serde_json::Map::new();
        incoming.insert(
            "/shared".into(),
            json!({
                "post": { "operationId": "aidePost" }
            }),
        );
        incoming.insert(
            "/aide-only".into(),
            json!({
                "get": { "operationId": "aideOnly" }
            }),
        );

        merge_path_items(&mut paths, incoming);

        assert_eq!(paths["/shared"]["get"]["operationId"], "manualGet");
        assert_eq!(paths["/shared"]["post"]["operationId"], "aidePost");
        assert_eq!(paths["/shared"]["parameters"][0]["name"], "shared");
        assert_eq!(paths["/aide-only"]["get"]["operationId"], "aideOnly");
    }
}
