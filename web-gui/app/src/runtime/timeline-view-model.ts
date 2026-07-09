import type { AgentTimelineItem } from "./types";
import type { SessionState } from "./session-state-reducer";
import { compactAgentTimelineItems } from "./timeline-display";

/**
 * Derive the final ordered {@link AgentTimelineItem[]} timeline from a
 * {@link SessionState}.
 *
 * This is the "render state" step: the normalized state (already deduped
 * during event application) is flattened, compacted, and sorted to produce
 * the display-ready timeline that {@link AgentPage} consumes.
 *
 * In Phase 1 this delegates to the existing {@link compactAgentTimelineItems}
 * pipeline so that output is identical to the previous batch approach.
 * Phase 2 will enrich this with lifecycle-aware rendering (e.g. show
 * "running" status for in-flight tool executions instead of only the
 * completion event).
 */
export function deriveTimelineView(state: SessionState): AgentTimelineItem[] {
  const items = Array.from(state.items.values());
  const sorted = items.sort(
    (left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp),
  );
  return compactAgentTimelineItems(sorted);
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}
