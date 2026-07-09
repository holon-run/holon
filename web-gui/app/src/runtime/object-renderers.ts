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
import type { DomainObject, RuntimeActivityObject, WorkItemObject } from "./session-object-types";
import type { RenderContext } from "./timeline-view-model";
import { projectRuntimeEvent } from "./session-reducer-core";

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
  // WorkItem StateObjects render from their own fields (objective, state),
  // not from the last event's projection. This produces one stable card per
  // work item, with lifecycle events shown as activities underneath.
  if (isWorkItemObject(obj)) {
    return renderWorkItemObject(obj, ctx);
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

function isWorkItemObject(obj: DomainObject): obj is WorkItemObject {
  // RuntimeActivityObject also carries work_item_ event types but has its own
  // `eventType` field; exclude it so only true WorkItemObjects match.
  return obj.render.eventType.startsWith("work_item_") && !("eventType" in obj);
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
