import "fake-indexeddb/auto";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  LEDGER_DB_NAME,
  LedgerIdentityConflictError,
  type LedgerHydrationFetchers,
  type LedgerScopeKey,
} from "./index";
import { EventLedger } from "./ledger";
import { LedgerIngestionPipeline } from "./ingestion-pipeline";

function makeScope(overrides: Partial<LedgerScopeKey> = {}): LedgerScopeKey {
  return {
    remoteKey: "http://127.0.0.1:7878",
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
    event_seq: seq,
    id: `evt-${seq}`,
    payload: {},
    payload_schema: "test",
    payload_schema_version: 1,
    provenance: {},
    ts: `2026-08-18T00:00:${String(seq % 60).padStart(2, "0")}Z`,
    type: "agent_state_changed",
    ...overrides,
  };
}

function briefEvent(seq: number, briefId: string, payload: Record<string, unknown> = {}) {
  return envelope(seq, {
    id: `evt-${seq}`,
    type: "brief_created",
    payload: { brief_id: briefId, ...payload },
  });
}

function emptyFetchers(): LedgerHydrationFetchers {
  return {
    fetchCanonicalRecords: async () => ({ recordsById: {}, missingIds: [] }),
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

describe("ledger ingestion pipeline", () => {
  beforeEach(async () => {
    await deleteLedger();
  });

  afterEach(async () => {
    vi.restoreAllMocks();
    await deleteLedger();
  });

  it("ingests self-contained events and advances ingestion and readiness together", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    expect(await pipeline.open()).toBe(true);
    const scope = makeScope();

    const status = await pipeline.ingest(scope, [
      envelope(1),
      envelope(2, { type: "scheduler_diagnostic", projection_effect: "none" }),
      envelope(3),
    ]);

    expect(status.ingestedThroughSeq).toBe(3);
    expect(status.projectionReadyThroughSeq).toBe(3);
    expect(status.state).toBe("idle");
    expect(status.pendingHydrationJobs).toBe(0);

    const ledger = await openLedgerHandle();
    const session = await ledger.getAgentSession(scope);
    expect(session?.ingestedThroughSeq).toBe(3);
    expect(session?.projectionReadyThroughSeq).toBe(3);
    const runtimeScope = await ledger.getRuntimeScope({
      remoteKey: scope.remoteKey,
      runtimeId: scope.runtimeId,
      visibilityScopeId: scope.visibilityScopeId,
      eventLogEpoch: scope.eventLogEpoch,
    });
    expect(runtimeScope?.eventHeadSeq).toBe(3);
    ledger.close();
  });

  it("never reloads an observed head behind the persisted contiguous cursor", async () => {
    const scope = makeScope();
    const ledger = await openLedgerHandle();
    await ledger
      .beginWrite()
      .advanceIngestionCursor(scope, 2)
      .applyProjectionChange(scope, { projectionReadyThroughSeq: 2 })
      .putRuntimeScope(
        {
          remoteKey: scope.remoteKey,
          runtimeId: scope.runtimeId,
          visibilityScopeId: scope.visibilityScopeId,
          eventLogEpoch: scope.eventLogEpoch,
        },
        { eventHeadSeq: 0 },
      )
      .commit();
    ledger.close();

    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();

    expect(await pipeline.resume(scope)).toMatchObject({
      ingestedThroughSeq: 2,
      projectionReadyThroughSeq: 2,
      observedEventHeadSeq: 2,
    });
  });

  it("keeps the contiguous cursor behind out-of-order gaps", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();

    const afterTail = await pipeline.ingest(scope, [envelope(5), envelope(6), envelope(7)]);
    expect(afterTail.ingestedThroughSeq).toBeUndefined();
    expect(afterTail.observedEventHeadSeq).toBe(7);

    const afterMid = await pipeline.ingest(scope, [envelope(3), envelope(4)]);
    expect(afterMid.ingestedThroughSeq).toBeUndefined();

    const afterFill = await pipeline.ingest(scope, [envelope(1), envelope(2)]);
    expect(afterFill.ingestedThroughSeq).toBe(7);
    expect(afterFill.projectionReadyThroughSeq).toBe(7);
  });

  it("treats live duplicate delivery as idempotent without cursor regression", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [envelope(1), envelope(2)]);
    const duplicate = await pipeline.ingest(scope, [envelope(2), envelope(2)]);

    expect(duplicate.ingestedThroughSeq).toBe(2);
    expect(duplicate.state).toBe("idle");
    const ledger = await openLedgerHandle();
    const events = await ledger.getRawEvents(scope);
    expect(events.map((event) => event.eventSeq)).toEqual([1, 2]);
    ledger.close();
  });

  it("never advances the raw cursor from filtered subsets or semantic fetches", async () => {
    const fetches: Array<{ kind: string; ids: string[] }> = [];
    const fetchers: LedgerHydrationFetchers = {
      fetchCanonicalRecords: async (_agentId, kind, ids) => {
        fetches.push({ kind, ids });
        return {
          recordsById: Object.fromEntries(ids.map((id) => [id, { record: { id } }])),
          missingIds: [],
        };
      },
    };
    const pipeline = new LedgerIngestionPipeline({ fetchers });
    await pipeline.open();
    const scope = makeScope();

    // A display-filtered page returns only part of the raw range.
    const filtered = await pipeline.ingest(scope, [envelope(4), envelope(6), envelope(8)]);
    expect(filtered.ingestedThroughSeq).toBeUndefined();

    // Hydrating referenced records (semantic timeline demand) fetches
    // canonical bodies but must not move any raw cursor.
    await pipeline.ingest(scope, [
      envelope(1),
      envelope(2),
      briefEvent(3, "brief-1"),
    ]);
    await pipeline.drainHydration(scope);
    expect(fetches.length).toBeGreaterThan(0);

    const ledger = await openLedgerHandle();
    const session = await ledger.getAgentSession(scope);
    // Events 4 and 6/8 are stored but 5 and 7 never arrived: the contiguous
    // cursor stops at 4, after the gap-filling 1..3 ingest plus the earlier
    // filtered straggler 4 — never at 8.
    expect(session?.ingestedThroughSeq).toBe(4);
    const runtimeScope = await ledger.getRuntimeScope({
      remoteKey: scope.remoteKey,
      runtimeId: scope.runtimeId,
      visibilityScopeId: scope.visibilityScopeId,
      eventLogEpoch: scope.eventLogEpoch,
    });
    expect(runtimeScope?.eventHeadSeq).toBe(8);
    ledger.close();
  });

  it("blocks readiness and the read gate on pending reference hydration", async () => {
    const fetchers: LedgerHydrationFetchers = {
      fetchCanonicalRecords: async () => ({
        recordsById: {},
        missingIds: ["brief-1"],
      }),
    };
    const pipeline = new LedgerIngestionPipeline({ fetchers });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [envelope(1), briefEvent(2, "brief-1"), envelope(3)]);
    await pipeline.drainHydration(scope);

    let status = pipeline.status(scope)!;
    expect(status.ingestedThroughSeq).toBe(3);
    expect(status.projectionReadyThroughSeq).toBe(1);
    expect(status.pendingHydrationJobs).toBe(1);
    expect(status.blockedByEventSeq).toBe(2);
    expect(status.blockedReason).toBe("pending_hydration");

    const gate = pipeline.readinessGate(scope);
    expect(gate.readyThroughSeq).toBe(1);
    expect(gate.blockedByEventSeq).toBe(2);

    const ledger = await openLedgerHandle();
    const jobs = await ledger.getPendingHydrationJobs(scope);
    expect(jobs.map((job) => job.jobId)).toEqual(["brief:brief-1"]);
    ledger.close();
  });

  it("completes reference hydration atomically and unblocks readiness", async () => {
    const briefRecord = { id: "brief-1", text: "result" };
    const fetchers: LedgerHydrationFetchers = {
      fetchCanonicalRecords: async () => ({
        recordsById: { "brief-1": { record: briefRecord, revision: 4 } },
        missingIds: [],
      }),
    };
    const pipeline = new LedgerIngestionPipeline({ fetchers });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [envelope(1), briefEvent(2, "brief-1"), envelope(3)]);
    await pipeline.drainHydration(scope);

    const status = pipeline.status(scope)!;
    expect(status.projectionReadyThroughSeq).toBe(3);
    expect(status.pendingHydrationJobs).toBe(0);
    expect(status.state).toBe("idle");

    const ledger = await openLedgerHandle();
    const record = await ledger.getCanonicalRecord(scope, "brief", "brief-1");
    expect(record?.record).toEqual(briefRecord);
    expect(record?.revision).toBe(4);
    expect(await ledger.getPendingHydrationJobs(scope)).toEqual([]);
    ledger.close();
  });

  it("satisfies an older invalidation with a newer canonical revision", async () => {
    const fetchers: LedgerHydrationFetchers = {
      fetchCanonicalRecords: async () => ({
        recordsById: { "brief-1": { record: { id: "brief-1" }, revision: 5 } },
        missingIds: [],
      }),
    };
    const pipeline = new LedgerIngestionPipeline({ fetchers });
    await pipeline.open();
    const scope = makeScope();

    // Two invalidations of the same record; the first names revision 3.
    await pipeline.ingest(scope, [
      briefEvent(1, "brief-1", { revision: 3 }),
      briefEvent(2, "brief-1", { revision: 3 }),
    ]);
    await pipeline.drainHydration(scope);

    const status = pipeline.status(scope)!;
    expect(status.pendingHydrationJobs).toBe(0);
    expect(status.projectionReadyThroughSeq).toBe(2);
  });

  it("keeps the strictest expected revision when a lower revision arrives late", async () => {
    const pipeline = new LedgerIngestionPipeline({
      fetchers: {
        fetchCanonicalRecords: async () => ({
          recordsById: { "brief-1": { record: { id: "brief-1" }, revision: 3 } },
          missingIds: [],
        }),
      },
    });
    await pipeline.open();
    const scope = makeScope();

    // A later event names revision 5; a late lower-revision invalidation
    // for the same record must not weaken the merged demand back to 2.
    await pipeline.ingest(scope, [briefEvent(10, "brief-1", { revision: 5 })]);
    const status = await pipeline.ingest(scope, [
      briefEvent(3, "brief-1", { revision: 2 }),
    ]);

    expect(status.blockedReason).toBe("pending_hydration");
    const ledger = await openLedgerHandle();
    const jobs = await ledger.getPendingHydrationJobs(scope);
    expect(jobs.find((job) => job.jobId === "brief:brief-1")?.expectedRevision).toBe(5);
    ledger.close();
  });

  it("keeps a revision-expecting job pending until the revision is proven", async () => {
    const fetchers: LedgerHydrationFetchers = {
      fetchCanonicalRecords: async () => ({
        recordsById: { "brief-1": { record: { id: "brief-1" }, revision: 2 } },
        missingIds: [],
      }),
    };
    const pipeline = new LedgerIngestionPipeline({ fetchers });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [briefEvent(1, "brief-1", { revision: 3 })]);
    await pipeline.drainHydration(scope);

    const status = pipeline.status(scope)!;
    expect(status.pendingHydrationJobs).toBe(1);
    expect(status.projectionReadyThroughSeq).toBeUndefined();
    expect(status.blockedReason).toBe("pending_hydration");
  });

  it("blocks readiness on unknown envelope contract versions", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();

    const status = await pipeline.ingest(scope, [
      envelope(1),
      envelope(2, { contract_version: 99 }),
      envelope(3),
    ]);

    expect(status.ingestedThroughSeq).toBe(3);
    expect(status.projectionReadyThroughSeq).toBe(1);
    expect(status.blockedByEventSeq).toBe(2);
    expect(status.blockedReason).toBe("unknown_envelope_version");
  });

  it("completes hydration demand atomically via a canonical tombstone", async () => {
    const fetchers: LedgerHydrationFetchers = {
      fetchCanonicalRecords: async () => ({ recordsById: {}, missingIds: ["brief-1"] }),
    };
    const pipeline = new LedgerIngestionPipeline({ fetchers });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [briefEvent(1, "brief-1"), envelope(2)]);
    await pipeline.drainHydration(scope);
    expect(pipeline.status(scope)!.pendingHydrationJobs).toBe(1);

    await pipeline.applyTombstone(scope, "brief", "brief-1", 1);

    const status = pipeline.status(scope)!;
    expect(status.pendingHydrationJobs).toBe(0);
    expect(status.projectionReadyThroughSeq).toBe(2);
    const ledger = await openLedgerHandle();
    const record = await ledger.getCanonicalRecord(scope, "brief", "brief-1");
    expect(record?.record).toMatchObject({ tombstone: true, deletedByEventSeq: 1 });
    expect(await ledger.getPendingHydrationJobs(scope)).toEqual([]);
    ledger.close();
  });

  it("resumes pending hydration after a crash between persistence and hydration", async () => {
    const scope = makeScope();

    // Phase 1: the fetcher never settles before the "crash".
    let releaseFetch: (() => void) | null = null;
    const stalled: LedgerHydrationFetchers = {
      fetchCanonicalRecords: () =>
        new Promise(() => {
          releaseFetch?.();
        }),
    };
    const crashed = new LedgerIngestionPipeline({ fetchers: stalled });
    await crashed.open();
    await crashed.ingest(scope, [envelope(1), briefEvent(2, "brief-1")]);
    crashed.dispose();
    releaseFetch = null;

    // Phase 2: restart. The restart scan finds the pending job, and the
    // drain completes it with a working fetcher.
    const resumed = new LedgerIngestionPipeline({
      fetchers: {
        fetchCanonicalRecords: async () => ({
          recordsById: { "brief-1": { record: { id: "brief-1" } } },
          missingIds: [],
        }),
      },
    });
    await resumed.open();
    const resumeStatus = await resumed.resume(scope);
    expect(resumeStatus.pendingHydrationJobs).toBe(1);
    expect(resumeStatus.projectionReadyThroughSeq).toBe(1);

    await resumed.drainHydration(scope);
    const status = resumed.status(scope)!;
    expect(status.pendingHydrationJobs).toBe(0);
    expect(status.projectionReadyThroughSeq).toBe(2);
    expect(status.ingestedThroughSeq).toBe(2);
  });

  it("heals stored out-of-order stragglers across a restart", async () => {
    const scope = makeScope();
    const first = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await first.open();
    await first.ingest(scope, [envelope(1), envelope(2), envelope(4)]);
    first.dispose();

    const second = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await second.open();
    await second.resume(scope);
    const status = await second.ingest(scope, [envelope(3)]);

    // The restart scan rediscovered seq 4, so filling seq 3 heals the gap.
    expect(status.ingestedThroughSeq).toBe(4);
    expect(status.projectionReadyThroughSeq).toBe(4);
  });

  it("escalates exhausted retries to snapshot repair and recovers", async () => {
    const repairFetch = vi.fn(async () => ({
      snapshotThroughSeq: 2,
      canonicalRecords: [
        { recordKind: "brief" as const, recordId: "brief-1", record: { id: "brief-1" } },
      ],
    }));
    const pipeline = new LedgerIngestionPipeline({
      fetchers: {
        fetchCanonicalRecords: async () => ({ recordsById: {}, missingIds: ["brief-1"] }),
      },
      snapshotRepair: { fetchProjectionSnapshot: repairFetch },
      maxHydrationAttempts: 2,
    });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [envelope(1), briefEvent(2, "brief-1")]);
    // Each drain call is one bounded retry round.
    for (let round = 0; round < 3; round += 1) await pipeline.drainHydration(scope);

    expect(repairFetch.mock.calls.length).toBeGreaterThanOrEqual(1);
    const status = pipeline.status(scope)!;
    expect(status.state).toBe("idle");
    expect(status.pendingHydrationJobs).toBe(0);
    expect(status.projectionReadyThroughSeq).toBe(2);

    const ledger = await openLedgerHandle();
    expect(await ledger.getPendingHydrationJobs(scope)).toEqual([]);
    ledger.close();
  });

  it("reports sync_error when repair cannot explain the divergence", async () => {
    const statuses: string[] = [];
    const pipeline = new LedgerIngestionPipeline({
      fetchers: {
        fetchCanonicalRecords: async () => ({
          recordsById: { "brief-a": { record: { id: "brief-a" } } },
          missingIds: ["brief-b"],
        }),
      },
      snapshotRepair: {
        fetchProjectionSnapshot: async () => ({
          // Covers brief-a's event but not brief-b's later event.
          snapshotThroughSeq: 2,
          canonicalRecords: [],
        }),
      },
      maxHydrationAttempts: 2,
      onStatus: (status) => statuses.push(status.state),
    });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [
      envelope(1),
      briefEvent(2, "brief-a"),
      briefEvent(3, "brief-b"),
    ]);
    for (let round = 0; round < 4; round += 1) await pipeline.drainHydration(scope);

    const status = pipeline.status(scope)!;
    expect(status.state).toBe("sync_error");
    expect(statuses).toContain("sync_error");
    expect(status.lastError).toBe("hydration_diverged_after_snapshot_repair");
    expect(status.failedHydrationJobs).toBe(1);
    expect(status.blockedReason).toBe("pending_hydration");

    // The failed job stays durable so a restart surfaces the same error.
    const ledger = await openLedgerHandle();
    const jobs = await ledger.getPendingHydrationJobs(scope);
    expect(jobs.map((job) => [job.jobId, job.state])).toEqual([["brief:brief-b", "failed"]]);
    ledger.close();
  });

  it("reports sync_error when no repair source exists", async () => {
    const pipeline = new LedgerIngestionPipeline({
      fetchers: {
        fetchCanonicalRecords: async () => ({ recordsById: {}, missingIds: ["brief-1"] }),
      },
      maxHydrationAttempts: 1,
    });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [briefEvent(1, "brief-1")]);
    for (let round = 0; round < 2; round += 1) await pipeline.drainHydration(scope);

    const status = pipeline.status(scope)!;
    expect(status.state).toBe("sync_error");
    expect(status.lastError).toBe("hydration_exhausted_without_repair_source");
  });

  it("never reuses a degraded handle: rebuilds and verifies before claiming exact", async () => {
    const scope = makeScope();
    let currentHandle: EventLedger | null = null;
    let openCalls = 0;
    const openLedger = async () => {
      openCalls += 1;
      const result = await EventLedger.open();
      if (result.kind !== "available") return result;
      currentHandle = result.ledger;
      return result;
    };
    const pipeline = new LedgerIngestionPipeline({
      fetchers: emptyFetchers(),
      openLedger,
    });
    await pipeline.open();
    await pipeline.ingest(scope, [envelope(1)]);
    expect(openCalls).toBe(1);

    // Simulate a degraded handle (e.g. superseded by a version change).
    const degraded = currentHandle!;
    degraded.noteSuperseded();
    expect(degraded.durability).toBe("memory_only");

    const status = await pipeline.ingest(scope, [envelope(2)]);
    // The pipeline discarded the degraded handle, rebuilt, verified, and
    // completed the ingest on the fresh exact handle.
    expect(openCalls).toBe(2);
    expect(status.durability).toBe("exact");
    expect(status.ingestedThroughSeq).toBe(2);

    const ledger = await openLedgerHandle();
    const session = await ledger.getAgentSession(scope);
    expect(session?.ingestedThroughSeq).toBe(2);
    ledger.close();
  });

  it("rejects conflicting redelivery of the same event atomically", async () => {
    const pipeline = new LedgerIngestionPipeline({ fetchers: emptyFetchers() });
    await pipeline.open();
    const scope = makeScope();

    await pipeline.ingest(scope, [envelope(1, { payload: { status: "a" } })]);
    await expect(
      pipeline.ingest(scope, [envelope(1, { payload: { status: "b" } })]),
    ).rejects.toBeInstanceOf(LedgerIdentityConflictError);

    const ledger = await openLedgerHandle();
    const session = await ledger.getAgentSession(scope);
    expect(session?.ingestedThroughSeq).toBe(1);
    const stored = await ledger.getRawEvent(scope, 1);
    expect((stored?.envelope as { payload?: { status?: string } }).payload?.status).toBe("a");
    ledger.close();

    // A fresh ingest after the conflict still works and stays monotonic.
    const status = await pipeline.ingest(scope, [envelope(2)]);
    expect(status.ingestedThroughSeq).toBe(2);
  });
});
