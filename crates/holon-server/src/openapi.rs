use aide::{
    axum::{routing::ApiMethodDocs, ApiRouter},
    openapi::{OpenApi, Operation},
};
use schemars::{generate::SchemaSettings, JsonSchema};
use serde_json::{json, Map, Value};

use crate::error::HttpErrorEnvelope;

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
    OpenMetrics,
    Avatar,
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
    RouteSpec {
        response_kind: ResponseKind::Avatar,
        ..route("get", "/agents/{agent_id}/avatar", "agentAvatar", "agents", "Versioned agent avatar", "Read deterministic built-in artwork without loading the Agent. Session and current target access are checked before cached or conditional responses. See RFC agent-avatar-resources.", None, AuthKind::RemoteAccess)
    },
    route("get", "/", "root", "discovery", "Root discovery", "Return the default agent id.", None, AuthKind::RemoteAccess),
    route_with_response("get", "/handshake", "handshake", "discovery", "Protocol handshake", "Return auth mode, protocol version, capabilities, and runtime hints.", None, "HandshakeResponse", AuthKind::RemoteAccess),
    route("get", "/models", "models", "discovery", "List available models", "Return model catalog entries and runtime availability.", None, AuthKind::RemoteAccess),
    route("get", "/models/providers", "modelProviders", "discovery", "List model providers", "Return the resolved Provider projection filtered by availability and media capability." , None, AuthKind::RemoteAccess),
    route("get", "/models/providers/{provider}/models", "providerModels", "discovery", "List models for one provider", "Return the resolved model projection for one provider id from `/models/providers`, filtered by availability and media operation and paged by `model_ref` cursor. Unknown providers return `unknown_provider`; stale cursors return `invalid_cursor`." , None, AuthKind::RemoteAccess),
    route("post", "/models/refresh", "refreshModels", "discovery", "Refresh available models", "Discover models for providers with missing or expired caches, then return the model catalog and runtime availability.", None, AuthKind::RemoteAccess),
    route_with_response("post", "/content-reports", "createContentReport", "content-reports", "Report AI content", "Save an authenticated user's report about user-visible AI content.", Some("CreateContentReportRequest"), "CreateContentReportResponse", AuthKind::RemoteAccess),
    aide_route_with_response("get", "/agents/list", "listAgents", "agents", "List agents", "Return lightweight public agent entries. Optional parent query parameter filters the public roster to direct children of that parent.", None, "AgentListResponse", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/brief-read-states", "briefReadStates", "agents", "Brief read states", "Return the authoritative per-Agent Brief read cursor and exact unread count for the current principal and visibility scope.", None, "BriefReadStates", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/brief-read-state", "briefReadState", "agents", "Brief read state", "Return the authoritative Brief read cursor and exact unread count for one visible public Agent.", None, "BriefReadState", AuthKind::RemoteAccess),
    route_with_response("post", "/agents/{agent_id}/brief-read-cursor", "markBriefRead", "agents", "Mark Briefs read", "Advance one Agent's Brief read cursor monotonically. Requested cursors beyond the committed event head are clamped to that head.", Some("MarkBriefReadRequest"), "MarkBriefReadResult", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/snapshot", "agentsSnapshot", "agents", "Agent roster snapshot", "Authoritative roster snapshot (RFC: observer sync): all-or-nothing membership with per-Agent event windows and latest Brief anchors from one committed read view. Served only while the agents.roster-snapshot.v1 capability is advertised; route registration alone is never sufficient.", None, "AgentRosterSnapshot", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/projection-snapshot", "agentProjectionSnapshot", "agents", "Agent projection snapshot", "Per-Agent canonical projection snapshot (RFC: observer sync): compact current state plus revision anchors at one committed consistency boundary. snapshot_through_seq equals the committed per-Agent event head of the same view; clients replay only event_seq greater than it. Served only while the agents.projection-snapshot.v1 capability is advertised; route registration alone is never sufficient.", None, "AgentProjectionSnapshot", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/conversation", "agentConversation", "agents", "Conversation summary snapshot", "Bounded conversation turn summaries, active turns, pending inputs, coverage boundary, and event head from one committed read transaction. Query parameters: limit and opaque before cursor. Served only while agents.conversation-read.v1 is advertised.", None, "ConversationSummaryResponse", AuthKind::RemoteAccess),
    route_with_response("post", "/agents/{agent_id}/conversation/locate", "agentConversationLocate", "agents", "Locate a canonical conversation source", "Resolve exact message or Brief ownership and a bounded bidirectional history window without mutating read markers. Non-conversation and pending sources are explicit.", Some("ConversationLocateRequest"), "ConversationLocateResponse", AuthKind::RemoteAccess),
    event_stream_route("get", "/agents/{agent_id}/conversation/stream", "agentConversationStream", "agents", "Conversation change stream", "Return bounded, coalesced conversation projection batches over Server-Sent Events. Resume with the opaque after query parameter or Last-Event-ID. Only checkpoint events carry an SSE id; clients persist it only after consuming the complete batch. Retention, epoch, schema, and query-version mismatches require a fresh snapshot. Served only while agents.conversation-read.v1 is advertised.", None, AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/turns/{turn_id}/activities", "agentConversationActivities", "agents", "Conversation turn activity snapshot", "Bounded activity records for one turn, including typed detail coverage, coverage boundary, and event head from one committed read transaction. Query parameters: limit and opaque before cursor. Served only while agents.conversation-read.v1 is advertised.", None, "ConversationActivityResponse", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/turns/{turn_id}/deletion-preview", "agentTurnDeletionPreview", "agents", "Turn deletion impact preview", "Body-free confirmation impact scope for deleting one turn's conversation content: input, Brief, tool-call, and transcript counts, whether the turn is already deleted, and derived-content handling notes. Served only while agents.conversation-turn-deletion.v1 is advertised.", None, "TurnContentDeletionPreview", AuthKind::Control),
    route_with_response("delete", "/agents/{agent_id}/turns/{turn_id}/content", "agentTurnContentDelete", "agents", "Delete turn conversation content", "Delete one turn's conversation content in one transaction: set the first content_deleted_at, remove turn-exclusive evidence, erase embedded body copies in retained tables, and invalidate derived context. Turn identity, timing, ownership, and settlement references survive. Idempotent retries keep the first timestamp. Active, unsettled, shared, or sealed turns are rejected with typed body-free conflicts. See RFC conversation-turn-deletion.", None, "TurnContentDeletion", AuthKind::Control),
    route_with_response("post", "/agents/{agent_id}/messages/{message_id}/withdraw", "agentMessageWithdraw", "agents", "Withdraw unexecuted operator input", "Atomically withdraw one operator-submitted input that execution has not claimed yet; the input never enters model context or triggers execution. Competes with claim and interject admission via compare-and-set; idempotent retries keep the first withdrawal timestamp.", None, "InputWithdrawal", AuthKind::Control),
    route_with_response("get", "/control/agents/{agent_id}/conversation/shadow-diagnostics", "agentConversationShadowDiagnostics", "runtime", "Conversation shadow diagnostics", "Control-authenticated, metadata-only bounded comparison of canonical turn/source metadata and the conversation projection from one deferred read transaction. It never returns Brief bodies, transcript/tool payloads, or a second event ledger. Query parameter: turn_limit.", None, "ConversationShadowDiagnostics", AuthKind::Control),
    aide_route("get", "/agents/{agent_id}", "getAgent", "agents", "Get agent", "Return the compact public agent status projection by default; pass detail=full for the complete AgentSummary read model.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/status", "agentStatus", "agents", "Agent status", "Return the compact public agent status projection by default; pass detail=full for the complete AgentSummary read model.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/model", "agentModel", "agents", "Agent model", "Return the agent model read state (source, runtime default, effective/requested/active routes, fallback chain, overrides, service tier, and resolved policy) exactly as the full summary .model object, without building a full or compact agent summary. Loaded agents are served from in-memory state; unloaded agents read only persisted model material without activating a runtime.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/briefs", "agentBriefs", "agents", "Recent briefs", "Return recent user-facing delivery briefs. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route_with_response("post", "/agents/{agent_id}/briefs:batchGet", "agentBriefsBatchGet", "agents", "Batch get briefs", "Return persisted briefs for the selected agent. Missing or cross-agent ids are reported in missing_brief_ids.", Some("BatchGetBriefsRequest"), "BatchGetBriefsResponse", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/briefs/{brief_id}", "agentBrief", "agents", "Brief detail", "Return a persisted user-facing delivery brief by id.", None, "BriefRecord", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/state", "agentState", "agents", "Agent state snapshot", "Return the lightweight bootstrap snapshot for an agent. Heavy task, work-item, operator notification, and execution details are available through dedicated routes and events.", None, "AgentStateSnapshotDto", AuthKind::RemoteAccess),
    event_stream_route("get", "/events/stream", "eventsStream", "events", "Global refresh hints", "Return SSE agent_roster_hint discovery records and notifications.changed ({\"v\":1}) only when the authenticated control recipient's inbox changes. This live stream carries no cursor, brief text, or replay. Re-read the authoritative snapshots on connect or reconnect.", None, AuthKind::RemoteAccess),
    route_with_response("get", "/notifications/pending", "pendingNotifications", "events", "Pending operator input", "Return the complete actionable wait snapshot for the authenticated control recipient. Opening or reading a target does not resolve the wait. Cache-Control: no-store.", None, "InboxNotificationList", AuthKind::Control),
    route_with_response("get", "/notifications/recent", "recentNotifications", "events", "Recent operator notifications", "Return at most 100 notifications from the last 30 days for the authenticated control recipient, newest first. One entry per brief and recipient. Cache-Control: no-store.", None, "InboxNotificationList", AuthKind::Control),
    route_with_response("get", "/agents/{agent_id}/events", "agentEvents", "events", "Agent event page", "Return a bounded page of versioned runtime event envelopes. Query parameters: before_seq, after_seq, limit, order, event_kind. Display-level filtering is client-side only. Identity is (event_log_epoch, agent_id, event_seq); unknown kinds retain their opaque payload. While events.projection-effect.v1 is advertised every envelope carries the additive projection_effect classification derived from the runtime event registry (envelope contract version 3); an event_kind-filtered page changes event inclusion only and is never proof of raw continuity. The page declares the envelope contract version once as a top-level contract_version field; envelopes carry payload_schema only for typed events and no longer repeat contract_version, payload_schema_version, or provenance.", None, "EventsPageResponse", AuthKind::RemoteAccess),
    event_stream_route("get", "/agents/{agent_id}/events/stream", "agentEventsStream", "events", "Agent event stream", "Return Server-Sent Events carrying raw StreamEventEnvelope JSON data. Query parameters: after_seq, limit. SSE id is event_seq; SSE event is the audit event kind; missing replay cursors return cursor_not_found before the stream opens, carrying event_log_epoch, oldest_retained_seq, and event_head_seq from one committed read view so clients can distinguish a retained-prefix gap from an epoch change. Envelopes carry the additive projection_effect field while events.projection-effect.v1 is advertised. If the receiver lags, the server closes the stream so clients can backfill after the last contiguous SSE id before reconnecting. Breaking change: the projection query parameter, StreamEventEnvelope.projection, StreamEventEnvelope.contract_version, and StreamEventEnvelope.provenance have been removed; the envelope contract version is declared once by the x-holon-event-contract-version response header, and payload_schema is present only on typed events.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/messages/{message_id}", "agentMessage", "messages", "Message detail", "Return a persisted message envelope by id for the selected agent.", None, AuthKind::RemoteAccess),
    route_with_response("post", "/agents/{agent_id}/messages:batchGet", "agentMessagesBatchGet", "messages", "Batch get messages", "Return persisted message envelopes for the selected agent. Missing or cross-agent ids are reported in missing_message_ids.", Some("BatchGetMessagesRequest"), "BatchGetMessagesResponse", AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/transcript", "agentTranscript", "agents", "Recent transcript", "Return recent transcript entries. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/transcript/{entry_id}", "agentTranscriptEntry", "agents", "Transcript entry detail", "Return a persisted transcript entry by id for the selected agent.", None, AuthKind::RemoteAccess),
    route_with_response("post", "/agents/{agent_id}/transcript:batchGet", "agentTranscriptBatchGet", "agents", "Batch get transcript entries", "Return persisted transcript entries for the selected agent. Missing or cross-agent ids are reported in missing_entry_ids.", Some("BatchGetTranscriptEntriesRequest"), "BatchGetTranscriptEntriesResponse", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tasks", "agentTasks", "tasks", "List tasks", "Return active task records by default, or bounded terminal task records. Query parameters: status (active or terminal), limit (terminal defaults to and is capped at 20).", None, "TaskListResponse", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tasks/{task_id}", "agentTaskStatus", "tasks", "Task status", "Return a task lifecycle snapshot by id.", None, "TaskStatusSnapshot", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tasks/{task_id}/output", "agentTaskOutput", "tasks", "Task output", "Return a task output snapshot. Query parameters: block, timeout_ms.", None, "TaskOutputResult", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tool-executions/{tool_execution_id}", "agentToolExecution", "tools", "Tool execution detail", "Return a persisted tool execution record by id.", None, "ToolExecutionRecord", AuthKind::RemoteAccess),
    route_with_response("get", "/agents/{agent_id}/tool-executions/{tool_execution_id}/artifacts/{artifact_index}", "agentToolExecutionArtifact", "tools", "Tool execution artifact", "Return UTF-8 content for an artifact referenced by the selected tool execution. Artifact paths are resolved server-side and confined to the agent runtime data directory.", None, "ToolExecutionArtifactContent", AuthKind::RemoteAccess),
    route_with_response("post", "/control/agents/{agent_id}/tasks/{task_id}/input", "taskInput", "control", "Task input", "Deliver text input to a managed task.", Some("TaskInputRequest"), "TaskInputResult", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/tasks/{task_id}/stop", "taskStop", "control", "Task stop", "Request cancellation for a managed task.", Some("TaskStopRequest"), "TaskStopResult", AuthKind::Control),
    route("get", "/agents/{agent_id}/work-items", "agentWorkItems", "work-items", "List work items", "Return work item records for the agent. Select all, active (including completing), or terminal history before applying limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/work-items/{work_item_id}", "agentWorkItem", "work-items", "Work item detail", "Return a work item record by id.", None, AuthKind::RemoteAccess),
    aide_route("get", "/agents/{agent_id}/worktree-summary", "agentWorktreeSummary", "agents", "Worktree summary", "Return managed worktree summary for an agent.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/timers", "agentTimers", "timers", "List timers", "Return timer records. Select all, active, or terminal (completed/cancelled) history before applying limit.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/timers/{timer_id}", "agentTimer", "timers", "Timer detail", "Return a timer record by id.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/skills", "agentSkills", "skills", "List agent skills", "Return skills enabled/effective for an agent.", None, AuthKind::RemoteAccess),
    route("get", "/agents/{agent_id}/skills/{skill_id}", "agentSkillDetail", "skills", "Agent skill detail", "Return metadata and SKILL.md content for a skill in the target agent's effective catalog. The agent selects the active execution-root version.", None, AuthKind::RemoteAccess),
    route("get", "/skills/catalog", "skillsCatalog", "skills", "Skills catalog", "Return the global user Skill Library catalog. Query parameter: scope.", None, AuthKind::RemoteAccess),
    route("get", "/skills/catalog/{skill_id}", "skillDetail", "skills", "Skill detail", "Return catalog metadata and SKILL.md content for a Global Skill Library skill.", None, AuthKind::RemoteAccess),
    route("get", "/workspaces/{workspace_id}/files", "workspaceFilesRoot", "workspaces", "Browse workspace root", "List directory entries at the workspace root. Query parameters: execution_root_id.", None, AuthKind::RemoteAccess),
    route("get", "/workspaces/{workspace_id}/files/{path}", "workspaceFiles", "workspaces", "Browse workspace files", "List a directory or read a file by path. Supports content negotiation: Accept: application/json returns structured metadata + content, other Accept values return raw body. Query parameters: execution_root_id, download, meta.", None, AuthKind::RemoteAccess),
    route_with_response("get", "/desktop/capabilities", "desktopCapabilities", "workspaces", "Desktop capabilities", "Explicit desktop integration available on the daemon host for authorized callers. Independent of the caller's network location; does not prove the browser and runtime share a filesystem.", None, "DesktopCapabilities", AuthKind::Control),
    route("post", "/desktop/reveal", "desktopReveal", "workspaces", "Reveal a file in Finder", "Requires explicit desktop integration, control authorization, and matching Origin. Local mode without required credentials also requires a loopback Host. Resolves the registered file locator before invoking Finder on the daemon host; authenticated remote callers are allowed.", Some("RevealFileRequest"), AuthKind::Control),
    route_with_response("post", "/file-references/resolve", "resolveFileReferences", "workspaces", "Resolve file references", "Resolve absolute paths, historical workspace URIs, or relative paths with an explicit base file into registered workspace and execution-root locations.", Some("ResolveFileReferencesRequest"), "ResolveFileReferencesResponse", AuthKind::RemoteAccess),
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
    route_with_response("patch", "/control/agents/{agent_id}/timers/{timer_id}", "editTimerContent", "control", "Edit timer content", "Update only the display title or trigger instruction. Omitted fields stay unchanged; null clears the value. The schedule and already queued wakes are not changed.", Some("TimerContentPatch"), "TimerRecord", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/timers/{timer_id}/cancel", "cancelTimer", "control", "Cancel timer", "Cancel an active or paused timer. Cancellation is idempotent for already-cancelled timers; completed or missing timers return a shared error envelope.", Some("CancelTimerRequest"), "TimerRecord", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/timers/{timer_id}/pause", "pauseTimer", "control", "Pause timer", "Pause future triggers without retracting an already committed timer wake. Repeated pauses preserve the frozen delay; terminal timers return a conflict.", Some("CancelTimerRequest"), "TimerRecord", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/timers/{timer_id}/resume", "resumeTimer", "control", "Resume timer", "Resume from the frozen delay or the next calendar match, without making up missed triggers. Repeated resumes do not reschedule; terminal timers return a conflict.", Some("CancelTimerRequest"), "TimerRecord", AuthKind::Control),
    route("post", "/control/agents/{agent_id}/create", "createAgent", "control", "Create named agent", "Create a public named agent, optionally from a template.", Some("CreateAgentRequest"), AuthKind::Control),
    route_with_response("get", "/control/agents/tree", "agentTree", "control", "Agent tree", "Return the authenticated operator Agent tree built from canonical lineage.", None, "AgentTreeProjection", AuthKind::Control),
    route_with_response("get", "/control/agents/{agent_id}/detail", "agentDetail", "control", "Agent detail", "Return the canonical Agent identity, lifecycle, profile, and deletion detail.", None, "AgentDetail", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/repair", "repairAgent", "control", "Repair agent bootstrap", "Retry incomplete post-create bootstrap steps without recreating the agent.", None, "AgentDetail", AuthKind::Control),
    route_with_response("patch", "/control/agents/{agent_id}/name", "renameAgent", "control", "Rename agent", "Update the public display name without changing the technical agent identity or lifecycle target.", Some("RenameAgentRequest"), "AgentDetail", AuthKind::Control),
    route_with_response("delete", "/control/agents/{agent_id}", "deleteAgent", "control", "Delete agent", "Fence a public self-owned agent and create or return its durable deletion job.", Some("DeleteAgentRequest"), "AgentDeletionResponse", AuthKind::Control),
    route_with_response("get", "/control/agents/{agent_id}/delete-status", "agentDeleteStatus", "control", "Agent deletion status", "Return the identity tombstone and current or completed deletion job.", None, "AgentDeletionStatusResponse", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/deletion/resolve", "resolveAgentDeletion", "control", "Resolve stopped agent deletion", "Explicit operator resolution for a stopped deletion job. Currently supports abandoning the deletion, which returns the identity to active and reports the cleanup scope that remains. Agents have no self-service path.", Some("ResolveAgentDeletionRequest"), "AgentDeletionResolveResponse", AuthKind::Control),
    route("post", "/control/agents/{agent_id}/reset-callback", "resetCallback", "control", "Reset external trigger callback", "Revoke the current external trigger and provision a fresh one with a new token.", None, AuthKind::Control),
    route("get", "/control/agents/{agent_id}/external-triggers", "listExternalTriggers", "control", "List external triggers", "Read-only diagnostic list of retained active and revoked external triggers for the target agent. Cursor pagination over (created_at desc, external_trigger_id desc) with an optional exact 16-hex token fingerprint filter. Returns metadata and fingerprints only; tokens, full hashes, and callback URLs are never included.", None, AuthKind::Control),
    route("post", "/control/agents/{agent_id}/external-triggers/{external_trigger_id}/reveal", "revealExternalTrigger", "control", "Reveal external trigger token", "Read the stored plain-text callback token for one active external trigger. Requires the independent static control token credential; trusted-local admission, browser sessions, and bearer session credentials are rejected. Responses use Cache-Control: no-store. Answers 404 when the target or trigger is absent, 409 trigger_not_active when revoked, and 409 token_unavailable when no stored token can be safely verified against its hash.", None, AuthKind::Control),
    route("post", "/control/agents/{agent_id}/workspace/attach", "attachWorkspace", "control", "Attach workspace", "Attach a workspace path to an agent.", Some("AttachWorkspaceRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/workspace/exit", "exitWorkspace", "control", "Exit workspace", "Return an agent to its default AgentHome workspace.", Some("ExitWorkspaceRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/workspace/detach", "detachWorkspace", "control", "Detach workspace", "Detach a workspace binding by workspace id.", Some("DetachWorkspaceRequest"), AuthKind::Control),
    route("get", "/control/agents/{agent_id}/timezone", "getAgentTimezone", "control", "Get agent timezone", "Return the effective agent timezone.", None, AuthKind::Control),
    route("post", "/control/agents/{agent_id}/timezone", "setAgentTimezone", "control", "Set agent timezone", "Set an IANA timezone override.", Some("SetAgentTimezoneRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/timezone/clear", "clearAgentTimezone", "control", "Clear agent timezone", "Restore runtime timezone fallback.", Some("ClearAgentTimezoneRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/model", "setAgentModel", "control", "Set agent model override", "Set an agent model override and optional reasoning effort.", Some("SetAgentModelRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/model/clear", "clearAgentModel", "control", "Clear agent model override", "Clear an agent model override.", Some("ClearAgentModelRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/control", "controlAgent", "control", "Control agent lifecycle", "Submit a lifecycle control action.", Some("ControlRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/current-run/abort", "abortCurrentRun", "control", "Abort current run", "Request abort for the current agent run.", Some("AbortCurrentRunRequest"), AuthKind::Control),
    route("get", "/control/agents/{agent_id}/acp-interactions", "listAcpInteractions", "control", "List pending ACP interactions", "List pending ACP permission and elicitation requests for a loaded agent.", None, AuthKind::Control),
    route("post", "/control/agents/{agent_id}/acp-interactions/{interaction_id}", "resolveAcpInteraction", "control", "Resolve ACP interaction", "Answer one pending ACP permission or elicitation request with an operator decision or form content.", None, AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/prompt", "controlPrompt", "control", "Submit operator prompt", "Submit a trusted operator prompt through the control plane.", Some("ControlPromptRequest"), "EnqueueResponse", AuthKind::Control),
    route("post", "/control/agents/{agent_id}/operator-bindings", "createOperatorTransportBinding", "control", "Create operator binding", "Create or update a remote operator transport binding.", Some("OperatorTransportBindingRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/operator-ingress", "operatorIngress", "control", "Operator ingress", "Deliver an authenticated remote operator prompt.", Some("OperatorIngressRequest"), AuthKind::Control),
    route("get", "/control/runtime/readiness", "runtimeReadiness", "runtime", "Runtime readiness", "Return daemon readiness metadata.", None, AuthKind::Control),
    route("get", "/control/runtime/status", "runtimeStatus", "runtime", "Runtime status", "Return daemon status and runtime activity metadata.", None, AuthKind::Control),
    route_with_response("get", "/control/runtime/performance", "runtimePerformance", "runtime", "Runtime performance diagnostics", "Return bounded in-process performance diagnostics for HTTP, projections, DB, reader pools, and scheduler activity.", None, "PerformanceDiagnosticsSnapshot", AuthKind::Control),
    openmetrics_route("get", "/control/runtime/metrics", "runtimeMetrics", "runtime", "Runtime OpenMetrics", "Return a bounded, low-cardinality OpenMetrics snapshot for runtime performance and diagnostics; reader pools use only fixed labels.", AuthKind::Control),
    route_with_response("get", "/control/runtime/traces", "runtimeTraces", "runtime", "Recent runtime traces", "Return summaries for the bounded in-memory recent trace ring.", None, "RecentTraceSummaryList", AuthKind::Control),
    route_with_response("get", "/control/runtime/traces/search", "runtimeTraceSearch", "runtime", "Search runtime traces", "Search the bounded in-memory recent trace ring and retained persistent diagnostics by trace or span attributes.", None, "RecentTraceSummaryList", AuthKind::Control),
    route_with_response("get", "/control/runtime/traces/{trace_id}", "runtimeTrace", "runtime", "Runtime trace waterfall", "Return the recorded span waterfall for a recent or retained persistent trace.", None, "RecentTrace", AuthKind::Control),
    route_with_response("get", "/control/runtime/config", "runtimeConfig", "runtime", "Runtime config", "Return the daemon effective runtime configuration surface.", None, "RuntimeConfigReadResponse", AuthKind::Control),
    route_with_response("patch", "/control/runtime/config", "runtimeConfigUpdate", "runtime", "Update runtime config", "Persist runtime-mutable config updates and classify their effect as restart/reload-required or rejected.", Some("RuntimeConfigUpdateRequest"), "RuntimeConfigUpdateResponse", AuthKind::Control),
    route_with_response("get", "/control/network/tailscale/serve", "tailscaleServeStatus", "network", "Tailscale Serve status", "Return the desired and actual Tailscale Serve state, including conflicts.", None, "TailscaleServeStatus", AuthKind::Control),
    route_with_response("post", "/control/network/tailscale/serve/enable", "enableTailscaleServe", "network", "Enable Tailscale Serve", "Enable the Holon root Serve rule without replacing a conflicting rule, then persist the desired state.", None, "TailscaleServeStatus", AuthKind::Control),
    route_with_response("post", "/control/network/tailscale/serve/disable", "disableTailscaleServe", "network", "Disable Tailscale Serve", "Disable the Holon root Serve rule without removing another rule, then persist the desired state.", None, "TailscaleServeStatus", AuthKind::Control),
    route_with_response("post", "/control/runtime/decision/test", "testDecision", "runtime", "Test Decision provider", "Run a fixed, side-effect-free Decision provider smoke test without returning credentials or mutating configuration.", Some("RuntimeDecisionTestRequest"), "RuntimeDecisionTestResponse", AuthKind::Control),
    route_with_response("post", "/control/runtime/config/migrate-model-routes", "migrateModelConfigRoutes", "runtime", "Migrate model config routes", "Inspect legacy model route references or persist a complete canonical migration across config.json and agent state.", Some("ModelConfigMigrationRequest"), "ModelConfigMigrationReport", AuthKind::Control),
    route("get", "/control/runtime/decision/local-onnx/preset/{preset}", "runtimeDecisionLocalOnnxPreset", "runtime", "Local ONNX preset status", "Return the managed-cache status for a local ONNX decision preset.", None, AuthKind::Control),
    route("post", "/control/runtime/decision/local-onnx/preset/{preset}", "runtimeDecisionLocalOnnxPresetDownload", "runtime", "Download local ONNX preset", "Explicitly download and verify a local ONNX decision preset into the managed cache.", None, AuthKind::Control),
    route("delete", "/control/runtime/decision/local-onnx/preset/{preset}", "runtimeDecisionLocalOnnxPresetCancel", "runtime", "Cancel local ONNX preset download", "Remove an incomplete managed-cache download without changing Decision configuration.", None, AuthKind::Control),
    route("get", "/control/runtime/credentials", "runtimeCredentials", "runtime", "Runtime credential profiles", "List credential profiles stored in the runtime credential store.", None, AuthKind::Control),
    route("put", "/control/runtime/credentials/{profile}", "setRuntimeCredential", "runtime", "Set runtime credential", "Set an API key credential profile in the runtime credential store.", Some("SetCredentialRequest"), AuthKind::Control),
    route("delete", "/control/runtime/credentials/{profile}", "deleteRuntimeCredential", "runtime", "Delete runtime credential", "Remove a credential profile from the runtime credential store.", None, AuthKind::Control),
    route("post", "/auth/codex/device/start", "startCodexDeviceLogin", "auth", "Start Codex device login", "Request an OpenAI Codex device code and start a background job that persists the OAuth credential profile after user authorization.", None, AuthKind::Control),
    route("post", "/auth/{provider}/device/start", "startOAuthDeviceLogin", "auth", "Start OAuth device login", "Request a provider OAuth device code and start a background job that persists the OAuth credential profile after user authorization. Supported providers include openai-codex, xai, and github-copilot (which uses a bundled public OAuth app by default, with optional client id overrides).", None, AuthKind::Control),
    route("get", "/auth/method", "authMethod", "auth", "Authentication method", "Return the configured authentication mode used by the Web login page.", None, AuthKind::None),
    route_with_response("post", "/auth/session/exchange", "sessionExchange", "auth", "Exchange session credential", "Exchange a static or bootstrap credential for a revocable session credential and browser session cookie.", Some("SessionExchangeRequest"), "SessionResponse", AuthKind::None),
    route_with_response("post", "/auth/session/exchange/native", "sessionExchangeNative", "auth", "Exchange native session credential", "Exchange a static or bootstrap credential for a revocable session credential and native session response.", Some("SessionExchangeRequest"), "NativeSessionResponse", AuthKind::None),
    route_with_response("post", "/auth/pairing/issue", "pairingIssue", "auth", "Issue pairing ticket", "Issue a short-lived, single-use local pairing ticket. Requires an explicit control token or session.", None, "PairingIssueResponse", AuthKind::Control),
    route_with_response("post", "/auth/pairing/redeem", "pairingRedeem", "auth", "Redeem pairing ticket", "Exchange a one-time pairing ticket for a browser session cookie.", Some("PairingRedeemRequest"), "SessionResponse", AuthKind::None),
    route_with_response("post", "/auth/pairing/redeem/native", "pairingRedeemNative", "auth", "Redeem native pairing ticket", "Exchange a one-time pairing ticket for a native session credential.", Some("PairingRedeemRequest"), "NativeSessionResponse", AuthKind::None),
    route_with_response("get", "/auth/session/me", "sessionMe", "auth", "Current session user", "Return the identity behind the current session: the authenticated OIDC user, or the stable local control identity for static-token deployments.", None, "CurrentUserResponse", AuthKind::None),
    route("post", "/control/runtime/shutdown", "runtimeShutdown", "runtime", "Runtime shutdown", "Request graceful runtime shutdown.", None, AuthKind::Control),
    route("post", "/control/agents/{agent_id}/debug-prompt", "debugPrompt", "control", "Debug prompt", "Render a diagnostic prompt preview.", Some("DebugPromptRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/wake", "controlWake", "control", "Wake agent", "Submit a trusted wake hint.", Some("ControlWakeRequest"), AuthKind::Control),
    route_with_response("get", "/control/agents/{agent_id}/scheduler-repair", "inspectSchedulerRepair", "control", "Inspect scheduler repair state", "Return active waits, queued wake-only messages, current source-revision drift, and retained-source repair-block diagnostics. Inspection does not authorize replay.", None, "SchedulerRepairInspection", AuthKind::Control),
    route_with_response("post", "/control/agents/{agent_id}/scheduler-repair", "applySchedulerRepair", "control", "Apply scheduler repair", "Dry-run or apply one OCC-guarded scheduler repair operation. Non-dry-run requests create and verify a SQLite backup before committing and emit an audit event.", Some("SchedulerRepairRequest"), "SchedulerRepairResult", AuthKind::Control),
    route("post", "/callbacks/enqueue/{callback_token}", "callbackEnqueue", "callbacks", "Callback enqueue ingress", "Capability-token callback ingress for enqueue delivery. The token is a secret path segment and examples intentionally use a placeholder.", Some("CallbackBody"), AuthKind::Capability),
    route("post", "/callbacks/wake/{callback_token}", "callbackWake", "callbacks", "Callback wake ingress", "Capability-token callback ingress for wake delivery. The token is a secret path segment and examples intentionally use a placeholder.", Some("CallbackBody"), AuthKind::Capability),
    route("post", "/control/agents/{agent_id}/skills/enable", "enableSkill", "skills", "Enable agent skill", "Enable a locally known skill for an agent.", Some("EnableSkillRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/skills/disable", "disableSkill", "skills", "Disable agent skill", "Disable a skill for an agent.", Some("DisableSkillRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/skills/install", "installSkill", "skills", "Install skill compatibility alias", "Compatibility alias for older agent skill install behavior.", Some("InstallSkillRequest"), AuthKind::Control),
    route("post", "/control/agents/{agent_id}/skills/uninstall", "uninstallSkill", "skills", "Uninstall skill compatibility alias", "Compatibility alias for disabling an agent skill.", Some("UninstallSkillRequest"), AuthKind::Control),
    aide_route("get", "/status", "defaultStatus", "compat", "Default agent status alias", "Compatibility alias for the default agent status route; pass detail=full for the complete AgentSummary read model.", None, AuthKind::RemoteAccess),
    aide_route("get", "/briefs", "defaultBriefs", "compat", "Default agent briefs alias", "Compatibility alias for the default agent briefs route. Query parameter: limit.", None, AuthKind::RemoteAccess),
    route_with_response("get", "/state", "defaultState", "compat", "Default agent state alias", "Compatibility alias for the default agent state route.", None, "AgentStateSnapshotDto", AuthKind::RemoteAccess),
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

const fn aide_route_with_response(
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

const fn openmetrics_route(
    method: &'static str,
    path: &'static str,
    operation_id: &'static str,
    tag: &'static str,
    summary: &'static str,
    description: &'static str,
    auth: AuthKind,
) -> RouteSpec {
    RouteSpec {
        method,
        path,
        operation_id,
        tag,
        summary,
        description,
        request_schema: None,
        response_schema: None,
        response_kind: ResponseKind::OpenMetrics,
        auth,
        metadata_source: MetadataSource::Manual,
    }
}

#[derive(Clone, Copy)]
pub struct ConversationShadowLimits {
    pub default: usize,
    pub maximum: usize,
}

pub fn generate(
    api_version: &str,
    mut schemas: Map<String, Value>,
    shadow_limits: ConversationShadowLimits,
) -> Value {
    lift_recursive_definitions(&mut schemas);
    let mut paths = serde_json::Map::new();
    for spec in ROUTES
        .iter()
        .filter(|spec| spec.metadata_source == MetadataSource::Manual)
    {
        let entry = paths
            .entry(format!("/api{}", spec.path))
            .or_insert_with(|| json!({}));
        entry[spec.method] = operation(spec, shadow_limits);
    }
    merge_path_items(&mut paths, aide_route_paths(shadow_limits));

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Holon HTTP control-plane API",
            "version": api_version,
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
            { "name": "content-reports" },
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
            "schemas": schemas
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

fn aide_route_paths(shadow_limits: ConversationShadowLimits) -> serde_json::Map<String, Value> {
    let mut router: ApiRouter<()> = ApiRouter::new();
    for spec in ROUTES
        .iter()
        .filter(|spec| spec.metadata_source == MetadataSource::Aide)
    {
        router = router.api_route_docs(
            &format!("/api{}", spec.path),
            ApiMethodDocs::new(spec.method, aide_operation(spec, shadow_limits)),
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

fn aide_operation(spec: &RouteSpec, shadow_limits: ConversationShadowLimits) -> Operation {
    let mut operation: Operation = serde_json::from_value(operation(spec, shadow_limits))
        .expect("convert baseline operation to aide");
    operation.extensions.insert(
        "x-holon-openapi-source".into(),
        json!("aide::axum::ApiRouter::api_route_docs"),
    );
    operation
}

fn operation(spec: &RouteSpec, shadow_limits: ConversationShadowLimits) -> Value {
    let mut parameters = path_parameters(spec.path);
    if matches!(spec.operation_id, "agentWorkItems" | "agentTimers") {
        parameters.extend([
            json!({
                "name": "status",
                "in": "query",
                "required": false,
                "description": "Select all records, nonterminal active records, or terminal history before limiting.",
                "schema": { "type": "string", "enum": ["all", "active", "terminal"], "default": "all" }
            }),
            json!({
                "name": "limit",
                "in": "query",
                "required": false,
                "description": "Maximum number of selected records; defaults to 50.",
                "schema": { "type": "integer", "minimum": 0, "default": 50 }
            }),
        ]);
    }
    if spec.operation_id == "agentTasks" {
        parameters.extend([
            json!({
                "name": "status",
                "in": "query",
                "required": false,
                "description": "Select active tasks or terminal history.",
                "schema": { "type": "string", "enum": ["active", "terminal"], "default": "active" }
            }),
            json!({
                "name": "limit",
                "in": "query",
                "required": false,
                "description": "Active tasks default to 50; terminal tasks default to and are capped at 20.",
                "schema": { "type": "integer", "minimum": 0 }
            }),
        ]);
    }
    if spec.operation_id == "listAgents" {
        parameters.push(json!({
            "name": "parent",
            "in": "query",
            "required": false,
            "description": "Exact direct parent agent id; filters only the lightweight public roster.",
            "schema": { "type": "string" }
        }));
    }
    if matches!(
        spec.operation_id,
        "getAgent" | "agentStatus" | "defaultStatus"
    ) {
        parameters.push(json!({
            "name": "detail",
            "in": "query",
            "required": false,
            "description": "Response detail level. Compact is the default; full returns the complete AgentSummary read model.",
            "schema": {
                "type": "string",
                "enum": ["compact", "full"],
                "default": "compact"
            }
        }));
    }
    if spec.operation_id == "runtimeTraceSearch" {
        parameters.push(json!({
            "name": "query",
            "in": "query",
            "required": true,
            "description": "Case-insensitive trace or span attribute search text.",
            "schema": { "type": "string", "minLength": 1 }
        }));
    }
    if matches!(
        spec.operation_id,
        "agentConversation" | "agentConversationActivities"
    ) {
        let (default, maximum) = if spec.operation_id == "agentConversation" {
            (30, 100)
        } else {
            (50, 200)
        };
        parameters.push(json!({
            "name": "limit",
            "in": "query",
            "required": false,
            "description": "Bounded page size.",
            "schema": {
                "type": "integer",
                "minimum": 1,
                "maximum": maximum,
                "default": default
            }
        }));
        parameters.push(json!({
            "name": "before",
            "in": "query",
            "required": false,
            "description": "Opaque pagination cursor returned by the preceding page.",
            "schema": { "type": "string", "minLength": 1 }
        }));
    }
    if spec.operation_id == "agentConversation" {
        parameters.push(json!({
            "name": "after",
            "in": "query",
            "required": false,
            "description": "Opaque forward history cursor. Mutually exclusive with before; not a stream checkpoint.",
            "schema": { "type": "string", "minLength": 1 }
        }));
    }
    if spec.operation_id == "agentConversationStream" {
        parameters.extend([
            json!({
                "name": "after",
                "in": "query",
                "required": false,
                "description": "Opaque conversation checkpoint. Last-Event-ID takes precedence when both are supplied.",
                "schema": { "type": "string", "minLength": 1 }
            }),
            json!({
                "name": "limit",
                "in": "query",
                "required": false,
                "description": "Maximum canonical ledger events recovered in one batch.",
                "schema": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 256,
                    "default": 256
                }
            }),
            json!({
                "name": "activity_limit",
                "in": "query",
                "required": false,
                "description": "Maximum active-turn activity items recovered across one batch.",
                "schema": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 64,
                    "default": 64
                }
            }),
            json!({
                "name": "Last-Event-ID",
                "in": "header",
                "required": false,
                "description": "Opaque conversation checkpoint from the last fully consumed checkpoint event.",
                "schema": { "type": "string", "minLength": 1 }
            }),
        ]);
    }
    if spec.operation_id == "agentConversationShadowDiagnostics" {
        parameters.push(json!({
            "name": "turn_limit",
            "in": "query",
            "required": false,
            "description": "Maximum recent canonical turns compared. The server applies the bounded maximum.",
            "schema": {
                "type": "integer",
                "minimum": 1,
                "maximum": shadow_limits.maximum,
                "default": shadow_limits.default
            }
        }));
    }
    if spec.operation_id == "agentTurnContentDelete" {
        parameters.push(json!({
            "name": "expected_revision",
            "in": "query",
            "required": false,
            "description": "Optional turn summary revision captured at confirmation time. Deletion re-validates it and fails closed on drift, so content added after confirmation is never deleted silently.",
            "schema": { "type": "integer", "minimum": 0 }
        }));
    }
    if spec.operation_id == "agentAvatar" {
        parameters.extend([
            json!({"name":"version","in":"query","schema":{"type":"string","enum":["light-mech-1-vivid-2"],"default":"light-mech-1-vivid-2"},"description":"Omission permanently selects this exact version, not latest. Unknown or repeated parameters return 400."}),
            json!({"name":"expression","in":"query","schema":{"type":"string","enum":["neutral","focused","questioning","waiting","sleeping"],"default":"neutral"},"description":"Explicit artwork only; the server does not read runtime status."}),
            json!({"name":"format","in":"query","schema":{"type":"string","enum":["png","svg"],"default":"png"}}),
            json!({"name":"size","in":"query","schema":{"type":"integer","enum":[32,64,128,256,512],"default":128},"description":"PNG pixel dimensions. SVG uses viewBox 0 0 100 100 and rejects size."}),
            json!({"name":"If-None-Match","in":"header","schema":{"type":"string"},"description":"Compare the actual image byte ETag, after authenticating and checking the target."}),
        ]);
    }
    if spec.operation_id == "modelProviders" {
        parameters.extend(model_operation_filter_parameters());
    }
    if spec.operation_id == "providerModels" {
        parameters.extend(model_operation_filter_parameters());
        parameters.extend([
            json!({
                "name": "limit",
                "in": "query",
                "required": false,
                "description": "Maximum models returned in one page. Values outside the range are clamped.",
                "schema": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 500,
                    "default": 100
                }
            }),
            json!({
                "name": "cursor",
                "in": "query",
                "required": false,
                "description": "Opaque `model_ref` cursor returned by the preceding page. Pages concatenate back into the full filtered sequence; an unknown cursor returns `invalid_cursor` instead of restarting.",
                "schema": { "type": "string", "minLength": 1 }
            }),
        ]);
    }
    let mut op = json!({
        "operationId": spec.operation_id,
        "tags": [spec.tag],
        "summary": spec.summary,
        "description": spec.description,
        "parameters": parameters,
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

/// Query parameters shared by the provider and provider-model listing routes.
///
/// `include_unavailable` is a diagnostic view: when set it skips both the
/// availability and `operation` filters, matching the Agent tool contract.
fn model_operation_filter_parameters() -> Vec<Value> {
    vec![
        json!({
            "name": "operation",
            "in": "query",
            "required": false,
            "description": "Restrict to entries with a model that is call-selectable for this media operation. Ignored when `include_unavailable=true`.",
            "schema": { "type": "string", "enum": ["view_image", "generate_image"] }
        }),
        json!({
            "name": "include_unavailable",
            "in": "query",
            "required": false,
            "description": "Diagnostic view. When true, both the availability and `operation` filters are skipped so every resolved entry is returned.",
            "schema": { "type": "boolean", "default": false }
        }),
    ]
}

fn path_parameters(path: &str) -> Vec<Value> {
    let mut params = Vec::new();
    if path.contains("{agent_id}") {
        params.push(path_param("agent_id", "Agent id."));
    }
    if path.contains("{provider}") {
        params.push(path_param(
            "provider",
            "Provider id taken verbatim from `GET /api/models/providers`.",
        ));
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
    if path.contains("{turn_id}") {
        params.push(path_param("turn_id", "Native turn id."));
    }
    if path.contains("{trace_id}") {
        params.push(path_param("trace_id", "Trace id."));
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
        ResponseKind::Avatar => json!({
            "200": {
                "description": "Built-in SVG or PNG, at most 2 MiB. No user strings or external resources.",
                "headers": {
                    "ETag": {"schema":{"type":"string"},"description":"Strong SHA-256 ETag of actual bytes."},
                    "Cache-Control": {"schema":{"type":"string","const":"private, no-cache"}},
                    "X-Content-Type-Options": {"schema":{"type":"string","const":"nosniff"}}
                },
                "content": {
                    "image/png": {"schema":{"type":"string","format":"binary"}},
                    "image/svg+xml": {"schema":{"type":"string","format":"binary"}}
                }
            },
            "304": {
                "description":"Not modified. No response body; authorization and target access were rechecked.",
                "headers": {
                    "ETag":{"schema":{"type":"string"}},
                    "Cache-Control":{"schema":{"type":"string","const":"private, no-cache"}}
                }
            },
            "400": {
                "description":"Unknown, repeated, or invalid semantic parameter, or invalid Agent ID.",
                "content":{"application/json":{"schema":{"$ref":"#/components/schemas/ErrorResponse"}}}
            },
            "4XX": {
                "description":"Existing session and Agent access errors; never image content.",
                "content":{"application/json":{"schema":{"$ref":"#/components/schemas/ErrorResponse"}}}
            },
            "503": {
                "description":"Bounded image generation capacity exhausted. Retry later.",
                "headers":{"Retry-After":{"schema":{"type":"string","const":"1"}}},
                "content":{"application/json":{"schema":{"$ref":"#/components/schemas/ErrorResponse"}}}
            },
            "5XX": {
                "description":"Server error JSON response.",
                "content":{"application/json":{"schema":{"$ref":"#/components/schemas/ErrorResponse"}}}
            }
        }),
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
        ResponseKind::OpenMetrics => json!({
            "200": {
                "description": "OpenMetrics 1.0 text exposition.",
                "content": {
                    "application/openmetrics-text; version=1.0.0; charset=utf-8": {
                        "schema": { "type": "string" }
                    }
                }
            },
            "4XX": {
                "description": "Client error JSON response.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } }
            },
            "5XX": {
                "description": "Server error JSON response.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } }
            }
        }),
    }
}

/// schemars emits subschemas it cannot inline (recursive types such as
/// `AgentTreeProjection`) under a root-level `definitions` object with
/// draft-style `#/definitions/...` refs. OpenAPI resolves refs from the
/// document root, where those paths do not exist, so lift each definition into
/// a top-level component schema and rewrite refs to component paths.
fn lift_recursive_definitions(schemas: &mut serde_json::Map<String, Value>) {
    let mut lifted: Vec<(String, Value)> = Vec::new();
    for schema in schemas.values_mut() {
        let Some(object) = schema.as_object_mut() else {
            continue;
        };
        let Some(Value::Object(definitions)) = object.remove("definitions") else {
            continue;
        };
        lifted.extend(definitions);
    }
    for (name, definition) in lifted {
        assert!(
            !schemas.contains_key(&name),
            "OpenAPI component name collision while lifting definition: {name}"
        );
        schemas.insert(name, definition);
    }
    for schema in schemas.values_mut() {
        rewrite_definition_refs(schema);
    }
}

fn rewrite_definition_refs(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                if key == "$ref" {
                    if let Value::String(reference) = field {
                        if let Some(name) = reference.strip_prefix("#/definitions/") {
                            *reference = format!("#/components/schemas/{name}");
                        }
                    }
                } else {
                    rewrite_definition_refs(field);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                rewrite_definition_refs(item);
            }
        }
        _ => {}
    }
}

pub fn component_schema<T: JsonSchema>() -> Value {
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

pub fn error_response_schema() -> Value {
    let mut schema = component_schema_with_refs::<HttpErrorEnvelope>();
    let object = schema
        .as_object_mut()
        .expect("HttpErrorEnvelope schema should be an object");
    if object.get("additionalProperties") == Some(&Value::Bool(true)) {
        object.remove("additionalProperties");
    }
    schema
}

pub fn component_schema_with_refs<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema).expect("serialize OpenAPI component schema");
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
    }
    value
}

#[cfg(test)]
mod tests;
