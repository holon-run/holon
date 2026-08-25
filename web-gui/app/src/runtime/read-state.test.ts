import { describe, expect, it } from "vitest";

import type { StreamEventEnvelopeDto } from "./client";
import {
  cachedReadState,
  canMarkConversationRead,
  evaluateLedgerReadMarkerGate,
  latestBriefDeliverySeq,
  markAgentDeliveriesRead,
  mergeCachedReadState,
  mergeCachedReadStates,
  readStoredRosterActivity,
  touchRosterActivityFromEvent,
  unreadBadgeView,
  writeStoredRosterActivity,
  type AgentRosterActivity,
} from "./read-state";
import { emptyAgentSession } from "./conversation-store";

class MemoryStorage implements Storage {
  private readonly items = new Map<string, string>();

  get length() {
    return this.items.size;
  }

  clear(): void {
    this.items.clear();
  }

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  key(index: number): string | null {
    return Array.from(this.items.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }

  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }
}

function event(
  eventSeq: number,
  type: string,
  payload: Record<string, unknown> = {},
): StreamEventEnvelopeDto {
  return {
    id: `event-${eventSeq}`,
    agent_id: "agent-a",
    event_seq: eventSeq,
    ts: `2026-08-11T00:00:${String(eventSeq).padStart(2, "0")}Z`,
    type,
    payload,
  };
}

describe("read state", () => {
  it("persists activity per remote and migrates legacy read marker names", () => {
    const storage = new MemoryStorage();
    storage.setItem(
      "holon.webGui.rosterActivityByRemote.v1",
      JSON.stringify({
        local: {
          "agent-a": {
            unreadCount: 2,
            lastUnreadSeq: 12,
            lastReadSeq: 7,
            briefAt: "2026-08-11T00:00:00Z",
          },
        },
      }),
    );

    expect(readStoredRosterActivity("local", storage)).toEqual({
      "agent-a": {
        unreadCount: 2,
        lastUnreadDeliverySeq: 12,
        lastReadDeliverySeq: 7,
        briefAt: "2026-08-11T00:00:00Z",
      },
    });

    writeStoredRosterActivity(
      "https://remote.example",
      { "agent-b": { unreadCount: 1 } },
      storage,
    );
    expect(readStoredRosterActivity("https://remote.example", storage)).toEqual({
      "agent-b": { unreadCount: 1 },
    });
    expect(readStoredRosterActivity("local", storage)["agent-a"]?.lastReadDeliverySeq).toBe(7);
  });

  it("merges cached read state only when no local read marker exists", () => {
    const cached = {
      unreadCount: 3,
      lastUnreadDeliverySeq: 12,
      lastReadDeliverySeq: 7,
    };
    const activity = { operatorAt: "2026-08-11T00:00:00Z" };

    expect(mergeCachedReadState(activity, cached)).toEqual({ ...activity, ...cached });
    expect(mergeCachedReadStates({ "agent-a": activity }, { "agent-a": cached })).toEqual({
      "agent-a": { ...activity, ...cached },
    });

    const localState = { unreadCount: 1, lastReadDeliverySeq: 19 };
    expect(mergeCachedReadState(localState, cached)).toBe(localState);
    expect(cachedReadState({ ...activity, ...cached })).toEqual(cached);
    expect(cachedReadState(activity)).toBeUndefined();
  });

  it("deduplicates replayed brief deliveries and ignores non-operator messages", () => {
    const afterBrief = touchRosterActivityFromEvent(
      {},
      "agent-a",
      event(10, "brief_created"),
      "agent-b",
    );
    const afterDuplicate = touchRosterActivityFromEvent(
      afterBrief,
      "agent-a",
      event(10, "brief_created"),
      "agent-b",
    );
    const afterAgentMessage = touchRosterActivityFromEvent(
      afterDuplicate,
      "agent-a",
      event(11, "message_enqueued", { origin: { kind: "agent" } }),
      "agent-b",
    );

    expect(afterAgentMessage["agent-a"]).toMatchObject({
      unreadCount: 1,
      lastUnreadDeliverySeq: 10,
    });
  });

  it("advances read markers monotonically and does not recount replayed deliveries", () => {
    let activity: Record<string, AgentRosterActivity> = {
      "agent-a": { unreadCount: 2, lastUnreadDeliverySeq: 10, lastReadDeliverySeq: 7 },
    };

    activity = markAgentDeliveriesRead(activity, "agent-a", 9);
    expect(activity["agent-a"]).toMatchObject({
      unreadCount: 0,
      lastReadDeliverySeq: 10,
    });

    const unchanged = markAgentDeliveriesRead(activity, "agent-a", 8);
    expect(unchanged).toBe(activity);
    const replayed = touchRosterActivityFromEvent(
      activity,
      "agent-a",
      event(10, "brief_created"),
      "agent-b",
    );
    expect(replayed["agent-a"]).toMatchObject({
      unreadCount: 0,
      lastUnreadDeliverySeq: 10,
      lastReadDeliverySeq: 10,
    });

    const next = touchRosterActivityFromEvent(
      replayed,
      "agent-a",
      event(11, "brief_created"),
      "agent-b",
    );
    expect(next["agent-a"]).toMatchObject({
      unreadCount: 1,
      lastUnreadDeliverySeq: 11,
      lastReadDeliverySeq: 10,
    });
  });

  it("marks a conversation readable only after its synchronized projection is visible", () => {
    const session = {
      ...emptyAgentSession(),
      eventsBySeq: {
        8: event(8, "brief_created"),
        10: event(10, "brief_created"),
      },
      eventSeqs: [8, 10],
      syncStatus: "streaming" as const,
      liveStatus: "streaming" as const,
    };
    const context = {
      route: "agent",
      selectedAgentId: "agent-a",
      documentVisible: true,
      session,
    };

    expect(canMarkConversationRead(context, "agent-a")).toBe(true);
    expect(latestBriefDeliverySeq(session)).toBe(10);
    expect(canMarkConversationRead({ ...context, documentVisible: false }, "agent-a")).toBe(false);
    expect(
      canMarkConversationRead(
        { ...context, session: { ...session, liveStatus: "recovering" } },
        "agent-a",
      ),
    ).toBe(false);
  });
});
describe("ledger read-marker gate", () => {
  const readiness = {
    readyThroughSeq: 12,
    ingestedThroughSeq: 12,
    observedHeadSeq: 12,
  };

  function gateInput(overrides: Partial<Parameters<typeof evaluateLedgerReadMarkerGate>[0]> = {}) {
    return {
      route: "agent",
      selectedAgentId: "agent-a",
      documentVisible: true,
      session: emptyAgentSession(),
      discoveryFresh: true,
      readiness,
      ...overrides,
    };
  }

  it("advances to the observed head when every gate condition passes", () => {
    const decision = evaluateLedgerReadMarkerGate(gateInput(), "agent-a");
    expect(decision).toEqual({ mayAdvance: true, candidateSeq: 12 });
  });

  it("blocks when the agent is not selected", () => {
    const decision = evaluateLedgerReadMarkerGate(
      gateInput({ route: "dashboard", selectedAgentId: "agent-b" }),
      "agent-a",
    );
    expect(decision.reason).toBe("not_selected");
  });

  it("blocks while the document is hidden", () => {
    const decision = evaluateLedgerReadMarkerGate(gateInput({ documentVisible: false }), "agent-a");
    expect(decision.reason).toBe("document_hidden");
  });

  it("blocks while the session is loading, gapped, or recovering", () => {
    const loading = evaluateLedgerReadMarkerGate(
      gateInput({ session: { ...emptyAgentSession(), loading: true } }),
      "agent-a",
    );
    expect(loading.reason).toBe("session_not_ready");
    const gapped = evaluateLedgerReadMarkerGate(
      gateInput({
        session: { ...emptyAgentSession(), gaps: [{ afterSeq: 1, beforeSeq: 9 }] },
      }),
      "agent-a",
    );
    expect(gapped.reason).toBe("session_not_ready");
    const recovering = evaluateLedgerReadMarkerGate(
      gateInput({ session: { ...emptyAgentSession(), syncStatus: "recovering" } }),
      "agent-a",
    );
    expect(recovering.reason).toBe("session_not_ready");
  });

  it("blocks while discovery is stale", () => {
    const decision = evaluateLedgerReadMarkerGate(gateInput({ discoveryFresh: false }), "agent-a");
    expect(decision.reason).toBe("discovery_stale");
  });

  it("blocks when the ledger readiness gate is unavailable", () => {
    const decision = evaluateLedgerReadMarkerGate(gateInput({ readiness: null }), "agent-a");
    expect(decision.reason).toBe("ledger_unavailable");
  });

  it("blocks when catch-up has not reached the observed head", () => {
    const decision = evaluateLedgerReadMarkerGate(
      gateInput({ readiness: { ...readiness, ingestedThroughSeq: 10 } }),
      "agent-a",
    );
    expect(decision.reason).toBe("not_caught_up");
  });

  it("blocks when an unresolved display invalidation holds readiness below the head", () => {
    const decision = evaluateLedgerReadMarkerGate(
      gateInput({
        readiness: { ...readiness, readyThroughSeq: 9, blockedByEventSeq: 10 },
      }),
      "agent-a",
    );
    expect(decision.reason).toBe("blocked_by_invalidation");
  });

  it("blocks without an observed head", () => {
    const decision = evaluateLedgerReadMarkerGate(
      gateInput({ readiness: { ...readiness, observedHeadSeq: undefined } }),
      "agent-a",
    );
    expect(decision.reason).toBe("no_observed_head");
  });
});

describe("unread badge view", () => {
  it("prefers the ledger view over the legacy count", () => {
    expect(unreadBadgeView(7, { mode: "exact", count: 3 })).toEqual({ mode: "exact", count: 3 });
  });

  it("does not use roster activity as an unread correctness fallback", () => {
    expect(unreadBadgeView(4, undefined)).toBeNull();
    expect(unreadBadgeView(0, undefined)).toBeNull();
    expect(unreadBadgeView(undefined, undefined)).toBeNull();
  });

  it("keeps a zero ledger view distinct from no view", () => {
    expect(unreadBadgeView(9, { mode: "exact", count: 0 })).toEqual({ mode: "exact", count: 0 });
  });
});
