import { describe, expect, it } from "vitest";

import type { AgentTimelineItem } from "../../runtime/types";
import { groupTimelineTurns } from "./timeline-utils";

function timelineItem(overrides: Partial<AgentTimelineItem>): AgentTimelineItem {
  return {
    id: "event",
    kind: "system",
    label: "Event",
    body: "",
    timestamp: "2026-07-14T00:00:00Z",
    meta: "event",
    minDisplayLevel: "info",
    sourceIds: [],
    ...overrides,
  };
}

describe("groupTimelineTurns", () => {
  it("filters a turn boundary with no visible items", () => {
    const turns = groupTimelineTurns([
      timelineItem({
        id: "turn-started",
        label: "Turn started",
        body: "Operator input",
        meta: "turn_started · event #1",
      }),
    ]);

    expect(turns).toEqual([]);
  });

  it("keeps the waiting state without requiring a result brief", () => {
    const waiting = timelineItem({
      id: "agent-waiting",
      label: "Waiting",
      body: "waiting for PR checks",
      meta: "agent_waiting · event #2",
    });
    const turns = groupTimelineTurns([
      timelineItem({
        id: "turn-started",
        label: "Turn started",
        body: "Operator input",
        meta: "turn_started · event #1",
      }),
      waiting,
    ]);

    expect(turns).toHaveLength(1);
    expect(turns[0]?.items).toEqual([waiting]);
    expect(turns[0]?.items.some((item) => item.meta.startsWith("brief_created"))).toBe(false);
  });
});
