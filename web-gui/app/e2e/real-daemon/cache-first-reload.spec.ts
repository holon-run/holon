import type { Page } from "@playwright/test";

import { expect, test } from "./daemon-fixture";

const MARKER = "real daemon persisted reload cache-first marker";
const BUDDY_ID = "persisted-reload-buddy";
const BOOTSTRAP_URL = /\/api\/(?:handshake|agents\/list)(?:\?|$)/;
const IDENTITY_URL = /\/api\/control\/agents\/default\/detail(?:\?|$)/;
const STATE_URL = /\/api\/agents\/default\/state(?:\?|$)/;
const SESSION_URL = /\/api\/auth\/session\/me(?:\?|$)/;

interface CacheDaemon {
  baseUrl: string;
  token: string;
  api(pathname: string, init?: RequestInit): Promise<Response>;
  enqueue(text: string): Promise<string>;
}

function conversationStatus(page: Page) {
  return page.evaluate(() => window.__HOLON_E2E__?.snapshot().conversationStatus?.kind);
}

async function warmPersistentCache(page: Page, daemon: CacheDaemon): Promise<void> {
  await page.addInitScript((token) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local", token,
    }));
  }, daemon.token);
  const created = await daemon.api(`/control/agents/${BUDDY_ID}/create`, {
    method: "POST",
    body: JSON.stringify({ authority_class: "operator_instruction", template: null }),
  });
  expect(created.ok).toBe(true);
  await daemon.enqueue(MARKER);
  await page.goto(`${daemon.baseUrl}/agents/default/conversation`);
  await expect(page.locator(".message-list")).toContainText(MARKER);
  await expect(page.locator(`.agent-row[title*="${BUDDY_ID}"]`)).toBeVisible();
  await expect(page.locator(".composer textarea")).toBeEnabled();
  await expect.poll(() => conversationStatus(page)).toBe("ready");

  // Observe the persisted data, rather than assuming a debounce has fired.
  // The subsequent reload must restore both roster and history from disk.
  await expect.poll(() => page.evaluate(async ({ marker, buddyId }) => {
    return await new Promise<boolean>((resolve, reject) => {
      const request = indexedDB.open("holon-webgui-cache");
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains("agentPresentations")
          || !db.objectStoreNames.contains("snapshots")) {
          db.close();
          resolve(false);
          return;
        }
        const tx = db.transaction(["agentPresentations", "snapshots"], "readonly");
        const presentations = tx.objectStore("agentPresentations").getAll();
        const snapshots = tx.objectStore("snapshots").getAll();
        tx.oncomplete = () => {
          db.close();
          const hasRoster = presentations.result.some((entry) =>
            entry.agents.some((agent: { id: string }) => agent.id === buddyId)
            && entry.agents.some((agent: { id: string }) => agent.id === "default"));
          const hasHistory = snapshots.result.some((entry) =>
            entry.agentId === "default" && JSON.stringify(entry.summary).includes(marker));
          resolve(hasRoster && hasHistory);
        };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
    });
  }, { marker: MARKER, buddyId: BUDDY_ID })).toBe(true);
}

async function holdRealResponse(page: Page, url: RegExp) {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  const probe = { requests: 0, responses: 0, release: () => release() };
  page.on("response", (response) => {
    if (url.test(response.url())) probe.responses++;
  });
  await page.route(url, async (route) => {
    probe.requests++;
    await gate;
    await route.continue();
  });
  return probe;
}

function observeReadWrites(page: Page): string[] {
  const writes: string[] = [];
  page.on("request", (request) => {
    if (request.method() === "POST"
      && /\/api\/agents\/default\/brief-read-cursor(?:\?|$)/.test(request.url())) {
      writes.push(request.url());
    }
  });
  return writes;
}

async function expectReadonlyCache(page: Page): Promise<void> {
  await expect(page.locator(`.agent-row[title*="${BUDDY_ID}"]`)).toBeVisible();
  await expect(page.locator('.agent-row[title*="default"]')).toBeVisible();
  await expect(page.locator(".message-list")).toContainText(MARKER);
  await expect(page.getByText("Loading agent…", { exact: true })).toHaveCount(0);
  await expect(page.locator(".composer textarea")).toBeEnabled();
  await page.locator(".composer textarea").fill("Draft while real access is pending");
  await expect(page.locator(".composer .send-button")).toBeDisabled();
}

test("reload restores persisted roster and conversation before real bootstrap and access responses", async ({
  daemonFactory, page,
}, testInfo) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await warmPersistentCache(page, daemon);
  const bootstrap = await holdRealResponse(page, BOOTSTRAP_URL);
  const identity = await holdRealResponse(page, IDENTITY_URL);
  const state = await holdRealResponse(page, STATE_URL);
  const readWrites = observeReadWrites(page);
  try {
    const started = performance.now();
    // A real document reload removes all kept-alive scopes and JS store state.
    await page.reload();
    await expectReadonlyCache(page);
    const cachedHistoryVisibleMs = performance.now() - started;
    await expect.poll(() => bootstrap.requests).toBeGreaterThan(0);
    await expect.poll(() => identity.requests).toBeGreaterThan(0);
    await expect.poll(() => state.requests).toBeGreaterThan(0);
    expect([bootstrap.responses, identity.responses, state.responses]).toEqual([0, 0, 0]);
    expect(readWrites).toEqual([]);
    await testInfo.attach("persisted-reload-preview", {
      body: JSON.stringify({
        cachedHistoryVisibleMs,
        bootstrapRequests: bootstrap.requests,
        identityRequests: identity.requests,
        stateRequests: state.requests,
        readMarkerWritesBeforeValidation: readWrites.length,
        realResponsesHeldUntilAfterCacheVisible: true,
      }, null, 2),
      contentType: "application/json",
    });

    bootstrap.release();
    await expect.poll(() => bootstrap.responses).toBeGreaterThan(0);
    await expectReadonlyCache(page);
    expect(readWrites).toEqual([]);
    identity.release();
    state.release();
    await expect(page.locator(".composer .send-button")).toBeEnabled();
    await expect(page.locator(".composer textarea")).toHaveValue("Draft while real access is pending");
    await expect.poll(() => conversationStatus(page)).toBe("ready");
    await expect(page.locator(".message-list")).toContainText(MARKER);
  } finally {
    bootstrap.release();
    identity.release();
    state.release();
  }
});

test("reload keeps private caches hidden until session confirmation even while bootstrap is pending", async ({
  daemonFactory, page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await warmPersistentCache(page, daemon);
  const session = await holdRealResponse(page, SESSION_URL);
  const bootstrap = await holdRealResponse(page, BOOTSTRAP_URL);
  const identity = await holdRealResponse(page, IDENTITY_URL);
  const state = await holdRealResponse(page, STATE_URL);
  const readWrites = observeReadWrites(page);
  try {
    await page.reload();
    await expect.poll(() => session.requests).toBeGreaterThan(0);
    // Give accidental asynchronous IDB hydration a chance to reveal private data.
    await page.waitForTimeout(300);
    await expect(page.getByText(MARKER, { exact: false })).toHaveCount(0);
    await expect(page.locator(`.agent-row[title*="${BUDDY_ID}"]`)).toHaveCount(0);
    await expect(page.locator('.agent-row[title*="default"]')).toHaveCount(0);
    expect(session.responses).toBe(0);
    expect(bootstrap.requests).toBe(0);
    expect(readWrites).toEqual([]);

    session.release();
    await expect.poll(() => session.responses).toBeGreaterThan(0);
    await expectReadonlyCache(page);
    await expect.poll(() => bootstrap.requests).toBeGreaterThan(0);
    await expect.poll(() => identity.requests).toBeGreaterThan(0);
    await expect.poll(() => state.requests).toBeGreaterThan(0);
    expect([bootstrap.responses, identity.responses, state.responses]).toEqual([0, 0, 0]);
    expect(readWrites).toEqual([]);

    bootstrap.release();
    identity.release();
    state.release();
    await expect(page.locator(".composer .send-button")).toBeEnabled();
    await expect(page.locator(".composer textarea")).toHaveValue("Draft while real access is pending");
    await expect.poll(() => conversationStatus(page)).toBe("ready");
    await expect(page.locator(".message-list")).toContainText(MARKER);
  } finally {
    session.release();
    bootstrap.release();
    identity.release();
    state.release();
  }
});

test("verified admission survives transient background state failures but not identity denial", async ({
  daemonFactory, page,
}) => {
  test.setTimeout(60_000);
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await warmPersistentCache(page, daemon);
  let failures = 0;
  await page.route(STATE_URL, async (route) => {
    failures++;
    await route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({ error: "temporary state service outage" }),
    });
  });
  const delayed = page.getByText("Agent status refresh is delayed", { exact: false });
  try {
    // Only a real background poll on this still-mounted route may retain
    // admission: no navigation, reload, or explicit retry is involved.
    await expect.poll(() => failures, { timeout: 15_000 }).toBeGreaterThan(0);
    await expect(delayed).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".composer textarea")).toBeEnabled();
    await expect(page.getByText("Access could not be verified", { exact: false })).toHaveCount(0);
    await expect(page.locator(".message-list")).toContainText(MARKER);

    const recovered = page.waitForResponse(
      (response) => STATE_URL.test(response.url()) && response.status() === 200,
      { timeout: 20_000 },
    );
    await page.unroute(STATE_URL);
    await recovered;
    await expect(delayed).toHaveCount(0, { timeout: 15_000 });
    await expect(page.locator(".composer textarea")).toBeEnabled();
    await expect.poll(() => conversationStatus(page)).toBe("ready");

    let denials = 0;
    await page.route(IDENTITY_URL, async (route) => {
      denials++;
      await route.fulfill({
        status: 403,
        contentType: "application/json",
        body: JSON.stringify({ error: "agent access denied" }),
      });
    });
    await expect.poll(() => denials, { timeout: 15_000 }).toBeGreaterThan(0);
    await expect(page.locator(".composer textarea:enabled")).toHaveCount(0);
    await expect(page.getByText(MARKER, { exact: false })).toHaveCount(0);
  } finally {
    await page.unroute(STATE_URL);
    await page.unroute(IDENTITY_URL);
  }
});
