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
import { projectRuntimeEvent, projectToolExecution } from "./session-reducer-core";

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
    return renderToolExecutionObject(obj);
  }

  const projection = projectRuntimeEvent(
    obj.render.eventType,
    obj.render.payload,
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
    timestamp: projection.timestamp || obj.render.timestamp,
    meta: obj.render.meta,
    minDisplayLevel: projection.minDisplayLevel,
    sourceIds: obj.sourceEventIds,
    relatedStateObjectRef: relatedStateObjectRefFor(obj),
    detail: projection.detail,
    rawEvent: obj.render.rawEvent,
    debug: obj.render.debug,
  };
}

function renderWorkItemObject(obj: WorkItemObject, ctx: RenderContext): AgentTimelineItem {
  return {
    id: obj.id,
    kind: "system" as const,
    label: "Work item",
    body: obj.objective || "Work item",
    timestamp: obj.render.timestamp,
    meta: obj.render.meta,
    minDisplayLevel: "verbose" as const,
    sourceIds: obj.sourceEventIds,
    stateObjectRef: {
      kind: "work_item",
      id: obj.id,
      objective: obj.objective,
      state: obj.state,
    },
    activities: renderWorkItemActivities(obj, ctx),
    rawEvent: obj.render.rawEvent,
    debug: obj.render.debug,
  };
}

function renderToolExecutionObject(obj: ToolExecutionObject): AgentTimelineItem | undefined {
  const projection = projectToolExecution(
    obj.render.eventType,
    obj.render.payload,
  );
  if (!projection) return undefined;

  return {
    id: obj.id,
    kind: projection.kind,
    label: projection.label,
    body: projection.body,
    timestamp: obj.render.timestamp,
    meta: obj.render.meta,
    minDisplayLevel: projection.minDisplayLevel,
    sourceIds: obj.sourceEventIds,
    stateObjectRef: {
      kind: "tool_execution",
      id: obj.id,
      toolName: obj.toolName,
      status: obj.status,
    },
    relatedStateObjectRef: obj.relatedStateObjectRef,
    detail: projection.detail,
    rawEvent: obj.render.rawEvent,
    debug: obj.render.debug,
  };
}

function renderTaskObject(obj: TaskObject, ctx: RenderContext): AgentTimelineItem {
  const summary = obj.summary || "Task";
  return {
    id: obj.id,
    kind: "tool" as const,
    label: taskStatusLabel(obj.status),
    body: summary,
    timestamp: obj.render.timestamp,
    meta: obj.render.meta,
    minDisplayLevel: isFailedTaskStatus(obj.status) ? ("info" as const) : ("verbose" as const),
    sourceIds: obj.sourceEventIds,
    stateObjectRef: {
      kind: "task",
      id: obj.id,
      status: obj.status,
      summary: obj.summary,
    },
    activities: renderTaskActivities(obj, ctx),
    rawEvent: obj.render.rawEvent,
    debug: obj.render.debug,
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
  // RuntimeActivityObject also carries work_item_ event types but has its own
  // `eventType` field; exclude it so only true WorkItemObjects match.
  return obj.render.eventType.startsWith("work_item_") && !("eventType" in obj);
}

function isTaskObject(obj: DomainObject): obj is TaskObject {
  return obj.render.eventType === "task_created" || obj.render.eventType === "task_status_updated" || obj.render.eventType === "task_result_received";
}

function isToolExecutionObject(obj: DomainObject): obj is ToolExecutionObject {
  return obj.render.eventType === "tool_executed" || obj.render.eventType === "tool_execution_failed";
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
  const projection = projectRuntimeEvent(
    activity.render.eventType,
    activity.render.payload,
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
    timestamp: projection.timestamp || activity.render.timestamp,
    meta: activity.render.meta,
    minDisplayLevel: projection.minDisplayLevel,
    sourceIds: activity.sourceEventIds,
    relatedStateObjectRef: activity.relatedStateObjectRef,
    detail: projection.detail,
    rawEvent: activity.render.rawEvent,
    debug: activity.render.debug,
  };
}

function isRuntimeActivityObject(obj: DomainObject): obj is RuntimeActivityObject {
  return "eventType" in obj;
}
