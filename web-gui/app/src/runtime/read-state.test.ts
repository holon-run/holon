import { describe, expect, it } from "vitest";

import type { StreamEventEnvelopeDto } from "./client";
import {
  cachedReadState,
  canMarkConversationRead,
  latestBriefDeliverySeq,
  markAgentDeliveriesRead,
  mergeCachedReadState,
  mergeCachedReadStates,
  readStoredRosterActivity,
  touchRosterActivityFromEvent,
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
