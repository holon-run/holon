import { describe, expect, it } from "vitest";

import type { StreamEventEnvelopeDto } from "./client";
import {
  emptyAgentSession,
  hasEventIdentityConflict,
  mergeEventPageIntoConversation,
  sessionForEventLogEpoch,
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
  it("drops the projection when the event log epoch changes", () => {
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
      { eventLogEpoch: "epoch-1" },
    );

    const reset = sessionForEventLogEpoch(current, "epoch-2");

    expect(reset).toMatchObject({
      eventLogEpoch: "epoch-2",
      eventsBySeq: {},
      eventSeqs: [],
      messagesById: {},
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


});
