import type { AgentTimelineItem, DisplayLevel, RuntimeMessageEnvelope, RuntimeBriefRecord, RuntimeTranscriptEntry } from "./types";
import type { SessionState } from "./session-state-reducer";
import type { DomainObject, InsertionEntry } from "./session-object-types";
import { compactAgentTimelineItems } from "./timeline-display";
import { projectRuntimeEvent, eventProjectionDisplayLevel } from "./session-reducer-core";

/**
 * Context passed to the render layer. Contains external lookup tables
 * (reference data, not state changes) and display configuration that the
 * renderers need to produce display-ready items.
 */
export interface RenderContext {
  eventDisplayLevel: DisplayLevel;
  includeDebug: boolean;
  messagesById?: Record<string, RuntimeMessageEnvelope>;
  transcriptEntriesById?: Record<string, RuntimeTranscriptEntry>;
  briefRecordsById?: Record<string, RuntimeBriefRecord>;
}

/**
 * Derive the final ordered {@link AgentTimelineItem[]} timeline from a
 * {@link SessionState}.
 *
 * Iterates `insertionOrder`, looks up each domain object, and renders it
 * into an {@link AgentTimelineItem} using the projection functions.
 * Events that produce no projection (e.g. hidden work-item mutation tools,
 * assistant rounds without transcript text) are silently skipped.
 */
export function deriveTimelineView(state: SessionState, ctx: RenderContext): AgentTimelineItem[] {
  const items: AgentTimelineItem[] = [];
  for (const entry of state.insertionOrder) {
    const obj = lookupObject(state, entry);
    if (!obj) continue;
    const item = renderObject(obj, ctx);
    if (item) items.push(item);
  }
  const sorted = items.sort(
    (left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp),
  );
  return compactAgentTimelineItems(sorted);
}

function lookupObject(state: SessionState, entry: InsertionEntry): DomainObject | undefined {
  switch (entry.objectType) {
    case "message": return state.messages.get(entry.id);
    case "tool_execution": return state.toolExecutions.get(entry.id);
    case "task": return state.tasks.get(entry.id);
    case "work_item": return state.workItems.get(entry.id);
    case "assistant_round": return state.rounds.get(entry.id);
    case "activity": return state.activitiesById.get(entry.id);
  }
}

/**
 * Render a single domain object into a display-ready {@link AgentTimelineItem}.
 *
 * Calls {@link projectRuntimeEvent} with the stored event payload and external
 * lookup tables to produce the display fields (kind, label, body, etc.).
 * Returns `undefined` when the event type produces no visible projection.
 */
function renderObject(obj: DomainObject, ctx: RenderContext): AgentTimelineItem | undefined {
  const projection = projectRuntimeEvent(
    obj.render.eventType,
    obj.render.payload,
    ctx.messagesById,
    ctx.transcriptEntriesById,
    ctx.briefRecordsById,
  );
  if (!projection) return undefined;

  return {
    id: obj.sourceEventIds[obj.sourceEventIds.length - 1] ?? obj.render.eventId,
    kind: projection.kind,
    label: projection.label,
    body: projection.body,
    timestamp: projection.timestamp ?? obj.render.timestamp,
    meta: obj.render.meta,
    minDisplayLevel: eventProjectionDisplayLevel(projection.minDisplayLevel, ctx.eventDisplayLevel),
    sourceIds: obj.sourceEventIds,
    detail: projection.detail,
    rawEvent: obj.render.rawEvent,
    debug: obj.render.debug,
  };
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}
