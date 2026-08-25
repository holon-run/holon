import {
  expect,
  test,
  type APIRequestContext,
  type BrowserContext,
  type Page,
  type TestInfo,
} from "@playwright/test";

interface LedgerSnapshot {
  agentId: string;
  ingestedThroughSeq: number;
  projectionReadyThroughSeq: number;
  pendingHydrationJobs: number;
  failedHydrationJobs: number;
  blockedByEventSeq?: number;
  blockedReason?: "pending_hydration";
  readThroughEventSeq?: number;
  unreadCount?: number;
  readGateDecision: {
    mayAdvance: boolean;
    candidateSeq?: number;
    reason?: string;
  };
  readGateContext: {
    route: string;
    selectedAgentId: string;
    documentVisible: boolean;
    discoveryFreshness: string;
    sessionLoading?: boolean;
    sessionSyncStatus?: string;
    sessionLiveStatus?: string;
    sessionGapCount?: number;
    pendingProjectionHydrationCount?: number;
  };
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
  type = "agent_state_changed",
  payload: Record<string, unknown> = {},
) {
  return {
    id: `${agentId}-event-${eventSeq}`,
    event_seq: eventSeq,
    event_log_epoch: "e2e-epoch",
    contract_version: 2,
    ts: `2026-08-25T00:00:${String(eventSeq).padStart(2, "0")}Z`,
    agent_id: agentId,
    type,
    payload_schema: `holon.runtime_event.${type}`,
    payload_schema_version: 1,
    payload,
  };
}

function brief(agentId: string, eventSeq: number) {
  return {
    id: `brief-${eventSeq}`,
    agent_id: agentId,
    workspace_id: "agent_home",
    created_at: `2026-08-25T00:00:${String(eventSeq).padStart(2, "0")}Z`,
    created_event_seq: eventSeq,
    kind: "result",
    content_source: { kind: "inline" },
    text: `Brief ${eventSeq}`,
  };
}

async function ledger(page: Page, agentId: string): Promise<LedgerSnapshot | null> {
  return page.evaluate((id) => window.__HOLON_E2E__?.ledger(id) ?? null, agentId);
}

async function openAgent(page: Page, agentId: string): Promise<void> {
  const button = page.getByRole("button", { name: `Open ${agentId}`, exact: true });
  await expect.poll(async () => {
    const current = await ledger(page, agentId);
    if (
      current?.readGateContext.route === "agent"
      && current.readGateContext.selectedAgentId === agentId
    ) {
      return "selected";
    }
    return await button.isVisible() ? "ready" : "waiting";
  }).not.toBe("waiting");

  const current = await ledger(page, agentId);
  if (
    current?.readGateContext.route !== "agent"
    || current.readGateContext.selectedAgentId !== agentId
  ) {
    await button.click();
  }
  await expect.poll(async () => (await ledger(page, agentId))?.readGateContext)
    .toMatchObject({ route: "agent", selectedAgentId: agentId });
}

test("reconnect applies the authoritative roster before cached event catch-up", async ({
  page,
  request,
}, testInfo) => {
  const session = sessionFor(testInfo);
  await attachSession(page.context(), session);
  await page.goto("/");
  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot()))
    .toMatchObject({
      globalStreamStatus: "streaming",
      discovery: { mode: "authoritative", freshness: "fresh" },
      agentIds: ["bootstrap-agent"],
    });

  const beforeDisconnect = await request.get(controlPath(session, "/__e2e__/state"))
    .then((response) => response.json());
  await request.post(controlPath(session, "/__e2e__/disconnect-streams"));
  await configure(request, session, {
    visibleAgentIds: ["bootstrap-agent", "offline-agent"],
    ledgerEnabledAgentIds: ["offline-agent"],
    eventsByAgentId: {
      "offline-agent": [envelope("offline-agent", 1)],
    },
  });

  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot()))
    .toMatchObject({
      globalStreamStatus: "streaming",
      discovery: { mode: "authoritative", freshness: "fresh" },
      agentIds: ["bootstrap-agent", "offline-agent"],
    });
  await expect.poll(() => ledger(page, "offline-agent")).toMatchObject({
    ingestedThroughSeq: 1,
    projectionReadyThroughSeq: 1,
  });
  await expect.poll(async () => request.get(controlPath(session, "/__e2e__/state"))
    .then((response) => response.json())
    .then((state) => state.streamGeneration))
    .toBeGreaterThan(beforeDisconnect.streamGeneration);

  await request.post(controlPath(session, "/__e2e__/disconnect-streams"));
  await configure(request, session, {
    visibleAgentIds: ["offline-agent"],
  });
  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot()))
    .toMatchObject({
      globalStreamStatus: "streaming",
      discovery: { mode: "authoritative", freshness: "fresh" },
      agentIds: ["offline-agent"],
    });
});

test("read markers converge across tabs in one context and stay isolated across contexts", async ({
  browser,
  context,
  page,
  request,
}, testInfo) => {
  const session = sessionFor(testInfo);
  const agentId = "read-agent";
  await configure(request, session, {
    visibleAgentIds: [agentId],
    ledgerEnabledAgentIds: [agentId],
    eventsByAgentId: {
      [agentId]: [
        envelope(agentId, 1),
        envelope(agentId, 2, "brief_created", { brief_id: "brief-2" }),
      ],
    },
    briefsById: {
      "brief-2": brief(agentId, 2),
      "brief-4": brief(agentId, 4),
    },
  });
  await attachSession(context, session);

  const sibling = await context.newPage();
  expect(sibling.context()).toBe(page.context());
  await sibling.goto("/");
  await sibling.bringToFront();
  await openAgent(sibling, agentId);
  await expect.poll(async () => (await ledger(sibling, agentId))?.readGateDecision)
    .toEqual({ mayAdvance: true, candidateSeq: 2 });
  await page.goto("/");
  await page.bringToFront();
  await openAgent(page, agentId);
  await expect.poll(async () => {
    const snapshot = await ledger(page, agentId);
    return {
      decision: snapshot?.readGateDecision,
      context: snapshot?.readGateContext,
      ingestedThroughSeq: snapshot?.ingestedThroughSeq,
      projectionReadyThroughSeq: snapshot?.projectionReadyThroughSeq,
    };
  }).toEqual({
    decision: { mayAdvance: true, candidateSeq: 2 },
    context: expect.any(Object),
    ingestedThroughSeq: 2,
    projectionReadyThroughSeq: 2,
  });
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    projectionReadyThroughSeq: 2,
    readThroughEventSeq: 2,
  });
  await request.post(controlPath(session, "/__e2e__/append-event"), {
    data: { envelope: envelope(agentId, 3), broadcast: false },
  });
  await page.reload();
  await openAgent(page, agentId);
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    ingestedThroughSeq: 3,
    projectionReadyThroughSeq: 3,
    readThroughEventSeq: 3,
  });
  await expect.poll(() => ledger(sibling, agentId)).toMatchObject({
    readThroughEventSeq: 3,
  });

  await request.post(controlPath(session, "/__e2e__/append-event"), {
    data: {
      envelope: envelope(agentId, 4, "brief_created", { brief_id: "brief-4" }),
      broadcast: false,
    },
  });
  await sibling.reload();
  await sibling.bringToFront();
  await openAgent(sibling, agentId);
  await expect.poll(() => ledger(sibling, agentId)).toMatchObject({
    ingestedThroughSeq: 4,
    projectionReadyThroughSeq: 4,
    readThroughEventSeq: 4,
  });
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    readThroughEventSeq: 4,
  });

  await page.close();
  const reopened = await context.newPage();
  await reopened.goto("/");
  await expect.poll(() => ledger(reopened, agentId)).toMatchObject({
    readThroughEventSeq: 4,
  });

  const isolatedContext = await browser.newContext();
  try {
    await attachSession(isolatedContext, session);
    const isolatedPage = await isolatedContext.newPage();
    await isolatedPage.goto("/");
    await expect.poll(() => ledger(isolatedPage, agentId)).toMatchObject({
      ingestedThroughSeq: 4,
    });
    expect((await ledger(isolatedPage, agentId))?.readThroughEventSeq).toBeUndefined();
  } finally {
    await isolatedContext.close();
  }
});

test("pending hydration survives reload and close without crossing the read gate", async ({
  context,
  page,
  request,
}, testInfo) => {
  const session = sessionFor(testInfo);
  const agentId = "hydration-agent";
  await configure(request, session, {
    visibleAgentIds: [agentId],
    ledgerEnabledAgentIds: [agentId],
    eventsByAgentId: {
      [agentId]: [
        envelope(agentId, 1),
        envelope(agentId, 2, "brief_created", { brief_id: "brief-2" }),
        envelope(agentId, 3),
      ],
    },
    briefsById: { "brief-2": brief(agentId, 2) },
    blockedBriefIds: ["brief-2"],
  });
  await attachSession(context, session);
  await page.goto("/");
  await openAgent(page, agentId);
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    ingestedThroughSeq: 3,
    projectionReadyThroughSeq: 1,
    pendingHydrationJobs: 1,
    blockedByEventSeq: 2,
    blockedReason: "pending_hydration",
  });
  expect((await ledger(page, agentId))?.readThroughEventSeq).toBeUndefined();

  await page.reload();
  await expect.poll(() => ledger(page, agentId)).toMatchObject({
    ingestedThroughSeq: 3,
    projectionReadyThroughSeq: 1,
    pendingHydrationJobs: 1,
    blockedByEventSeq: 2,
  });
  expect((await ledger(page, agentId))?.readThroughEventSeq).toBeUndefined();

  await page.close();
  await request.post(controlPath(session, "/__e2e__/release-briefs"), {
    data: { briefIds: ["brief-2"] },
  });
  const reopened = await context.newPage();
  await reopened.goto("/");
  await openAgent(reopened, agentId);
  await expect.poll(() => ledger(reopened, agentId)).toMatchObject({
    ingestedThroughSeq: 3,
    projectionReadyThroughSeq: 3,
    pendingHydrationJobs: 0,
    failedHydrationJobs: 0,
    readThroughEventSeq: 3,
  });
  await expect(reopened.getByText("Brief 2", { exact: true })).toHaveCount(1);
});
