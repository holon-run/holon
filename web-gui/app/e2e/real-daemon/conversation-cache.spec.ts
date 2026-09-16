import type { Page } from "@playwright/test";

import { expect, test } from "./daemon-fixture";

async function installLocalToken(page: Page, token: string): Promise<void> {
  await page.addInitScript((value) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local",
      token: value,
    }));
 }, token);
}

interface SummaryProbe {
  requests: Array<{ agentId: string; ifNoneMatch?: string }>;
  responses: Array<{ agentId: string; status: number }>;
}

const SUMMARY_URL = /\/api\/agents\/([^/]+)\/conversation(?:\?|$)/;

function trackConversationSummary(page: Page): SummaryProbe {
  const probe: SummaryProbe = { requests: [], responses: [] };
  page.on("request", (request) => {
    const match = SUMMARY_URL.exec(request.url());
    if (match && request.method() === "GET") {
      probe.requests.push({
        agentId: decodeURIComponent(match[1]),
        ifNoneMatch: request.headers()["if-none-match"],
      });
    }
  });
  page.on("response", (response) => {
    const match = SUMMARY_URL.exec(response.url());
    if (match) {
      probe.responses.push({
        agentId: decodeURIComponent(match[1]),
        status: response.status(),
      });
    }
  });
  return probe;
}

async function conversationStatus(page: Page): Promise<string | undefined> {
  return await page.evaluate(
    () => window.__HOLON_E2E__?.snapshot().conversationStatus?.kind,
  );
}

test("conversation caches cut refetches across agent switches, reloads, and revalidation", async ({
  daemonFactory,
  page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await installLocalToken(page, daemon.token);
  const probe = trackConversationSummary(page);

  // A second agent makes scope switching observable in the roster.
  const created = await daemon.api("/control/agents/cache-buddy/create", {
    method: "POST",
    body: JSON.stringify({ authority_class: "operator_instruction", template: null }),
  });
  expect(created.ok).toBe(true);

  // Deep-link straight into the default agent's conversation page.
  await page.goto(`${daemon.baseUrl}/agents/default`);
  await expect.poll(() => conversationStatus(page)).toBe("ready");

  // Give the default agent conversation visible content.
  await daemon.enqueue("cache-effect marker");
  await expect
    .poll(() => page.locator("[data-turn-id]").count())
    .toBeGreaterThan(0);

  // P0 — kept-alive scopes: switching away and back never refetches.
  const summaryCountDefault = () =>
    probe.requests.filter((request) => request.agentId === "default").length;
  // In-app (SPA) navigation releases the scope; keep-alive must retain it.
  await page.locator('.agent-row[title*="cache-buddy"]').click();
  await expect.poll(() => conversationStatus(page)).toBe("ready");
  const afterSwitchAway = summaryCountDefault();
  await page.locator('.agent-row[title*="default"]').click();
  await expect.poll(() => conversationStatus(page)).toBe("ready");
  // Allow any accidental refetch to surface before asserting none happened.
  await page.waitForTimeout(500);
  expect(summaryCountDefault()).toBe(afterSwitchAway);

  const turnsBeforeReload = await page.locator("[data-turn-id]").count();
  expect(turnsBeforeReload).toBeGreaterThan(0);
  // Observe persistence itself instead of guessing when the throttle has fired.
  const turnIds = await page.locator("[data-turn-id]").evaluateAll((elements) =>
    elements.map((element) => element.getAttribute("data-turn-id")!));
  await expect.poll(() => page.evaluate(async (expectedIds) => {
    return await new Promise<boolean>((resolve, reject) => {
      const request = indexedDB.open("holon-webgui-cache");
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction("snapshots", "readonly");
        const entries = tx.objectStore("snapshots").getAll();
        tx.oncomplete = () => {
          db.close();
          resolve(entries.result.some((entry) => entry.agentId === "default"
            && expectedIds.every((id) => entry.summary.turns.some((turn: { turn_id: string }) => turn.turn_id === id))));
        };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
    });
  }, turnIds)).toBe(true);

  // P2 — persisted snapshot: a reload renders the cached turns even while
  // the summary endpoint is unreachable, then revalidates once unblocked.
  let blocked = true;
  await page.route(SUMMARY_URL, async (route) => {
    if (blocked) {
      await route.abort("connectionfailed");
      return;
    }
    await route.continue();
  });
  await page.reload();
  await expect
    .poll(() => page.locator("[data-turn-id]").count())
    .toBe(turnsBeforeReload);
  await expect.poll(() => conversationStatus(page)).toBe("reconnecting");

  blocked = false;
  await page.unroute(SUMMARY_URL);
  await expect.poll(() => conversationStatus(page)).toBe("ready");

  // P3 — conditional revalidation: an idle conversation reload answers
  // 304, so the full snapshot payload is never transferred again.
  await page.waitForTimeout(500);
  const responsesBefore = probe.responses.length;
  const requestsBefore = probe.requests.length;
  await page.reload();
  await expect.poll(() => conversationStatus(page)).toBe("ready");
  await expect
    .poll(() => probe.responses.length)
    .toBeGreaterThan(responsesBefore);
  const revalidation = probe.responses[responsesBefore];
  expect(revalidation?.agentId).toBe("default");
  expect(revalidation?.status).toBe(304);
  const revalidationRequest = probe.requests[requestsBefore];
  expect(revalidationRequest?.agentId).toBe("default");
  expect(revalidationRequest?.ifNoneMatch).toBeTruthy();
  // The cached turns are still rendered after the 304 round trip.
  await expect
    .poll(() => page.locator("[data-turn-id]").count())
    .toBe(turnsBeforeReload);
});

test("idle scopes do not starve HTTP requests in another tab", async ({ daemonFactory, page }) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  for (const id of ["cache-a", "cache-b", "cache-c"]) {
    await daemon.api(`/control/agents/${id}/create`, {
      method: "POST",
      body: JSON.stringify({ authority_class: "operator_instruction", template: null }),
    });
  }
  await installLocalToken(page, daemon.token);
  await page.goto(`${daemon.baseUrl}/agents/default/conversation`);
  for (const id of ["default", "cache-a", "cache-b", "cache-c"]) {
    await page.locator(`.agent-row[title*="${id}"]`).first().click();
    await expect.poll(() => conversationStatus(page)).toBe("ready");
  }
  const second = await page.context().newPage();
  await installLocalToken(second, daemon.token);
  await second.goto(`${daemon.baseUrl}/agents/default/conversation`, { waitUntil: "domcontentloaded" });
  await expect.poll(() => conversationStatus(second), { timeout: 10000 }).toBe("ready");
  const status = await second.evaluate(async () => {
    const response = await fetch("/api/auth/method", { signal: AbortSignal.timeout(3000) });
    return response.status;
  });
  expect(status).toBe(200);
  await second.close();
});
