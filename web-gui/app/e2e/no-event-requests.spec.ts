import {
  expect,
  test,
  type APIRequestContext,
  type BrowserContext,
  type Page,
} from "@playwright/test";

import { sessionFor } from "./test-session";

/**
 * #3808 slice 3: the raw event ledger is retired. Across bootstrap, agent
 * selection, refresh, reconnect, and epoch rotation the production GUI must
 * reach agents only through roster snapshots and the conversation read
 * model, never the per-agent or global raw event transports.
 */
const forbiddenEventPath = /^\/api\/(?:agents\/[^/]+\/events(?:\/stream)?|events\/stream)\/?$/;

// Fail on the wire, not only when a retired diagnostics method is called.
const observed = new WeakMap<Page, string[]>();

test.beforeEach(async ({ page }) => {
  const requests: string[] = [];
  observed.set(page, requests);
  page.on("request", (request) => {
    const pathname = new URL(request.url()).pathname;
    if (forbiddenEventPath.test(pathname)) requests.push(pathname);
  });
});

test.afterEach(async ({ page }) => {
  expect(observed.get(page)).toEqual([]);
});

function controlPath(session: string, path: string): string {
  return `${path}?session=${encodeURIComponent(session)}`;
}

async function attachSession(context: BrowserContext, session: string): Promise<void> {
  await context.addCookies([{
    name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/",
  }]);
}

async function configure(request: APIRequestContext, session: string, body: Record<string, unknown>): Promise<void> {
  expect((await request.post(controlPath(session, "/__e2e__/configure"), { data: body })).ok()).toBe(true);
}

async function seedConversation(request: APIRequestContext, session: string, agentId: string, turnIds: string[]): Promise<void> {
  const turns = turnIds.map((turnId, index) => ({
    turn_id: turnId, key: { turn_id: turnId, turn_index: index + 1 }, revision: 1,
    presentation_class: "operator", inputs: [{ message_id: `input-${turnId}`, preview: `Question ${turnId}` }],
    execution: { kind: "active" }, result: { kind: "pending" }, settled: false,
    attention: null, detail_coverage: { kind: "complete" }, brief_ids: [],
  }));
  expect((await request.post(controlPath(session, "/__e2e__/conversation"), { data: {
    agentId, turns, activitiesByTurnId: Object.fromEntries(turnIds.map((id, index) => [id, [{
      kind: "assistant", id: `activity-${id}`, key: { event_seq: index + 1, activity_id: `activity-${id}` },
      revision: 1, summary: `Answer ${id}`,
    }]])),
  } })).ok()).toBe(true);
}

async function conversation(page: Page, agentId: string) {
  return page.evaluate((id) => window.__HOLON_E2E__?.conversation(id) ?? null, agentId);
}

test("production DOM and network: bootstrap, selection, refresh, reconnect, epoch rotation without raw events", async ({ context, page, request }, info) => {
  const session = sessionFor(info);
  const agentId = "production-no-events-agent";
  let streamConnections = 0;
  page.on("response", (response) => {
    if (new URL(response.url()).pathname === `/api/agents/${agentId}/conversation/stream`
        && response.status() === 200) streamConnections += 1;
  });
  await configure(request, session, { visibleAgentIds: [agentId] });
  await seedConversation(request, session, agentId, ["production-before"]);
  await attachSession(context, session);

  await page.goto("/");
  if (process.env.HOLON_E2E_PRODUCTION === "1") {
    expect(await page.evaluate(() => window.__HOLON_E2E__)).toBeUndefined();
  }
  const roster = page.getByRole("region", { name: "agents" });
  await expect(roster).toBeVisible();
  await roster.getByRole("button").filter({ hasText: agentId }).click();
  const oldTurn = page.locator('[data-turn-id="production-before"]');
  await expect(oldTurn).toBeVisible();
  await expect.poll(() => streamConnections).toBeGreaterThan(0);
  const connectionsBeforeReload = streamConnections;
  await page.reload();
  await expect(oldTurn).toBeVisible();
  // Snapshot hydration precedes stream subscription. Wait for this document's
  // stream so the reload connection cannot satisfy the reconnect assertion.
  await expect.poll(() => streamConnections).toBeGreaterThan(connectionsBeforeReload);

  // Close an established SSE connection, then prove the replacement delivers
  // a new live batch without a page reload or raw-event recovery.
  const previousConnections = streamConnections;
  expect((await request.post(controlPath(session, "/__e2e__/disconnect-streams"))).ok()).toBe(true);
  await expect.poll(() => streamConnections, { timeout: 15_000 }).toBeGreaterThan(previousConnections);
  await seedConversation(request, session, agentId, ["production-before", "production-reconnected"]);
  await expect(page.locator('[data-turn-id="production-reconnected"]')).toBeVisible();
  await expect(oldTurn).toHaveCount(1);

  await configure(request, session, { failConversationByAgentId: [agentId] });
  const failedSnapshot = page.waitForResponse((response) =>
    new URL(response.url()).pathname === `/api/agents/${agentId}/conversation`
      && response.status() === 503);
  await page.reload();
  await failedSnapshot;
  await expect(page.locator(".conversation-sync-status .is-reconnecting")).toBeVisible();
  await configure(request, session, { failConversationByAgentId: [] });
  await expect(page.locator(".conversation-sync-status .is-reconnecting")).toHaveCount(0, { timeout: 15_000 });
  await expect(oldTurn).toHaveCount(1);

  await configure(request, session, { eventLogEpoch: "production-rotated" });
  await seedConversation(request, session, agentId, ["production-after"]);
  await page.reload();
  await expect(page.locator('[data-turn-id="production-after"]')).toBeVisible();
  await expect(oldTurn).toHaveCount(0);
});

test("bootstrap, selection, refresh, reconnect, and epoch rotation never request raw events", async ({ context, page, request }, info) => {
  const session = sessionFor(info);
  const agentId = "no-events-agent";
  await configure(request, session, { visibleAgentIds: [agentId] });
  await seedConversation(request, session, agentId, ["bootstrap-turn"]);
  await attachSession(context, session);

  // Bootstrap without a selected agent still discovers the roster.
  await page.goto("/");
  await expect(page.getByRole("region", { name: "agents" })).toBeVisible();

  // Selecting the agent opens the conversation read model.
  await page.goto(`/agents/${agentId}/conversation`);
  await expect.poll(() => conversation(page, agentId), { timeout: 15_000 })
    .toMatchObject({ status: "ready", eventLogEpoch: "e2e-epoch", turnIds: ["bootstrap-turn"] });
  expect(await page.evaluate(async (id) => {
    const bridge = window.__HOLON_E2E__!;
    return {
      status: bridge.ledgerStatus(id),
      ledger: await bridge.ledger(id),
      partitions: await bridge.ledgerPartitions(id),
      unselected: bridge.conversation("not-selected"),
      databases: (await indexedDB.databases()).map((db) => db.name),
    };
  }, agentId)).toEqual({
    status: { durability: "unavailable", ingestionState: "retired" },
    ledger: null,
    partitions: [],
    unselected: null,
    databases: expect.not.arrayContaining(["holon.webGui.eventLedger.v1"]),
  });

  // Refresh keeps the same guarantees and persists the offline snapshot.
  await page.reload();
  await expect.poll(() => conversation(page, agentId), { timeout: 15_000 })
    .toMatchObject({ status: "ready", turnIds: ["bootstrap-turn"] });
  await expect.poll(() => page.evaluate(async () => {
    return await new Promise<boolean>((resolve, reject) => {
      const request = indexedDB.open("holon-webgui-cache");
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction("snapshots", "readonly");
        const entries = tx.objectStore("snapshots").getAll();
        tx.oncomplete = () => {
          db.close();
          resolve(entries.result.some((entry) => entry.agentId === "no-events-agent"
            && entry.summary.turns.some((turn: { turn_id: string }) => turn.turn_id === "bootstrap-turn")));
        };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
    });
  })).toBe(true);

  // Reconnect: a real failed conversation snapshot recovers without raw events.
  await configure(request, session, { failConversationByAgentId: [agentId] });
  await page.reload();
  // Cached-content preservation during the outage is reset-scope's
  // acceptance; here the outage must surface without any raw-event backfill.
  await expect.poll(() => conversation(page, agentId), { timeout: 15_000 })
    .toMatchObject({ status: "reconnecting" });
  await configure(request, session, { failConversationByAgentId: [] });
  await expect.poll(() => conversation(page, agentId), { timeout: 15_000 })
    .toMatchObject({ status: "ready", turnIds: ["bootstrap-turn"] });

  // Epoch rotation replaces the conversation scope without event backfill.
  await configure(request, session, { eventLogEpoch: "epoch-rotated" });
  await seedConversation(request, session, agentId, ["rotated-turn"]);
  await page.reload();
  await expect.poll(() => conversation(page, agentId), { timeout: 15_000 })
    .toMatchObject({ status: "ready", eventLogEpoch: "epoch-rotated", turnIds: ["rotated-turn"] });
});
