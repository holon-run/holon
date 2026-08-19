import "fake-indexeddb/auto";

import { beforeEach, describe, expect, it } from "vitest";

import { EventLedger } from "./ledger";
import type { LedgerScopeKey } from "./keys";
import { readMarkerBoundary } from "./read-markers";

function makeScope(): LedgerScopeKey {
  return {
    remoteKey: "http://127.0.0.1:7878",
    runtimeId: "rt_test",
    visibilityScopeId: "vis_test",
    eventLogEpoch: "epoch-1",
    agentId: "agent-a",
  };
}

function envelope(eventSeq: number, type: string): Record<string, unknown> {
  return { id: `event-${eventSeq}`, agent_id: "agent-a", event_seq: eventSeq, type, payload: {} };
}

async function seedReadState(
  ledger: EventLedger,
  scope: LedgerScopeKey,
  patch: {
    unreadBaselineSeq?: number;
    readThroughEventSeq?: number;
    certainty?: "exact" | "truncated";
    historyTruncatedBeforeSeq?: number;
  },
): Promise<void> {
  await ledger
    .beginWrite()
    .putReadState(scope, patch)
    .commit();
}

async function seedRawEvents(
  ledger: EventLedger,
  scope: LedgerScopeKey,
  events: Array<{ seq: number; type: string; effect: "none" | "display_invalidation" }>,
): Promise<void> {
  const batch = ledger.beginWrite();
  for (const item of events) {
    batch.putRawEvent(
      scope,
      item.seq,
      envelope(item.seq, item.type),
      { projectionEffect: item.effect },
    );
  }
  await batch.commit();
}

async function openLedger(): Promise<EventLedger> {
  const result = await EventLedger.open();
  expect(result.kind).toBe("available");
  if (result.kind !== "available") throw new Error("unreachable");
  return result.ledger;
}

describe("read markers", () => {
  beforeEach(async () => {
    await EventLedger.deleteDatabase();
  });

  it("advances the read marker as a monotonic maximum across concurrent tabs", async () => {
    const scope = makeScope();
    const tabA = await openLedger();
    const tabB = await openLedger();
    await seedReadState(tabA, scope, { unreadBaselineSeq: 10, certainty: "exact" });

    // Two tabs race marker advances; only the maximum may survive.
    const [fromA, fromB] = await Promise.all([
      tabA.advanceReadMarker(scope, 15),
      tabB.advanceReadMarker(scope, 12),
    ]);
    const maxFirst = Math.max(fromA.record.readThroughEventSeq ?? 0, fromB.record.readThroughEventSeq ?? 0);
    expect(maxFirst).toBe(15);

    const later = await tabB.advanceReadMarker(scope, 20);
    expect(later.advanced).toBe(true);
    expect(later.record.readThroughEventSeq).toBe(20);

    // A stale tab cannot regress the marker.
    const stale = await tabA.advanceReadMarker(scope, 16);
    expect(stale.advanced).toBe(false);
    const settled = await tabA.getReadState(scope);
    expect(settled?.readThroughEventSeq).toBe(20);
    tabA.close();
    tabB.close();
  });

  it("rejects marker candidates that are not non-negative integers", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    await expect(ledger.advanceReadMarker(scope, -1)).rejects.toThrow();
    await expect(ledger.advanceReadMarker(scope, 1.5)).rejects.toThrow();
    ledger.close();
  });

  it("acknowledges truncation by opening a new exact generation without rewriting history", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    await seedReadState(ledger, scope, {
      unreadBaselineSeq: 3,
      readThroughEventSeq: 2,
      certainty: "truncated",
      historyTruncatedBeforeSeq: 5,
    });

    const record = await ledger.acknowledgeReadTruncation(scope, 9);

    expect(record).not.toBeNull();
    expect(record?.certainty).toBe("exact");
    expect(record?.unreadBaselineSeq).toBe(9);
    expect(record?.acknowledgedTruncationBeforeSeq).toBe(9);
    // Historical truncation facts survive acknowledgement.
    expect(record?.historyTruncatedBeforeSeq).toBe(5);
    expect(record?.readThroughEventSeq).toBe(2);

    // Re-acknowledging at a lower head never regresses the facts.
    const again = await ledger.acknowledgeReadTruncation(scope, 7);
    expect(again?.acknowledgedTruncationBeforeSeq).toBe(9);
    expect(again?.unreadBaselineSeq).toBe(9);
    ledger.close();
  });

  it("acknowledging an absent read state changes nothing", async () => {
    const ledger = await openLedger();
    const record = await ledger.acknowledgeReadTruncation(makeScope(), 9);
    expect(record).toBeNull();
    ledger.close();
  });

  it("counts only qualifying brief events strictly above the boundary", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    await seedRawEvents(ledger, scope, [
      { seq: 8, type: "brief_created", effect: "none" },
      { seq: 11, type: "brief_created", effect: "none" },
      { seq: 12, type: "scheduler_tick", effect: "none" },
      { seq: 13, type: "message_enqueued", effect: "none" },
      { seq: 14, type: "brief_created", effect: "none" },
    ]);

    // Boundary from max(unreadBaselineSeq, readThroughEventSeq).
    await seedReadState(ledger, scope, { unreadBaselineSeq: 10 });
    expect(readMarkerBoundary(await ledger.getReadState(scope))).toBe(10);

    const count = await ledger.countQualifyingUnreadEvents(scope, 10, 14);
    expect(count).toBe(2);
    // Internal events never extend the range that matters: the count is
    // bounded by what the caller claims is hydrated.
    expect(await ledger.countQualifyingUnreadEvents(scope, 10, 11)).toBe(1);
    expect(await ledger.countQualifyingUnreadEvents(scope, 14, 14)).toBe(0);
    ledger.close();
  });

  it("keeps a fresh-profile baseline free of unread older history", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    await seedRawEvents(ledger, scope, [
      { seq: 1, type: "brief_created", effect: "none" },
      { seq: 2, type: "brief_created", effect: "none" },
      { seq: 3, type: "brief_created", effect: "none" },
    ]);
    // A fresh browser profile establishes its baseline at the snapshot
    // boundary; history below it starts as read, not as unread.
    await seedReadState(ledger, scope, { unreadBaselineSeq: 3, certainty: "exact" });
    expect(await ledger.countQualifyingUnreadEvents(scope, 3, 3)).toBe(0);
    expect(await ledger.countQualifyingUnreadEvents(scope, readMarkerBoundary(await ledger.getReadState(scope)), 3)).toBe(0);
    ledger.close();
  });

  it("keeps read states of different scopes isolated", async () => {
    const ledger = await openLedger();
    const scopeA = makeScope();
    const scopeB = { ...makeScope(), agentId: "agent-b" };
    await seedReadState(ledger, scopeA, { unreadBaselineSeq: 1 });
    await ledger.advanceReadMarker(scopeB, 7);
    expect((await ledger.getReadState(scopeA))?.readThroughEventSeq).toBeUndefined();
    expect((await ledger.getReadState(scopeB))?.readThroughEventSeq).toBe(7);
    ledger.close();
  });
});
