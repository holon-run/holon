use holon_server::openapi::{
    component_schema, component_schema_with_refs, error_response_schema, ConversationShadowLimits,
};
use serde_json::{json, Map, Value};

use crate::{
    diagnostics::PerformanceDiagnosticsSnapshot,
    domain::conversation::ConversationShadowDiagnostics,
    http::{
        AgentDeletionResponse, AgentDeletionStatusResponse, BatchGetBriefsRequest,
        BatchGetMessagesRequest, BatchGetTranscriptEntriesRequest, CancelTimerRequest,
        CompleteWorkItemRequest, ControlPromptRequest, ConversationActivityResponse,
        ConversationLocateRequest, ConversationLocateResponse, ConversationReadQuery,
        ConversationShadowQuery, ConversationStreamMessage, ConversationSummaryResponse,
        CreateContentReportRequest, CreateContentReportResponse, CreateTimerRequest,
        CurrentUserResponse, DeleteAgentRequest, DesktopCapabilities, EnqueueResponse,
        HandshakeResponse, MarkBriefReadRequest, MemoryGetRequest, ModelConfigMigrationRequest,
        NativeSessionResponse, PairingIssueResponse, PairingRedeemRequest, PickWorkItemRequest,
        PickWorkItemResponse, ResolveFileReferencesRequest, ResolveFileReferencesResponse,
        RevealFileRequest, RuntimeConfigReadResponse, RuntimeConfigUpdateRequest,
        RuntimeConfigUpdateResponse, RuntimeDecisionTestRequest, RuntimeDecisionTestResponse,
        SearchRequest, SearchResponse, SessionExchangeRequest, SessionResponse,
        TailscaleServeStatus, UpdateWorkItemRequest, CONVERSATION_SHADOW_DEFAULT_LIMIT,
    },
    http_dto::{AgentStateSnapshotDto, SlimTaskDto, SlimWorkItemDto, TaskRecordDto},
    memory::MemoryGetResult,
    model_config_migration::ModelConfigMigrationReport,
    observability::{RecentTrace, RecentTraceSummary},
    runtime::{SchedulerRepairInspection, SchedulerRepairRequest, SchedulerRepairResult},
    types::{
        AddSkillRequest, AgentListEntry, BriefRecord, CheckSkillRequest, ReconcileSkillRequest,
        RefreshCatalogRequest, SyncTemplateRemoteSourcesRequest, TaskInputResult, TaskOutputResult,
        TaskStatusSnapshot, TaskStopResult, TimerRecord, ToolExecutionRecord, WorkItemRecord,
    },
};

const API_VERSION: &str = env!("HOLON_PRODUCT_VERSION");

pub fn generate_openapi_json() -> Value {
    holon_server::openapi::generate(
        API_VERSION,
        component_schemas(),
        ConversationShadowLimits {
            default: CONVERSATION_SHADOW_DEFAULT_LIMIT,
            maximum: crate::runtime_db::conversation::MAX_CONVERSATION_SHADOW_TURNS,
        },
    )
}

fn component_schemas() -> Map<String, Value> {
    let request_schema = json!({
        "type": "object",
        "description": "Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize.",
        "additionalProperties": true
    });
    let mut schemas = serde_json::Map::new();
    schemas.insert("JsonValue".into(), json!({
        "description": "Arbitrary JSON value. Used as a conservative baseline for routes whose DTO is not yet stabilized."
    }));
    schemas.insert("ErrorResponse".into(), error_response_schema());
    schemas.insert(
        "InboxNotificationList".into(),
        component_schema::<crate::runtime_db::notifications::InboxNotificationList>(),
    );
    schemas.insert(
        "HandshakeResponse".into(),
        component_schema_with_refs::<HandshakeResponse>(),
    );
    schemas.insert(
        "CreateContentReportRequest".into(),
        component_schema::<CreateContentReportRequest>(),
    );
    schemas.insert(
        "CreateContentReportResponse".into(),
        component_schema::<CreateContentReportResponse>(),
    );
    schemas.insert(
        "AgentListResponse".into(),
        component_schema_with_refs::<Vec<AgentListEntry>>(),
    );
    schemas.insert(
        "CurrentUserResponse".into(),
        component_schema_with_refs::<CurrentUserResponse>(),
    );
    schemas.insert(
        "SessionExchangeRequest".into(),
        component_schema_with_refs::<SessionExchangeRequest>(),
    );
    schemas.insert(
        "SessionResponse".into(),
        component_schema_with_refs::<SessionResponse>(),
    );
    schemas.insert(
        "NativeSessionResponse".into(),
        component_schema_with_refs::<NativeSessionResponse>(),
    );
    schemas.insert(
        "PairingIssueResponse".into(),
        component_schema_with_refs::<PairingIssueResponse>(),
    );
    schemas.insert(
        "PairingRedeemRequest".into(),
        component_schema_with_refs::<PairingRedeemRequest>(),
    );
    schemas.insert(
        "ControlPromptRequest".into(),
        component_schema_with_refs::<ControlPromptRequest>(),
    );
    schemas.insert(
        "EnqueueResponse".into(),
        component_schema_with_refs::<EnqueueResponse>(),
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
        "TaskListResponse".into(),
        component_schema_with_refs::<Vec<TaskRecordDto>>(),
    );
    schemas.insert(
        "AgentStateSnapshotDto".into(),
        component_schema::<AgentStateSnapshotDto>(),
    );
    schemas.insert(
        "StreamEventEnvelope".into(),
        component_schema::<crate::http::StreamEventEnvelope>(),
    );
    schemas.insert(
        "EventsPageResponse".into(),
        component_schema::<crate::http::EventsPageResponse>(),
    );
    schemas.insert(
        "ConversationReadQuery".into(),
        component_schema::<ConversationReadQuery>(),
    );
    schemas.insert(
        "ConversationShadowQuery".into(),
        component_schema::<ConversationShadowQuery>(),
    );
    schemas.insert(
        "ConversationSummaryResponse".into(),
        component_schema::<ConversationSummaryResponse>(),
    );
    schemas.insert(
        "ConversationLocateRequest".into(),
        component_schema::<ConversationLocateRequest>(),
    );
    schemas.insert(
        "ConversationLocateResponse".into(),
        component_schema::<ConversationLocateResponse>(),
    );
    schemas.insert(
        "ConversationActivityResponse".into(),
        component_schema::<ConversationActivityResponse>(),
    );
    schemas.insert(
        "ConversationStreamMessage".into(),
        component_schema::<ConversationStreamMessage>(),
    );
    schemas.insert(
        "ConversationShadowDiagnostics".into(),
        component_schema::<ConversationShadowDiagnostics>(),
    );
    schemas.insert("SlimTaskDto".into(), component_schema::<SlimTaskDto>());
    schemas.insert(
        "SlimWorkItemDto".into(),
        component_schema::<SlimWorkItemDto>(),
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
    schemas.insert(
        "MarkBriefReadRequest".into(),
        component_schema::<MarkBriefReadRequest>(),
    );
    schemas.insert(
        "BriefReadState".into(),
        json!({
            "type": "object",
            "properties": {
                "agent_id": { "type": "string" },
                "event_log_epoch": { "type": "string" },
                "visibility_scope_id": { "type": "string" },
                "event_head_seq": { "type": "integer", "format": "int64", "minimum": 0 },
                "oldest_retained_seq": { "type": "integer", "format": "int64", "minimum": 0 },
                "read_through_event_seq": { "type": "integer", "format": "int64", "minimum": 0 },
                "unread_count": { "type": "integer", "format": "int64", "minimum": 0 },
                "revision": { "type": "integer", "format": "int64", "minimum": 0 },
                "reset_required": { "type": "boolean" },
                "retention_gap": { "type": "boolean" }
            },
            "required": [
                "agent_id", "event_log_epoch", "visibility_scope_id",
                "event_head_seq", "oldest_retained_seq",
                "read_through_event_seq", "unread_count", "revision",
                "reset_required", "retention_gap"
            ],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BriefReadStates".into(),
        json!({
            "type": "array",
            "items": { "$ref": "#/components/schemas/BriefReadState" }
        }),
    );
    schemas.insert(
        "MarkBriefReadResult".into(),
        json!({
            "type": "object",
            "properties": {
                "state": { "$ref": "#/components/schemas/BriefReadState" },
                "applied_read_through_event_seq": {
                    "type": "integer", "format": "int64", "minimum": 0
                }
            },
            "required": ["state", "applied_read_through_event_seq"],
            "additionalProperties": false
        }),
    );
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
        "TailscaleServeStatus".into(),
        component_schema::<TailscaleServeStatus>(),
    );
    schemas.insert(
        "PerformanceDiagnosticsSnapshot".into(),
        component_schema::<PerformanceDiagnosticsSnapshot>(),
    );
    schemas.insert(
        "RecentTraceSummaryList".into(),
        component_schema::<Vec<RecentTraceSummary>>(),
    );
    schemas.insert("RecentTrace".into(), component_schema::<RecentTrace>());
    schemas.insert(
        "RuntimeConfigUpdateRequest".into(),
        component_schema::<RuntimeConfigUpdateRequest>(),
    );
    schemas.insert(
        "RuntimeConfigUpdateResponse".into(),
        component_schema::<RuntimeConfigUpdateResponse>(),
    );
    schemas.insert(
        "RuntimeDecisionTestRequest".into(),
        component_schema::<RuntimeDecisionTestRequest>(),
    );
    schemas.insert(
        "RuntimeDecisionTestResponse".into(),
        component_schema::<RuntimeDecisionTestResponse>(),
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
        "SchedulerRepairInspection".into(),
        component_schema::<SchedulerRepairInspection>(),
    );
    schemas.insert(
        "SchedulerRepairRequest".into(),
        component_schema::<SchedulerRepairRequest>(),
    );
    schemas.insert(
        "SchedulerRepairResult".into(),
        component_schema::<SchedulerRepairResult>(),
    );
    schemas.insert(
        "DeleteAgentRequest".into(),
        component_schema::<DeleteAgentRequest>(),
    );
    schemas.insert(
        "AgentDeletionResponse".into(),
        component_schema::<AgentDeletionResponse>(),
    );
    schemas.insert(
        "AgentDeletionStatusResponse".into(),
        component_schema::<AgentDeletionStatusResponse>(),
    );
    schemas.insert(
        "AgentDetail".into(),
        component_schema::<crate::types::AgentDetail>(),
    );
    schemas.insert(
        "AgentTreeProjection".into(),
        component_schema::<crate::types::AgentTreeProjection>(),
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
        "DesktopCapabilities".into(),
        component_schema::<DesktopCapabilities>(),
    );
    schemas.insert(
        "RevealFileRequest".into(),
        component_schema::<RevealFileRequest>(),
    );
    schemas.insert(
        "ResolveFileReferencesRequest".into(),
        component_schema::<ResolveFileReferencesRequest>(),
    );
    schemas.insert(
        "ResolveFileReferencesResponse".into(),
        component_schema::<ResolveFileReferencesResponse>(),
    );
    schemas.insert(
        "AddSkillRequest".into(),
        component_schema::<AddSkillRequest>(),
    );
    schemas.insert(
        "BatchGetBriefsRequest".into(),
        component_schema::<BatchGetBriefsRequest>(),
    );
    schemas.insert(
        "BatchGetMessagesRequest".into(),
        component_schema::<BatchGetMessagesRequest>(),
    );
    schemas.insert(
        "BatchGetTranscriptEntriesRequest".into(),
        component_schema::<BatchGetTranscriptEntriesRequest>(),
    );
    schemas.insert(
        "BatchGetBriefsResponse".into(),
        json!({
            "type": "object",
            "properties": {
                "briefs": {
                    "type": "array",
                    "items": { "$ref": "#/components/schemas/BriefRecord" }
                },
                "missing_brief_ids": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["briefs"],
            "additionalProperties": false
        }),
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
    schemas.insert(
        "BatchGetTranscriptEntriesResponse".into(),
        json!({
            "type": "object",
            "properties": {
                "entries": {
                    "type": "array",
                    "items": { "$ref": "#/components/schemas/JsonValue" }
                },
                "missing_entry_ids": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["entries"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CheckSkillRequest".into(),
        component_schema::<CheckSkillRequest>(),
    );
    schemas.insert(
        "ReconcileSkillRequest".into(),
        component_schema::<ReconcileSkillRequest>(),
    );
    schemas.insert(
        "RefreshCatalogRequest".into(),
        component_schema::<RefreshCatalogRequest>(),
    );
    schemas.insert(
        "UpdateSkillRequest".into(),
        component_schema::<ReconcileSkillRequest>(),
    );
    schemas.insert(
        "SyncTemplateRemoteSourcesRequest".into(),
        component_schema::<SyncTemplateRemoteSourcesRequest>(),
    );
    schemas.insert(
        "AgentRosterSnapshot".into(),
        json!({
            "type": "object",
            "description": "Authoritative roster snapshot (RFC: observer sync). All-or-nothing membership with per-Agent event windows and latest Brief anchors. Served by GET /api/agents/snapshot only while the agents.roster-snapshot.v1 capability is advertised; route registration alone is never sufficient.",
            "properties": {
                "contract_version": { "type": "integer", "const": 1 },
                "runtime_id": { "type": "string", "description": "Stable public identity of the runtime installation. Distinguishes a replaced server at the same URL from an ordinary restart. Not a secret." },
                "event_log_epoch": { "type": "string", "description": "Current event log epoch; a changed value means historical event continuity is intentionally reset." },
                "visibility_scope_id": { "type": "string", "description": "Server-generated scope id derived from stable runtime identity, resolved authority, normalized visibility entitlement, and policy generation. Never contains credentials or tokens." },
                "agents": {
                    "type": "array",
                    "description": "Active public Agents visible to the caller in one committed read view. Private child Agents are never included. A failure to assemble one Agent fails the whole response.",
                    "items": { "$ref": "#/components/schemas/AgentRosterEntry" }
                }
            },
            "required": ["contract_version", "runtime_id", "event_log_epoch", "visibility_scope_id", "agents"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AgentRosterEntry".into(),
        json!({
            "type": "object",
            "properties": {
                "agent": { "$ref": "#/components/schemas/AgentListEntry" },
                "event_window": { "$ref": "#/components/schemas/AgentEventWindow" },
                "latest_brief": {
                    "oneOf": [
                        { "$ref": "#/components/schemas/AgentLatestBrief" },
                        { "type": "null" }
                    ],
                    "description": "Latest canonical Brief; null when the Agent has no Brief yet."
                }
            },
            "required": ["agent", "event_window", "latest_brief"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AgentEventWindow".into(),
        json!({
            "type": "object",
            "description": "Committed event window for one Agent inside one snapshot read view. Values come from a committed database view, never from an in-memory watcher or a sequence allocator.",
            "properties": {
                "event_head_seq": { "type": "integer", "minimum": 0, "description": "Greatest committed event_seq visible in the response read view." },
                "oldest_retained_seq": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Durable retention floor. Zero means no retained prefix has ever been deleted; a positive value is the earliest sequence that may still be replayable."
                }
            },
            "required": ["event_head_seq", "oldest_retained_seq"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AgentLatestBrief".into(),
        json!({
            "type": "object",
            "description": "Latest Brief anchor derived from canonical Brief storage, not a second UI-summary table.",
            "properties": {
                "brief_id": { "type": "string" },
                "created_event_seq": {
                    "oneOf": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "null" }
                    ],
                    "description": "Immutable brief_created event linkage; null when the Brief cannot be linked to a unique retained event. Null-linked Briefs do not participate in exact unread calculation."
                },
                "created_at": { "type": "string", "format": "date-time" },
                "preview": { "type": "string", "maxLength": 512, "description": "Bounded to 512 UTF-8 bytes. Full Brief content remains available from the canonical Brief APIs." }
            },
            "required": ["brief_id", "created_event_seq", "created_at", "preview"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AgentProjectionSnapshot".into(),
        json!({
            "type": "object",
            "description": "Per-Agent canonical projection snapshot at one consistency boundary (RFC: observer sync). Served by GET /api/agents/{agent_id}/projection-snapshot only while the agents.projection-snapshot.v1 capability is advertised.",
            "properties": {
                "contract_version": { "type": "integer", "const": 1 },
                "runtime_id": { "type": "string" },
                "event_log_epoch": { "type": "string" },
                "visibility_scope_id": { "type": "string" },
                "agent_id": { "type": "string" },
                "snapshot_through_seq": { "type": "integer", "minimum": 0, "description": "Every event with event_seq <= snapshot_through_seq that affects current canonical state is already reflected in the projection or its revision anchors." },
                "event_head_seq": { "type": "integer", "minimum": 0, "description": "May exceed snapshot_through_seq; names a committed event available through the event page. Clients replay (snapshot_through_seq, event_head_seq] and later events." },
                "oldest_retained_seq": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Durable retention floor; zero means no retained prefix has ever been deleted."
                },
                "projection": { "$ref": "#/components/schemas/AgentCanonicalProjection" }
            },
            "required": ["contract_version", "runtime_id", "event_log_epoch", "visibility_scope_id", "agent_id", "snapshot_through_seq", "event_head_seq", "oldest_retained_seq", "projection"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AgentCanonicalProjection".into(),
        json!({
            "type": "object",
            "description": "First concrete AgentCanonicalProjection: compact current state plus stable revision anchors only. Deliberately excludes verbose timelines and full transcript/message history (see docs/implementation-decisions/104-agent-canonical-projection-v1-boundary.md).",
            "properties": {
                "agent": { "$ref": "#/components/schemas/AgentListEntry" },
                "current_work_item": {
                    "oneOf": [
                        { "$ref": "#/components/schemas/AgentWorkItemAnchor" },
                        { "type": "null" }
                    ],
                    "description": "Current WorkItem anchor; null when the Agent has no current WorkItem."
                },
                "conversation": { "$ref": "#/components/schemas/ConversationRevisionAnchors" },
                "latest_brief": {
                    "oneOf": [
                        { "$ref": "#/components/schemas/AgentLatestBrief" },
                        { "type": "null" }
                    ]
                },
                "hydration_tombstones": {
                    "type": "array",
                    "description": "Records deleted at or before the snapshot boundary; they terminate pending hydration without fetching a record that no longer exists.",
                    "items": { "$ref": "#/components/schemas/AgentHydrationKey" }
                },
                "hydration_references": {
                    "type": "array",
                    "description": "Records referenced by the projection but still resolvable through the batch record APIs.",
                    "items": { "$ref": "#/components/schemas/AgentHydrationKey" }
                }
            },
            "required": ["agent", "current_work_item", "conversation", "latest_brief", "hydration_tombstones", "hydration_references"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AgentWorkItemAnchor".into(),
        json!({
            "type": "object",
            "properties": {
                "work_item_id": { "type": "string" },
                "state": { "type": "string" },
                "plan_status": { "type": "string" },
                "revision": { "type": "integer", "minimum": 0, "description": "Canonical WorkItem revision at the snapshot boundary." },
                "updated_at": { "type": "string", "format": "date-time" }
            },
            "required": ["work_item_id", "state", "plan_status", "revision", "updated_at"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ConversationRevisionAnchors".into(),
        json!({
            "type": "object",
            "description": "Latest-record anchors for conversation families whose canonical records carry no per-record revision counter. Always present; null fields mean the family has no record at the boundary.",
            "properties": {
                "latest_message_id": {
                    "oneOf": [{ "type": "string" }, { "type": "null" }]
                },
                "latest_transcript_entry_id": {
                    "oneOf": [{ "type": "string" }, { "type": "null" }]
                }
            },
            "required": ["latest_message_id", "latest_transcript_entry_id"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AgentHydrationKey".into(),
        json!({
            "type": "object",
            "description": "Identifies one canonical record for hydration termination (tombstone) or batch resolution (reference).",
            "properties": {
                "record_kind": { "type": "string", "enum": ["message", "brief", "transcript_entry"] },
                "record_id": { "type": "string" }
            },
            "required": ["record_kind", "record_id"],
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ProjectionEffect".into(),
        json!({
            "type": "string",
            "enum": ["none", "display_invalidation"],
            "description": "Additive top-level StreamEventEnvelope classification shared by event pages and SSE. The runtime event registry is the source of truth; legacy or otherwise unclassified events default conservatively to display_invalidation. Emitted only while events.projection-effect.v1 is advertised; introducing the field increments the envelope contract version."
        }),
    );
    schemas.insert(
        "CursorNotFoundError".into(),
        json!({
            "type": "object",
            "description": "Rich cursor_not_found body for event pages and SSE. event_log_epoch, oldest_retained_seq, and event_head_seq must come from one committed read view so clients can distinguish a retained-prefix gap from an epoch change and select the correct reset path.",
            "properties": {
                "ok": { "type": "boolean", "const": false },
                "error": { "type": "string" },
                "code": { "type": "string", "const": "cursor_not_found" },
                "after_seq": { "type": "integer", "minimum": 0 },
                "event_log_epoch": { "type": "string" },
                "oldest_retained_seq": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Durable retention floor; zero means no retained prefix has ever been deleted."
                },
                "event_head_seq": { "type": "integer", "minimum": 0 }
            },
            "required": ["ok", "error", "code", "after_seq", "event_log_epoch", "oldest_retained_seq", "event_head_seq"],
            "additionalProperties": true
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
        "RenameAgentRequest",
        "AttachWorkspaceRequest",
        "ExitWorkspaceRequest",
        "DetachWorkspaceRequest",
        "SetAgentModelRequest",
        "SetAgentTimezoneRequest",
        "ClearAgentTimezoneRequest",
        "ClearAgentModelRequest",
        "ControlRequest",
        "AbortCurrentRunRequest",
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
    schemas
}

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursive_component_definitions_lift_to_openapi_components() {
        let api = generate_openapi_json();
        let schemas = api["components"]["schemas"]
            .as_object()
            .expect("components schemas object");
        let tree = &schemas["AgentTreeProjection"];
        assert!(
            tree.get("definitions").is_none(),
            "recursive subschemas must not stay nested under their parent component"
        );
        assert!(
            schemas.contains_key("AgentTreeNode"),
            "AgentTreeNode should be lifted to a top-level component"
        );
        let raw = serde_json::to_string(&schemas).expect("serialize schemas");
        assert!(
            !raw.contains("#/definitions/"),
            "draft-style definitions refs must be rewritten to component refs"
        );
    }

    #[test]
    fn generated_openapi_includes_current_http_surface() {
        let api = generate_openapi_json();
        let paths = api["paths"].as_object().expect("paths object");
        let operation_count = paths
            .values()
            .flat_map(|path| path.as_object().into_iter().flat_map(|ops| ops.keys()))
            .count();
        assert!(operation_count >= 40, "expected baseline coverage");
        let error_response = &api["components"]["schemas"]["ErrorResponse"];
        let required_error_fields = error_response["required"]
            .as_array()
            .expect("ErrorResponse required fields");
        for field in ["ok", "error", "code"] {
            assert!(
                required_error_fields
                    .iter()
                    .any(|required| required == field),
                "ErrorResponse must require {field}"
            );
        }
        assert_eq!(error_response["properties"]["code"]["type"], "string");
        assert!(
            error_response.get("additionalProperties").is_none(),
            "implicit additional properties preserve extension compatibility without generating an invalid Kotlin map subclass"
        );
        assert_eq!(
            error_response["properties"]["correlation"]["$ref"],
            "#/components/schemas/RuntimeErrorContext"
        );
        assert_eq!(
            error_response["properties"]["domain"]["anyOf"][0]["$ref"],
            "#/components/schemas/RuntimeErrorDomain"
        );
        assert!(paths["/api/events/stream"]["get"].is_object());
        assert!(paths["/api/agents/{agent_id}/events/stream"]["get"].is_object());
        let locate = &paths["/api/agents/{agent_id}/conversation/locate"]["post"];
        assert_eq!(locate["operationId"], "agentConversationLocate");
        assert_eq!(
            locate["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ConversationLocateRequest"
        );
        assert_eq!(
            locate["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ConversationLocateResponse"
        );
        for schema in ["ConversationLocateRequest", "ConversationLocateResponse"] {
            assert!(api["components"]["schemas"][schema].is_object(), "{schema}");
        }
        let history_parameters = paths["/api/agents/{agent_id}/conversation"]["get"]["parameters"]
            .as_array()
            .expect("conversation history parameters");
        for name in ["before", "after"] {
            assert!(
                history_parameters
                    .iter()
                    .any(|parameter| parameter["name"] == name && parameter["in"] == "query"),
                "missing history parameter {name}"
            );
        }
        let conversation_stream = &paths["/api/agents/{agent_id}/conversation/stream"]["get"];
        assert!(conversation_stream["requestBody"].is_null());
        let stream_parameters = conversation_stream["parameters"]
            .as_array()
            .expect("conversation stream parameters");
        for (name, location) in [
            ("agent_id", "path"),
            ("after", "query"),
            ("limit", "query"),
            ("activity_limit", "query"),
            ("Last-Event-ID", "header"),
        ] {
            assert!(
                stream_parameters
                    .iter()
                    .any(|parameter| parameter["name"] == name && parameter["in"] == location),
                "missing {location} parameter {name}"
            );
        }
        assert_eq!(
            paths["/api/agents/{agent_id}/events"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/EventsPageResponse"
        );
        let envelope = &api["components"]["schemas"]["StreamEventEnvelope"];
        for field in [
            "event_log_epoch",
            "payload_schema",
            "payload_schema_version",
        ] {
            assert!(envelope["properties"][field].is_object(), "{field}");
        }
        for field in ["contract_version", "provenance"] {
            assert!(
                envelope["properties"].get(field).is_none(),
                "StreamEventEnvelope must not redeclare per-event {field}"
            );
        }
        assert!(
            api["components"]["schemas"]["EventsPageResponse"]["properties"]["contract_version"]
                .is_object()
        );
        assert!(paths["/api/callbacks/wake/{callback_token}"]["post"].is_object());
        assert_eq!(
            paths["/api/agents/{agent_id}/status"]["get"]["x-holon-openapi-source"],
            "aide::axum::ApiRouter::api_route_docs"
        );
        assert!(
            paths["/api/control/agents/{agent_id}/tasks"]["post"]["x-holon-openapi-source"]
                .is_null()
        );
        assert_eq!(
            paths["/api/agents/{agent_id}/state"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/AgentStateSnapshotDto"
        );
        for (path, method, schema) in [
            ("/api/handshake", "get", "HandshakeResponse"),
            ("/api/agents/list", "get", "AgentListResponse"),
            (
                "/api/control/agents/{agent_id}/prompt",
                "post",
                "EnqueueResponse",
            ),
            ("/api/auth/session/me", "get", "CurrentUserResponse"),
        ] {
            assert_eq!(
                paths[path][method]["responses"]["200"]["content"]["application/json"]["schema"]
                    ["$ref"],
                format!("#/components/schemas/{schema}")
            );
        }
        assert_eq!(
            paths["/api/control/agents/{agent_id}/prompt"]["post"]["requestBody"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/ControlPromptRequest"
        );
        let error = &api["components"]["schemas"]["ErrorResponse"];
        for field in [
            "ok",
            "error",
            "code",
            "hint",
            "domain",
            "retryable",
            "context",
            "correlation",
        ] {
            assert!(error["properties"][field].is_object(), "{field}");
        }
    }
}
