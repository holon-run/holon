/**
 * Specialized renderer for domain objects.
 *
 * Maps domain objects from the normalized {@link SessionState} to display-ready
 * {@link AgentTimelineItem}s. Delegates event-type-specific formatting
 * (body, label, detail, display level) to the shared projection function
 * {@link projectRuntimeEvent}, and wraps the result with domain-object
 * metadata (source IDs, raw event, timestamp, debug info).
 */

import type { AgentTimelineActivity, AgentTimelineItem, TimelineStateObjectRef } from "./types";
import type { DomainObject, RuntimeActivityObject, TaskObject, ToolExecutionObject, WorkItemObject } from "./session-object-types";
import type { RenderContext } from "./timeline-view-model";
import { eventMeta, projectRuntimeEvent, projectToolExecution } from "./session-reducer-core";

/**
 * Render any domain object into a display-ready timeline item.
 *
 * Calls {@link projectRuntimeEvent} for body/label/detail/kind formatting,
 * then wraps the result with metadata from the domain object (source IDs,
 * raw event, timestamp, debug info). Returns undefined when the projection
 * produces no visible item (e.g. hidden work-item mutation tools, assistant
 * rounds without transcript text).
 */
export function renderDomainObject(
  obj: DomainObject,
  ctx: RenderContext,
): AgentTimelineItem | undefined {
  const event = ctx.eventsById[obj.primaryEventId];
  if (!event) return undefined;
  const eventType = event.type ?? "runtime_event";
  const payload = asRecord(event.payload);

  // Specialized renderers: objects with stable identity that aggregate
  // multiple events render from their own fields, producing one stable card
  // with lifecycle events shown as activities underneath.
  if (isWorkItemObject(obj)) {
    return renderWorkItemObject(obj, ctx);
  }
  if (isTaskObject(obj)) {
    return renderTaskObject(obj, ctx);
  }
  if (isToolExecutionObject(obj)) {
    return renderToolExecutionObject(obj, ctx);
  }

  const projection = projectRuntimeEvent(
    eventType,
    payload,
    ctx.messagesById,
    ctx.transcriptEntriesById,
    ctx.briefRecordsById,
  );
  if (!projection) return undefined;

  return {
    id: obj.id,
    kind: projection.kind,
    label: projection.label,
    body: projection.body,
    timestamp: projection.timestamp || event.ts || obj.updatedAt,
    meta: eventMeta(eventType, payload, event.event_seq),
    minDisplayLevel: projection.minDisplayLevel,
    sourceIds: orderedSourceEventIds(obj, ctx),
    relatedStateObjectRef: relatedStateObjectRefFor(obj),
    detail: projection.detail,
    rawEvent: event,
    debug: ctx.includeDebug ? JSON.stringify(event, null, 2) : undefined,
  };
}

function renderWorkItemObject(obj: WorkItemObject, ctx: RenderContext): AgentTimelineItem {
  const event = ctx.eventsById[obj.primaryEventId];
  const eventType = event?.type ?? "runtime_event";
  const payload = asRecord(event?.payload);
  return {
    id: obj.id,
    kind: "system" as const,
    label: "Work item",
    body: obj.objective || "Work item",
    timestamp: event?.ts ?? obj.updatedAt,
    meta: eventMeta(eventType, payload, event?.event_seq),
    minDisplayLevel: "verbose" as const,
    sourceIds: orderedSourceEventIds(obj, ctx),
    stateObjectRef: {
      kind: "work_item",
      id: obj.id,
      objective: obj.objective,
      state: obj.state,
    },
    activities: renderWorkItemActivities(obj, ctx),
    rawEvent: event,
    debug: ctx.includeDebug && event ? JSON.stringify(event, null, 2) : undefined,
  };
}

function renderToolExecutionObject(obj: ToolExecutionObject, ctx: RenderContext): AgentTimelineItem | undefined {
  const event = ctx.eventsById[obj.primaryEventId];
  if (!event) return undefined;
  const eventType = event.type ?? "runtime_event";
  const payload = asRecord(event.payload);
  const projection = projectToolExecution(
    eventType,
    payload,
  );
  if (!projection) return undefined;

  return {
    id: obj.id,
    kind: projection.kind,
    label: projection.label,
    body: projection.body,
    timestamp: event.ts ?? obj.updatedAt,
    meta: eventMeta(eventType, payload, event.event_seq),
    minDisplayLevel: projection.minDisplayLevel,
    sourceIds: orderedSourceEventIds(obj, ctx),
    stateObjectRef: {
      kind: "tool_execution",
      id: obj.id,
      toolName: obj.toolName,
      status: obj.status,
    },
    relatedStateObjectRef: obj.relatedStateObjectRef,
    detail: projection.detail,
    rawEvent: event,
    debug: ctx.includeDebug ? JSON.stringify(event, null, 2) : undefined,
  };
}

function renderTaskObject(obj: TaskObject, ctx: RenderContext): AgentTimelineItem {
  const event = ctx.eventsById[obj.primaryEventId];
  const eventType = event?.type ?? "runtime_event";
  const payload = asRecord(event?.payload);
  const summary = obj.summary || "Task";
  return {
    id: obj.id,
    kind: "tool" as const,
    label: taskStatusLabel(obj.initialStatus ?? obj.status),
    body: summary,
    timestamp: event?.ts ?? obj.updatedAt,
    meta: eventMeta(eventType, payload, event?.event_seq),
    minDisplayLevel: isFailedTaskStatus(obj.status) ? ("info" as const) : ("verbose" as const),
    sourceIds: orderedSourceEventIds(obj, ctx),
    stateObjectRef: {
      kind: "task",
      id: obj.id,
      status: obj.status,
      summary: obj.summary,
    },
    activities: renderTaskActivities(obj, ctx),
    rawEvent: event,
    debug: ctx.includeDebug && event ? JSON.stringify(event, null, 2) : undefined,
  };
}

function taskStatusLabel(status: string): string {
  switch (status) {
    case "queued":
    case "created": return "Task queued";
    case "running": return "Task running";
    case "cancelling": return "Task cancelling";
    case "completed": return "Task completed";
    case "failed": return "Task failed";
    case "cancelled": return "Task cancelled";
    case "interrupted": return "Task interrupted";
    default: return "Task";
  }
}

function isFailedTaskStatus(status: string): boolean {
  return status === "failed" || status === "cancelled" || status === "interrupted";
}

function renderTaskActivities(obj: TaskObject, ctx: RenderContext): AgentTimelineItem["activities"] {
  const activityIds = obj.activityIds ?? [];
  if (!activityIds.length || !ctx.activitiesById) return undefined;
  const activities = activityIds
    .map((activityId) => ctx.activitiesById?.get(activityId))
    .filter((activity): activity is RuntimeActivityObject => Boolean(activity))
    .map((activity) => renderRuntimeActivity(activity, ctx))
    .filter((activity): activity is NonNullable<ReturnType<typeof renderRuntimeActivity>> => Boolean(activity));
  return activities.length ? activities : undefined;
}

function isWorkItemObject(obj: DomainObject): obj is WorkItemObject {
  return "objective" in obj && !("eventType" in obj);
}

function isTaskObject(obj: DomainObject): obj is TaskObject {
  return "initialStatus" in obj;
}

function isToolExecutionObject(obj: DomainObject): obj is ToolExecutionObject {
  return "toolName" in obj;
}

function relatedStateObjectRefFor(obj: DomainObject): TimelineStateObjectRef | undefined {
  if (isRuntimeActivityObject(obj)) return obj.relatedStateObjectRef;
  return undefined;
}

function renderWorkItemActivities(obj: WorkItemObject, ctx: RenderContext): AgentTimelineItem["activities"] {
  const activityIds = obj.activityIds ?? [];
  if (!activityIds.length || !ctx.activitiesById) return undefined;
  const activities = activityIds
    .map((activityId) => ctx.activitiesById?.get(activityId))
    .filter((activity): activity is RuntimeActivityObject => Boolean(activity))
    .map((activity) => renderRuntimeActivity(activity, ctx))
    .filter((activity): activity is NonNullable<ReturnType<typeof renderRuntimeActivity>> => Boolean(activity));
  return activities.length ? activities : undefined;
}

function renderRuntimeActivity(activity: RuntimeActivityObject, ctx: RenderContext): AgentTimelineActivity | undefined {
  const event = ctx.eventsById[activity.primaryEventId];
  if (!event) return undefined;
  const payload = asRecord(event.payload);
  const projection = projectRuntimeEvent(
    activity.eventType,
    payload,
    ctx.messagesById,
    ctx.transcriptEntriesById,
    ctx.briefRecordsById,
  );
  if (!projection) return undefined;

  return {
    id: activity.id,
    kind: projection.kind,
    label: projection.label,
    body: projection.body,
    timestamp: projection.timestamp || event.ts || activity.updatedAt,
    meta: eventMeta(activity.eventType, payload, event.event_seq),
    minDisplayLevel: projection.minDisplayLevel,
    sourceIds: orderedSourceEventIds(activity, ctx),
    relatedStateObjectRef: activity.relatedStateObjectRef,
    detail: projection.detail,
    rawEvent: event,
    debug: ctx.includeDebug ? JSON.stringify(event, null, 2) : undefined,
  };
}

function isRuntimeActivityObject(obj: DomainObject): obj is RuntimeActivityObject {
  return "eventType" in obj;
}

function orderedSourceEventIds(obj: DomainObject, ctx: RenderContext): string[] {
  return [...obj.sourceEventIds].sort((leftId, rightId) => {
    const left = ctx.eventsById[leftId];
    const right = ctx.eventsById[rightId];
    const leftSeq = left?.event_seq;
    const rightSeq = right?.event_seq;
    if (leftSeq != null && rightSeq != null && leftSeq !== rightSeq) return leftSeq - rightSeq;
    return leftId.localeCompare(rightId);
  });
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}
