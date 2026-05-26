use aide::{
    axum::{routing::ApiMethodDocs, ApiRouter},
    openapi::{OpenApi, Operation},
};
use schemars::{generate::SchemaSettings, JsonSchema};
use serde_json::{json, Value};

use crate::types::{TaskInputResult, TaskOutputResult, TaskStatusSnapshot, TaskStopResult};

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
    aide_route("get", "/agents/{agent_id}/state", "agentState", "agents", "Agent state snapshot", "Return the current bootstrap snapshot for an agent.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/events", "agentEvents", "events", "Agent event page", "Return a bounded page of projected runtime events. Query parameters: before_seq, after_seq, limit, order, projection. The operator projection redacts raw/debug payload fields; local_debug requires control auth and preserves raw payloads.", None, AuthKind::RemoteAccess),
    event_stream_route("get", "/agents/{agent_id}/events/stream", "agentEventsStream", "events", "Agent event stream", "Return Server-Sent Events carrying StreamEventEnvelope JSON data. Query parameters: after_seq, limit, projection. SSE id is event_seq; SSE event is the audit event kind; missing replay cursors return cursor_not_found before the stream opens.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/transcript", "agentTranscript", "agents", "Recent transcript", "Return recent transcript entries. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/tasks", "agentTasks", "tasks", "List active tasks", "Return active task records. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tasks/{task_id}", "agentTaskStatus", "tasks", "Task status", "Return a task lifecycle snapshot by id.", None, "TaskStatusSnapshot", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tasks/{task_id}/output", "agentTaskOutput", "tasks", "Task output", "Return a task output snapshot. Query parameters: block, timeout_ms.", None, "TaskOutputResult", AuthKind::RemoteAccess),
    route_with_response("post", "/control/agents/{agent_id}/tasks/{task_id}/input", "taskInput", "control", "Task input", "Deliver text input to a managed task.", Some("TaskInputRequest"), "TaskInputResult", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/tasks/{task_id}/stop", "taskStop", "control", "Task stop", "Request cancellation for a managed task.", Some("TaskStopRequest"), "TaskStopResult", AuthKind::Control),
    route("get", "/agents/{agent_id}/work-items", "agentWorkItems", "work-items", "List work items", "Return latest work item records for the agent. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/work-items/{work_item_id}", "agentWorkItem", "work-items", "Work item detail", "Return a work item record by id.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/worktree-summary", "agentWorktreeSummary", "agents", "Worktree summary", "Return managed worktree summary for an agent.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/timers", "agentTimers", "timers", "List timers", "Return recent timer records. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/timers/{timer_id}", "agentTimer", "timers", "Timer detail", "Return a timer record by id.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/skills", "agentSkills", "skills", "List skills", "Return installed skills for an agent.", None, AuthKind::RemoteAccess),
    route("post", "/enqueue", "enqueueDefault", "ingress", "Enqueue default agent message", "Enqueue a public channel/webhook message for the default agent.", Some("EnqueueRequest"), AuthKind::RemoteAccess),
    route("post", "/agents/{agent_id}/enqueue", "enqueueAgent", "ingress", "Enqueue agent message", "Enqueue a public channel/webhook message for the named agent.", Some("EnqueueRequest"), AuthKind::RemoteAccess),
    route("post", "/webhooks/generic/{agent_id}", "genericWebhook", "ingress", "Generic webhook", "Convert an arbitrary JSON webhook body into a trusted integration message.", Some("GenericJsonPayload"), AuthKind::None),
    route("post", "/control/agents/{agent_id}/tasks", "createCommandTask", "control", "Create command task", "Schedule a command task for an agent.", Some("CreateCommandTaskRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/work-items", "createWorkItem", "control", "Create work item", "Create or enqueue a public work item objective.", Some("CreateWorkItemRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/timers", "createTimer", "control", "Create timer", "Schedule a timer for an agent.", Some("CreateTimerRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/create", "createAgent", "control", "Create named agent", "Create a public named agent, optionally from a template.", Some("CreateAgentRequest"), AuthKind::Control),
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
    route("post", "/control/runtime/shutdown", "runtimeShutdown", "runtime", "Runtime shutdown", "Request graceful runtime shutdown.", None, AuthKind::Control),
    route("post", "/control/agents/{agent_id}/debug-prompt", "debugPrompt", "control", "Debug prompt", "Render a diagnostic prompt preview.", Some("DebugPromptRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/wake", "controlWake", "control", "Wake agent", "Submit a trusted wake hint.", Some("ControlWakeRequest"), AuthKind::Control),
    route("post", "/callbacks/enqueue/{callback_token}", "callbackEnqueue", "callbacks", "Callback enqueue ingress", "Capability-token callback ingress for enqueue delivery. The token is a secret path segment and examples intentionally use a placeholder.", Some("CallbackBody"), AuthKind::Capability),
    route("post", "/callbacks/wake/{callback_token}", "callbackWake", "callbacks", "Callback wake ingress", "Capability-token callback ingress for wake delivery. The token is a secret path segment and examples intentionally use a placeholder.", Some("CallbackBody"), AuthKind::Capability),
    route("post", "/control/agents/{agent_id}/skills/install", "installSkill", "skills", "Install skill", "Install an agent skill.", Some("InstallSkillRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/skills/uninstall", "uninstallSkill", "skills", "Uninstall skill", "Uninstall an agent skill.", Some("UninstallSkillRequest"), AuthKind::Control),
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
            .entry(spec.path.to_string())
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
            { "name": "ingress" },
            { "name": "control" },
            { "name": "runtime" },
            { "name": "tasks" },
            { "name": "work-items" },
            { "name": "timers" },
            { "name": "skills" },
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
            spec.path,
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
    if path.contains("{timer_id}") {
        params.push(path_param("timer_id", "Timer id."));
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
        "TaskStatusSnapshot".into(),
        component_schema::<TaskStatusSnapshot>(),
    );
    schemas.insert(
        "TaskOutputResult".into(),
        component_schema::<TaskOutputResult>(),
    );
    schemas.insert(
        "TaskInputResult".into(),
        component_schema::<TaskInputResult>(),
    );
    schemas.insert(
        "TaskStopResult".into(),
        component_schema::<TaskStopResult>(),
    );
    for name in [
        "EnqueueRequest",
        "IncomingOrigin",
        "CreateCommandTaskRequest",
        "TaskInputRequest",
        "TaskStopRequest",
        "CreateWorkItemRequest",
        "CreateTimerRequest",
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
        "DebugPromptRequest",
        "ControlWakeRequest",
        "InstallSkillRequest",
        "UninstallSkillRequest",
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
        assert!(paths["/agents/{agent_id}/events/stream"]["get"].is_object());
        assert!(paths["/callbacks/wake/{callback_token}"]["post"].is_object());
        assert_eq!(
            paths["/agents/{agent_id}/status"]["get"]["x-holon-openapi-source"],
            "aide::axum::ApiRouter::api_route_docs"
        );
        assert!(
            paths["/control/agents/{agent_id}/tasks"]["post"]["x-holon-openapi-source"].is_null()
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
