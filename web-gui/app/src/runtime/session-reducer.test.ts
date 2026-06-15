import { describe, expect, it } from "vitest";

import type { AgentTimelineItem } from "./types";
import type { SessionEventEnvelope } from "./session-reducer";
import {
  compactAgentTimelineItems,
  debugAgentSessionEvents,
  filterTimelineByDisplayLevel,
  mergeAgentTimelineItems,
  reduceAgentSessionTimeline,
} from "./session-reducer";

describe("reduceAgentSessionTimeline", () => {
  it("projects operator input events into the timeline", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          {
            id: "event-1",
            event_seq: 1,
            ts: "2026-06-15T10:00:00Z",
            type: "message_enqueued",
            payload: {
              origin: { kind: "operator" },
              body: { text: "hello" },
            },
          },
        ],
      },
    });

    expect(timeline).toEqual([
      expect.objectContaining({
        id: "event-1",
        kind: "operator",
        label: "Operator input",
        body: "hello",
        minDisplayLevel: "info",
      }),
    ]);
  });

  it("keeps the raw event on projected timeline items", () => {
    const rawEvent = event({
      id: "event-raw",
      event_seq: 2,
      type: "brief_created",
      payload: {
        kind: "result",
        text: "done",
      },
    });

    const timeline = reduceAgentSessionTimeline({ events: { events: [rawEvent] } });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        label: "Result",
        body: "done",
        rawEvent,
      }),
    );
  });

  it("hides successful WorkItem mutation tools and keeps failed ones", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("tool-success", "CreateWorkItem"),
          toolEvent("tool-failed", "UpdateWorkItem", { error: "permission denied" }),
        ],
      },
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "tool-failed",
        kind: "tool",
        label: "Work item update failed",
        body: expect.stringContaining("permission denied"),
      }),
    );
  });

  it("projects ViewImage tools with image context instead of duration-only summaries", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("view-image", "ViewImage", {
            duration_ms: 9700,
            path: "/Users/jolestar/Desktop/Screenshot.png",
            view_image_result: {
              dimensions: { width: 1200, height: 800 },
              visual_observation: "A browser screenshot showing the conversation timeline.",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        label: "Tool finished",
        body: "Viewed image · Screenshot.png · 1200×800 · A browser screenshot showing the conversation timeline. · 9.7s",
      }),
    );
  });

  it("falls back to the tool name when a tool has no readable summary", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [toolEvent("opaque-tool", "OpaqueTool", { duration_ms: 9700 })],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        body: "OpaqueTool · 9.7s",
      }),
    );
  });

  it("projects structured tool errors as readable error text instead of raw json", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("patch-failed", "ApplyPatch", {
            duration_ms: 120,
            error: {
              message: "context mismatch near projectApplyPatchTool",
              diagnostics: [{ path: "src/runtime/session-reducer.ts" }],
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        label: "Patch failed",
        body: "ApplyPatch · 120ms · context mismatch near projectApplyPatchTool",
        detail: {
          label: "Error",
          text: "context mismatch near projectApplyPatchTool",
          tone: "data",
        },
      }),
    );
  });

  it("parses json-string tool errors before rendering failed tool details", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "patch-failed-json",
            event_seq: 81783,
            type: "tool_execution_failed",
            payload: {
              tool_name: "ApplyPatch",
              error: JSON.stringify({
                kind: "ambiguous_context",
                message: "hunk context matches 8 locations in web-gui/app/src/runtime/session-reducer.ts",
                details: { candidate_count: 8 },
              }),
              error_kind: "ambiguous_context",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        label: "Patch failed",
        body: "ApplyPatch · hunk context matches 8 locations in web-gui/app/src/runtime/session-reducer.ts",
        detail: {
          label: "Error",
          text: "hunk context matches 8 locations in web-gui/app/src/runtime/session-reducer.ts",
          tone: "data",
        },
      }),
    );
  });

  it("renders work_item_picked objective and reason without internal ids", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "picked",
            event_seq: 10,
            type: "work_item_picked",
            payload: {
              reason: "next priority",
              record: {
                id: "work_123",
                objective: "Fix timeline",
              },
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "picked",
        kind: "system",
        label: "Work item",
        body: "Picked work item · Fix timeline · next priority",
        minDisplayLevel: "verbose",
      }),
    );
  });

  it("renders focus release details from top-level work item fields", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "released",
            event_seq: 11,
            type: "work_item_focus_released",
            payload: {
              work_item_id: "work_456",
              reason: "completed",
              readiness: "ready",
              revision: 3,
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "released",
        kind: "system",
        label: "Work item",
        body: "Released work item focus · completed · ready",
        minDisplayLevel: "debug",
      }),
    );
  });

  it("renders completion report promotion details and preview", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "promoted",
            event_seq: 12,
            type: "work_item_completion_report_promoted",
            payload: {
              work_item_id: "work_789",
              brief_id: "brief_123",
              source_turn_index: 7,
              source_round: 2,
              text_preview: "Finished the implementation.",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "promoted",
        kind: "system",
        label: "Work item",
        body: "Promoted completion report · Finished the implementation.",
      }),
    );
  });

  it("renders completion report candidate promotion details and preview", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "candidate-promoted",
            event_seq: 13,
            type: "work_item_completion_report_candidate_promoted",
            payload: {
              work_item_id: "work_abc",
              brief_id: "brief_abc",
              turn_index: 8,
              round: 1,
              text_preview: "Candidate completion text.",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "candidate-promoted",
        kind: "system",
        label: "Work item",
        body: "Promoted completion report candidate · Candidate completion text.",
      }),
    );
  });

  it("keeps stale WorkItem reminders at debug level", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "stale",
            event_seq: 11,
            type: "work_item_stale_reminder_injected",
            payload: {
              record: {
                id: "work_123",
                objective: "Fix timeline",
              },
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "stale",
        kind: "system",
        label: "Work item",
        minDisplayLevel: "debug",
      }),
    );
  });

  it("keeps wait condition registrations out of the Info timeline", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "wait-condition",
            event_seq: 14,
            type: "wait_condition_registered",
            payload: {
              reason: "awaiting external change",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "wait-condition",
        kind: "system",
        label: "Waiting",
        minDisplayLevel: "debug",
      }),
    );
    expect(filterTimelineByDisplayLevel(timeline, "info")).toEqual([]);
  });

  it("dedupes assistant previews covered by final briefs", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "preview",
            event_seq: 20,
            type: "assistant_round_recorded",
            payload: {
              text_preview: "Implemented the fix...",
            },
          }),
          event({
            id: "brief",
            event_seq: 21,
            type: "brief_created",
            payload: {
              kind: "result",
              text: "Implemented the fix and verified it.",
            },
          }),
        ],
      },
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "brief",
        label: "Result",
        body: "Implemented the fix and verified it.",
      }),
    );
  });
});

describe("filterTimelineByDisplayLevel", () => {
  it("filters items by display level and preserves info-level tool activities", () => {
    const filtered = filterTimelineByDisplayLevel(
      [
        timelineItem({ id: "info", minDisplayLevel: "info" }),
        timelineItem({ id: "verbose", minDisplayLevel: "verbose" }),
        timelineItem({ id: "debug", minDisplayLevel: "debug" }),
        timelineItem({
          id: "activity-parent",
          minDisplayLevel: "debug",
          activities: [
            {
              ...timelineItem({ id: "tool-activity", kind: "tool", minDisplayLevel: "verbose" }),
              meta: "tool_executed · event #1",
            },
            {
              ...timelineItem({ id: "system-activity", kind: "system", minDisplayLevel: "info" }),
              meta: "brief_created · event #2",
            },
          ],
        }),
      ],
      "info",
    );

    expect(filtered.map((item) => item.id)).toEqual(["info", "activity-parent"]);
    expect(filtered[1].activities?.map((activity) => activity.id)).toEqual(["tool-activity"]);
  });

  it("applies explicit item limits after filtering", () => {
    const filtered = filterTimelineByDisplayLevel(
      [
        timelineItem({ id: "first", timestamp: "2026-06-15T10:00:00Z" }),
        timelineItem({ id: "second", timestamp: "2026-06-15T10:01:00Z" }),
        timelineItem({ id: "third", timestamp: "2026-06-15T10:02:00Z" }),
      ],
      "debug",
      { itemLimit: 2 },
    );

    expect(filtered.map((item) => item.id)).toEqual(["second", "third"]);
  });
});

describe("debugAgentSessionEvents", () => {
  it("projects every identifiable raw event as a debug timeline item", () => {
    const events = [
      event({
        id: "assistant-without-preview",
        event_seq: 50,
        type: "assistant_round_recorded",
        payload: {
          turn_id: "turn-1",
        },
      }),
      toolEvent("work-item-mutation", "CreateWorkItem", {
        objective: "Track debug visibility",
      }),
      event({
        type: "no-identity",
        payload: {
          text: "missing id and sequence",
        },
      }),
    ];

    const timeline = debugAgentSessionEvents(events);

    expect(timeline.map((item) => item.id)).toEqual(["debug:assistant-without-preview", "debug:work-item-mutation"]);
    expect(timeline[0]).toEqual(
      expect.objectContaining({
        label: "Assistant Round Recorded",
        meta: "assistant_round_recorded · event #50",
        detail: expect.objectContaining({
          label: "Event details",
          text: "Turn Id: turn-1",
        }),
        rawEvent: events[0],
      }),
    );
    expect(timeline[1]).toEqual(
      expect.objectContaining({
        label: "Tool finished",
        body: expect.stringContaining("Track debug visibility"),
        detail: expect.objectContaining({
          label: "Work item change",
          text: expect.stringContaining("Objective: Track debug visibility"),
        }),
      }),
    );
  });

  it("applies item limits to the newest debug events", () => {
    const timeline = debugAgentSessionEvents(
      [
        event({ id: "old", ts: "2026-06-15T10:00:00Z", type: "brief_created", payload: { text: "old" } }),
        event({ id: "new", ts: "2026-06-15T10:01:00Z", type: "brief_created", payload: { text: "new" } }),
      ],
      { itemLimit: 1 },
    );

    expect(timeline.map((item) => item.id)).toEqual(["debug:new"]);
  });
});

describe("mergeAgentTimelineItems", () => {
  it("merges duplicate assistant items by source identity", () => {
    const merged = mergeAgentTimelineItems(
      [
        timelineItem({
          id: "event-1",
          kind: "assistant",
          body: "same answer",
          meta: "brief_created · event #1",
          sourceIds: ["event-1"],
        }),
      ],
      [
        timelineItem({
          id: "event-1",
          kind: "assistant",
          body: "same answer",
          meta: "brief_created",
          sourceIds: ["brief-1"],
        }),
      ],
    );

    expect(merged).toHaveLength(1);
    expect(merged[0]).toEqual(
      expect.objectContaining({
        id: "event-1",
        sourceIds: ["brief-1", "event-1"],
      }),
    );
  });

  it("keeps repeated final briefs with the same text from different events", () => {
    const merged = mergeAgentTimelineItems(
      [
        timelineItem({
          id: "brief-old",
          kind: "assistant",
          body: "Ignored.",
          meta: "brief_created · event #1",
          sourceIds: ["brief-old"],
        }),
      ],
      [
        timelineItem({
          id: "brief-new",
          kind: "assistant",
          body: "Ignored.",
          meta: "brief_created · event #2",
          sourceIds: ["brief-new"],
        }),
      ],
    );

    expect(merged.map((item) => item.id)).toEqual(["brief-old", "brief-new"]);
  });
});

describe("compactAgentTimelineItems", () => {
  it("flattens meaningful activities and drops ephemeral runtime activities", () => {
    const compacted = compactAgentTimelineItems([
      timelineItem({
        id: "parent",
        activities: [
          {
            ...timelineItem({ id: "ephemeral-tool", kind: "tool" }),
            meta: "tool_executed · event #1",
          },
          {
            ...timelineItem({ id: "work-item", kind: "system", label: "Work item" }),
            meta: "work_item_picked · event #2",
          },
        ],
      }),
    ]);

    expect(compacted.map((item) => item.id)).toEqual(["parent", "work-item"]);
  });
});

function event(overrides: SessionEventEnvelope): SessionEventEnvelope {
  return {
    ts: "2026-06-15T10:00:00Z",
    ...overrides,
  };
}

function toolEvent(id: string, toolName: string, payload: Record<string, unknown> = {}): SessionEventEnvelope {
  return event({
    id,
    event_seq: Number(id.replace(/\D/g, "")) || undefined,
    type: "tool_executed",
    payload: {
      tool_name: toolName,
      duration_ms: 11,
      ...payload,
    },
  });
}

function timelineItem(overrides: Partial<AgentTimelineItem> = {}): AgentTimelineItem {
  return {
    id: "item",
    kind: "event",
    label: "Event",
    body: "body",
    timestamp: "2026-06-15T10:00:00Z",
    meta: "event",
    minDisplayLevel: "info",
    sourceIds: [overrides.id ?? "item"],
    ...overrides,
  };
}
