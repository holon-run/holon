import { describe, expect, it } from "vitest";

import type { SessionEventEnvelope } from "./session-reducer";
import { reduceAgentSessionTimeline } from "./session-reducer";

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

  it("renders work_item_picked objective, id, and reason", () => {
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
