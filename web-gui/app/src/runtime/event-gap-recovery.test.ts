import { describe, expect, it, vi } from "vitest";

import { EventGapRecoveryTracker, recoverEventGap, type SequencedEvent } from "./event-gap-recovery";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function event(event_seq: number): SequencedEvent {
  return { event_seq };
}

function eventPage(events: SequencedEvent[], eventLogEpoch?: string) {
  return { events, eventLogEpoch };
}

describe("EventGapRecoveryTracker", () => {
  it("keeps the contiguous cursor behind an observed high watermark", () => {
    const tracker = new EventGapRecoveryTracker();
    tracker.register("agent-a", 10);

    expect(tracker.observe("agent-a", 13)).toEqual({
      contiguousSeq: 10,
      highestObservedSeq: 13,
      recovering: true,
    });
    expect(tracker.observe("agent-a", 10)).toEqual({
      contiguousSeq: 10,
      highestObservedSeq: 13,
      recovering: true,
    });
  });

  it("captures the old cursor and continues when another gap arrives during backfill", async () => {
    const tracker = new EventGapRecoveryTracker();
    tracker.register("agent-a", 10);
    tracker.observe("agent-a", 13);
    const firstPage = deferred<ReturnType<typeof eventPage>>();
    const fetchPage = vi
      .fn<(afterSeq: number) => Promise<ReturnType<typeof eventPage>>>()
      .mockImplementationOnce(() => firstPage.promise)
      .mockResolvedValueOnce(eventPage([event(14), event(15)]))
      .mockResolvedValueOnce(eventPage([]));
    const applied: number[][] = [];

    const recovery = recoverEventGap(tracker, "agent-a", {
      limit: 100,
      fetchPage,
      applyEvents: (events) => applied.push(events.map((item) => item.event_seq as number)),
    });
    expect(fetchPage).toHaveBeenNthCalledWith(1, 10);

    tracker.observe("agent-a", 15);
    firstPage.resolve(eventPage([event(11), event(12), event(13)]));
    await recovery;

    expect(fetchPage).toHaveBeenNthCalledWith(2, 13);
    expect(applied).toEqual([
      [11, 12, 13],
      [14, 15],
    ]);
    expect(tracker.snapshotFor("agent-a")).toEqual({
      contiguousSeq: 15,
      highestObservedSeq: 15,
      recovering: false,
    });
  });

  it("paginates without skipping, ignores duplicates, and does not run concurrent backfills", async () => {
    const tracker = new EventGapRecoveryTracker();
    tracker.register("agent-a", 20);
    tracker.observe("agent-a", 24);
    const firstPage = deferred<ReturnType<typeof eventPage>>();
    const fetchPage = vi
      .fn<(afterSeq: number) => Promise<ReturnType<typeof eventPage>>>()
      .mockImplementationOnce(() => firstPage.promise)
      .mockResolvedValueOnce(eventPage([event(23), event(24)]))
      .mockResolvedValueOnce(eventPage([]));

    const firstRecovery = recoverEventGap(tracker, "agent-a", {
      limit: 2,
      fetchPage,
      applyEvents: () => undefined,
    });
    await recoverEventGap(tracker, "agent-a", {
      limit: 2,
      fetchPage,
      applyEvents: () => undefined,
    });
    firstPage.resolve(eventPage([event(21), event(21), event(22)]));
    await firstRecovery;

    expect(fetchPage).toHaveBeenCalledTimes(3);
    expect(fetchPage).toHaveBeenNthCalledWith(1, 20);
    expect(fetchPage).toHaveBeenNthCalledWith(2, 22);
    expect(fetchPage).toHaveBeenNthCalledWith(3, 24);
    expect(tracker.snapshotFor("agent-a")?.contiguousSeq).toBe(24);
  });

  it("keeps a failed gap recoverable and clears unregistered state", async () => {
    const tracker = new EventGapRecoveryTracker();
    tracker.register("agent-a", 30);
    tracker.observe("agent-a", 32);

    await expect(
      recoverEventGap(tracker, "agent-a", {
        limit: 100,
        fetchPage: async () => {
          throw new Error("temporary failure");
        },
        applyEvents: () => undefined,
      }),
    ).rejects.toThrow("temporary failure");
    expect(tracker.snapshotFor("agent-a")?.recovering).toBe(true);

    await recoverEventGap(tracker, "agent-a", {
      limit: 100,
      fetchPage: async () => eventPage([event(31), event(32)]),
      applyEvents: () => undefined,
    });
    expect(tracker.snapshotFor("agent-a")?.recovering).toBe(false);

    tracker.unregister("agent-a");
    expect(tracker.snapshotFor("agent-a")).toBeUndefined();
  });

  it("does not let a stale backfill mutate a re-registered agent", async () => {
    const tracker = new EventGapRecoveryTracker();
    tracker.register("agent-a", 40);
    tracker.observe("agent-a", 42);
    const deferredPage = deferred<ReturnType<typeof eventPage>>();

    const recovery = recoverEventGap(tracker, "agent-a", {
      limit: 100,
      fetchPage: () => deferredPage.promise,
      applyEvents: () => undefined,
    });
    tracker.unregister("agent-a");
    tracker.register("agent-a", 100);
    deferredPage.resolve(eventPage([event(41), event(42)]));
    await recovery;

    expect(tracker.snapshotFor("agent-a")).toEqual({
      contiguousSeq: 100,
      highestObservedSeq: 100,
      recovering: false,
    });
  });

  it("restarts from zero when an empty page reports a new event log epoch", async () => {
    const tracker = new EventGapRecoveryTracker();
    tracker.register("agent-a", 40, "epoch-old");
    tracker.observe("agent-a", 42, "epoch-old");
    const fetchPage = vi
      .fn<(afterSeq: number) => Promise<ReturnType<typeof eventPage>>>()
      .mockResolvedValueOnce(eventPage([], "epoch-new"))
      .mockResolvedValueOnce(eventPage([
        { event_seq: 1, event_log_epoch: "epoch-new" },
        { event_seq: 2, event_log_epoch: "epoch-new" },
      ], "epoch-new"));
    const applied: number[][] = [];

    await recoverEventGap(tracker, "agent-a", {
      limit: 100,
      fetchPage,
      applyEvents: (events) => applied.push(events.map((item) => item.event_seq as number)),
    });

    expect(fetchPage).toHaveBeenNthCalledWith(1, 40);
    expect(fetchPage).toHaveBeenNthCalledWith(2, 0);
    expect(applied).toEqual([[1, 2]]);
    expect(tracker.snapshotFor("agent-a")).toEqual({
      contiguousSeq: 2,
      highestObservedSeq: 2,
      recovering: false,
    });
  });
});
