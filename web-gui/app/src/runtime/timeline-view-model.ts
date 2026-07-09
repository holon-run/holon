import type { AgentTimelineItem } from "./types";
import type { SessionState } from "./session-state-reducer";
import type { DomainObject, InsertionEntry } from "./session-object-types";
import { compactAgentTimelineItems } from "./timeline-display";

/**
 * Derive the final ordered {@link AgentTimelineItem[]} timeline from a
 * {@link SessionState}.
 *
 * Iterates `insertionOrder`, looks up each domain object from its typed
 * map, and builds an {@link AgentTimelineItem} from the cached viewDraft.
 * Step 3a uses viewDraft directly; Step 3b will replace this with
 * per-object renderers that derive the view from typed fields.
 */
export function deriveTimelineView(state: SessionState): AgentTimelineItem[] {
  const items: AgentTimelineItem[] = [];
  for (const entry of state.insertionOrder) {
    const obj = lookupObject(state, entry);
    if (!obj) continue;
    items.push(objectToItem(obj));
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

function objectToItem(obj: DomainObject): AgentTimelineItem {
  const d = obj.viewDraft;
  return {
    id: obj.sourceEventIds[obj.sourceEventIds.length - 1] ?? obj.id,
    kind: d.kind,
    label: d.label,
    body: d.body,
    timestamp: d.timestamp,
    meta: d.meta,
    minDisplayLevel: d.minDisplayLevel,
    sourceIds: obj.sourceEventIds,
    detail: d.detail,
    rawEvent: d.rawEvent,
    debug: d.debug,
  };
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}
