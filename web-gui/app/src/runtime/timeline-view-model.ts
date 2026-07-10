import type { AgentTimelineItem, DisplayLevel, RuntimeMessageEnvelope, RuntimeBriefRecord, RuntimeTranscriptEntry } from "./types";
import type { SessionState } from "./session-state-reducer";
import type { DomainObject, InsertionEntry } from "./session-object-types";
import { compactAgentTimelineItems } from "./timeline-display";
import { renderDomainObject } from "./object-renderers";

/**
 * Context passed to the render layer. Contains external lookup tables
 * (reference data, not state changes) and display configuration that the
 * renderers need to produce display-ready items.
 */
export interface RenderContext {
  eventDisplayLevel: DisplayLevel;
  includeDebug: boolean;
  activitiesById?: SessionState["activitiesById"];
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
 * Calls {@link renderDomainObject} to produce the display-ready item.
 * Returns `undefined` when the object produces no visible projection.
 */
function renderObject(obj: DomainObject, ctx: RenderContext): AgentTimelineItem | undefined {
  // Skip activity objects that are children of a parent StateObject.
  // Tool execution objects may carry relatedStateObjectRef for breadcrumb
  // navigation but should still render as standalone timeline items.
  if ("relatedStateObjectRef" in obj && obj.relatedStateObjectRef && !("toolName" in obj)) return undefined;
  const item = renderDomainObject(obj, ctx);
  if (!item) return undefined;

  return {
    ...item,
    id: item.stateObjectRef ? obj.id : (obj.sourceEventIds[obj.sourceEventIds.length - 1] ?? obj.render.eventId),
    timestamp: item.timestamp || obj.render.timestamp,
  };
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}
