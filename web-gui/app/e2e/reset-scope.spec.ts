import {
  expect,
  test,
  type APIRequestContext,
  type BrowserContext,
  type Page,
} from "@playwright/test";

import { sessionFor } from "./test-session";

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

async function expectConversation(page: Page, agentId: string, epoch: string, turnIds: string[]): Promise<void> {
  await expect.poll(() => conversation(page, agentId), { timeout: 15_000 }).toMatchObject({
    status: "ready", eventLogEpoch: epoch, turnIds,
  });
  for (const id of turnIds) {
    await expect(page.locator(`[data-turn-id="${id}"]`)).toHaveCount(1);
    await expect(page.getByText(`Answer ${id}`, { exact: true })).toBeVisible();
  }
}

// Fail on the wire, not only when a retired diagnostics method happens to be called.
const forbiddenRequests = new WeakMap<Page, string[]>();
test.beforeEach(async ({ page }) => {
  const requests: string[] = [];
  forbiddenRequests.set(page, requests);
  page.on("request", (request) => {
    if (/^\/api\/(?:agents\/[^/]+\/events(?:\/stream)?|events\/stream)\/?$/.test(new URL(request.url()).pathname)) {
      requests.push(request.url());
    }
  });
});
test.afterEach(async ({ page }) => {
  expect(forbiddenRequests.get(page)).toEqual([]);
});

test("retention recovery replaces missing history with the bounded conversation and deduplicates reloads", async ({ context, page, request }, info) => {
  const session = sessionFor(info);
  const agentId = "retention-agent";
  await configure(request, session, { visibleAgentIds: [agentId] });
  await seedConversation(request, session, agentId, ["before-retention"]);
  await attachSession(context, session);
  await page.goto(`/agents/${agentId}/conversation`);
  await expectConversation(page, agentId, "e2e-epoch", ["before-retention"]);

  await configure(request, session, { oldestRetainedSeqByAgentId: { [agentId]: 1500 } });
  await seedConversation(request, session, agentId, ["retained"]);
  await page.reload();
  await expectConversation(page, agentId, "e2e-epoch", ["retained"]);
  await expect(page.locator('[data-turn-id="before-retention"]')).toHaveCount(0);
  await page.reload();
  await expectConversation(page, agentId, "e2e-epoch", ["retained"]);
});

test("a failed conversation refresh preserves cached content and catches up when HTTP recovers", async ({ context, page, request }, info) => {
  const session = sessionFor(info);
  const agentId = "recovery-agent";
  await configure(request, session, { visibleAgentIds: [agentId] });
  await seedConversation(request, session, agentId, ["cached"]);
  await attachSession(context, session);
  await page.goto(`/agents/${agentId}/conversation`);
  await expectConversation(page, agentId, "e2e-epoch", ["cached"]);
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
          resolve(entries.result.some((entry) => entry.agentId === "recovery-agent"
            && entry.summary.turns.some((turn: { turn_id: string }) => turn.turn_id === "cached")));
        };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
    });
  })).toBe(true);

  await configure(request, session, { failConversationByAgentId: [agentId] });
  const failed = page.waitForResponse((response) => new URL(response.url()).pathname === `/api/agents/${agentId}/conversation` && response.status() === 503);
  await page.reload();
  await failed;
  await expect.poll(() => conversation(page, agentId)).toMatchObject({ status: "reconnecting", turnIds: ["cached"] });
  await expect(page.locator('[data-turn-id="cached"]')).toHaveCount(1);
  await seedConversation(request, session, agentId, ["cached", "recovered"]);
  await configure(request, session, { failConversationByAgentId: [] });
  await expectConversation(page, agentId, "e2e-epoch", ["cached", "recovered"]);
});

for (const scope of ["eventLogEpoch", "visibilityScopeId"] as const) {
  test(`${scope} rotation replaces the old conversation without an event ledger`, async ({ context, page, request }, info) => {
    const session = sessionFor(info);
    const agentId = "scope-agent";
    await configure(request, session, { visibleAgentIds: [agentId], [scope]: "scope-before" });
    await seedConversation(request, session, agentId, ["old-scope"]);
    await attachSession(context, session);
    await page.goto(`/agents/${agentId}/conversation`);
    await expectConversation(page, agentId, scope === "eventLogEpoch" ? "scope-before" : "e2e-epoch", ["old-scope"]);

    await configure(request, session, { [scope]: "scope-after" });
    await seedConversation(request, session, agentId, ["new-scope"]);
    await page.getByRole("button", { name: "Dashboard", exact: true }).first().click();
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await page.getByRole("region", { name: "agents" }).getByRole("button").filter({ hasText: agentId }).click();
    await expectConversation(page, agentId, scope === "eventLogEpoch" ? "scope-after" : "e2e-epoch", ["new-scope"]);
    await expect(page.locator('[data-turn-id="old-scope"]')).toHaveCount(0);
    await page.reload();
    await expectConversation(page, agentId, scope === "eventLogEpoch" ? "scope-after" : "e2e-epoch", ["new-scope"]);
    expect(await page.evaluate(async () => (await indexedDB.databases()).map((db) => db.name)))
      .not.toContain("holon.webGui.eventLedger.v1");
  });
}
