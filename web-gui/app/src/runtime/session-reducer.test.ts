import { describe, expect, it } from "vitest";

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
});
