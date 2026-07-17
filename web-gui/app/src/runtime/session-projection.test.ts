import { describe, expect, it } from "vitest";

import {
  SESSION_PROJECTION_GENERATION,
  createSessionProjectionState,
  deriveSessionTimeline,
  reduceSessionProjection,
  type ProjectionEvent,
  type SessionProjectionState,
} from "./session-projection";

function event(
  eventSeq: number,
  type: string,
  payload: Record<string, unknown>,
): ProjectionEvent {
  return {
    id: `event-${eventSeq}`,
    event_seq: eventSeq,
    event_log_epoch: "epoch-1",
    ts: `2026-07-16T00:00:${String(eventSeq).padStart(2, "0")}Z`,
    type,
    payload,
  };
}

function receive(
  projection: SessionProjectionState,
  events: ProjectionEvent[],
): SessionProjectionState {
  return reduceSessionProjection(projection, {
    type: "events_received",
    eventLogEpoch: "epoch-1",
    events,
  });
}

function projectionSnapshot(projection: SessionProjectionState) {
  return {
    eventSeqs: projection.eventSeqs,
    gaps: projection.gaps,
    messages: Array.from(projection.domain.messages.values()),
    tasks: Array.from(projection.domain.tasks.values()),
    workItems: Array.from(projection.domain.workItems.values()),
    timeline: deriveSessionTimeline(projection),
  };
}

describe("SessionProjectionState", () => {
  const events = [
    event(1, "message_enqueued", {
      message_id: "message-1",
      origin: { kind: "operator" },
      body: "Ship the projection",
    }),
    event(2, "message_processing_started", {
      message_id: "message-1",
      origin: { kind: "operator" },
    }),
    event(3, "task_created", {
      task_id: "task-1",
      status: "created",
      summary: "Run checks",
    }),
    event(4, "task_status_updated", {
      task_id: "task-1",
      status: "running",
    }),
    event(5, "task_result_received", {
      task_id: "task-1",
      status: "completed",
      exit_status: 0,
    }),
  ];

  it("converges across single-batch, split, and older-page-later ingestion", () => {
    const single = receive(createSessionProjectionState(), events);
    const split = receive(
      receive(createSessionProjectionState(), events.slice(0, 2)),
      events.slice(2),
    );
    const olderPageLater = receive(
      receive(createSessionProjectionState(), events.slice(2)),
      events.slice(0, 2),
    );

    expect(projectionSnapshot(split)).toEqual(projectionSnapshot(single));
    expect(projectionSnapshot(olderPageLater)).toEqual(projectionSnapshot(single));
    expect(single.domain.messages.get("message:message-1")).toEqual(
      expect.objectContaining({
        status: "processing",
        primaryEventId: "event-1",
        sourceEventIds: ["event-1", "event-2"],
      }),
    );
    expect(single.domain.tasks.get("task:task-1")).toEqual(
      expect.objectContaining({
        status: "completed",
        initialStatus: "created",
        summary: "Run checks",
        sourceEventIds: ["event-3", "event-4", "event-5"],
      }),
    );
  });

  it("is idempotent for duplicate epoch/sequence events", () => {
    const once = receive(createSessionProjectionState(), events);
    const twice = receive(once, events);

    expect(twice).toBeDefined();
    expect(projectionSnapshot(twice)).toEqual(projectionSnapshot(once));
  });

  it("promotes an assistant round across event batches using semantic identity", () => {
    const recorded = receive(createSessionProjectionState(), [
      event(6, "assistant_round_recorded", {
        assistant_round_id: "round-1",
        text_preview: "Partial result...",
      }),
    ]);
    const promoted = receive(recorded, [
      event(7, "brief_created", {
        brief_id: "brief-1",
        finalizes_assistant_round_id: "round-1",
        kind: "result",
        text: "Complete result.",
      }),
    ]);

    expect(promoted.domain.rounds).toHaveLength(1);
    expect(promoted.domain.rounds.get("assistant_round:round-1")).toEqual(
      expect.objectContaining({
        status: "brief_promoted",
        sourceEventIds: ["event-6", "event-7"],
      }),
    );
    expect(deriveSessionTimeline(promoted)).toEqual([
      expect.objectContaining({
        id: "assistant_round:round-1",
        body: "Complete result.",
      }),
    ]);
  });

  it("promotes a cached assistant round when its live brief arrives", () => {
    const recordedEvent = event(6, "assistant_round_recorded", {
      assistant_round_id: "round-1",
      text_preview: "Partial result...",
    });
    const restored = reduceSessionProjection(createSessionProjectionState(), {
      type: "cache_restored",
      generation: SESSION_PROJECTION_GENERATION,
      eventLogEpoch: "epoch-1",
      eventsBySeq: { 6: recordedEvent },
      eventSeqs: [6],
      messagesById: {},
      transcriptEntriesById: {},
      briefRecordsById: {},
      newestSeq: 6,
      oldestSeq: 6,
    });
    const promoted = receive(restored, [
      event(7, "brief_created", {
        brief_id: "brief-1",
        finalizes_assistant_round_id: "round-1",
        kind: "result",
        text: "Complete result.",
      }),
    ]);

    expect(promoted.domain.rounds).toHaveLength(1);
    expect(deriveSessionTimeline(promoted)).toEqual([
      expect.objectContaining({
        id: "assistant_round:round-1",
        body: "Complete result.",
        sourceIds: ["event-6", "event-7"],
      }),
    ]);
  });

  it("only uses transcript entry identity when the brief relation is finalizes", () => {
    const projection = receive(createSessionProjectionState(), [
      event(8, "assistant_round_recorded", {
        assistant_round_id: "round-1",
        text_preview: "First round",
      }),
      event(9, "brief_created", {
        brief_id: "brief-final",
        content_source: {
          kind: "transcript_entry",
          relation: "finalizes",
          entry_id: "round-1",
        },
        text: "Promoted first round",
      }),
      event(10, "brief_created", {
        brief_id: "brief-independent",
        content_source: {
          kind: "transcript_entry",
          relation: "references",
          entry_id: "round-1",
        },
        text: "Independent result",
      }),
      event(11, "assistant_round_recorded", {
        assistant_round_id: "round-2",
        text_preview: "Second round",
      }),
    ]);

    expect(Array.from(projection.domain.rounds.keys())).toEqual([
      "assistant_round:round-1",
      "brief:brief-independent",
      "assistant_round:round-2",
    ]);
  });

  it("records and closes gaps when backfill arrives", () => {
    const withGap = receive(createSessionProjectionState(), [events[0], events[2]]);
    expect(withGap.gaps).toEqual([{ afterSeq: 1, beforeSeq: 3 }]);

    const recovered = receive(withGap, [events[1]]);
    expect(recovered.gaps).toEqual([]);
    expect(recovered.eventSeqs).toEqual([1, 2, 3]);
  });

  it("hydrates references without rebuilding normalized domain state", () => {
    const projected = receive(createSessionProjectionState(), [events[0]]);
    const domain = projected.domain;
    const hydrated = reduceSessionProjection(projected, {
      type: "messages_hydrated",
      messages: [{
        id: "message-1",
        origin: { kind: "operator" },
        body: { text: "Canonical operator input" },
      }],
      missingIds: [],
    });

    expect(hydrated.domain).toBe(domain);
    expect(deriveSessionTimeline(hydrated)[0]?.body).toBe("Canonical operator input");
  });

  it("rebuilds once from canonical cache input and rejects stale generations", () => {
    const restored = reduceSessionProjection(createSessionProjectionState(), {
      type: "cache_restored",
      generation: SESSION_PROJECTION_GENERATION,
      eventLogEpoch: "epoch-1",
      eventsBySeq: Object.fromEntries(events.map((item) => [item.event_seq!, item])),
      eventSeqs: events.map((item) => item.event_seq!),
      messagesById: {},
      transcriptEntriesById: {},
      briefRecordsById: {},
    });
    const stale = reduceSessionProjection(restored, {
      type: "cache_restored",
      generation: SESSION_PROJECTION_GENERATION - 1,
      eventLogEpoch: "epoch-1",
      eventsBySeq: {},
      eventSeqs: [],
      messagesById: {},
      transcriptEntriesById: {},
      briefRecordsById: {},
    });

    expect(projectionSnapshot(restored)).toEqual(
      projectionSnapshot(receive(createSessionProjectionState(), events)),
    );
    expect(stale.eventSeqs).toEqual([]);
    expect(stale.invalidatedReason).toBe("cache_generation_mismatch");
  });
});
