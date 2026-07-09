import type { AgentTimelineItem } from "./types";
import {
  mergeTimelineItemFields,
  sessionItemPriority,
  sessionObjectKey,
} from "./timeline-display";

/**
 * Normalized session state built by applying events incrementally.
 *
 * Each event produces at most one projected {@link AgentTimelineItem}.
 * Items are stored by their semantic dedup key so that multiple events
 * targeting the same object (e.g. a slim message followed by its hydrated
 * form) are merged into a single entry — the same semantics as the previous
 * batch `mergeAgentTimelineItems([], items)` pass.
 *
 * Phase 2 will enrich this state with lifecycle metadata (tool running →
 * completed, task created → finished, work item transitions, etc.) so that
 * the view model can render the current status of each object rather than
 * only the last event projection.
 */
export interface SessionState {
  /** Items keyed by semantic dedup key (operator:srcId, assistant:id, item:id). */
  items: Map<string, AgentTimelineItem>;
  /** Keys in first-insertion order, for stable iteration. */
  insertionOrder: string[];
}

/**
 * Create an empty {@link SessionState}.
 */
export function createSessionState(): SessionState {
  return {
    items: new Map(),
    insertionOrder: [],
  };
}

/**
 * Insert or merge a projected timeline item into the state.
 *
 * If an item with the same semantic key already exists, the two are merged
 * using the same priority and activity-merge logic as
 * `mergeAgentTimelineItems`. This makes incremental event-by-event
 * application equivalent to the previous batch dedup pass.
 */
export function upsertTimelineItem(state: SessionState, item: AgentTimelineItem): void {
  const key = sessionObjectKey(item);
  const existing = state.items.get(key);

  if (!existing) {
    state.items.set(key, item);
    state.insertionOrder.push(key);
    return;
  }

  const newItemPriority = sessionItemPriority(item);
  const existingPriority = sessionItemPriority(existing);

  if (newItemPriority >= existingPriority) {
    // New item wins or ties — keep new display fields, merge old sourceIds/activities.
    state.items.set(key, mergeTimelineItemFields(item, existing));
  } else {
    // Existing item wins — keep its display fields, merge new sourceIds/activities.
    state.items.set(key, mergeTimelineItemFields(existing, item));
  }
}
