import {
  expect,
  test,
  type APIRequestContext,
  type BrowserContext,
  type Page,
  type TestInfo,
} from "@playwright/test";

interface LedgerSnapshot {
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  durability: string;
  ingestionState: string;
  ingestionError?: string;
  ingestedThroughSeq: number;
  projectionReadyThroughSeq: number;
  pendingHydrationJobs: number;
  failedHydrationJobs: number;
  readThroughEventSeq?: number;
  certainty?: "exact" | "truncated";
  historyTruncatedBeforeSeq?: number;
  acknowledgedTruncationBeforeSeq?: number;
}

interface LedgerPartition {
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  eventSeqs: number[];
  readThroughEventSeq?: number;
  certainty?: "exact" | "truncated";
}

interface LedgerStatus {
  durability: string;
  ingestionState: string;
  ingestionError?: string;
}

function sessionFor(testInfo: TestInfo): string {
  const testCase = testInfo.testId.replace(/[^a-zA-Z0-9_-]/g, "-");
  return `${testInfo.workerIndex}-${testInfo.repeatEachIndex}-${testInfo.retry}-${testCase}`;
}

function controlPath(session: string, path: string): string {
  return `${path}?session=${encodeURIComponent(session)}`;
}

async function attachSession(context: BrowserContext, session: string): Promise<void> {
  await context.addCookies([{
    name: "holon_e2e_session",
    value: session,
    domain: "127.0.0.1",
    path: "/",
  }]);
}

async function configure(
  request: APIRequestContext,
  session: string,
  body: Record<string, unknown>,
): Promise<void> {
  const response = await request.post(controlPath(session, "/__e2e__/configure"), { data: body });
  expect(response.ok()).toBe(true);
}

function envelope(
  agentId: string,
  eventSeq: number,
  eventLogEpoch = "e2e-epoch",
  type = "agent_state_changed",
  payload: Record<string, unknown> = {},
) {
  return {
    id: `${agentId}-${eventLogEpoch}-event-${eventSeq}`,
    event_seq: eventSeq,
    event_log_epoch: eventLogEpoch,
    contract_version: 2,
    ts: `2026-08-25T00:00:${String(eventSeq).padStart(2, "0")}Z`,
    agent_id: agentId,
    type,
    payload_schema: `holon.runtime_event.${type}`,
    payload_schema_version: 1,
    payload,
  };
}

function latestBrief(briefId: string, eventSeq: number) {
  return {
    brief_id: briefId,
    created_at: `2026-08-25T00:00:${String(eventSeq).padStart(2, "0")}Z`,
    created_event_seq: eventSeq,
    preview: `Brief ${eventSeq}`,
  };
}

async function ledger(page: Page, agentId: string): Promise<LedgerSnapshot | null> {
  return page.evaluate((id) => window.__HOLON_E2E__?.ledger(id) ?? null, agentId);
}

async function ledgerStatus(page: Page, agentId: string): Promise<LedgerStatus | null> {
  return page.evaluate((id) => window.__HOLON_E2E__?.ledgerStatus(id) ?? null, agentId);
}

async function partitions(page: Page, agentId: string): Promise<LedgerPartition[]> {
  return page.evaluate(
    (id) => window.__HOLON_E2E__?.ledgerPartitions(id) ?? [],
    agentId,
  );
}

async function openAgent(page: Page, agentId: string): Promise<void> {
  const agentButton = page.getByRole("region", { name: "agents" })
    .getByRole("button")
    .filter({ hasText: agentId });
  await expect(agentButton).toBeVisible();
  await agentButton.click();
  await expect.poll(async () => (await ledger(page, agentId))?.readGateContext)
    .toMatchObject({ route: "agent", selectedAgentId: agentId });
}

async function briefFetchCount(
  request: APIRequestContext,
  session: string,
  agentId: string,
): Promise<number> {
  const response = await request.get(controlPath(session, "/__e2e__/requests"));
  expect(response.ok()).toBe(true);
  const body = await response.json() as { requests: string[] };
  return body.requests.filter(
    (entry) => entry === `POST /api/agents/${agentId}/briefs:batchGet`,
  ).length;
}

async function globalStreamCount(
  request: APIRequestContext,
  session: string,
): Promise<number> {
  const response = await request.get(controlPath(session, "/__e2e__/state"));
  expect(response.ok()).toBe(true);
  const body = await response.json() as { globalStreamCount: number };
  return body.globalStreamCount;
}

async function hydrationAttemptCount(page: Page, agentId: string): Promise<number | null> {
  return page.evaluate(async (id) => {
    const database = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("holon.webGui.eventLedger.v1");
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      const records = await new Promise<Array<{ agentId?: string; attemptCount?: number }>>(
        (resolve, reject) => {
          const request = database
            .transaction("pending_hydration", "readonly")
            .objectStore("pending_hydration")
            .getAll();
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error);
        },
      );
      const attempts = records
        .filter((record) => record.agentId === id)
        .map((record) => record.attemptCount ?? 0);
      return attempts.length > 0 ? Math.max(...attempts) : null;
    } finally {
      database.close();
    }
  }, agentId);
}

async function driveHydrationUntil(
  page: Page,
  request: APIRequestContext,
  session: string,
  agentId: string,
  terminal: (snapshot: LedgerSnapshot | null) => boolean,
): Promise<void> {
  for (let attempt = 0; attempt < 16; attempt += 1) {
    if (terminal(await ledger(page, agentId))) return;
    const beforeAttemptCount = await hydrationAttemptCount(page, agentId);
    await page.reload();
    await expect.poll(async () => {
      if (terminal(await ledger(page, agentId))) return true;
      const currentAttemptCount = await hydrationAttemptCount(page, agentId);
      return currentAttemptCount !== null &&
        currentAttemptCount > (beforeAttemptCount ?? -1);
    }).toBe(true);
  }
  const finalSnapshot = await ledger(page, agentId);
  const finalBriefFetchCount = await briefFetchCount(request, session, agentId);
  expect(
    terminal(finalSnapshot),
    JSON.stringify({ finalSnapshot, finalBriefFetchCount }, null, 2),
  ).toBe(true);
}

test("retention reset is truncated until acknowledgement opens a new exact generation", async ({
  context,
  page,
  request,
}, testInfo) => {
  const session = sessionFor(testInfo);
  const agentId = "retention-agent";
  await configure(request, session, {
    visibleAgentIds: [agentId],
    ledgerEnabledAgentIds: [agentId],
    eventsByAgentId: {
      [agentId]: [1, 2, 3, 4, 5].map((seq) => envelope(agentId, seq)),
    },
  });
  await attachSession(context, session);
  await page.goto("/");
  await openAgent(page, agentId);
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    ingestedThroughSeq: 5,
    projectionReadyThroughSeq: 5,
    readThroughEventSeq: 5,
    certainty: "exact",
  });

  await configure(request, session, {
    eventsByAgentId: { [agentId]: [envelope(agentId, 8)] },
    oldestRetainedSeqByAgentId: { [agentId]: 7 },
    snapshotThroughSeqByAgentId: { [agentId]: 8 },
  });
  await request.post(controlPath(session, "/__e2e__/disconnect-streams"));

  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    ingestedThroughSeq: 8,
    projectionReadyThroughSeq: 8,
    certainty: "truncated",
    historyTruncatedBeforeSeq: 7,
  });
  await expect(
    page.getByRole("region", { name: "Agent conversation" }).getByRole("status"),
  ).toContainText("Some earlier history");
  await page.getByRole("button", { name: "Acknowledge earlier history" }).click();
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    certainty: "exact",
    historyTruncatedBeforeSeq: 7,
    acknowledgedTruncationBeforeSeq: 8,
  });
  await expect(page.getByRole("button", { name: "Acknowledge earlier history" })).toHaveCount(0);
});

test("event log epoch replacement clears the old projection and read partition", async ({
  context,
  page,
  request,
}, testInfo) => {
  const session = sessionFor(testInfo);
  const agentId = "epoch-agent";
  await configure(request, session, {
    eventLogEpoch: "epoch-before",
    visibleAgentIds: [agentId],
    ledgerEnabledAgentIds: [agentId],
    eventsByAgentId: {
      [agentId]: [
        envelope(agentId, 1, "epoch-before"),
        envelope(agentId, 2, "epoch-before"),
      ],
    },
  });
  await attachSession(context, session);
  await page.goto("/");
  await openAgent(page, agentId);
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    eventLogEpoch: "epoch-before",
    ingestedThroughSeq: 2,
    projectionReadyThroughSeq: 2,
    readThroughEventSeq: 2,
    certainty: "exact",
  });

  await configure(request, session, {
    eventLogEpoch: "epoch-after",
    eventsByAgentId: {
      [agentId]: [
        envelope(agentId, 1, "epoch-after"),
        envelope(agentId, 2, "epoch-after"),
        envelope(agentId, 3, "epoch-after"),
      ],
    },
  });
  await request.post(controlPath(session, "/__e2e__/disconnect-streams"));

  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    eventLogEpoch: "epoch-after",
    ingestedThroughSeq: 3,
    projectionReadyThroughSeq: 3,
    readThroughEventSeq: 3,
    certainty: "exact",
  });
  await expect.poll(() => partitions(page, agentId)).toEqual([{
    runtimeId: "e2e-runtime",
    visibilityScopeId: "e2e-scope",
    eventLogEpoch: "epoch-after",
    eventSeqs: [1, 2, 3],
    observedHeadSeq: 3,
    readThroughEventSeq: 3,
    certainty: "exact",
  }]);
});

test("visibility scope rotation clears the old auth-scoped partition", async ({
  context,
  page,
  request,
}, testInfo) => {
  const session = sessionFor(testInfo);
  const agentId = "scope-agent";
  await configure(request, session, {
    visibilityScopeId: "scope-before",
    visibleAgentIds: [agentId],
    ledgerEnabledAgentIds: [agentId],
    eventsByAgentId: {
      [agentId]: [envelope(agentId, 1), envelope(agentId, 2)],
    },
  });
  await attachSession(context, session);
  await page.goto("/");
  await openAgent(page, agentId);
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    visibilityScopeId: "scope-before",
    ingestedThroughSeq: 2,
    projectionReadyThroughSeq: 2,
    readThroughEventSeq: 2,
    certainty: "exact",
  });

  await configure(request, session, {
    visibilityScopeId: "scope-after",
    eventsByAgentId: {
      [agentId]: [envelope(agentId, 1), envelope(agentId, 2), envelope(agentId, 3)],
    },
  });
  await request.post(controlPath(session, "/__e2e__/disconnect-streams"));

  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    visibilityScopeId: "scope-after",
    ingestedThroughSeq: 3,
    projectionReadyThroughSeq: 3,
    readThroughEventSeq: 3,
    certainty: "exact",
  });
  await expect.poll(() => partitions(page, agentId)).toEqual([{
    runtimeId: "e2e-runtime",
    visibilityScopeId: "scope-after",
    eventLogEpoch: "e2e-epoch",
    eventSeqs: [1, 2, 3],
    observedHeadSeq: 3,
    readThroughEventSeq: 3,
    certainty: "exact",
  }]);
});

test("divergence repair, sync error, and degraded handle recovery preserve exactness", async ({
  context,
  page,
  request,
}, testInfo) => {
  const session = sessionFor(testInfo);
  const repairAgentId = "repair-agent";
  const errorAgentId = "error-agent";
  const degradedAgentId = "degraded-agent";
  const repairEvent = envelope(
    repairAgentId,
    2,
    "e2e-epoch",
    "brief_created",
    { brief_id: "repair-brief" },
  );
  const errorEventA = envelope(
    errorAgentId,
    2,
    "e2e-epoch",
    "brief_created",
    { brief_id: "error-brief-a" },
  );
  const errorEventB = envelope(
    errorAgentId,
    3,
    "e2e-epoch",
    "brief_created",
    { brief_id: "error-brief-b" },
  );
  await configure(request, session, {
    visibleAgentIds: [repairAgentId, errorAgentId, degradedAgentId],
    ledgerEnabledAgentIds: [repairAgentId, errorAgentId, degradedAgentId],
    eventsByAgentId: {
      [repairAgentId]: [envelope(repairAgentId, 1), repairEvent],
      [errorAgentId]: [envelope(errorAgentId, 1), errorEventA, errorEventB],
      [degradedAgentId]: [envelope(degradedAgentId, 1), envelope(degradedAgentId, 2)],
    },
    snapshotThroughSeqByAgentId: {
      [repairAgentId]: 0,
      [errorAgentId]: 0,
      [degradedAgentId]: 0,
    },
  });
  await attachSession(context, session);
  await page.goto("/");

  await openAgent(page, repairAgentId);
  await expect.poll(() => briefFetchCount(request, session, repairAgentId))
    .toBeGreaterThanOrEqual(1);
  await configure(request, session, {
    snapshotThroughSeqByAgentId: {
      [repairAgentId]: 2,
      [errorAgentId]: 2,
      [degradedAgentId]: 0,
    },
    projectionLatestBriefByAgentId: {
      [repairAgentId]: latestBrief("repair-brief", 2),
    },
  });
  await driveHydrationUntil(
    page,
    request,
    session,
    repairAgentId,
    (snapshot) => snapshot?.ingestionState === "idle" &&
      snapshot.projectionReadyThroughSeq === 2,
  );
  await expect.poll(() => ledger(page, repairAgentId)).toMatchObject({
    durability: "exact",
    ingestionState: "idle",
    ingestedThroughSeq: 2,
    projectionReadyThroughSeq: 2,
    pendingHydrationJobs: 0,
    failedHydrationJobs: 0,
  });

  await openAgent(page, errorAgentId);
  await expect.poll(() => briefFetchCount(request, session, errorAgentId))
    .toBeGreaterThanOrEqual(1);
  await driveHydrationUntil(
    page,
    request,
    session,
    errorAgentId,
    (snapshot) => snapshot?.ingestionState === "sync_error",
  );
  await expect.poll(() => ledger(page, errorAgentId)).toMatchObject({
    durability: "exact",
    ingestionState: "sync_error",
    ingestionError: "hydration_diverged_after_snapshot_repair",
    ingestedThroughSeq: 3,
    projectionReadyThroughSeq: 2,
    pendingHydrationJobs: 0,
    failedHydrationJobs: 1,
  });

  await openAgent(page, degradedAgentId);
  await expect.poll(() => ledger(page, degradedAgentId)).toMatchObject({
    durability: "exact",
    ingestedThroughSeq: 2,
    projectionReadyThroughSeq: 2,
  });
  await page.evaluate(() => {
    const originalPut = IDBObjectStore.prototype.put;
    const originalOpen = indexedDB.open.bind(indexedDB);
    const ownOpenDescriptor = Object.getOwnPropertyDescriptor(indexedDB, "open");
    let armed = true;
    let abortedWrites = 0;
    let rejectedReopens = 0;
    IDBObjectStore.prototype.put = function (...args) {
      const request = originalPut.apply(this, args);
      if (armed && this.name === "raw_events") {
        armed = false;
        abortedWrites += 1;
        this.transaction.abort();
      }
      return request;
    };
    Object.defineProperty(indexedDB, "open", {
      configurable: true,
      value: (...args: Parameters<IDBFactory["open"]>) => {
        if (!armed) {
          rejectedReopens += 1;
          throw new DOMException("Injected ledger reopen failure", "AbortError");
        }
        return originalOpen(...args);
      },
    });
    Object.defineProperty(window, "__HOLON_E2E_LEDGER_ABORTED_WRITES__", {
      configurable: true,
      get: () => abortedWrites,
    });
    Object.defineProperty(window, "__HOLON_E2E_LEDGER_REOPEN_REJECTIONS__", {
      configurable: true,
      get: () => rejectedReopens,
    });
    Object.defineProperty(window, "__HOLON_E2E_RELEASE_LEDGER_REOPEN__", {
      configurable: true,
      value: () => {
        IDBObjectStore.prototype.put = originalPut;
        if (ownOpenDescriptor) {
          Object.defineProperty(indexedDB, "open", ownOpenDescriptor);
        } else {
          Reflect.deleteProperty(indexedDB, "open");
        }
      },
    });
  });
  await expect.poll(() => globalStreamCount(request, session)).toBeGreaterThan(0);
  const degradedEvent = envelope(degradedAgentId, 3);
  const failedWrite = await request.post(controlPath(session, "/__e2e__/append-event"), {
    data: { envelope: degradedEvent },
  });
  expect(failedWrite.ok()).toBe(true);
  await expect.poll(() => page.evaluate(
    () => Reflect.get(window, "__HOLON_E2E_LEDGER_ABORTED_WRITES__"),
  )).toBeGreaterThan(0);
  await expect.poll(async () => (await ledgerStatus(page, degradedAgentId))?.durability)
    .not.toBe("exact");

  await page.evaluate(() => {
    const release = Reflect.get(window, "__HOLON_E2E_RELEASE_LEDGER_REOPEN__");
    if (typeof release === "function") release();
    Reflect.deleteProperty(window, "__HOLON_E2E_RELEASE_LEDGER_REOPEN__");
    Reflect.deleteProperty(window, "__HOLON_E2E_LEDGER_REOPEN_REJECTIONS__");
  });
  await expect.poll(() => ledger(page, degradedAgentId)).toMatchObject({
    ingestedThroughSeq: 2,
    projectionReadyThroughSeq: 2,
  });
  await expect.poll(() => partitions(page, degradedAgentId)).toEqual([{
    runtimeId: "e2e-runtime",
    visibilityScopeId: "e2e-scope",
    eventLogEpoch: "e2e-epoch",
    eventSeqs: [1, 2],
    observedHeadSeq: 2,
    readThroughEventSeq: 2,
    certainty: "exact",
  }]);
  const recoveredWrite = await request.post(controlPath(session, "/__e2e__/append-event"), {
    data: { envelope: degradedEvent },
  });
  expect(recoveredWrite.ok()).toBe(true);
  await expect.poll(() => ledger(page, degradedAgentId)).toMatchObject({
    durability: "exact",
    ingestionState: "idle",
    ingestedThroughSeq: 3,
    projectionReadyThroughSeq: 3,
    readThroughEventSeq: 3,
    certainty: "exact",
  });
  await expect.poll(() => partitions(page, degradedAgentId)).toEqual([{
    runtimeId: "e2e-runtime",
    visibilityScopeId: "e2e-scope",
    eventLogEpoch: "e2e-epoch",
    eventSeqs: [1, 2, 3],
    observedHeadSeq: 3,
    readThroughEventSeq: 3,
    certainty: "exact",
  }]);
});

