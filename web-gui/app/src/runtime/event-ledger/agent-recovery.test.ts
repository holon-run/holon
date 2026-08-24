import "fake-indexeddb/auto";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  LEDGER_DB_NAME,
  type AgentReplayBudget,
  type LedgerHydrationFetchers,
  type LedgerScopeKey,
  type RecoveryEventPage,
  type RecoveryProjectionSnapshot,
} from "./index";
import { EventLedger } from "./ledger";
import { LedgerIngestionPipeline } from "./ingestion-pipeline";
import { AgentRecoveryCoordinator } from "./agent-recovery";

const REMOTE_KEY = "http://127.0.0.1:7878";

function makeScope(overrides: Partial<LedgerScopeKey> = {}): LedgerScopeKey {
  return {
    remoteKey: REMOTE_KEY,
    runtimeId: "rt_test",
    visibilityScopeId: "vis_test",
    eventLogEpoch: "epoch-1",
    agentId: "agent-1",
    ...overrides,
  };
}

function envelope(
  seq: number,
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    agent_id: "agent-1",
    contract_version: 2,
    event_log_epoch: "epoch-1",
    id: `evt-${seq}`,
    event_seq: seq,
    payload: {},
    payload_schema: "test",
    payload_schema_version: 1,
    provenance: {},
    ts: `2026-08-18T00:00:${String(seq % 60).padStart(2, "0")}Z`,
    type: "agent_state_changed",
    ...overrides,
  };
}

function snapshot(
  overrides: Partial<RecoveryProjectionSnapshot> = {},
): RecoveryProjectionSnapshot {
  return {
    runtimeId: "rt_test",
    visibilityScopeId: "vis_test",
    eventLogEpoch: "epoch-1",
    snapshotThroughSeq: 5,
    eventHeadSeq: 8,
    oldestRetainedSeq: 0,
    canonicalRecords: [
      {
        recordKind: "brief",
        recordId: "brief-5",
        record: { brief_id: "brief-5", preview: "latest" },
        revision: 5,
      },
    ],
    hydrationReferences: [{ recordKind: "message", recordId: "msg-1" }],
    hydrationTombstones: [{ recordKind: "message", recordId: "msg-gone" }],
    ...overrides,
  };
}

function emptyFetchers(): LedgerHydrationFetchers {
  return {
    fetchCanonicalRecords: async () => ({ recordsById: {}, missingIds: [] }),
  };
}

function page(
  events: Array<Record<string, unknown>>,
  overrides: Partial<RecoveryEventPage> = {},
): RecoveryEventPage {
  return { events, hasNewer: false, ...overrides };
}

/** Scripted event pages keyed by the afterSeq they were requested with. */
function pageSource(pages: Record<number, RecoveryEventPage>) {
  const requests: number[] = [];
  return {
    requests,
    fetch: async (_agentId: string, afterSeq: number): Promise<RecoveryEventPage> => {
      requests.push(afterSeq);
      return pages[afterSeq] ?? page([]);
    },
  };
}

async function openLedgerHandle(): Promise<EventLedger> {
  const result = await EventLedger.open();
  if (result.kind !== "available") throw new Error("expected available ledger");
  return result.ledger;
}

function deleteLedger(): Promise<void> {
  return new Promise((resolve) => {
    const request = indexedDB.deleteDatabase(LEDGER_DB_NAME);
    request.onsuccess = () => resolve();
    request.onerror = () => resolve();
    request.onblocked = () => resolve();
  });
}

function makeCoordinator(
  pipeline: LedgerIngestionPipeline,
  fetchProjectionSnapshot: (agentId: string) => Promise<RecoveryProjectionSnapshot | null>,
  fetchEventPage: (agentId: string, afterSeq: number, limit: number) => Promise<RecoveryEventPage>,
  options: { replayBudget?: Partial<AgentReplayBudget>; now?: () => number } = {},
) {
  return new AgentRecoveryCoordinator({
    remoteKey: REMOTE_KEY,
    pipeline,
    fetchProjectionSnapshot,
    fetchEventPage,
    ...options,
  });
}

describe("agent recovery coordinator", () => {
  beforeEach(async () => {
    await deleteLedger();
  });

  afterEach(async () => {
    vi.restoreAllMocks();
    await deleteLedger();
  });

  it("bootstraps a new visible agent: install, baseline, replay, live", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const source = pageSource({ 5: page([envelope(6), envelope(7), envelope(8)]) });
    const coordinator = makeCoordinator(pipeline, async () => snapshot(), source.fetch);

    const update = await coordinator.sync("agent-1");
    expect(update.phase).toBe("live");
    expect(update.scope).toEqual(makeScope());
    expect(update.ingestedThroughSeq).toBe(8);
    expect(source.requests).toEqual([5]);

    const ledger = await openLedgerHandle();
    const session = await ledger.getAgentSession(makeScope());
    expect(session?.ingestedThroughSeq).toBe(8);
    expect(session?.projectionReadyThroughSeq).toBe(8);
    const readState = await ledger.getReadState(makeScope());
    expect(readState?.unreadBaselineSeq).toBe(5);
    expect(readState?.certainty).toBe("exact");
    expect(readState?.readThroughEventSeq).toBeUndefined();
    const brief = await ledger.getCanonicalRecord(makeScope(), "brief", "brief-5");
    expect(brief?.revision).toBe(5);
    const gone = await ledger.getCanonicalRecord(makeScope(), "message", "msg-gone");
    expect(gone?.record).toEqual({
      tombstone: true,
      deletedAt: expect.any(Number),
      deletedByEventSeq: 5,
    });
    const jobs = await ledger.getPendingHydrationJobs(makeScope());
    expect(jobs.map((job) => job.jobId)).toContain("message:msg-1");
    expect(jobs.find((job) => job.jobId === "message:msg-1")?.createdByEventSeq).toBe(5);
    const runtimeScope = await ledger.getRuntimeScope({
      remoteKey: REMOTE_KEY,
      runtimeId: "rt_test",
      visibilityScopeId: "vis_test",
      eventLogEpoch: "epoch-1",
    });
    expect(runtimeScope?.eventHeadSeq).toBe(8);
    ledger.close();
  });

  it("skips bootstrap when the snapshot capability is absent", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const coordinator = makeCoordinator(pipeline, async () => null, async () => page([]));

    const update = await coordinator.sync("agent-1");
    expect(update.phase).toBe("idle");
    expect(update.skipped).toBe("capability_absent");
    expect(coordinator.capabilitySkipped("agent-1")).toBe(true);

    const ledger = await openLedgerHandle();
    expect(await ledger.getAgentSession(makeScope())).toBeUndefined();
    ledger.close();
  });

  it("buffers live hints during recovery and replays them without loss or duplication", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    let releaseSnapshot: ((value: RecoveryProjectionSnapshot) => void) | null = null;
    const snapshotGate = new Promise<RecoveryProjectionSnapshot>((resolve) => {
      releaseSnapshot = resolve;
    });
    const coordinator = makeCoordinator(
      pipeline,
      () => snapshotGate,
      async () => page([envelope(6), envelope(7), envelope(8)]),
    );

    const syncPromise = coordinator.sync("agent-1");
    await vi.waitFor(() => expect(coordinator.phaseOf("agent-1")).toBe("fetching_snapshot"));
    // A live event arrives while the snapshot fetch is still in flight.
    const buffered = await coordinator.offer("agent-1", [envelope(9)]);
    expect(buffered).toBeNull();
    releaseSnapshot!(snapshot());
    const update = await syncPromise;
    expect(update.phase).toBe("live");
    expect(update.ingestedThroughSeq).toBe(9);

    // Duplicates stay idempotent after live.
    const duplicate = await coordinator.offer("agent-1", [envelope(9)]);
    expect(duplicate?.ingestedThroughSeq).toBe(9);

    const ledger = await openLedgerHandle();
    const events = await ledger.getRawEvents(makeScope());
    expect(events.filter((event) => event.eventSeq === 9)).toHaveLength(1);
    const session = await ledger.getAgentSession(makeScope());
    expect(session?.ingestedThroughSeq).toBe(9);
    ledger.close();
  });

  it("catches up an existing contiguous cache exactly to the observed head", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();
    await pipeline.ingest(scope, [envelope(1), envelope(2), envelope(3), envelope(4), envelope(5)]);
    const source = pageSource({ 5: page([envelope(6), envelope(7)]), 7: page([envelope(8)]) });
    const coordinator = makeCoordinator(pipeline, async () => snapshot(), source.fetch);

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 8 });
    expect(update.phase).toBe("live");
    expect(update.ingestedThroughSeq).toBe(8);
    expect(source.requests).toEqual([5, 7]);
    // A plain catch-up never installs a read baseline.
    const ledger = await openLedgerHandle();
    expect(await ledger.getReadState(scope)).toBeUndefined();
    ledger.close();
  });

  it("prioritizes a retention gap over replay cost on the live fast path", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    let snapshotFetches = 0;
    const coordinator = makeCoordinator(
      pipeline,
      async () => {
        snapshotFetches += 1;
        return snapshot(
          snapshotFetches === 1
            ? { snapshotThroughSeq: 5, eventHeadSeq: 5 }
            : { snapshotThroughSeq: 100, eventHeadSeq: 100, oldestRetainedSeq: 7 },
        );
      },
      async () => page([]),
      { replayBudget: { maxEstimatedGap: 1 } },
    );
    expect((await coordinator.sync("agent-1")).phase).toBe("live");

    const update = await coordinator.sync("agent-1", {
      eventHeadSeq: 100,
      oldestRetainedSeq: 7,
    });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("retained_prefix_gap");
    expect(snapshotFetches).toBe(2);
  });

  it("bootstraps before fetching an over-budget retained suffix", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();
    await pipeline.ingest(scope, [envelope(1), envelope(2), envelope(3), envelope(4), envelope(5)]);
    const ledger = await openLedgerHandle();
    await ledger
      .beginWrite()
      .putReadState(scope, { unreadBaselineSeq: 3, readThroughEventSeq: 4 })
      .commit();
    ledger.close();

    let snapshotFetches = 0;
    const source = pageSource({
      95: page([envelope(96), envelope(97), envelope(98), envelope(99), envelope(100)]),
    });
    const coordinator = makeCoordinator(
      pipeline,
      async () => {
        snapshotFetches += 1;
        return snapshot({ snapshotThroughSeq: 95, eventHeadSeq: 100 });
      },
      source.fetch,
      { replayBudget: { maxEstimatedGap: 10 } },
    );

    const update = await coordinator.sync("agent-1", {
      eventHeadSeq: 100,
      oldestRetainedSeq: 0,
    });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("replay_budget_exceeded");
    expect(snapshotFetches).toBe(1);
    expect(source.requests).toEqual([95]);

    const verify = await openLedgerHandle();
    expect((await verify.getRawEvents(scope)).map((event) => event.eventSeq)).toEqual([
      96, 97, 98, 99, 100,
    ]);
    const readState = await verify.getReadState(scope);
    expect(readState?.readThroughEventSeq).toBe(4);
    expect(readState?.unreadBaselineSeq).toBe(3);
    expect(readState?.historyTruncatedBeforeSeq).toBe(96);
    expect(readState?.certainty).toBe("truncated");
    verify.close();
  });

  it("bootstraps once when the page budget is exhausted mid-replay", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();
    await pipeline.ingest(scope, [envelope(1), envelope(2)]);
    const ledger = await openLedgerHandle();
    await ledger.beginWrite().putReadState(scope, { readThroughEventSeq: 2 }).commit();
    ledger.close();

    let snapshotFetches = 0;
    const source = pageSource({
      2: page([envelope(3), envelope(4)], { hasNewer: true }),
      5: page([envelope(6)]),
    });
    const coordinator = makeCoordinator(
      pipeline,
      async () => {
        snapshotFetches += 1;
        return snapshot({ snapshotThroughSeq: 5, eventHeadSeq: 6 });
      },
      source.fetch,
      { replayBudget: { maxPages: 1 } },
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 6 });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("replay_budget_exceeded");
    expect(snapshotFetches).toBe(1);
    expect(source.requests).toEqual([2, 5]);
    const verify = await openLedgerHandle();
    expect((await verify.getAgentSession(scope))?.ingestedThroughSeq).toBe(6);
    expect((await verify.getReadState(scope))?.certainty).toBe("truncated");
    verify.close();
  });

  it.each([
    {
      budgetName: "event",
      replayBudget: { maxEvents: 1 },
      firstPage: page([envelope(3), envelope(4)]),
    },
    {
      budgetName: "byte",
      replayBudget: { maxBytes: 10 },
      firstPage: page([envelope(3)], { responseBytes: 11 }),
    },
  ])("bootstraps once when the $budgetName budget is exhausted", async ({
    replayBudget,
    firstPage,
  }) => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    await pipeline.ingest(makeScope(), [envelope(1), envelope(2)]);
    const source = pageSource({
      2: firstPage,
      5: page([envelope(6)], { responseBytes: 1 }),
    });
    const coordinator = makeCoordinator(
      pipeline,
      async () => snapshot({ snapshotThroughSeq: 5, eventHeadSeq: 6 }),
      source.fetch,
      { replayBudget },
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 6 });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("replay_budget_exceeded");
    expect(source.requests).toEqual([2, 5]);
  });

  it("includes fetch and apply time in the replay budget", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    await pipeline.ingest(makeScope(), [envelope(1), envelope(2)]);
    const source = pageSource({ 2: page([envelope(3)]), 5: page([envelope(6)]) });
    let clockReads = 0;
    const coordinator = makeCoordinator(
      pipeline,
      async () => snapshot({ snapshotThroughSeq: 5, eventHeadSeq: 6 }),
      source.fetch,
      {
        replayBudget: { maxElapsedMs: 5 },
        now: () => (clockReads++ < 2 ? 0 : 10),
      },
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 6 });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("replay_budget_exceeded");
    expect(source.requests).toEqual([2, 5]);
  });

  it("fails instead of entering live when replay stays over budget after bootstrap", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    await pipeline.ingest(makeScope(), [envelope(1), envelope(2)]);
    let snapshotFetches = 0;
    const source = pageSource({
      2: page([envelope(3), envelope(4)]),
      4: page([envelope(5), envelope(6)]),
    });
    const coordinator = makeCoordinator(
      pipeline,
      async () => {
        snapshotFetches += 1;
        return snapshot({ snapshotThroughSeq: 4, eventHeadSeq: 6 });
      },
      source.fetch,
      { replayBudget: { maxEvents: 1 } },
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 6 });
    expect(update.phase).toBe("error");
    expect(update.error).toBe("replay_budget_exceeded_after_snapshot");
    expect(update.resetReason).toBe("replay_budget_exceeded");
    expect(snapshotFetches).toBe(1);
    expect(source.requests).toEqual([2, 4]);
  });

  it("resets on a retained-prefix gap: keep the marker, record truncation, rebuild", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();
    await pipeline.ingest(scope, [envelope(1), envelope(2), envelope(3), envelope(4), envelope(5)]);
    const ledger = await openLedgerHandle();
    await ledger
      .beginWrite()
      .putReadState(scope, { unreadBaselineSeq: 3, readThroughEventSeq: 4 })
      .commit();
    ledger.close();

    const source = pageSource({ 8: page([]) });
    const coordinator = makeCoordinator(
      pipeline,
      async () => snapshot({ snapshotThroughSeq: 8, eventHeadSeq: 8, oldestRetainedSeq: 7 }),
      source.fetch,
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 8, oldestRetainedSeq: 7 });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("retained_prefix_gap");

    const verify = await openLedgerHandle();
    const readState = await verify.getReadState(scope);
    // Marker survives; truncation is recorded; certainty drops to truncated
    // because the effective boundary (4) is below the floor - 1 (6).
    expect(readState?.readThroughEventSeq).toBe(4);
    expect(readState?.unreadBaselineSeq).toBe(3);
    expect(readState?.historyTruncatedBeforeSeq).toBe(7);
    expect(readState?.certainty).toBe("truncated");
    expect(await verify.getRawEvents(scope)).toEqual([]);
    expect((await verify.getAgentSession(scope))?.ingestedThroughSeq).toBe(8);
    verify.close();
  });

  it("resets a runtime epoch: clears the old scope and never migrates markers", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const oldScope = makeScope();
    await pipeline.ingest(oldScope, [envelope(1), envelope(2)]);
    const ledger = await openLedgerHandle();
    await ledger
      .beginWrite()
      .putReadState(oldScope, { unreadBaselineSeq: 1, readThroughEventSeq: 2 })
      .commit();
    ledger.close();

    // Catch-up pages reveal a rotated epoch before any data is applied.
    const source = pageSource({
      2: page([], { eventLogEpoch: "epoch-2", eventHeadSeq: 6 }),
      6: page([]),
    });
    const coordinator = makeCoordinator(
      pipeline,
      async () => snapshot({ eventLogEpoch: "epoch-2", snapshotThroughSeq: 6, eventHeadSeq: 6 }),
      source.fetch,
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 6 });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("epoch_change");

    const verify = await openLedgerHandle();
    expect(await verify.getAgentSession(oldScope)).toBeUndefined();
    expect(await verify.getReadState(oldScope)).toBeUndefined();
    expect(await verify.getRawEvents(oldScope)).toEqual([]);
    expect(
      await verify.getRuntimeScope({
        remoteKey: REMOTE_KEY,
        runtimeId: "rt_test",
        visibilityScopeId: "vis_test",
        eventLogEpoch: "epoch-1",
      }),
    ).toBeUndefined();
    const newScope = makeScope({ eventLogEpoch: "epoch-2" });
    const readState = await verify.getReadState(newScope);
    expect(readState?.unreadBaselineSeq).toBe(6);
    expect(readState?.readThroughEventSeq).toBeUndefined();
    expect(readState?.certainty).toBe("exact");
    expect((await verify.getAgentSession(newScope))?.ingestedThroughSeq).toBe(6);
    verify.close();
  });

  it("clears an old visibility scope before exposing the new scope", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const oldScope = makeScope();
    await pipeline.ingest(oldScope, [envelope(1)]);
    const ledger = await openLedgerHandle();
    await ledger.beginWrite().putReadState(oldScope, { unreadBaselineSeq: 1 }).commit();
    ledger.close();

    const coordinator = makeCoordinator(
      pipeline,
      async () => snapshot({ visibilityScopeId: "vis_other", snapshotThroughSeq: 4, eventHeadSeq: 4 }),
      async () => page([]),
    );

    // A forced divergence re-bootstrap discovers the rotated visibility.
    const update = await coordinator.sync("agent-1", {}, {
      forceReset: "hydration_divergence",
    });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("visibility_scope_change");
    expect(update.scope).toEqual(makeScope({ visibilityScopeId: "vis_other" }));

    const verify = await openLedgerHandle();
    // Old scope's accessible cache is gone before new data is visible.
    expect(await verify.getAgentSession(oldScope)).toBeUndefined();
    expect(await verify.getReadState(oldScope)).toBeUndefined();
    expect(await verify.getRawEvents(oldScope)).toEqual([]);
    const newScope = makeScope({ visibilityScopeId: "vis_other" });
    expect((await verify.getAgentSession(newScope))?.ingestedThroughSeq).toBe(4);
    expect((await verify.getReadState(newScope))?.unreadBaselineSeq).toBe(4);
    verify.close();
  });

  it("does not preserve read state when a budget reset discovers a new scope", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const oldScope = makeScope();
    await pipeline.ingest(oldScope, [envelope(1), envelope(2)]);
    const ledger = await openLedgerHandle();
    await ledger
      .beginWrite()
      .putReadState(oldScope, { unreadBaselineSeq: 1, readThroughEventSeq: 2 })
      .commit();
    ledger.close();

    const coordinator = makeCoordinator(
      pipeline,
      async () =>
        snapshot({
          visibilityScopeId: "vis_other",
          snapshotThroughSeq: 100,
          eventHeadSeq: 100,
        }),
      async () => page([]),
      { replayBudget: { maxEstimatedGap: 1 } },
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 100 });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("visibility_scope_change");

    const verify = await openLedgerHandle();
    expect(await verify.getReadState(oldScope)).toBeUndefined();
    const newScope = makeScope({ visibilityScopeId: "vis_other" });
    const readState = await verify.getReadState(newScope);
    expect(readState?.unreadBaselineSeq).toBe(100);
    expect(readState?.readThroughEventSeq).toBeUndefined();
    expect(readState?.historyTruncatedBeforeSeq).toBeUndefined();
    expect(readState?.certainty).toBe("exact");
    verify.close();
  });

  it("recovers from cursor_not_found with one bounded retention reset", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();
    await pipeline.ingest(scope, [envelope(1), envelope(2), envelope(3)]);
    const source = pageSource({
      3: {
        events: [],
        responseBytes: 100,
        cursorNotFound: {
          afterSeq: 3,
          eventLogEpoch: "epoch-1",
          oldestRetainedSeq: 6,
          eventHeadSeq: 9,
        },
      },
      9: page([]),
    });
    const coordinator = makeCoordinator(
      pipeline,
      async () => snapshot({ snapshotThroughSeq: 9, eventHeadSeq: 9, oldestRetainedSeq: 6 }),
      source.fetch,
      { replayBudget: { maxBytes: 1 } },
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 9 });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("cursor_error");

    const verify = await openLedgerHandle();
    // No marker existed before the reset, so the rebuilt generation gets a
    // fresh exact baseline while the truncation boundary stays recorded.
    const readState = await verify.getReadState(scope);
    expect(readState?.historyTruncatedBeforeSeq).toBe(6);
    expect(readState?.certainty).toBe("exact");
    expect(readState?.unreadBaselineSeq).toBe(9);
    expect((await verify.getAgentSession(scope))?.ingestedThroughSeq).toBe(9);
    verify.close();
  });

  it("recovers from an immutable content conflict via re-bootstrap", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();
    await pipeline.ingest(scope, [envelope(1), envelope(2, { payload: { original: true } })]);

    const source = pageSource({
      2: page([envelope(3), envelope(2, { payload: { rewritten: true } })]),
      1: page([envelope(2, { payload: { rewritten: true } }), envelope(3)]),
    });
    const coordinator = makeCoordinator(
      pipeline,
      async () => snapshot({ snapshotThroughSeq: 1, eventHeadSeq: 3 }),
      source.fetch,
    );

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 3 });
    expect(update.phase).toBe("live");
    expect(update.resetReason).toBe("immutable_conflict");

    const verify = await openLedgerHandle();
    expect((await verify.getAgentSession(scope))?.ingestedThroughSeq).toBe(3);
    expect(await verify.getRawEvent(scope, 2)).toMatchObject({
      envelope: { payload: { rewritten: true } },
    });
    verify.close();
  });

  it("re-bootstraps once on hydration divergence escalation", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();

    let snapshotFetches = 0;
    const coordinator = makeCoordinator(
      pipeline,
      async () => {
        snapshotFetches += 1;
        return snapshot({ snapshotThroughSeq: 2, eventHeadSeq: 2 });
      },
      async () => page([]),
    );
    const first = await coordinator.sync("agent-1");
    expect(first.phase).toBe("live");
    expect(snapshotFetches).toBe(1);

    expect(coordinator.requestDivergenceReset("agent-1")).toBe(true);
    await vi.waitFor(async () => {
      expect(snapshotFetches).toBe(2);
    });
    expect(coordinator.phaseOf("agent-1")).toBe("live");
    // A second escalation before another live stretch is refused.
    expect(coordinator.requestDivergenceReset("agent-1")).toBe(false);
  });

  it("drains hints that arrive during the pre-live drain after the live transition", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const source = pageSource({ 5: page([envelope(6), envelope(7), envelope(8)]) });
    let releaseReplay: ((value: RecoveryEventPage) => void) | null = null;
    const replayGate = new Promise<RecoveryEventPage>((resolve) => {
      releaseReplay = resolve;
    });
    const fetchEventPage = (): Promise<RecoveryEventPage> => replayGate;
    const coordinator = makeCoordinator(pipeline, async () => snapshot(), fetchEventPage);

    // Re-entrancy: while the pre-live drain ingests seq 9, a later live
    // event (10) is offered and buffers behind the still-settling sync.
    const realIngest = pipeline.ingest.bind(pipeline);
    let drainedNine = false;
    const ingestSpy = vi.spyOn(pipeline, "ingest").mockImplementation(async (scope, events) => {
      const result = await realIngest(scope, events);
      if (!drainedNine && events.some((event) => (event as { event_seq?: number }).event_seq === 9)) {
        drainedNine = true;
        void coordinator.offer("agent-1", [envelope(10)]).then(() => undefined);
      }
      return result;
    });

    const syncPromise = coordinator.sync("agent-1");
    await vi.waitFor(() => expect(coordinator.phaseOf("agent-1")).toBe("replaying"));
    const bufferedNine = await coordinator.offer("agent-1", [envelope(9)]);
    expect(bufferedNine).toBeNull();
    releaseReplay!(page([envelope(6), envelope(7), envelope(8)]));
    const update = await syncPromise;
    expect(update.phase).toBe("live");
    expect(update.ingestedThroughSeq).toBe(10);
    ingestSpy.mockRestore();

    const ledger = await openLedgerHandle();
    const events = await ledger.getRawEvents(makeScope());
    ledger.close();
    expect(events.map((event) => event.eventSeq)).toContain(10);
  });

  it("fails an empty-page spin early instead of paging toward maxPages", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();
    await pipeline.ingest(scope, [envelope(1), envelope(2)]);
    const source = pageSource({ 2: page([], { hasNewer: true }) });
    const coordinator = makeCoordinator(pipeline, async () => snapshot(), source.fetch);

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 50 });
    expect(update.phase).toBe("error");
    expect(update.error).toBe("empty_replay_page_no_progress");
    // Bounded: the stale-round guard stops the loop after three empty pages.
    expect(source.requests).toHaveLength(3);
  });

  it("never enters live when a page ends before the committed target head", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    await pipeline.ingest(makeScope(), [envelope(1), envelope(2)]);
    const source = pageSource({ 2: page([]) });
    const coordinator = makeCoordinator(pipeline, async () => snapshot(), source.fetch);

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 3 });
    expect(update.phase).toBe("error");
    expect(update.error).toBe("replay_ended_before_target");
    expect(source.requests).toEqual([2]);
  });

  it("clears each distinct stale runtime scope exactly once during an identity reset", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    // The same agent holds sessions in two distinct stale scopes; each is
    // cleared exactly once and the current scope is never cleared.
    const staleEpochScope = makeScope({ eventLogEpoch: "epoch-0" });
    const staleVisibilityScope = makeScope({ eventLogEpoch: "epoch-x" });
    await pipeline.ingest(staleEpochScope, [envelope(1, { event_log_epoch: "epoch-0" })]);
    await pipeline.ingest(staleVisibilityScope, [envelope(1, { event_log_epoch: "epoch-x" })]);

    const clearSpy = vi.spyOn(pipeline, "clearRuntimeScope");
    // Catch-up page metadata reveals the current epoch, so the catch-up
    // escalates into an identity reset bootstrap.
    const source = pageSource({
      1: page([], { eventLogEpoch: "epoch-1", eventHeadSeq: 8 }),
      5: page([envelope(6), envelope(7), envelope(8)]),
    });
    const coordinator = makeCoordinator(pipeline, async () => snapshot(), source.fetch);

    const update = await coordinator.sync("agent-1", { eventHeadSeq: 8 });
    expect(update.phase).toBe("live");
    const cleared = clearSpy.mock.calls.map(([scope]) => scope);
    expect(cleared).toHaveLength(2);
    expect(cleared).toContainEqual({
      remoteKey: staleEpochScope.remoteKey,
      runtimeId: staleEpochScope.runtimeId,
      visibilityScopeId: staleEpochScope.visibilityScopeId,
      eventLogEpoch: "epoch-0",
    });
    expect(cleared).toContainEqual({
      remoteKey: staleVisibilityScope.remoteKey,
      runtimeId: staleVisibilityScope.runtimeId,
      visibilityScopeId: staleVisibilityScope.visibilityScopeId,
      eventLogEpoch: staleVisibilityScope.eventLogEpoch,
    });
  });

  it("answers a live catch-up from the cached scope without the session scan", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();
    await pipeline.ingest(scope, [envelope(1), envelope(2), envelope(3)]);
    const source = pageSource({ 3: page([envelope(4)]) });
    const coordinator = makeCoordinator(pipeline, async () => snapshot(), source.fetch);

    const first = await coordinator.sync("agent-1");
    expect(first.phase).toBe("live");
    const findSpy = vi.spyOn(pipeline, "findAgentSessions");

    const second = await coordinator.sync("agent-1", { eventHeadSeq: 4 });
    expect(second.phase).toBe("live");
    expect(second.ingestedThroughSeq).toBe(4);
    expect(source.requests).toEqual([3]);
    expect(findSpy).not.toHaveBeenCalled();
  });
});
