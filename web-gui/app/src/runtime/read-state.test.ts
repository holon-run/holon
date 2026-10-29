import { describe, expect, it } from "vitest";
import {
  evaluateConversationReadMarkerGate,
  type ConversationReadMarkerGateInput,
} from "./read-state";

const ready: ConversationReadMarkerGateInput = {
  route: "agent",
  selectedAgentId: "agent-a",
  documentVisible: true,
  conversationReady: true,
  conversationVisible: true,
  discoveryFresh: true,
  scopeCurrent: true,
  throughSeq: 12,
  observedHeadSeq: 10,
};

describe("conversation read-marker gate", () => {
  it("acknowledges only the server-observed head covered by the displayed view", () => {
    expect(evaluateConversationReadMarkerGate(ready, "agent-a"))
      .toEqual({ mayAdvance: true, candidateSeq: 10 });
  });

  it.each([
    [{ route: "dashboard" }, "not_selected"],
    [{ selectedAgentId: "agent-b" }, "not_selected"],
    [{ conversationVisible: false }, "conversation_covered"],
    [{ documentVisible: false }, "document_hidden"],
    [{ conversationReady: false }, "session_not_ready"],
    [{ discoveryFresh: false }, "discovery_stale"],
    [{ scopeCurrent: false }, "scope_mismatch"],
    [{ observedHeadSeq: undefined }, "no_observed_head"],
    [{ observedHeadSeq: 0 }, "no_observed_head"],
    [{ throughSeq: null }, "not_caught_up"],
    [{ throughSeq: 9 }, "not_caught_up"],
  ] as const)("rejects %j", (overrides, reason) => {
    expect(evaluateConversationReadMarkerGate({ ...ready, ...overrides }, "agent-a"))
      .toEqual({ mayAdvance: false, reason });
  });
});
