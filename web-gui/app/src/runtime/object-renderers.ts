/**
 * Specialized renderer for domain objects.
 *
 * Maps domain objects from the normalized {@link SessionState} to display-ready
 * {@link AgentTimelineItem}s. Delegates event-type-specific formatting
 * (body, label, detail, display level) to the shared projection function
 * {@link projectRuntimeEvent}, and wraps the result with domain-object
 * metadata (source IDs, raw event, timestamp, debug info).
 */

import type { AgentTimelineItem, TimelineStateObjectRef } from "./types";
import type { DomainObject, WorkItemObject } from "./session-object-types";
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
    stateObjectRef: stateObjectRefFor(obj),
    detail: projection.detail,
    rawEvent: obj.render.rawEvent,
    debug: obj.render.debug,
  };
}

function stateObjectRefFor(obj: DomainObject): TimelineStateObjectRef | undefined {
  if (isWorkItemObject(obj)) {
    return {
      kind: "work_item",
      id: obj.id,
      objective: obj.objective,
      state: obj.state,
    };
  }
  return undefined;
}

function isWorkItemObject(obj: DomainObject): obj is WorkItemObject {
  return obj.render.eventType.startsWith("work_item_");
}
