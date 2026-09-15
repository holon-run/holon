import "fake-indexeddb/auto";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  AgentSessionRepository,
  SESSION_CATCHUP_MAX_PAGES,
  type LedgerIngestionIntegration,
  type AgentSessionRepositoryDependencies,
  type AgentSessionRepositoryState,
} from "./agent-session-repository";
import { LEDGER_DB_NAME, type LedgerScopeKey } from "./event-ledger";
import type { AgentProjectionSnapshotDto, StreamEventEnvelopeDto } from "./client";
import { emptyAgentSession } from "./conversation-store";
import type { AgentSessionState } from "./runtime-store-helpers";
import type { RuntimeMessageEnvelope } from "./types";

interface TestState extends AgentSessionRepositoryState {}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function event(seq: number): StreamEventEnvelopeDto {
  return {
    id: `event-${seq}`,
    agent_id: "agent-a",
    event_seq: seq,
    type: "brief_created",
    payload: {},
  };
}

function createHarness(
  session: AgentSessionState = emptyAgentSession(),
  extra: { ledgerIngestion?: LedgerIngestionIntegration } = {},
) {
  let generation = 1;
  let state: TestState = {
    route: "agent",
    selectedAgentId: "agent-a",
    displayLevel: "info",
    globalStreamStatus: "streaming",
    sessionsByAgentId: { "agent-a": session },
    refreshAgentDetail: vi.fn(async () => undefined),
    refreshAgentWorkItems: vi.fn(async () => undefined),
    refreshAgentState: vi.fn(async () => undefined),
  };
  const client = {
    getAgentEvents: vi.fn(),
    getAgentProjectionSnapshot: vi.fn(
      async (_agentId: string): Promise<AgentProjectionSnapshotDto | null> => null,
    ),
  };
  const dependencies: AgentSessionRepositoryDependencies<TestState> = {
    get: () => state,
    set: (update) => {
      const partial = typeof update === "function" ? update(state) : update;
      state = { ...state, ...partial };
    },
    getClient: () => client,
    getConnectionConfig: () => ({ mode: "local" }),
    getGeneration: () => generation,
    ...(extra.ledgerIngestion ? { ledgerIngestion: extra.ledgerIngestion } : {}),
  };
  return {
    client,
    dependencies,
    getState: () => state,
    repository: new AgentSessionRepository(dependencies),
    advanceGeneration: () => {
      generation += 1;
    },
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("AgentSessionRepository ledger ingestion", () => {
  const scope: LedgerScopeKey = {
    remoteKey: "http://127.0.0.1:7878",
    runtimeId: "rt_test",
    visibilityScopeId: "vis_test",
    eventLogEpoch: "epoch-1",
    agentId: "agent-a",
  };

  function deleteLedger(): Promise<void> {
    return new Promise((resolve) => {
      const request = indexedDB.deleteDatabase(LEDGER_DB_NAME);
      request.onsuccess = () => resolve();
      request.onerror = () => resolve();
      request.onblocked = () => resolve();
    });
  }

  function ledgerIntegration(
    overrides: Partial<LedgerIngestionIntegration> = {},
  ): LedgerIngestionIntegration {
    return {
      resolveScope: () => scope,
      fetchers: {
        fetchCanonicalRecords: async () => ({ recordsById: {}, missingIds: [] }),
      },
      ...overrides,
    };
  }

  beforeEach(async () => {
    await deleteLedger();
  });

  afterEach(async () => {
    vi.restoreAllMocks();
    await deleteLedger();
  });

  it("ingests session events through the owned ledger pipeline", async () => {
    const harness = createHarness(
      emptyAgentSession(),
      { ledgerIngestion: ledgerIntegration() },
    );
    await harness.repository.initializeLedgerIngestion();

    const status = await harness.repository.ingestSessionEvents("agent-a", [
      event(1),
      event(2),
    ]);

    expect(status?.ingestedThroughSeq).toBe(2);
    expect(status?.projectionReadyThroughSeq).toBe(2);
    expect(harness.repository.sessionLedgerStatus("agent-a")?.ingestedThroughSeq).toBe(2);
    const gate = harness.repository.sessionLedgerReadiness("agent-a");
    expect(gate?.readyThroughSeq).toBe(2);
  });

  it("advances read markers, counts only qualifying unread, and acknowledges truncation", async () => {
    const harness = createHarness(
      emptyAgentSession(),
      { ledgerIngestion: ledgerIntegration() },
    );
    await harness.repository.initializeLedgerIngestion();

    await harness.repository.ingestSessionEvents("agent-a", [
      event(1),
      event(2),
      event(3),
    ]);

    // Without a read state every hydrated user-facing brief above boundary
    // 0 counts; internal events would not (ledger-level tests cover that).
    const before = await harness.repository.unreadSnapshot("agent-a");
    expect(before?.count).toBe(3);
    expect(before?.certainty).toBe("exact");
    expect(before?.countedThroughSeq).toBe(3);

    const advanced = await harness.repository.advanceReadMarker("agent-a", 3);
    expect(advanced?.advanced).toBe(true);
    expect(advanced?.record.readThroughEventSeq).toBe(3);

    const after = await harness.repository.unreadSnapshot("agent-a");
    expect(after?.count).toBe(0);
    expect(after?.boundarySeq).toBe(3);

    // A lower candidate never regresses the durable marker.
    const stale = await harness.repository.advanceReadMarker("agent-a", 2);
    expect(stale?.advanced).toBe(false);

    // An internal event after the marker does not add unread.
    await harness.repository.ingestSessionEvents("agent-a", [
      { ...event(4), type: "scheduler_tick", projection_effect: "none" },
    ]);
    const internal = await harness.repository.unreadSnapshot("agent-a");
    expect(internal?.count).toBe(0);

    // Explicit truncation acknowledgement opens a new exact generation at
    // the observed head.
    const acknowledged = await harness.repository.acknowledgeReadTruncation("agent-a");
    expect(acknowledged?.certainty).toBe("exact");
    expect(acknowledged?.acknowledgedTruncationBeforeSeq).toBe(4);
    expect(acknowledged?.unreadBaselineSeq).toBe(4);
  });

  it("acknowledges truncation at an explicit gated head instead of the observed head", async () => {
    const harness = createHarness(
      emptyAgentSession(),
      { ledgerIngestion: ledgerIntegration() },
    );
    await harness.repository.initializeLedgerIngestion();

    await harness.repository.ingestSessionEvents("agent-a", [
      event(1),
      event(2),
      event(3),
    ]);
    await harness.repository.advanceReadMarker("agent-a", 3);

    // The auto-restore path passes the gated head it caught up to; the
    // acknowledgement boundary must not jump to the higher observed head.
    const acknowledged = await harness.repository.acknowledgeReadTruncation("agent-a", 2);
    expect(acknowledged?.certainty).toBe("exact");
    expect(acknowledged?.unreadBaselineSeq).toBe(2);
    expect(acknowledged?.acknowledgedTruncationBeforeSeq).toBe(2);
  });

  it("returns null read-marker results when the scope is unresolved", async () => {
    const harness = createHarness(
      emptyAgentSession(),
      { ledgerIngestion: ledgerIntegration({ resolveScope: () => null }) },
    );
    await harness.repository.initializeLedgerIngestion();
    expect(await harness.repository.advanceReadMarker("agent-a", 3)).toBeNull();
    expect(await harness.repository.acknowledgeReadTruncation("agent-a")).toBeNull();
    expect(await harness.repository.unreadSnapshot("agent-a")).toBeNull();
  });

  it("stays dormant when the runtime identity scope is unresolved", async () => {
    const harness = createHarness(
      emptyAgentSession(),
      { ledgerIngestion: ledgerIntegration({ resolveScope: () => null }) },
    );
    await harness.repository.initializeLedgerIngestion();

    const status = await harness.repository.ingestSessionEvents("agent-a", [event(1)]);
    expect(status).toBeNull();
    expect(harness.repository.sessionLedgerStatus("agent-a")).toBeNull();
    expect(harness.repository.sessionLedgerReadiness("agent-a")).toBeNull();
  });

  it("drops the ledger pipeline when switching remotes", async () => {
    const harness = createHarness(
      emptyAgentSession(),
      { ledgerIngestion: ledgerIntegration() },
    );
    await harness.repository.initializeLedgerIngestion();
    harness.repository.switchRemote();

    const status = await harness.repository.ingestSessionEvents("agent-a", [event(1)]);
    expect(status).toBeNull();
  });

  it("recovery bootstraps the durable ledger, discovers scope, and routes live events through it", async () => {
    const harness = createHarness(
      emptyAgentSession(),
      { ledgerIngestion: ledgerIntegration({ resolveScope: () => null }) },
    );
    harness.client.getAgentEvents.mockResolvedValue({
      events: [event(2)],
      event_log_epoch: "epoch-1",
      newest_seq: 2,
      oldest_seq: 1,
      has_older: false,
      has_newer: false,
    });
    harness.client.getAgentProjectionSnapshot.mockResolvedValue({
      agent_id: "agent-a",
      contract_version: 1,
      runtime_id: "rt_test",
      visibility_scope_id: "vis_test",
      event_log_epoch: "epoch-1",
      snapshot_through_seq: 2,
      event_head_seq: 2,
      oldest_retained_seq: 0,
      projection: {
        agent: {},
        conversation: { latest_message_id: null, latest_transcript_entry_id: null },
        current_work_item: null,
        hydration_references: [],
        hydration_tombstones: [],
        latest_brief: null,
      },
    });

    // Recovery discovers the runtime identity scope from the projection
    // snapshot even though resolveScope stays dormant.
    await harness.repository.syncAgentRecovery("agent-a");
    const scope = harness.repository.knownLedgerScope("agent-a");
    expect(scope).toMatchObject({
      runtimeId: "rt_test",
      visibilityScopeId: "vis_test",
      eventLogEpoch: "epoch-1",
      agentId: "agent-a",
    });

    // Live envelopes route through the recovery coordinator's offer path
    // and land in the durable ledger under the discovered scope.
    const status = await harness.repository.ingestSessionEvents("agent-a", [event(3)]);
    expect(status?.ingestedThroughSeq).toBe(3);
    expect(status?.scope).toEqual(scope);

    // Live ingestion does not re-trigger snapshot discovery.
    const before = harness.client.getAgentProjectionSnapshot.mock.calls.length;
    await harness.repository.ingestSessionEvents("agent-a", [event(4)]);
    expect(harness.client.getAgentProjectionSnapshot.mock.calls.length).toBe(before);
  });
});
