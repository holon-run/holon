import { describe, expect, it } from "vitest";

import type { StreamEventEnvelopeDto } from "./client";
import {
  emptyAgentSession,
  hasEventIdentityConflict,
  mergeCachedSessionIntoCurrent,
  mergeEventPageIntoConversation,
  semanticHistoryState,
  semanticTimelineHasNewItem,
  semanticTimelineItemIds,
  sessionForEventLogEpoch,
  withSemanticHistoryState,
} from "./conversation-store";

function event(
  eventSeq: number,
  type: string,
  payload: Record<string, unknown> = {},
): StreamEventEnvelopeDto {
  return {
    id: `event-${eventSeq}`,
    event_seq: eventSeq,
    event_log_epoch: "epoch-1",
    ts: `2026-08-11T00:00:${String(eventSeq).padStart(2, "0")}Z`,
    agent_id: "agent-a",
    type,
    payload,
  };
}

describe("conversation state", () => {
  it("drops projection and semantic history when the event log epoch changes", () => {
    const current = mergeEventPageIntoConversation(
      emptyAgentSession(),
      [event(7, "message_enqueued", {
        message_id: "message-7",
        origin: { kind: "operator" },
        body: "old message",
      })],
      7,
      true,
      "info",
      {
        eventLogEpoch: "epoch-1",
        historyDisplayLevel: "info",
      },
    );

    const reset = sessionForEventLogEpoch(current, "epoch-2");

    expect(reset).toMatchObject({
      eventLogEpoch: "epoch-2",
      eventsBySeq: {},
      eventSeqs: [],
      messagesById: {},
      semanticHistoryByDisplayLevel: {},
    });
    expect(reset.newestSeq).toBeUndefined();
    expect(reset.oldestSeq).toBeUndefined();
  });

  it("detects conflicting immutable content for the same epoch and sequence", () => {
    const existing = event(7, "legacy_event", { value: 1 });
    const current = {
      ...emptyAgentSession(),
      eventLogEpoch: "epoch-1",
      eventsBySeq: { 7: existing },
      eventSeqs: [7],
    };

    expect(hasEventIdentityConflict(current, [{ ...existing }])).toBe(false);
    expect(
      hasEventIdentityConflict(current, [
        { ...existing, id: "event-conflict", payload: { value: 2 } },
      ]),
    ).toBe(true);
  });

  it("restores cached projection only into an empty session and preserves UI state", () => {
    const current = {
      ...emptyAgentSession(),
      loading: true,
      liveStatus: "connecting" as const,
    };
    const cached = mergeEventPageIntoConversation(
      emptyAgentSession(),
      [event(1, "legacy_event")],
      1,
      false,
      "info",
      {
        eventLogEpoch: "epoch-1",
        historyDisplayLevel: "info",
      },
    );

    const restored = mergeCachedSessionIntoCurrent(current, cached);

    expect(restored.eventSeqs).toEqual([1]);
    expect(restored.semanticHistoryByDisplayLevel.info).toMatchObject({
      cursorSeq: 1,
      hasOlder: false,
    });
    expect(restored.loading).toBe(true);
    expect(restored.liveStatus).toBe("connecting");
    expect(mergeCachedSessionIntoCurrent(restored, cached)).toBe(restored);
  });

  it("merges raw pages while updating only the requested semantic history cursor", () => {
    const current = withSemanticHistoryState(
      withSemanticHistoryState(emptyAgentSession(), "info", {
        eventLogEpoch: "epoch-1",
        cursorSeq: 90,
        hasOlder: true,
        loading: true,
      }),
      "verbose",
      {
        eventLogEpoch: "epoch-1",
        cursorSeq: 70,
        hasOlder: true,
        loading: false,
      },
    );
    const merged = mergeEventPageIntoConversation(
      current,
      [
        event(89, "legacy_event"),
        event(80, "message_enqueued", {
          message_id: "message-80",
          origin: { kind: "operator" },
          body: "older operator message",
        }),
      ],
      80,
      false,
      "info",
      {
        newestSeq: 89,
        eventLogEpoch: "epoch-1",
        historyDisplayLevel: "info",
        historyLoading: false,
      },
    );

    expect(merged).toMatchObject({
      eventSeqs: [80, 89],
      oldestSeq: 80,
      newestSeq: 89,
      semanticHistoryByDisplayLevel: {
        info: { cursorSeq: 80, hasOlder: false, loading: false },
        verbose: { cursorSeq: 70, hasOlder: true, loading: false },
      },
    });
    expect(semanticHistoryState(merged, "info").cursorSeq).toBe(80);
  });

  it("detects when a merged page adds an item visible at the selected display level", () => {
    const initial = emptyAgentSession();
    const initialIds = semanticTimelineItemIds(initial, "info");
    const rawOnly = mergeEventPageIntoConversation(
      initial,
      [event(2, "legacy_event")],
      2,
      true,
      "info",
      { eventLogEpoch: "epoch-1" },
    );
    const withMessage = mergeEventPageIntoConversation(
      rawOnly,
      [event(1, "message_enqueued", {
        message_id: "message-1",
        origin: { kind: "operator" },
        body: "visible message",
      })],
      1,
      false,
      "info",
      { eventLogEpoch: "epoch-1" },
    );

    expect(semanticTimelineHasNewItem(rawOnly, "info", initialIds)).toBe(false);
    expect(semanticTimelineHasNewItem(withMessage, "info", initialIds)).toBe(true);
  });
});
