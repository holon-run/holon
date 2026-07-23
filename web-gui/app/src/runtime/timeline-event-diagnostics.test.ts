import { describe, expect, it } from "vitest";

import { createSessionProjectionState, reduceSessionProjection } from "./session-projection";
import type { SessionEventEnvelope } from "./session-events";
import {
  diagnoseTimelineEvent,
  filterTimelineEvents,
  timelineEventFamily,
  timelineEventGapCount,
  timelineEventsFilename,
} from "./timeline-event-diagnostics";

function event(overrides: Partial<SessionEventEnvelope> = {}): SessionEventEnvelope {
  return {
    id: "event-1",
    event_seq: 1,
    event_log_epoch: "epoch-a",
    agent_id: "agent-a",
    contract_version: 1,
    ts: "2026-07-23T00:00:00Z",
    type: "provider_request_started",
    payload_schema: "holon.runtime_event.legacy",
    payload_schema_version: 1,
    provenance: {},
    payload: {},
    ...overrides,
  };
}

describe("timeline event diagnostics", () => {
  it("classifies semantic source events as shown or merged", () => {
    const projection = reduceSessionProjection(createSessionProjectionState(), {
      type: "events_received",
      events: [event()],
      eventLogEpoch: "epoch-a",
    });
    expect(diagnoseTimelineEvent(event(), projection, [{
      id: "item-1",
      kind: "system",
      label: "Provider",
      body: "Started",
      timestamp: "2026-07-23T00:00:00Z",
      meta: "",
      minDisplayLevel: "debug",
      sourceIds: ["event-1"],
    }]).disposition).toBe("shown");
    expect(diagnoseTimelineEvent(event(), projection, [{
      id: "item-1",
      kind: "system",
      label: "Provider",
      body: "Started",
      timestamp: "2026-07-23T00:00:00Z",
      meta: "",
      minDisplayLevel: "debug",
      sourceIds: ["event-1", "event-2"],
    }]).disposition).toBe("merged");
  });

  it("uses the event sequence identity when the envelope has no id", () => {
    const withoutId = event({ id: undefined });
    const projection = reduceSessionProjection(createSessionProjectionState(), {
      type: "events_received",
      events: [withoutId],
      eventLogEpoch: "epoch-a",
    });
    expect(diagnoseTimelineEvent(withoutId, projection, [{
      id: "item-1",
      kind: "system",
      label: "Provider",
      body: "Started",
      timestamp: "2026-07-23T00:00:00Z",
      meta: "",
      minDisplayLevel: "debug",
      sourceIds: ["event-1"],
    }]).disposition).toBe("shown");
  });

  it("distinguishes rejected, hidden, and not-loaded events", () => {
    const accepted = event();
    const projection = reduceSessionProjection(createSessionProjectionState(), {
      type: "events_received",
      events: [accepted],
      eventLogEpoch: "epoch-a",
    });
    expect(diagnoseTimelineEvent(accepted, projection, []).disposition).toBe("hidden");
    expect(diagnoseTimelineEvent(event({ event_seq: 2, id: "event-2" }), projection, []).disposition).toBe("unhandled");
    expect(diagnoseTimelineEvent(event({
      contract_version: 2,
      payload_schema: "unsupported",
    }), projection, []).disposition).toBe("rejected");
  });
});

describe("timeline event filtering", () => {
  it("classifies families and filters by query and disposition", () => {
    const events = [
      event({ type: "task_created", payload: { task_id: "task-1" } }),
      event({ id: "event-2", event_seq: 2, type: "work_item_written", payload: { objective: "Fix timeline" } }),
    ];
    const diagnostics = new Map([
      [1, { disposition: "shown" as const, reason: "", timelineItemIds: [] }],
      [2, { disposition: "hidden" as const, reason: "", timelineItemIds: [] }],
    ]);
    expect(timelineEventFamily("task_created")).toBe("task");
    expect(timelineEventFamily("work_item_written")).toBe("work_item");
    expect(filterTimelineEvents(events, diagnostics, { query: "timeline", family: "all", status: "hidden" }))
      .toEqual([events[1]]);
  });

  it("counts sequence gaps and produces a safe export filename", () => {
    expect(timelineEventGapCount([event(), event({ id: "event-3", event_seq: 3 })])).toBe(1);
    expect(timelineEventsFilename("agent/unsafe", new Date("2026-07-23T00:00:00Z")))
      .toContain("agent_unsafe");
  });
});
