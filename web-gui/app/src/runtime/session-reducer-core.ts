/**
 * Session reduction and projection layer.
 *
 * This module contains two parallel systems for timeline rendering:
 *
 * ## Legacy system (still in active use)
 * - `reduceAgentSessionTimeline()`: Direct event→projection→timeline pipeline
 * - `debugAgentSessionEvents()`: Debug-focused event→projection pipeline
 * - Projection functions (`projectRuntimeEvent`, `projectToolExecution`, etc.)
 * - Helper functions (`stringField`, `readableText`, etc.)
 *
 * ## New system (preferred for SessionState-based rendering)
 * - `applyEvent()`: Event→SessionState router (called by session-state-reducer)
 * - `deriveTimelineView()`: SessionState→DomainObjects→TimelineItems
 * - `renderDomainObject()` in `object-renderers.ts`
 *
 * ## Migration notes
 * The new system (SessionState + specialized renderers) is the preferred
 * architecture for timeline rendering. The legacy system remains for:
 * - Backward compatibility with `reduceAgentSessionTimeline()` callers
 * - Debug UI (`debugAgentSessionEvents()` in AgentPage.tsx)
 * - Test coverage
 *
 * Helper functions in this module are shared by both systems and should
 * eventually be extracted to a separate utilities module to avoid duplication
 * with `object-renderers.ts`.
 */

import type {
  AgentTimelineActivity,
  AgentTimelineItem,
  AgentTimelineItemDetail,
  AgentTimelineItemKind,
  TimelineStateObjectRef,
  DisplayLevel,
  RuntimeMessageEnvelope,
  RuntimeBriefRecord,
  RuntimeTranscriptEntry,
} from "./types";
import type { SessionEventEnvelope } from "./session-events";
import { createSessionState, getObject, upsertObject } from "./session-state-reducer";
import type { SessionState } from "./session-state-reducer";
import type {
  DomainObject,
  MessageObject,
  RuntimeActivityObject,
  SessionObjectType,
  TaskObject,
  WorkItemObject,
  RenderData,
} from "./session-object-types";
import { deriveTimelineView } from "./timeline-view-model";
import type { RenderContext } from "./timeline-view-model";

function projectDebugEvent(
  eventType: string,
  payload: Record<string, unknown> | undefined,
): (Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> & { timestamp?: string }) | undefined {
  if (eventType === "tool_executed" || eventType === "tool_execution_failed") {
    return projectToolExecution(eventType, payload, { includeHiddenWorkItemMutations: true });
  }
  return projectRuntimeEvent(eventType, payload);
}

export interface ReduceAgentSessionInput {
  events: {
    events?: SessionEventEnvelope[];
  };
  eventDisplayLevel?: DisplayLevel;
  includeDebug?: boolean;
  messagesById?: Record<string, RuntimeMessageEnvelope>;
  transcriptEntriesById?: Record<string, RuntimeTranscriptEntry>;
  briefRecordsById?: Record<string, RuntimeBriefRecord>;
}

interface SessionItemDraft {
  id: string;
  kind: AgentTimelineItemKind;
  label: string;
  body: string;
  timestamp: string;
  meta: string;
  minDisplayLevel: DisplayLevel;
  sourceIds: string[];
  detail?: AgentTimelineItemDetail;
  rawEvent?: unknown;
  debug?: string;
}

const infoRuntimeEvents = new Set(["brief_created", "agent_waiting"]);
const verboseRuntimeEventPrefixes = ["work_item_"];
const workItemActivityEventNames = new Set([
  "work_item_picked",
  "work_item_focus_released",
  "work_item_completion_report_promoted",
  "work_item_completion_report_candidate_promoted",
]);
const workItemDebugOnlyEventNames = new Set(["work_item_stale_reminder_injected"]);
const debugRuntimeEventNames = new Set(["work_item_focus_released", "work_item_stale_reminder_injected"]);
const debugRuntimeEventPrefixes = ["provider_", "task_"];
const debugRuntimeEvents = new Set([
  "message_enqueued",
  "message_processing_started",
  "turn_local_checkpoint_resume_requested",
  "turn_local_checkpoint_requested",
  "turn_local_checkpoint_recorded",
  "continuation_trigger_received",
  "continuation_resolved",
  "closure_decided",
]);
const debugOnlyToolNames = new Set(["WaitFor"]);

export function reduceAgentSessionTimeline(input: ReduceAgentSessionInput): AgentTimelineItem[] {
  const state = createSessionState();
  const applyCtx: ApplyContext = {
    eventDisplayLevel: input.eventDisplayLevel ?? "debug",
    includeDebug: input.includeDebug ?? false,
    messagesById: input.messagesById,
    transcriptEntriesById: input.transcriptEntriesById,
    briefRecordsById: input.briefRecordsById,
  };

  for (const event of input.events.events ?? []) {
    applyEvent(state, event, applyCtx);
  }

  return deriveTimelineView(state, {
    eventDisplayLevel: applyCtx.eventDisplayLevel,
    includeDebug: applyCtx.includeDebug,
    activitiesById: state.activitiesById,
    messagesById: applyCtx.messagesById,
    transcriptEntriesById: applyCtx.transcriptEntriesById,
    briefRecordsById: applyCtx.briefRecordsById,
  });
}

interface ApplyContext {
  eventDisplayLevel: DisplayLevel;
  includeDebug: boolean;
  messagesById?: Record<string, RuntimeMessageEnvelope>;
  transcriptEntriesById?: Record<string, RuntimeTranscriptEntry>;
  briefRecordsById?: Record<string, RuntimeBriefRecord>;
}

/**
 * Route a single event through the apply layer: determine the domain object
 * type and identity key, store render data, then upsert into normalized state.
 *
 * Projection (event -> display fields) is deferred to the render layer
 * (`deriveTimelineView`). This function only handles object routing,
 * identity, and render-data storage.
 */
function applyEvent(state: SessionState, event: SessionEventEnvelope, ctx: ApplyContext): void {
  if (!event.id && event.event_seq == null) return;

  const eventId = event.id ?? `event-${event.event_seq}`;
  const eventType = event.type ?? "runtime_event";
  const payload = asRecord(event.payload);
  const ts = event.ts ?? "";
  const meta = eventMeta(eventType, payload, event.event_seq);

  const render: RenderData = {
    eventType,
    payload,
    timestamp: ts,
    eventId,
    eventSeq: event.event_seq,
    meta,
    debug: ctx.includeDebug ? debugJson(event) : undefined,
    rawEvent: event,
  };

  const baseFields = {
    sourceEventIds: [eventId],
    createdAt: ts,
    updatedAt: ts,
    render,
  };

  // Route by event type to determine object type, identity key, and status
  if (eventType === "message_enqueued" || eventType === "message_processing_started") {
    const origin = asRecord(payload?.origin);
    const originKind = stringField(origin, "kind")?.toLowerCase();
    const messageId = stringField(payload, "message_id");
    const objId = messageId ? `message:${messageId}` : eventId;

    // For message_processing_started, if the message object already exists (from
    // message_enqueued with the same message_id), only update the status and
    // accumulate sourceEventIds without overwriting render data (which carries
    // the message body text).
    if (eventType === "message_processing_started") {
      const existing = getObject(state, "message", objId);
      if (existing) {
        existing.status = "processing";
        existing.updatedAt = ts;
        if (!existing.sourceEventIds.includes(eventId)) {
          existing.sourceEventIds.push(eventId);
        }
        return;
      }
    }

    upsertObject(state, "message", objId, {
      ...baseFields,
      id: objId,
      status: eventType === "message_processing_started" ? "processing" : "enqueued",
      role: originKind === "operator" ? "operator" : "unknown",
    } as DomainObject);
  } else if (eventType === "tool_executed" || eventType === "tool_execution_failed") {
    const toolExecutionId = firstStringField(payload, ["tool_execution_id", "toolExecutionId"]) ?? eventId;
    const result = asRecord(payload?.exec_command_result);
    const disposition = firstStringField(payload, ["exec_command_disposition"]) ?? firstStringField(result, ["disposition"]);
    const promoted = disposition === "promoted_to_task";
    const promotedTaskId = promoted ? firstStringField(asRecord(payload?.task_handle), ["task_id"]) : undefined;
    const toolName = stringField(payload, "tool_name") ?? "tool";
    const relatedRef = toolExecutionRelatedStateObjectRef(toolName, payload, promotedTaskId);
    upsertObject(state, "tool_execution", toolExecutionId, {
      ...baseFields,
      id: toolExecutionId,
      status: eventType === "tool_execution_failed" ? "failed" : promoted ? "promoted" : "completed",
      toolName,
      taskId: promotedTaskId,
      relatedStateObjectRef: relatedRef,
    } as DomainObject);
  } else if (eventType === "task_created" || eventType === "task_status_updated" || eventType === "task_result_received") {
    const taskId = stringField(payload, "task_id");
    const taskObjId = taskId ? `task:${taskId}` : eventId;
    const existingTask = state.tasks.get(taskObjId);
    const taskStatus = firstStringField(payload, ["task_status", "status"]) as TaskObject["status"];
    const summary = stringField(payload, "summary") ?? state.tasks.get(taskObjId)?.summary;
    const isActivity = eventType !== "task_created";
    const activityIds = isActivity ? [eventId] : undefined;
    upsertObject(state, "task", taskId ? `task:${taskId}` : eventId, {
      ...baseFields,
      id: taskObjId,
      status: taskStatus ?? (eventType === "task_created" ? "created" : "running"),
      initialStatus: existingTask?.initialStatus ?? (taskStatus ?? "created"),
      summary,
      activityIds,
    } as DomainObject);
    if (isActivity) {
      upsertObject(state, "activity", eventId, {
        ...baseFields,
        id: eventId,
        status: eventType,
        eventType,
        relatedStateObjectRef: {
          kind: "task",
          id: taskObjId,
          status: taskStatus ?? "running",
          summary,
        },
      } as DomainObject);
    }
  } else if (isDebugOnlyWorkItemEvent(eventType)) {
    upsertObject(state, "activity", eventId, {
      ...baseFields,
      id: eventId,
      status: eventType,
      eventType,
    } as DomainObject);
  } else if (eventType.startsWith("work_item_")) {
    const workItemId = workItemObjectId(payload) ?? eventId;
    const previousWorkItem = state.workItems.get(workItemId);
    const objective = workItemObjective(payload) ?? previousWorkItem?.objective;
    const stateName = workItemState(payload) ?? previousWorkItem?.state;
    const activityIds = isWorkItemActivityEvent(eventType) ? [eventId] : undefined;
    upsertObject(state, "work_item", workItemId, {
      ...baseFields,
      id: workItemId,
      status: workItemStatus(eventType, stateName, previousWorkItem?.status),
      objective,
      state: stateName,
      activityIds,
    } as DomainObject);
    if (activityIds) {
      upsertObject(state, "activity", eventId, {
        ...baseFields,
        id: eventId,
        status: eventType,
        eventType,
        relatedStateObjectRef: {
          kind: "work_item",
          id: workItemId,
          objective,
          state: stateName,
        },
      } as DomainObject);
    }
  } else if (eventType === "brief_created" || eventType === "assistant_round_recorded") {
    upsertObject(state, "assistant_round", eventId, {
      ...baseFields,
      id: eventId,
      status: "recorded",
    } as DomainObject);
  } else {
    upsertObject(state, "activity", eventId, {
      ...baseFields,
      id: eventId,
      status: eventType,
      eventType,
    } as DomainObject);
  }
}

export function debugAgentSessionEvents(events: SessionEventEnvelope[], options: { itemLimit?: number } = {}): AgentTimelineItem[] {
  const projected = events
    .filter((event) => event.id || event.event_seq != null)
    .map((event) => debugEventTimelineItem(event))
    .sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
  return projected.slice(-(options.itemLimit ?? 220));
}

function debugEventTimelineItem(event: SessionEventEnvelope): AgentTimelineItem {
  const id = event.id ?? `event-${event.event_seq}`;
  const payload = asRecord(event.payload);
  const eventType = event.type ?? "runtime_event";
  const projection = projectDebugEvent(eventType, payload);
  const meta = eventMeta(eventType, payload, event.event_seq);
  const body = projection?.body || readableText(payload) || summarizeDebugEvent(eventType, payload) || humanizeEventType(eventType);
  const detail = debugEventDetail(eventType, payload, projection?.detail);
  return item({
    id: `debug:${id}`,
    kind: projection?.kind ?? "event",
    label: projection?.label ?? humanizeEventType(eventType),
    body,
    timestamp: projection?.timestamp ?? event.ts ?? "",
    meta,
    minDisplayLevel: "debug",
    sourceIds: [id],
    detail,
    rawEvent: event,
    debug: debugJson(event),
  });
}

function debugEventDetail(
  eventType: string,
  payload: Record<string, unknown> | undefined,
  projectedDetail: AgentTimelineItemDetail | undefined,
): AgentTimelineItemDetail | undefined {
  if (projectedDetail) return projectedDetail;

  const facts = readableEventFacts(payload);
  if (facts.length) {
    return {
      label: "Event details",
      text: facts.join("\n"),
      tone: eventType.includes("failed") || eventType.includes("error") ? "data" : "data",
    };
  }

  const readable = readableText(payload);
  if (readable) return { label: "Details", text: readable, tone: "data" };
  return undefined;
}

function eventMeta(eventType: string, payload: Record<string, unknown> | undefined, eventSeq: number | undefined): string {
  const eventRef = eventSeq == null ? undefined : `event #${eventSeq}`;
  if (eventType === "message_enqueued" && messageEnvelopeProjection(payload)?.origin === "operator") {
    return compactJoin(["Sent", eventRef]);
  }
  return eventRef == null ? eventType : `${eventType} · ${eventRef}`;
}

export function eventProjectionDisplayLevel(level: DisplayLevel, eventDisplayLevel: DisplayLevel): DisplayLevel {
  // `eventDisplayLevel` describes the API page that supplied the event. It must
  // not promote or demote a semantic projection: display filtering is applied
  // later against each item's intrinsic `minDisplayLevel`.
  void eventDisplayLevel;
  return level;
}

export function projectRuntimeEvent(
  eventType: string,
  payload: Record<string, unknown> | undefined,
  messagesById?: Record<string, RuntimeMessageEnvelope>,
  transcriptEntriesById?: Record<string, RuntimeTranscriptEntry>,
  briefRecordsById?: Record<string, RuntimeBriefRecord>,
): (Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> & { timestamp?: string }) | undefined {
  if (eventType === "message_enqueued") {
    const message = messageEnvelopeProjection(payload, messagesById);
    if (message?.origin === "operator") {
      return {
        kind: "operator",
        label: "Operator input",
        body: message.body || "Loading operator input…",
        minDisplayLevel: "info",
      };
    }

    return {
      kind: "system",
      label: "Message queued",
      body: message?.body || readableText(payload) || "Runtime message queued.",
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType === "brief_created") {
    return {
      kind: "assistant",
      label: stringField(payload, "kind") === "result" ? "Result" : "Brief Created",
      body:
        briefTextForPayload(payload, briefRecordsById) ||
        readableTextWithoutSummary(payload) ||
        "Brief text unavailable.",
      timestamp: stringField(payload, "created_at"),
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType === "assistant_round_recorded") {
    return projectAssistantRoundRecorded(payload, transcriptEntriesById);
  }

  if (eventType === "tool_executed" || eventType === "tool_execution_failed") {
    return projectToolExecution(eventType, payload);
  }

  if (eventType === "message_processing_started") {
    return {
      kind: "system",
      label: "Started processing",
      body: compactJoin([stringField(payload, "origin") === "operator" ? "Operator input" : undefined, stringField(payload, "run_id")]) ||
        "Agent started processing input.",
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType === "turn_started") {
    const turnIndex = numberField(payload, "turn_index");
    const messageKind = stringField(payload, "message_kind");
    const triggerLabel = messageKind ? turnTriggerLabel(messageKind) : undefined;
    return {
      kind: "system",
      label: "Turn started",
      body: compactJoin([
        turnIndex != null ? `Turn #${turnIndex}` : undefined,
        triggerLabel,
      ]) || "Turn started",
      timestamp: stringField(payload, "created_at"),
      minDisplayLevel: "info",
    };
  }

  if (debugRuntimeEvents.has(eventType)) {
    return {
      kind: "system",
      label: systemRuntimeLabel(eventType),
      body: summarizeSystemRuntimeEvent(eventType, payload),
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType === "task_created" || eventType === "task_status_updated" || eventType === "task_result_received") {
    return projectTaskLifecycleEvent(eventType, payload);
  }

  if (eventType.startsWith("work_item_")) {
    return {
      kind: "system",
      label: "Work item",
      body: summarizeWorkItemEvent(eventType, payload),
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType === "wait_condition_registered" || eventType === "agent_waiting") {
    return {
      kind: "system",
      label: "Waiting",
      body: readableText(payload) || "Agent is waiting for an external condition.",
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType.includes("failed") || eventType.includes("error")) {
    return {
      kind: "system",
      label: "Runtime alert",
      body: readableText(payload) || humanizeEventType(eventType),
      minDisplayLevel: "info",
    };
  }

  if (eventType.startsWith("provider_") || eventType.startsWith("task_")) {
    return {
      kind: "event",
      label: humanizeEventType(eventType),
      body: summarizeDebugEvent(eventType, payload),
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  return {
    kind: "event",
    label: humanizeEventType(eventType),
    body: readableText(payload) || humanizeEventType(eventType),
    minDisplayLevel: "debug",
  };
}

function runtimeEventDisplayLevel(eventType: string): DisplayLevel {
  if (infoRuntimeEvents.has(eventType)) return "info";
  if (eventType.includes("failed") || eventType.includes("error")) return "info";
  if (debugRuntimeEventNames.has(eventType)) return "debug";
  if (debugRuntimeEvents.has(eventType)) return "debug";
  if (debugRuntimeEventPrefixes.some((prefix) => eventType.startsWith(prefix))) return "debug";
  if (verboseRuntimeEventPrefixes.some((prefix) => eventType.startsWith(prefix))) return "verbose";
  return "debug";
}

function projectAssistantRoundRecorded(
  payload: Record<string, unknown> | undefined,
  transcriptEntriesById?: Record<string, RuntimeTranscriptEntry>,
): Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> | undefined {
  if (stringField(payload, "round_purpose") === "runtime_checkpoint") {
    return undefined;
  }
  const transcriptText = transcriptTextForPayload(payload, transcriptEntriesById);
  if (transcriptText) {
    return {
      kind: "assistant",
      label: "Assistant round",
      body: transcriptText,
      minDisplayLevel: "verbose",
    };
  }

  const textPreview = stringField(payload, "text_preview");
  if (textPreview) {
    return {
      kind: "assistant",
      label: "Assistant round",
      body: textPreview,
      minDisplayLevel: "verbose",
    };
  }

  return undefined;
}

function item(draft: SessionItemDraft): AgentTimelineItem {
  return draft;
}

export function projectToolExecution(
  eventType: string,
  payload: Record<string, unknown> | undefined,
  options: { includeHiddenWorkItemMutations?: boolean } = {},
): Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> | undefined {
  const toolName = stringField(payload, "tool_name") ?? "tool";
  const failed = eventType === "tool_execution_failed" || Boolean(payload?.error);
  if (!failed && isWorkItemMutationTool(toolName) && !options.includeHiddenWorkItemMutations) return undefined;
  const projection = projectKnownToolExecution(toolName, payload);
  const label = toolFriendlyLabel(toolName, failed);
  const summary = stringField(payload, "summary");
  const commandPreview = execCommandPreview(payload);
  const result = asRecord(payload?.exec_command_result);
  const exitStatus = numberField(payload, "exit_status") ?? numberField(result, "exit_status");
  const durationMs = numberField(payload, "duration_ms") ?? numberField(result, "duration_ms");
  const error = toolErrorMessage(payload);
  const disposition = firstStringField(payload, ["exec_command_disposition"]) ?? firstStringField(result, ["disposition"]);
  const promoted = disposition === "promoted_to_task";
  const stringPreview = toolStringPreview(toolName, payload, commandPreview) || undefined;
  const toolSummary = projection?.body ?? stringPreview ?? summary ?? genericToolDescription(toolName, payload) ?? toolName;
  // Suppress duration for promoted tasks (yield_time, not real execution) and for read/control tools
  // where duration_ms is just API round-trip time with no user-facing meaning.
  const suppressDuration = promoted || isReadControlTool(toolName);
  const effectiveDuration = suppressDuration ? undefined : durationMs;
  const promotedTaskId = promoted ? firstStringField(asRecord(payload?.task_handle), ["task_id"]) : undefined;
  const body = compactJoin([
    toolSummary,
    promotedTaskId ? `task ${shortTaskId(promotedTaskId)}` : undefined,
    exitStatus == null ? undefined : `exit ${exitStatus}`,
    effectiveDuration == null ? undefined : formatDuration(effectiveDuration),
    error,
  ]);
  const outputPreview = commandOutputPreview(payload);
  const detail = projection?.detail ?? toolExecutionDetail(toolName, payload, commandPreview, outputPreview, toolSummary, failed ? error : undefined);

  return {
    kind: "tool",
    label,
    body: body || (failed ? "Failed." : "Completed."),
    detail,
    minDisplayLevel: toolTimelineDisplayLevel(toolName),
  };
}

function shortTaskId(taskId: string): string {
  return taskId.length > 20 ? taskId.slice(0, 20) + "…" : taskId;
}

function toolTimelineDisplayLevel(toolName: string): DisplayLevel {
  if (debugOnlyToolNames.has(toolName)) return "debug";
  return "verbose";
}

function projectKnownToolExecution(
  toolName: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  if (toolName === "ApplyPatch") return projectApplyPatchTool(payload);
  if (toolName === "ListTasks") return projectListTasksTool(payload);
  if (toolName === "ListWorkItems") return projectListWorkItemsTool(payload);
  if (toolName === "GetWorkItem") return projectGetWorkItemTool(payload);
  if (isWorkItemMutationTool(toolName)) return projectWorkItemMutationTool(payload);
  if (toolName === "ViewImage") return projectViewImageTool(payload);
  if (isWebSearchTool(toolName)) return projectWebSearchTool(toolName, payload);
  if (isWebFetchTool(toolName)) return projectWebFetchTool(payload);
  if (toolName === "MemorySearch") return projectMemorySearchTool(payload);
  if (toolName === "MemoryGet") return projectMemoryGetTool(payload);
  if (toolName === "TaskOutput") return projectTaskOutputTool(payload);
  if (toolName === "TaskStatus") return projectTaskStatusTool(payload);
  if (toolName === "TaskStop") return projectTaskStopTool(payload);
  if (toolName === "TaskInput") return projectTaskInputTool(payload);
  if (toolName === "SpawnAgent") return projectSpawnAgentTool(payload);
  if (toolName === "UseWorkspace") return projectUseWorkspaceTool(payload);
  if (toolName === "Enqueue") return projectEnqueueTool(payload);
  if (toolName === "GenerateImage") return projectGenerateImageTool(payload);
  if (toolName === "AgentGet") return projectAgentGetTool(payload);
  if (toolName === "ListModelProviders") return projectListModelProvidersTool(payload);
  if (toolName === "ListProviderModels") return projectListProviderModelsTool(payload);
  if (toolName === "WaitFor") return projectWaitForTool(payload);
  return undefined;
}

function isWorkItemMutationTool(toolName: string): boolean {
  return toolName === "CreateWorkItem" || toolName === "UpdateWorkItem" || toolName === "PickWorkItem" || toolName === "CompleteWorkItem";
}

/**
 * Tools that operate on a single task and carry a task_id in the payload.
 * Excludes ListTasks (no single task target).
 */
function isTaskOperationTool(toolName: string): boolean {
  return toolName === "TaskOutput" || toolName === "TaskStatus" || toolName === "TaskStop" || toolName === "TaskInput";
}

/**
 * Compute the relatedStateObjectRef for a tool execution, linking it to
 * a parent WorkItem, Task, or promoted-Task for navigation breadcrumbs.
 */
function toolExecutionRelatedStateObjectRef(
  toolName: string,
  payload: Record<string, unknown> | undefined,
  promotedTaskId?: string,
): TimelineStateObjectRef | undefined {
  // Promoted ExecCommand → Task
  if (promotedTaskId) {
    return { kind: "task", id: `task:${promotedTaskId}`, status: "running" };
  }
  // WorkItem tools: CreateWorkItem, UpdateWorkItem, PickWorkItem, CompleteWorkItem, GetWorkItem
  if (isWorkItemMutationTool(toolName) || toolName === "GetWorkItem") {
    const workItemId = workItemObjectId(payload);
    if (workItemId) return { kind: "work_item", id: workItemId };
  }
  // Task tools: TaskOutput, TaskStatus, TaskStop, TaskInput
  if (isTaskOperationTool(toolName)) {
    const taskId = stringField(payload, "task_id");
    if (taskId) return { kind: "task", id: `task:${taskId}`, status: "unknown" };
  }
  return undefined;
}

function isWebSearchTool(toolName: string): boolean {
  return toolName === "WebSearch";
}

function isWebFetchTool(toolName: string): boolean {
  return toolName === "WebFetch";
}

/**
 * Read/control tools where duration_ms is just API round-trip time, not meaningful execution time.
 * Suppress showing it in the timeline to avoid noise like "success · 272ms".
 */
function isReadControlTool(toolName: string): boolean {
  return (
    toolName === "TaskOutput" ||
    toolName === "TaskStatus" ||
    toolName === "TaskStop" ||
    toolName === "TaskInput" ||
    toolName === "ListTasks" ||
    toolName === "ListWorkItems" ||
    toolName === "GetWorkItem" ||
    toolName === "MemorySearch" ||
    toolName === "MemoryGet" ||
    toolName === "AgentGet" ||
    toolName === "ListModelProviders" ||
    toolName === "ListProviderModels"
  );
}

function toolStringPreview(
  toolName: string,
  payload: Record<string, unknown> | undefined,
  commandPreview: string | undefined,
): string | undefined {
  if (commandPreview) return commandPreview;
  if (toolName === "WaitFor") return stringField(payload, "reason");
  return readableTextWithoutSummary(payload);
}

function genericToolDescription(toolName: string, payload: Record<string, unknown> | undefined): string {
  const waitReason = stringField(payload, "reason");
  if (waitReason) return waitReason;

  const readable = readableText(payload);
  if (readable) return readable;

  const objective = stringField(payload, "objective");
  if (objective) return objective;

  const workItemId = stringField(payload, "work_item_id");
  if (workItemId) return workItemId;

  const resource = stringField(payload, "resource");
  if (resource) return resource;

  return toolName;
}

function workItemObjectId(payload: Record<string, unknown> | undefined): string | undefined {
  const record = workItemRecord(payload);
  return firstStringField(payload, ["work_item_id", "current_work_item_id"])
    ?? firstStringField(record, ["id", "work_item_id", "workItemId"]);
}

function workItemObjective(payload: Record<string, unknown> | undefined): string | undefined {
  const record = workItemRecord(payload);
  return firstStringField(record, ["objective", "objective_preview"])
    ?? firstStringField(payload, ["objective", "objective_preview"]);
}

function workItemState(payload: Record<string, unknown> | undefined): string | undefined {
  const record = workItemRecord(payload);
  return firstStringField(record, ["state", "plan_status", "readiness"])
    ?? firstStringField(payload, ["state", "plan_status", "readiness"]);
}

function workItemRecord(payload: Record<string, unknown> | undefined): Record<string, unknown> | undefined {
  const record = asRecord(payload?.record);
  if (record) return record;
  const workItem = asRecord(payload?.work_item);
  if (workItem) return workItem;
  return asRecord(payload?.workItem);
}

function isWorkItemActivityEvent(eventType: string): boolean {
  return workItemActivityEventNames.has(eventType);
}

function isDebugOnlyWorkItemEvent(eventType: string): boolean {
  return workItemDebugOnlyEventNames.has(eventType);
}

function workItemStatus(
  eventType: string,
  stateName: string | undefined,
  previousStatus: WorkItemObject["status"] | undefined,
): WorkItemObject["status"] {
  if (isKnownWorkItemStatus(stateName)) return stateName;
  if (eventType === "work_item_completed" || eventType === "work_item_completion_report_promoted") return "completed";
  if (eventType === "work_item_blocked") return "blocked";
  if (eventType === "work_item_focus_released") return previousStatus ?? "yielded";
  return previousStatus ?? "unknown";
}

function isKnownWorkItemStatus(value: string | undefined): value is WorkItemObject["status"] {
  return (
    value === "unknown" ||
    value === "open" ||
    value === "runnable" ||
    value === "queued" ||
    value === "yielded" ||
    value === "blocked" ||
    value === "waiting_for_operator" ||
    value === "draft" ||
    value === "ready" ||
    value === "needs_input" ||
    value === "completed" ||
    value === "cancelled"
  );
}

function projectApplyPatchTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.apply_patch_result);
  if (!result) return undefined;

  const changedFiles = arrayField(result, "changed_files")
    ?.map(asRecord)
    .filter((file): file is Record<string, unknown> => Boolean(file));
  const changedPaths = stringArrayField(result, "changed_paths");
  const pathSummary =
    changedFiles?.map((file) => stringField(file, "path")).filter((path): path is string => Boolean(path)) ?? changedPaths;
  const fileCount = numberField(result, "changed_file_count") ?? pathSummary.length;
  const body = compactJoin([
    fileCount ? `${fileCount} file${fileCount === 1 ? "" : "s"}` : undefined,
    pathSummary.length ? pathSummary.join(", ") : stringField(result, "summary_text"),
  ]);
  const detailText = applyPatchDetailText(result, changedFiles, changedPaths);

  return {
    body,
    detail: detailText
      ? {
          label: "Patch diff",
          text: detailText,
          tone: "diff",
        }
      : undefined,
  };
}

function projectListTasksTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.list_tasks_result) ?? asRecord(payload?.result);
  const tasks = arrayField(result, "tasks") ?? arrayField(result, "active_tasks");
  const total = numberField(result, "total_active") ?? numberField(result, "total") ?? tasks?.length;
  const returned = numberField(result, "returned") ?? tasks?.length;
  const taskSummaries = summarizeTaskRecords(tasks);
  return {
    body: compactJoin([
      total == null ? "Listed tasks" : `${total} active task${total === 1 ? "" : "s"}`,
      returned != null && total != null && returned !== total ? `${returned} returned` : undefined,
      taskSummaries.length ? taskSummaries.slice(0, 3).join("; ") : undefined,
    ]),
    detail: taskSummaries.length
      ? { label: "Tasks", text: taskSummaries.join("\n"), tone: "data" }
      : { label: "Result", text: debugJson(result ?? payload ?? {}), tone: "data" },
  };
}

function extractTaskFromOutput(payload: Record<string, unknown> | undefined): {
  task?: Record<string, unknown>;
  taskId?: string;
  taskStatus?: string;
  retrievalStatus?: string;
  kind?: string;
  summary?: string;
  exitStatus?: number;
  outputPreview?: string;
  truncated?: boolean;
} {
  const result = asRecord(payload?.task_output_result) ?? unwrapToolResult(payload);
  const task = asRecord(result.task) ?? asRecord(result.task_record);
  const taskId = firstStringField(task, ["task_id", "id"]) ?? stringField(result, "task_id") ?? stringField(payload, "task_id");
  const resultStatus = stringField(result, "status");
  const taskStatus =
    firstStringField(task, ["status"]) ?? (resultStatus && !isTaskOutputRetrievalStatus(resultStatus) ? resultStatus : undefined);
  const retrievalStatus =
    stringField(result, "retrieval_status") ?? (resultStatus && isTaskOutputRetrievalStatus(resultStatus) ? resultStatus : undefined);
  const kind = stringField(task, "kind") ?? stringField(result, "kind");
  const summary = stringField(task, "summary") ?? stringField(result, "summary") ?? stringField(result, "result_summary");
  const exitStatus = numberField(task, "exit_status") ?? numberField(result, "exit_status");
  const outputPreview =
    stringField(task, "output_preview") ??
    stringField(result, "output_preview") ??
    stringField(result, "output") ??
    stringField(result, "stdout") ??
    stringField(result, "stderr");
  const truncated = task?.output_truncated === true || result.output_truncated === true || result.truncated === true;
  return { task, taskId, taskStatus, retrievalStatus, kind, summary, exitStatus, outputPreview, truncated };
}

function isTaskOutputRetrievalStatus(status: string): boolean {
  return status === "success" || status === "timeout" || status === "not_ready";
}

function formatTaskOutputRetrievalStatus(status: string | undefined): string | undefined {
  if (!status || status === "success") return undefined;
  if (status === "timeout") return "retrieval timeout";
  if (status === "not_ready") return "not ready";
  return status;
}

function projectTaskOutputTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const info = extractTaskFromOutput(payload);
  const body = compactJoin([
    "Task output",
    info.taskId ? shortTaskId(info.taskId) : undefined,
    formatTaskOutputRetrievalStatus(info.retrievalStatus),
    info.taskStatus,
    info.kind,
    info.summary,
    info.exitStatus != null ? `exit ${info.exitStatus}` : undefined,
    info.truncated ? "truncated" : undefined,
  ]);
  return {
    body,
    detail: info.outputPreview
      ? {
          label: info.truncated ? "Task output (truncated)" : "Task output",
          text: truncateText(info.outputPreview, 2000),
          tone: "output",
        }
      : undefined,
  };
}

function projectTaskStatusTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.task_status_result) ?? unwrapToolResult(payload);
  const taskId = stringField(result, "task_id") ?? stringField(payload, "task_id");
  const status = stringField(result, "status");
  const summary = stringField(result, "summary");
  const kind = stringField(result, "kind");
  const body = compactJoin([
    "Task status",
    taskId ? shortTaskId(taskId) : undefined,
    status === "success" ? undefined : status,
    kind,
    summary,
  ]);
  return {
    body,
    detail: undefined,
  };
}

function projectTaskStopTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.task_stop_result) ?? unwrapToolResult(payload);
  const taskId = stringField(result, "task_id") ?? stringField(payload, "task_id");
  const status = stringField(result, "status");
  const body = compactJoin([
    "Stopped task",
    taskId ? shortTaskId(taskId) : undefined,
    status === "success" ? undefined : status,
  ]);
  return {
    body,
    detail: undefined,
  };
}

function projectTaskInputTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.task_input_result) ?? unwrapToolResult(payload);
  const taskId = stringField(result, "task_id") ?? stringField(payload, "task_id");
  const input = stringField(payload, "input") ?? stringField(result, "input");
  const body = compactJoin([
    "Task input",
    taskId ? shortTaskId(taskId) : undefined,
    input ? truncateText(input.replace(/\s+/g, " ").trim(), 100) : undefined,
  ]);
  return {
    body,
    detail: undefined,
  };
}

function projectListWorkItemsTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.list_work_items_result) ?? asRecord(payload?.result);
  const items = arrayField(result, "work_items") ?? arrayField(result, "items");
  const total = numberField(result, "total") ?? numberField(result, "total_open") ?? items?.length;
  const filter = stringField(payload, "filter") ?? stringField(result, "filter");
  const itemSummaries = summarizeWorkItemRecords(items);
  return {
    body: compactJoin([
      total == null ? "Listed work items" : `${total} work item${total === 1 ? "" : "s"}`,
      filter ? `filter: ${filter}` : undefined,
      itemSummaries.length ? itemSummaries.slice(0, 3).join("; ") : undefined,
    ]),
    detail: itemSummaries.length
      ? { label: "Work items", text: itemSummaries.join("\n"), tone: "data" }
      : { label: "Result", text: debugJson(result ?? payload ?? {}), tone: "data" },
  };
}

function projectGetWorkItemTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.get_work_item_result) ?? asRecord(payload?.result);
  const workItem = asRecord(result?.work_item) ?? asRecord(result);
  const summary = summarizeWorkItemRecord(workItem);
  const workItemId = stringField(payload, "work_item_id") ?? stringField(workItem, "id");
  return {
    body: summary || compactJoin(["Loaded work item", workItemId]) || "Loaded work item",
    detail: { label: "Work item", text: debugJson(result ?? payload ?? {}), tone: "data" },
  };
}

function projectWorkItemMutationTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.result);
  const workItem = asRecord(result?.work_item) ?? asRecord(result) ?? payload;
  const summary = summarizeWorkItemRecord(workItem);
  const facts = readableEventFacts(payload);
  return {
    body: summary || genericToolDescription(stringField(payload, "tool_name") ?? "WorkItem", payload),
    detail: facts.length ? { label: "Work item change", text: facts.join("\n"), tone: "data" } : undefined,
  };
}

function projectViewImageTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const input = asRecord(payload?.input);
  const result = asRecord(payload?.view_image_result) ?? asRecord(payload?.result);
  const visualRef = asRecord(result?.visual_reference);
  const sizeInfo = asRecord(visualRef?.size);
  const width = numberField(sizeInfo, "width");
  const height = numberField(sizeInfo, "height");
  const imagePath =
    firstStringField(input, ["path", "image_path"]) ??
    firstStringField(payload, ["path", "image_path"]) ??
    firstStringField(visualRef, ["path", "image_path"]);
  const observationObj = asRecord(result?.observation);
  const observation = firstStringField(observationObj, ["summary"]) ?? firstStringField(result, ["visual_observation", "observation", "text_preview"]);
  const body = compactJoin([
    "Viewed image",
    imagePath ? basename(imagePath) : undefined,
    width != null && height != null ? `${width}×${height}` : undefined,
    observation ? truncateText(observation, 120) : undefined,
  ]);

  return {
    body,
    detail: observation
      ? { label: "Visual observation", text: observation, tone: "data" }
      : { label: "Result", text: debugJson(result ?? payload ?? {}), tone: "data" },
  };
}

function projectWebSearchTool(
  toolName: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const output = unwrapToolResult(payload);
  const query = stringField(output, "query") ?? firstStringField(asRecord(payload?.input), ["query", "search_query", "q"]);
  const results = arrayField(output, "results")?.map(asRecord).filter((r): r is Record<string, unknown> => Boolean(r)) ?? [];
  const body = compactJoin([
    "Web search",
    query,
    results.length ? `${results.length} result${results.length === 1 ? "" : "s"}` : undefined,
  ]);

  const detailText = results.length
    ? results
        .slice(0, 10)
        .map((item, index) =>
          compactJoin([
            `${index + 1}. ${stringField(item, "title") ?? "Untitled"}`,
            stringField(item, "url"),
            stringField(item, "source"),
            stringField(item, "snippet") ? truncateText(stringField(item, "snippet")!, 200) : undefined,
          ]),
        )
        .join("\n\n")
    : undefined;

  return {
    body,
    detail: detailText ? { label: "Search results", text: detailText, tone: "output" } : undefined,
  };
}

function projectWebFetchTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const output = unwrapToolResult(payload);
  const url = stringField(output, "url") ?? firstStringField(asRecord(payload?.input), ["url"]);
  const status = numberField(output, "status");
  const bytesRead = numberField(output, "bytes_read");
  const body = compactJoin([
    "Web fetch",
    url ? truncateText(url, 80) : undefined,
    status != null ? `${status}` : undefined,
    bytesRead != null ? formatBytesRead(bytesRead) : undefined,
  ]);
  const text = stringField(output, "text");
  const truncated = output && typeof output === "object" && "truncated" in output ? output.truncated === true : false;
  return {
    body,
    detail: text
      ? { label: truncated ? "Fetched content (truncated)" : "Fetched content", text: truncateText(text, 600), tone: "output" }
      : undefined,
  };
}

function unwrapToolResult(payload: Record<string, unknown> | undefined): Record<string, unknown> {
  if (!payload) return {};
  const output = asRecord(payload.output) ?? asRecord(payload.result) ?? payload;
  const envelope = asRecord(output.envelope);
  if (envelope) return asRecord(envelope.result) ?? envelope;
  return output;
}

function projectMemorySearchTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const output = unwrapToolResult(payload);
  const query = stringField(output, "query") ?? firstStringField(asRecord(payload?.input), ["query"]);
  const results = arrayField(output, "results") ?? [];
  const resultCount = results.length;
  const body = compactJoin([
    "Memory search",
    query ? `“${truncateText(query, 60)}”` : undefined,
    resultCount === 0 ? "no matches" : `${resultCount} ${resultCount === 1 ? "result" : "results"}`,
  ]);
  const detailText = results.length
    ? results
        .map((item) => {
          const record = asRecord(item);
          const sourceRef = stringField(record, "source_ref");
          const preview = stringField(record, "preview");
          return compactJoin([sourceRef, preview]);
        })
        .filter(Boolean)
        .join("\n\n")
    : undefined;
  return {
    body,
    detail: detailText ? { label: "Memory results", text: detailText, tone: "output" } : undefined,
  };
}

function projectMemoryGetTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const output = unwrapToolResult(payload);
  const memory = asRecord(output.memory) ?? output;
  const sourceRef = stringField(memory, "source_ref") ?? firstStringField(asRecord(payload?.input), ["source_ref"]);
  const content = stringField(memory, "content");
  const body = compactJoin([
    "Memory get",
    sourceRef ? truncateText(sourceRef, 80) : undefined,
    content ? `${formatBytesRead(content.length)} retrieved` : undefined,
  ]);
  return {
    body,
    detail: content
      ? { label: "Memory content", text: truncateText(content, 800), tone: "output" }
      : undefined,
  };
}

function formatBytesRead(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

function projectSpawnAgentTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const input = asRecord(payload?.input);
  const result = asRecord(payload?.spawn_agent_result) ?? unwrapToolResult(payload);
  const agentId = stringField(result, "agent_id") ?? stringField(input, "agent_id") ?? stringField(payload, "agent_id");
  const preset = stringField(input, "preset") ?? stringField(payload, "preset") ?? stringField(result, "preset");
  const template = stringField(input, "template") ?? stringField(payload, "template") ?? stringField(result, "template");
  const initialMessage =
    stringField(input, "initial_message") ?? stringField(payload, "initial_message") ?? stringField(result, "initial_message");
  const body = compactJoin([
    "Spawned agent",
    agentId ?? "agent",
    preset && preset !== "private_child" ? preset : undefined,
    template,
    initialMessage ? truncateText(initialMessage.replace(/\s+/g, " ").trim(), 80) : undefined,
  ]);
  return { body, detail: undefined };
}

function projectUseWorkspaceTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const input = asRecord(payload?.input);
  const path = stringField(input, "path") ?? stringField(payload, "path");
  const workspaceId = stringField(input, "workspace_id") ?? stringField(payload, "workspace_id");
  const mode = stringField(input, "mode") ?? stringField(payload, "mode");
  const body = compactJoin([
    "Switched workspace",
    path ?? workspaceId,
    mode && mode !== "direct" ? mode : undefined,
  ]);
  return { body, detail: undefined };
}

function projectEnqueueTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const input = asRecord(payload?.input);
  const text = stringField(input, "text") ?? stringField(payload, "text");
  const priority = stringField(input, "priority") ?? stringField(payload, "priority");
  const body = compactJoin([
    "Enqueued follow-up",
    priority && priority !== "normal" ? priority : undefined,
    text ? truncateText(text.replace(/\s+/g, " ").trim(), 100) : undefined,
  ]);
  return { body, detail: undefined };
}

function projectGenerateImageTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const input = asRecord(payload?.input);
  const result = asRecord(payload?.generate_image_result) ?? unwrapToolResult(payload);
  const prompt = stringField(input, "prompt") ?? stringField(payload, "prompt") ?? stringField(result, "prompt");
  const name = stringField(input, "name") ?? stringField(payload, "name") ?? stringField(result, "name");
  const size = stringField(input, "size") ?? stringField(payload, "size") ?? stringField(result, "size");
  const imageUri = firstStringField(result, ["image_uri", "uri", "path"]);
  const body = compactJoin([
    "Generated image",
    name ?? (imageUri ? basename(imageUri) : undefined),
    size,
    prompt ? truncateText(prompt, 80) : undefined,
  ]);
  return { body, detail: undefined };
}

function projectAgentGetTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const input = asRecord(payload?.input);
  const agentId = stringField(input, "agent_id") ?? stringField(payload, "agent_id");
  const result = unwrapToolResult(payload);
  const visibility = stringField(result, "visibility");
  const ownership = stringField(result, "ownership");
  const body = compactJoin([
    "Inspected agent",
    agentId ?? "self",
    visibility,
    ownership,
  ]);
  return { body, detail: undefined };
}

function projectListModelProvidersTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = unwrapToolResult(payload);
  const providers = arrayField(result, "providers") ?? arrayField(result, "model_providers");
  const count = providers?.length;
  const body = compactJoin([
    "Listed model providers",
    count != null ? `${count} provider${count === 1 ? "" : "s"}` : undefined,
  ]);
  return { body, detail: undefined };
}

function projectListProviderModelsTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const provider = stringField(payload, "provider");
  const result = unwrapToolResult(payload);
  const models = arrayField(result, "models") ?? arrayField(result, "provider_models");
  const count = models?.length;
  const body = compactJoin([
    "Listed provider models",
    provider,
    count != null ? `${count} model${count === 1 ? "" : "s"}` : undefined,
  ]);
  return { body, detail: undefined };
}

function projectWaitForTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const reason = stringField(payload, "reason");
  const wake = stringField(payload, "wake");
  const resource = stringField(payload, "resource");
  const recheckAfterMs = numberField(payload, "recheck_after_ms");
  const body = compactJoin([
    "Waiting",
    wake,
    resource ? truncateText(resource, 60) : undefined,
    reason ? truncateText(reason, 80) : undefined,
    recheckAfterMs != null ? `recheck in ${formatDuration(recheckAfterMs)}` : undefined,
  ]);
  return { body, detail: undefined };
}

function summarizeWorkItemRecords(items: unknown[] | undefined): string[] {
  return (items ?? [])
    .map(asRecord)
    .filter((item): item is Record<string, unknown> => Boolean(item))
    .map(summarizeWorkItemRecord)
    .filter(Boolean);
}

function summarizeTaskRecords(tasks: unknown[] | undefined): string[] {
  return (tasks ?? [])
    .map(asRecord)
    .filter((task): task is Record<string, unknown> => Boolean(task))
    .map((task) => {
      const command = firstStringField(asRecord(task.command), ["cmd_preview", "cmd"]);
      const retrieval = firstStringField(asRecord(task.retrieval), ["status", "output"]);
      return compactJoin([
        stringField(task, "summary") ?? command ?? stringField(task, "kind"),
        stringField(task, "status"),
        stringField(task, "kind"),
        firstStringField(task, ["task_id", "id"]),
        retrieval,
      ]);
    })
    .filter(Boolean);
}

function summarizeWorkItemRecord(record: Record<string, unknown> | undefined): string {
  const id = stringField(record, "id") ?? stringField(record, "work_item_id");
  const objective = firstStringField(record, ["objective", "objective_preview"]);
  const lifecycle = firstStringField(record, ["lifecycle", "state", "status"]);
  const planStatus = stringField(record, "plan_status");
  const readiness = stringField(record, "readiness");
  return compactJoin([objective, lifecycle, planStatus, readiness, id]);
}

function applyPatchDetailText(
  result: Record<string, unknown>,
  changedFiles: Record<string, unknown>[] | undefined,
  changedPaths: string[],
): string {
  if (changedFiles?.length) {
    return changedFiles
      .map((file) => {
        const path = stringField(file, "path") ?? "unknown path";
        const action = stringField(file, "action");
        const diffPreview = stringField(file, "diff_preview");
        const hunks = arrayField(file, "hunks");
        const hunkSummary = hunks?.length ? `${hunks.length} hunk${hunks.length === 1 ? "" : "s"}` : undefined;
        const header = compactJoin([action, path, hunkSummary]);
        return diffPreview ? `${header}\n${diffPreview}` : header;
      })
      .join("\n\n");
  }

  const patch = stringField(result, "patch") ?? stringField(result, "diff_preview");
  if (patch) return patch;
  return changedPaths.length ? changedPaths.join("\n") : "";
}

function toolExecutionDetail(
  toolName: string,
  payload: Record<string, unknown> | undefined,
  commandPreview: string | undefined,
  outputPreview: string | undefined,
  summary: string | undefined,
  error: string | undefined,
): AgentTimelineItemDetail | undefined {
  if (toolName === "ExecCommandBatch") {
    const batchDetail = commandBatchDetail(payload);
    if (batchDetail) return { label: "Commands", text: batchDetail, tone: "command" };
  }

  if (error) return { label: "Error", text: error, tone: "data" };
  if (commandPreview && outputPreview) return { label: "Output", text: outputPreview, tone: "command" };
  if (commandPreview) {
    return { label: toolName === "ExecCommandBatch" ? "Commands" : "Command", text: commandPreview, tone: "command" };
  }
  if (outputPreview) return { label: "Output", text: outputPreview, tone: "output" };
  const readable = readableText(payload);
  const readableDuplicatesSummary = Boolean(readable && summary && normalizeTextKey(readable) === normalizeTextKey(summary));
  const detailText = readable && !readableDuplicatesSummary ? readable : undefined;
  return detailText ? { label: "Result", text: detailText, tone: "data" } : undefined;
}

function commandBatchDetail(payload: Record<string, unknown> | undefined): string | undefined {
  const batchItems = arrayField(payload, "exec_command_batch_items");
  if (!batchItems?.length) return undefined;

  const resultItems = arrayField(asRecord(payload?.exec_command_result), "items") ?? [];
  const lines = batchItems
    .map((item, index) => {
      const itemRecord = asRecord(item);
      const command = firstStringField(itemRecord, ["cmd_display", "cmd"]);
      if (!command) return undefined;
      const resultRecord = asRecord(resultItems[index]);
      return formatBatchCommandLine(index, command, resultRecord);
    })
    .filter((line): line is string => Boolean(line));

  return lines.length ? lines.join("\n") : undefined;
}

function formatBatchCommandLine(index: number, command: string, resultItem: Record<string, unknown> | undefined): string {
  const status = stringField(resultItem, "status");
  const result = asRecord(resultItem?.result);
  const exitStatus = numberField(result, "exit_status");
  const outputPreview = firstStringField(result, ["stdout_preview", "stderr_preview", "output_preview"]);
  const statusText = compactJoin([status, exitStatus == null ? undefined : `exit ${exitStatus}`]);
  const headline = `${index + 1}. ${statusText ? `[${statusText}] ` : ""}${command}`;
  if (!outputPreview) return headline;
  return `${headline}\n   ${indentPreview(outputPreview)}`;
}

function indentPreview(value: string): string {
  return value
    .trim()
    .split("\n")
    .slice(0, 6)
    .join("\n   ");
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function truncateText(value: string, maxLength: number): string {
  return value.length > maxLength ? `${value.slice(0, maxLength - 1)}…` : value;
}

function execCommandPreview(payload: Record<string, unknown> | undefined): string | undefined {
  const direct = firstStringField(payload, ["exec_command_display", "cmd_display"]);
  if (direct) return direct;

  const batchItems = arrayField(payload, "exec_command_batch_items");
  if (batchItems?.length) {
    const commands = batchItems
      .map((item) => {
        const record = asRecord(item);
        return firstStringField(record, ["cmd_display", "cmd"]);
      })
      .filter((command): command is string => Boolean(command));
    if (commands.length) return commands.join("\n");
  }

  const result = asRecord(payload?.exec_command_result);
  const resultItems = arrayField(result, "items");
  if (resultItems?.length) {
    const commands = resultItems
      .map((item) => {
        const record = asRecord(item);
        const itemResult = asRecord(record?.result);
        return (
          firstStringField(itemResult, ["cmd_display", "cmd", "cmd_preview"]) ??
          firstStringField(record, ["cmd_display", "cmd", "cmd_preview"])
        );
      })
      .filter((command): command is string => Boolean(command));
    if (commands.length) return commands.join("\n");
  }

  return (
    firstStringField(result, ["cmd_display", "cmd"]) ??
    firstStringField(payload, ["exec_command_cmd", "cmd", "cmd_preview"]) ??
    firstStringField(asRecord(payload?.command_cost), ["cmd_preview"]) ??
    firstStringField(asRecord(payload?.exec_command_cost), ["cmd_preview"])
  );
}

function commandOutputPreview(payload: Record<string, unknown> | undefined): string | undefined {
  const result = asRecord(payload?.exec_command_result);
  return (
    firstStringField(payload, ["stdout_preview", "stderr_preview", "output_preview"]) ??
    firstStringField(result, ["stdout_preview", "stderr_preview", "output_preview", "summary_text"])
  );
}

function toolFriendlyLabel(toolName: string, failed: boolean): string {
  if (toolName === "ApplyPatch") return failed ? "Patch failed" : "Applied patch";
  if (toolName === "ExecCommand") return failed ? "Command failed" : "Command finished";
  if (toolName === "ExecCommandBatch") return failed ? "Command batch failed" : "Command batch finished";
  if (toolName === "WaitFor") return failed ? "Wait failed" : "Waiting";
  if (toolName === "UpdateWorkItem") return failed ? "Work item update failed" : "Updated work item";
  if (toolName === "PickWorkItem") return failed ? "Work item switch failed" : "Picked work item";
  if (toolName === "CompleteWorkItem") return failed ? "Work item completion failed" : "Completed work item";
  if (isWebSearchTool(toolName)) return failed ? "Web search failed" : "Web search completed";
  if (isWebFetchTool(toolName)) return failed ? "Web fetch failed" : "Web fetch completed";
  if (toolName === "TaskOutput") return failed ? "Task output failed" : "Task output";
  if (toolName === "TaskStatus") return failed ? "Task status failed" : "Task status";
  if (toolName === "TaskStop") return failed ? "Task stop failed" : "Stopped task";
  if (toolName === "TaskInput") return failed ? "Task input failed" : "Task input";
  if (toolName === "SpawnAgent") return failed ? "Agent spawn failed" : "Spawned agent";
  if (toolName === "UseWorkspace") return failed ? "Workspace switch failed" : "Switched workspace";
  if (toolName === "Enqueue") return failed ? "Enqueue failed" : "Enqueued follow-up";
  if (toolName === "GenerateImage") return failed ? "Image generation failed" : "Generated image";
  if (toolName === "AgentGet") return failed ? "Agent inspection failed" : "Inspected agent";
  if (toolName === "ListModelProviders") return failed ? "Provider list failed" : "Listed model providers";
  if (toolName === "ListProviderModels") return failed ? "Model list failed" : "Listed provider models";
  return failed ? "Tool failed" : "Tool finished";
}

function formatDuration(milliseconds: number): string {
  if (milliseconds < 1000) return `${milliseconds}ms`;
  return `${(milliseconds / 1000).toFixed(milliseconds < 10_000 ? 1 : 0)}s`;
}

function summarizeWorkItemEvent(eventType: string, payload: Record<string, unknown> | undefined): string {
  const action = stringField(payload, "action") ?? eventType.replace(/^work_item_/, "");
  const objective = workItemObjective(payload);
  const workItemId = workItemObjectId(payload);
  const reason = stringField(payload, "reason");
  const state = workItemState(payload);
  if (eventType === "work_item_picked") {
    return compactJoin(["Picked work item", objective, reason, state]);
  }
  if (eventType === "work_item_focus_released") {
    return compactJoin(["Released work item focus", objective, reason, stringField(payload, "readiness")]);
  }
  if (eventType === "work_item_completion_report_promoted") {
    return compactJoin(["Promoted completion report", objective, stringField(payload, "text_preview")]);
  }
  if (eventType === "work_item_completion_report_candidate_promoted") {
    return compactJoin(["Promoted completion report candidate", objective, stringField(payload, "text_preview")]);
  }
  return compactJoin([humanizeEventType(`work_item_${action}`), objective, state, objective ? undefined : workItemId]);
}

export function projectTaskLifecycleEvent(
  eventType: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> {
  const status = firstStringField(payload, ["task_status", "status"]);
  const summary = stringField(payload, "summary");
  const outputPreview = stringField(payload, "output_summary_preview");
  const error = stringField(payload, "error");
  const taskId = stringField(payload, "task_id");
  const exitStatus = numberField(payload, "exit_status");
  const outputPath = stringField(payload, "output_path");
  const label = taskLifecycleLabel(eventType, status);
  const body = compactJoin([
    summary || taskId,
    status && !label.toLowerCase().includes(status.toLowerCase()) ? status : undefined,
    exitStatus == null ? undefined : `exit ${exitStatus}`,
    error,
    outputPreview,
  ]) || humanizeEventType(eventType);
  const detailText = compactJoin([
    taskId ? `task: ${taskId}` : undefined,
    outputPath ? `output: ${outputPath}` : undefined,
    outputPreview,
  ]);

  return {
    kind: "event",
    label,
    body,
    minDisplayLevel: error || isFailedTaskStatus(status) ? "info" : "verbose",
    detail: detailText ? { label: "Task details", text: detailText, tone: outputPreview ? "output" : "data" } : undefined,
  };
}

function taskLifecycleLabel(eventType: string, status: string | undefined): string {
  if (eventType === "task_created") return "Task queued";
  if (eventType === "task_result_received") {
    if (status === "completed") return "Task completed";
    if (status === "failed") return "Task failed";
    if (status === "cancelled") return "Task cancelled";
    if (status === "interrupted") return "Task interrupted";
    return "Task result received";
  }
  if (status === "running") return "Task running";
  if (status === "cancelling") return "Task cancelling";
  if (status === "completed") return "Task completed";
  if (status === "failed") return "Task failed";
  if (status === "cancelled") return "Task cancelled";
  if (status === "interrupted") return "Task interrupted";
  return "Task updated";
}

function isFailedTaskStatus(status: string | undefined): boolean {
  return status === "failed" || status === "cancelled" || status === "interrupted";
}

function summarizeDebugEvent(eventType: string, payload: Record<string, unknown> | undefined): string {
  const model = stringField(payload, "active_model");
  const stopReason = stringField(payload, "stop_reason");
  const tokens = numberField(payload, "input_tokens") == null && numberField(payload, "output_tokens") == null
    ? undefined
    : `${numberField(payload, "input_tokens") ?? 0} in / ${numberField(payload, "output_tokens") ?? 0} out`;
  return compactJoin([humanizeEventType(eventType), model, stopReason, tokens]);
}

function summarizeSystemRuntimeEvent(eventType: string, payload: Record<string, unknown> | undefined): string {
  if (eventType === "turn_local_checkpoint_resume_requested") {
    return "Refreshing local context; asking the model to continue.";
  }
  if (eventType === "turn_local_checkpoint_requested") {
    return compactJoin(["Context checkpoint requested", stringField(payload, "checkpoint_mode")]);
  }
  if (eventType === "turn_local_checkpoint_recorded") {
    if (payload?.checkpoint_recorded === false) return "Context checkpoint produced no visible text.";
    return compactJoin(["Context checkpoint recorded", stringField(payload, "text_preview")]);
  }
  if (eventType === "continuation_trigger_received") {
    return readableText(payload) || "Continuation trigger received.";
  }
  if (eventType === "continuation_resolved") {
    return readableText(payload) || "Continuation resolved.";
  }
  if (eventType === "closure_decided") {
    return readableText(asRecord(payload?.closure) ?? payload) || "Closure decided.";
  }
  return readableText(payload) || humanizeEventType(eventType);
}

function systemRuntimeLabel(eventType: string): string {
  if (eventType.startsWith("turn_local_checkpoint_")) return "Context checkpoint";
  if (eventType === "turn_started") return "Turn started";
  if (eventType.startsWith("continuation_")) return "Continuation";
  if (eventType === "closure_decided") return "Closure";
  return humanizeEventType(eventType);
}

function turnTriggerLabel(messageKind: string): string | undefined {
  switch (messageKind) {
    case "OperatorPrompt": return "operator";
    case "InternalFollowup": return "internal followup";
    case "SystemTick": return "system tick";
    case "TimerTick": return "timer";
    case "CallbackEvent": return "callback";
    case "WebhookEvent": return "webhook";
    case "ChannelEvent": return "channel event";
    case "TaskResultContinuation": return "task result";
    default: return undefined;
  }
}

function messageEnvelopeProjection(
  payload: Record<string, unknown> | undefined,
  messagesById?: Record<string, RuntimeMessageEnvelope>,
): { origin: "operator" | "runtime"; body: string } | undefined {
  if (!payload) return undefined;
  const source = hydratedMessageForPayload(payload, messagesById) ?? payload;
  const origin = asRecord(source.origin);
  const originKind = stringField(origin, "kind")?.toLowerCase();
  const body = asRecord(source.body);
  return {
    origin: originKind === "operator" ? "operator" : "runtime",
    body: messageBodyText(body),
  };
}

function hydratedMessageForPayload(
  payload: Record<string, unknown>,
  messagesById: Record<string, RuntimeMessageEnvelope> | undefined,
): RuntimeMessageEnvelope | undefined {
  const messageId = stringField(payload, "message_id");
  return messageId ? messagesById?.[messageId] : undefined;
}

function transcriptTextForPayload(
  payload: Record<string, unknown> | undefined,
  transcriptEntriesById: Record<string, RuntimeTranscriptEntry> | undefined,
): string | undefined {
  const entryId = transcriptEntryIdForPayload(payload);
  const entry = entryId ? transcriptEntriesById?.[entryId] : undefined;
  return transcriptEntryText(entry);
}

function briefTextForPayload(
  payload: Record<string, unknown> | undefined,
  briefRecordsById: Record<string, RuntimeBriefRecord> | undefined,
): string | undefined {
  const briefId = briefIdForPayload(payload);
  const text = briefId ? briefRecordsById?.[briefId]?.text : undefined;
  return text && text.trim() ? text : undefined;
}

export function briefIdForPayload(payload: Record<string, unknown> | undefined): string | undefined {
  return stringField(payload, "brief_id") ?? stringField(payload, "id");
}

export function transcriptEntryIdForPayload(payload: Record<string, unknown> | undefined): string | undefined {
  const directId = stringField(payload, "assistant_round_id") ?? stringField(payload, "finalizes_assistant_round_id");
  if (directId) return directId;
  const contentSource = asRecord(payload?.content_source);
  return stringField(contentSource, "entry_id");
}

function transcriptEntryText(entry: RuntimeTranscriptEntry | undefined): string | undefined {
  const data = asRecord(entry?.data);
  const text = stringField(data, "text");
  if (text) return text;
  const blocks = Array.isArray(data?.blocks) ? data.blocks : [];
  const parts = blocks.flatMap((block) => {
    const record = asRecord(block);
    if (stringField(record, "type") !== "text") return [];
    return stringField(record, "text") ?? stringField(record, "content") ?? [];
  });
  return compactJoin(parts);
}

function messageBodyText(body: Record<string, unknown> | undefined): string {
  if (!body) return "";
  const text = stringField(body, "text");
  if (text) return text;
  const message = stringField(body, "message");
  if (message) return message;
  if (body.value != null) return debugJson(body.value);
  return "";
}

function readableText(value: unknown): string {
  if (typeof value === "string") return value;
  const record = asRecord(value);
  if (!record) return "";

  for (const key of ["text", "content", "summary", "brief", "message", "reason", "text_preview"]) {
    const candidate = record[key];
    if (typeof candidate === "string" && candidate.trim()) return candidate;
  }

  return "";
}

function readableTextWithoutSummary(value: unknown): string {
  if (typeof value === "string") return value;
  const record = asRecord(value);
  if (!record) return "";

  for (const key of ["text", "content", "brief", "message", "reason", "text_preview"]) {
    const candidate = record[key];
    if (typeof candidate === "string" && candidate.trim()) return candidate;
  }

  return "";
}

function readableEventFacts(payload: Record<string, unknown> | undefined): string[] {
  if (!payload) return [];
  const facts = new Map<string, string>();
  collectReadableEventFacts(payload, facts);
  return Array.from(facts.entries())
    .map(([key, value]) => `${humanizeEventType(key)}: ${truncateText(value, 240)}`)
    .slice(0, 12);
}

function collectReadableEventFacts(value: Record<string, unknown>, facts: Map<string, string>, prefix = "", depth = 0): void {
  const preferredKeys = [
    "summary",
    "summary_text",
    "text_preview",
    "output_preview",
    "stdout_preview",
    "stderr_preview",
    "diff_preview",
    "message",
    "reason",
    "status",
    "priority",
    "objective",
    "objective_preview",
    "work_item_id",
    "current_work_item_id",
    "task_id",
    "task_status",
    "output_summary_preview",
    "agent_id",
    "turn_id",
    "run_id",
    "active_model",
    "stop_reason",
  ];

  for (const key of preferredKeys) {
    const factKey = prefix ? `${prefix}_${key}` : key;
    const candidate = value[key];
    if (typeof candidate === "string" && candidate.trim()) facts.set(factKey, candidate.trim());
    if (typeof candidate === "number" || typeof candidate === "boolean") facts.set(factKey, String(candidate));
  }

  if (depth >= 2 || facts.size >= 12) return;

  for (const [key, candidate] of Object.entries(value)) {
    if (facts.size >= 12) return;
    const record = asRecord(candidate);
    if (!record) continue;
    collectReadableEventFacts(record, facts, prefix ? `${prefix}_${key}` : key, depth + 1);
  }
}

function toolErrorMessage(payload: Record<string, unknown> | undefined): string | undefined {
  const direct = stringField(payload, "error");
  if (direct) return structuredErrorMessage(direct) ?? direct;

  const error = payload?.error;
  if (typeof error === "string" && error.trim()) return error;

  const errorRecord = asRecord(error) ?? asRecord(payload?.tool_error);
  const message = firstStringField(errorRecord, ["message", "summary", "summary_text", "reason", "detail"]);
  if (message) return message;

  const nested = firstStringField(asRecord(errorRecord?.error), ["message", "summary", "summary_text", "reason", "detail"]);
  if (nested) return nested;

  return undefined;
}

function structuredErrorMessage(value: string): string | undefined {
  const text = value.trim();
  if (!text.startsWith("{") && !text.startsWith("[")) return undefined;

  try {
    const record = asRecord(JSON.parse(text));
    return firstStringField(record, ["message", "summary", "summary_text", "reason", "detail"]);
  } catch {
    return undefined;
  }
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value != null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined;
}

function stringField(value: Record<string, unknown> | undefined, key: string): string | undefined {
  const candidate = value?.[key];
  return typeof candidate === "string" && candidate.trim() ? candidate : undefined;
}

function cleanStringField(value: Record<string, unknown> | undefined, key: string): string | undefined {
  const candidate = stringField(value, key);
  if (!candidate) return undefined;
  const normalized = candidate.trim().toLowerCase();
  return normalized === "none" || normalized === "null" ? undefined : candidate;
}

function firstStringField(value: Record<string, unknown> | undefined, keys: string[]): string | undefined {
  for (const key of keys) {
    const candidate = stringField(value, key);
    if (candidate) return candidate;
  }
  return undefined;
}

function stringArrayField(value: Record<string, unknown> | undefined, key: string): string[] {
  const candidate = value?.[key];
  if (!Array.isArray(candidate)) return [];
  return candidate.filter((item): item is string => typeof item === "string" && Boolean(item.trim()));
}

function numberField(value: Record<string, unknown> | undefined, key: string): number | undefined {
  const candidate = value?.[key];
  return typeof candidate === "number" ? candidate : undefined;
}

function arrayField(value: Record<string, unknown> | undefined, key: string): unknown[] | undefined {
  const candidate = value?.[key];
  return Array.isArray(candidate) ? candidate : undefined;
}

function humanizeEventType(value: string): string {
  return value
    .replace(/_/g, " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function compactJoin(parts: Array<string | undefined | null>): string {
  return parts.filter(Boolean).join(" · ");
}

function normalizeTextKey(text: string): string {
  return text.trim().replace(/\s+/g, " ");
}

function sortableTime(value: string): number {
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function debugJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
