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

function envelope(agentId: string, eventSeq: number, type = "agent_state_changed", payload: Record<string, unknown> = {}) {
  return {
    id: `${agentId}-event-${eventSeq}`, event_seq: eventSeq, event_log_epoch: "e2e-epoch",
    ts: `2026-08-25T00:00:${String(eventSeq).padStart(2, "0")}Z`, agent_id: agentId,
    type, payload_schema: `holon.runtime_event.${type}`, payload,
  };
}

function brief(agentId: string, eventSeq: number) {
  return {
    id: `brief-${eventSeq}`, agent_id: agentId, workspace_id: "agent_home",
    created_at: `2026-08-25T00:00:${String(eventSeq).padStart(2, "0")}Z`, created_event_seq: eventSeq,
    kind: "result", content_source: { kind: "inline" }, text: `Brief ${eventSeq}`,
  };
}

function seed(agentId: string) {
  return {
    visibleAgentIds: [agentId],
    eventsByAgentId: { [agentId]: [
      envelope(agentId, 1), envelope(agentId, 2, "brief_created", { brief_id: "brief-2" }),
    ] },
    briefsById: { "brief-2": brief(agentId, 2), "brief-4": brief(agentId, 4) },
  };
}

async function appendUnread(request: APIRequestContext, session: string, agentId: string): Promise<void> {
  for (const event of [
    envelope(agentId, 3), envelope(agentId, 4, "brief_created", { brief_id: "brief-4" }),
  ]) {
    expect((await request.post(controlPath(session, "/__e2e__/append-event"), {
      data: { envelope: event, broadcast: false },
    })).ok()).toBe(true);
  }
}

async function conversation(page: Page, agentId: string) {
  return page.evaluate((id) => window.__HOLON_E2E__?.conversation(id) ?? null, agentId);
}

async function readState(page: Page, agentId: string) {
  return page.evaluate((id) => window.__HOLON_E2E__?.readState(id) ?? null, agentId);
}

function observeRetiredRequests(context: BrowserContext): string[] {
  const requests: string[] = [];
  context.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (/^\/api\/(?:agents\/[^/]+\/(?:events(?:\/stream)?|projection-snapshot)|events\/stream)\/?$/.test(path)) {
      requests.push(request.url());
    }
  });
  return requests;
}

const retiredRequests = new WeakMap<BrowserContext, string[]>();
test.beforeEach(async ({ context }) => {
  retiredRequests.set(context, observeRetiredRequests(context));
});
test.afterEach(async ({ context }) => {
  expect(retiredRequests.get(context)).toEqual([]);
});

async function expectCachedTurn(page: Page, agentId: string, turnId: string): Promise<void> {
  await expect.poll(() => page.evaluate(({ agentId, turnId }) =>
    new Promise<boolean>((resolve, reject) => {
      const request = indexedDB.open("holon-webgui-cache");
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction("snapshots", "readonly");
        const entries = tx.objectStore("snapshots").getAll();
        tx.oncomplete = () => {
          db.close();
          resolve(entries.result.some((entry) => entry.agentId === agentId
            && entry.summary.turns.some((turn: { turn_id: string }) => turn.turn_id === turnId)));
        };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
    }), { agentId, turnId })).toBe(true);
}

async function openAgent(page: Page, agentId: string): Promise<void> {
  const button = page.getByRole("button", { name: `Open ${agentId}`, exact: true });
  await expect(button).toBeVisible();
  await button.click();
  await expect(page).toHaveURL(new RegExp(`/agents/${agentId}/conversation`));
}

test("refresh applies the authoritative roster and epoch-scoped conversation recovery", async ({ page, request }, info) => {
  const session = sessionFor(info);
  const agentId = "offline-agent";
  await attachSession(page.context(), session);
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__HOLON_E2E__?.snapshot())).toMatchObject({
    discovery: { mode: "authoritative", freshness: "fresh" }, agentIds: ["bootstrap-agent"],
  });

  await configure(request, session, { ...seed(agentId), visibleAgentIds: ["bootstrap-agent", agentId] });
  // Roster refresh discovers agents and unread state, not conversation history.
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.__HOLON_E2E__?.snapshot())).toMatchObject({
    discovery: { mode: "authoritative", freshness: "fresh" }, agentIds: ["bootstrap-agent", agentId],
  });
  await expect.poll(() => readState(page, agentId)).toMatchObject({
    event_log_epoch: "e2e-epoch", read_through_event_seq: 0, unread_count: 1,
  });
  expect(await conversation(page, agentId)).toBeNull();
  await openAgent(page, agentId);
  await expect.poll(() => conversation(page, agentId)).toMatchObject({
    status: "ready", eventLogEpoch: "e2e-epoch", throughSeq: 2, turnIds: ["turn-2"],
  });
  await expect(page.getByText("Brief 2", { exact: true })).toBeVisible();
  await expect.poll(() => readState(page, agentId)).toMatchObject({ read_through_event_seq: 2 });

  await page.getByRole("button", { name: "Dashboard", exact: true }).first().click();
  await configure(request, session, {
    visibleAgentIds: [agentId], eventLogEpoch: "epoch-after",
    eventsByAgentId: { [agentId]: [{
      ...envelope(agentId, 1, "brief_created", { brief_id: "brief-1" }), event_log_epoch: "epoch-after",
    }] },
    briefsById: { "brief-1": brief(agentId, 1) },
  });
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.__HOLON_E2E__?.snapshot())).toMatchObject({
    discovery: { mode: "authoritative", freshness: "fresh" }, agentIds: [agentId],
  });
  await expect.poll(() => readState(page, agentId)).toMatchObject({
    event_log_epoch: "epoch-after", read_through_event_seq: 0, unread_count: 1,
  });
  await openAgent(page, agentId);
  await expect.poll(() => conversation(page, agentId)).toMatchObject({
    status: "ready", eventLogEpoch: "epoch-after", throughSeq: 1, turnIds: ["turn-1"],
  });
  await expect(page.getByText("Brief 1", { exact: true })).toBeVisible();
  await expect(page.locator('[data-turn-id="turn-2"]')).toHaveCount(0);
  await expect.poll(() => readState(page, agentId)).toMatchObject({
    event_log_epoch: "epoch-after", read_through_event_seq: 1, unread_count: 0,
  });
  expect(await page.evaluate(async () => (await indexedDB.databases()).map((db) => db.name)))
    .not.toContain("holon.webGui.eventLedger.v1");
});

test("read markers converge across tabs in one context and stay isolated across contexts", async ({
  browser, context, page, request,
}, info) => {
  const session = sessionFor(info);
  const agentId = "read-agent";
  await configure(request, session, seed(agentId));
  await attachSession(context, session);
  // This tab never enters a conversation or publishes its own read marker.
  await page.goto("/");
  await expect.poll(() => readState(page, agentId)).toMatchObject({ read_through_event_seq: 0, unread_count: 1 });

  // Same runtime/scope identity, separate server cursor and browser storage:
  // server-side cursor sharing cannot disguise accidental cross-context copies.
  const isolatedSession = `${session}-isolated`;
  await configure(request, isolatedSession, seed(agentId));
  const isolatedContext = await browser.newContext();
  const isolatedRequests = observeRetiredRequests(isolatedContext);
  try {
    await attachSession(isolatedContext, isolatedSession);
    const isolatedPage = await isolatedContext.newPage();
    await isolatedPage.goto(new URL("/", page.url()).href);
    await expect.poll(() => readState(isolatedPage, agentId)).toMatchObject({ read_through_event_seq: 0, unread_count: 1 });

    const sibling = await context.newPage();
    await sibling.goto(`/agents/${agentId}/conversation`);
    await sibling.bringToFront();
    await expect.poll(() => conversation(sibling, agentId)).toMatchObject({
      status: "ready", throughSeq: 2, turnIds: ["turn-2"],
    });
    await expect.poll(() => readState(sibling, agentId)).toMatchObject({ read_through_event_seq: 2 });
    await expect.poll(() => readState(page, agentId)).toMatchObject({ read_through_event_seq: 2, unread_count: 0 });
    expect(await conversation(page, agentId)).toBeNull();
    await expect(page.locator(".agent-row-unread")).toHaveCount(0);

    await appendUnread(request, session, agentId);
    await sibling.reload();
    await sibling.bringToFront();
    await expect.poll(() => conversation(sibling, agentId)).toMatchObject({
      status: "ready", throughSeq: 4, turnIds: ["turn-2", "turn-4"],
    });
    await expect(sibling.getByText("Brief 4", { exact: true })).toBeVisible();
    await expect.poll(() => readState(sibling, agentId)).toMatchObject({ read_through_event_seq: 4 });
    await expect.poll(() => readState(page, agentId)).toMatchObject({ read_through_event_seq: 4 });
    await expect(isolatedPage.locator(".agent-row-unread")).toHaveCount(1);
    expect(await readState(isolatedPage, agentId)).toMatchObject({ read_through_event_seq: 0, unread_count: 1 });

    await sibling.close();
    await page.close();
    const reopened = await context.newPage();
    await reopened.goto("/");
    await expect.poll(() => readState(reopened, agentId)).toMatchObject({ read_through_event_seq: 4, unread_count: 0 });
    await isolatedPage.reload();
    await expect.poll(() => readState(isolatedPage, agentId)).toMatchObject({ read_through_event_seq: 0, unread_count: 1 });
  } finally {
    await isolatedContext.close();
    expect(isolatedRequests).toEqual([]);
  }
});

for (const briefStatus of ["loading", "failed"] as const) {
  test(`${briefStatus} brief acknowledges the ready scope head independently of lazy body loading`, async ({
    context, page, request,
  }, info) => {
    const session = sessionFor(info);
    const agentId = `brief-${briefStatus}-agent`;
    await configure(request, session, {
      ...seed(agentId),
      blockedBriefIds: briefStatus === "loading" ? ["brief-2"] : [],
    });
    await attachSession(context, session);

    let failBrief = briefStatus === "failed";
    await context.route(`**/api/agents/${agentId}/briefs/brief-2`, async (route) => {
      if (failBrief) {
        await route.fulfill({ status: 503, json: { error: "brief temporarily unavailable" } });
      } else {
        await route.continue();
      }
    });
    const readAdvances: number[] = [];
    context.on("request", (request) => {
      if (request.method() === "POST" && new URL(request.url()).pathname === `/api/agents/${agentId}/brief-read-cursor`) {
        readAdvances.push(request.postDataJSON().read_through_event_seq);
      }
    });

    const expectBlockedBrief = async (activePage: Page) => {
      await activePage.bringToFront();
      // The marker acknowledges the displayed scope/head, not lazy Brief text.
      await expect.poll(() => conversation(activePage, agentId)).toMatchObject({
        status: "ready", throughSeq: 2, turnIds: ["turn-2"],
      });
      await expect(activePage.locator(`.conversation-brief.is-${briefStatus === "failed" ? "error" : "loading"}`)).toBeVisible();
      await expectCachedTurn(activePage, agentId, "turn-2");
      await expect.poll(() => readState(activePage, agentId)).toMatchObject({
        event_head_seq: 2, read_through_event_seq: 2, unread_count: 0,
      });
      await expect.poll(() => readAdvances).toContain(2);
      await expect(activePage.getByText("Brief 2", { exact: true })).toHaveCount(0);
    };

    await page.goto(`/agents/${agentId}/conversation`);
    await expectBlockedBrief(page);
    await page.reload();
    await expectBlockedBrief(page);
    await page.close();
    const reopened = await context.newPage();
    await reopened.goto(`/agents/${agentId}/conversation`);
    await expectBlockedBrief(reopened);
    await reopened.close();

    failBrief = false;
    expect((await request.post(controlPath(session, "/__e2e__/release-briefs"), {
      data: { briefIds: ["brief-2"] },
    })).ok()).toBe(true);
    const recovered = await context.newPage();
    await recovered.goto(`/agents/${agentId}/conversation`);
    await recovered.bringToFront();
    await expect(recovered.getByText("Brief 2", { exact: true })).toHaveCount(1);
    await expect.poll(() => readState(recovered, agentId)).toMatchObject({
      event_head_seq: 2, read_through_event_seq: 2, unread_count: 0,
    });
    await expect.poll(() => readAdvances).toContain(2);
  });
}

test("cached conversation survives reload and close without advancing a stale read marker", async ({
  context, page, request,
}, info) => {
  const session = sessionFor(info);
  const agentId = "cached-agent";
  await configure(request, session, seed(agentId));
  await attachSession(context, session);
  await page.goto(`/agents/${agentId}/conversation`);
  await expect.poll(() => conversation(page, agentId)).toMatchObject({ status: "ready", throughSeq: 2, turnIds: ["turn-2"] });
  await expect(page.getByText("Brief 2", { exact: true })).toBeVisible();
  await expect.poll(() => readState(page, agentId)).toMatchObject({ read_through_event_seq: 2 });
  await expectCachedTurn(page, agentId, "turn-2");

  const readAdvances: number[] = [];
  context.on("request", (request) => {
    if (request.method() === "POST" && new URL(request.url()).pathname === `/api/agents/${agentId}/brief-read-cursor`) {
      readAdvances.push(request.postDataJSON().read_through_event_seq);
    }
  });
  await appendUnread(request, session, agentId);
  await configure(request, session, { failConversationByAgentId: [agentId] });
  const failed = page.waitForResponse((response) =>
    new URL(response.url()).pathname === `/api/agents/${agentId}/conversation` && response.status() === 503);
  await page.reload();
  await failed;
  await expect.poll(() => conversation(page, agentId)).toMatchObject({
    status: "reconnecting", throughSeq: 2, turnIds: ["turn-2"],
  });
  await expect(page.locator('[data-turn-id="turn-2"]')).toHaveCount(1);
  await expect.poll(() => readState(page, agentId)).toMatchObject({ event_head_seq: 4, read_through_event_seq: 2, unread_count: 1 });

  await page.close();
  const reopened = await context.newPage();
  await reopened.goto(`/agents/${agentId}/conversation`);
  await expect.poll(() => conversation(reopened, agentId)).toMatchObject({
    status: "reconnecting", throughSeq: 2, turnIds: ["turn-2"],
  });
  await expect(reopened.locator('[data-turn-id="turn-2"]')).toHaveCount(1);
  await expect.poll(() => readState(reopened, agentId)).toMatchObject({ event_head_seq: 4, read_through_event_seq: 2, unread_count: 1 });
  expect(readAdvances.filter((seq) => seq > 2)).toEqual([]);

  await configure(request, session, { failConversationByAgentId: [] });
  await reopened.reload();
  await reopened.bringToFront();
  await expect.poll(() => conversation(reopened, agentId)).toMatchObject({
    status: "ready", throughSeq: 4, turnIds: ["turn-2", "turn-4"],
  });
  await expect.poll(() => readState(reopened, agentId)).toMatchObject({ event_head_seq: 4, read_through_event_seq: 4, unread_count: 0 });
  await expect(reopened.getByText("Brief 2", { exact: true })).toHaveCount(1);
  await expect(reopened.getByText("Brief 4", { exact: true })).toHaveCount(1);
  expect(readAdvances).toContain(4);
});

test("dashboard-only tab receives unread changes and a covered conversation stays unread", async ({ page, context, request }, info) => {
  const session = sessionFor(info);
  const agentId = "panel-read-agent";
  await configure(request, session, seed(agentId));
  await attachSession(context, session);
  await page.goto("/");
  await expect.poll(() => readState(page, agentId)).toMatchObject({ read_through_event_seq: 0, unread_count: 1 });
  expect(await conversation(page, agentId)).toBeNull();
  await expect(page.locator(".agent-row-unread")).toHaveCount(1);
  const sibling = await context.newPage();
  await sibling.addInitScript(() => localStorage.setItem("holon.webGui.contextPanel.v1", JSON.stringify({ open: true, mode: "expanded" })));
  await sibling.goto(`/agents/${agentId}/conversation`);
  await sibling.bringToFront();
  await expect(sibling.locator(".side-panel")).toBeVisible();
  await expect.poll(() => conversation(sibling, agentId)).toMatchObject({ status: "ready", throughSeq: 2, turnIds: ["turn-2"] });
  await expect.poll(() => readState(sibling, agentId)).toMatchObject({ read_through_event_seq: 0, unread_count: 1 });
  expect(await readState(page, agentId)).toMatchObject({ read_through_event_seq: 0, unread_count: 1 });
  await sibling.keyboard.press("Escape");
  // Escape changes expanded -> normal; the panel may stay open without
  // covering the conversation, which is sufficient to release the read gate.
  await expect(sibling.locator(".side-panel")).toHaveAttribute("data-mode", "normal");
  await expect(sibling.locator(".side-panel")).toHaveAttribute("data-full", "false");
  await expect.poll(() => readState(sibling, agentId)).toMatchObject({ read_through_event_seq: 2, unread_count: 0 });
  await expect.poll(() => readState(page, agentId)).toMatchObject({ read_through_event_seq: 2, unread_count: 0 });
  expect(await conversation(page, agentId)).toBeNull();
  await expect(page.locator(".agent-row-unread")).toHaveCount(0);
  await sibling.close();
});
