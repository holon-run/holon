import http from "node:http";
import type { Page } from "@playwright/test";
import type { ConversationSummaryResponse } from "@holon/conversation-sdk";

import { expect, test as daemonTest } from "./daemon-fixture";

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
  responses: Array<{ agentId: string; status: number; etag?: string }>;
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
        etag: response.headers().etag,
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

const test = daemonTest.extend<{ cacheProvider: string }>({
  cacheProvider: async ({}, use) => {
    // A successful local provider makes the conversation genuinely idle before
    // ETag revalidation, rather than depending on the default 500 retry budget.
    const provider = http.createServer((request, response) => {
      request.resume();
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({
        id: "resp_cache",
        status: "completed",
        output: [{
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "cache-effect brief" }],
        }],
        usage: { input_tokens: 1, output_tokens: 1 },
      }));
    });
    await new Promise<void>((resolve, reject) => {
      provider.once("error", reject);
      provider.listen(0, "127.0.0.1", resolve);
    });
    try {
      const address = provider.address();
      if (!address || typeof address === "string") throw new Error("missing provider port");
      await use(`http://127.0.0.1:${address.port}/v1`);
    } finally {
      provider.closeAllConnections();
      await new Promise<void>((resolve, reject) =>
        provider.close((error) => error ? reject(error) : resolve()));
    }
  },
});

test("conversation caches cut refetches across agent switches, reloads, and revalidation", async ({
  daemonFactory,
  cacheProvider,
  page,
}) => {
  const daemon = await daemonFactory({
    webDist: "dist-e2e",
    env: {
      OPENAI_API_KEY: "test",
      HOLON_OPENAI_BASE_URL: cacheProvider,
    },
  });
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
  const messageId = await daemon.enqueue("cache-effect marker");
  // A visible input alone does not mean the response (and its ETag) is stable.
  await expect.poll(async () => {
    const response = await daemon.api("/agents/default/conversation");
    expect(response.ok).toBe(true);
    const summary = await response.json() as ConversationSummaryResponse;
    return summary.turns.find((turn) =>
      turn.inputs.some((input) => input.message_id === messageId))?.execution.kind;
  }, { timeout: 15_000 }).toBe("terminal");
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
  // Keep the pass-through route so Playwright disables the browser HTTP cache.
  // Otherwise Chromium may merge a wire 304 into a cached 200, obscuring the
  // SDK's explicit persisted-snapshot revalidation below.
  await expect.poll(() => conversationStatus(page)).toBe("ready");

  // P3 — conditional revalidation: an idle conversation reload answers
  // 304, so the full snapshot payload is never transferred again.
  // `ready` precedes stream consumption and persistence. Wait for the actual
  // full-response ETag in IndexedDB, not just the rendered turns.
  const etag = probe.responses.filter((response) =>
    response.agentId === "default" && response.status === 200).at(-1)?.etag;
  expect(etag).toBeTruthy();
  await expect.poll(() => page.evaluate(async (expectedEtag) => {
    return await new Promise<boolean>((resolve, reject) => {
      const request = indexedDB.open("holon-webgui-cache");
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction("snapshots", "readonly");
        const entries = tx.objectStore("snapshots").getAll();
        tx.oncomplete = () => {
          db.close();
          resolve(entries.result.some((entry) =>
            entry.agentId === "default" && entry.etag === expectedEtag));
        };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
    });
  }, etag)).toBe(true);
  const responsesBefore = probe.responses.length;
  const requestsBefore = probe.requests.length;
  await page.reload();
  await expect.poll(() => conversationStatus(page)).toBe("ready");
  await expect
    .poll(() => probe.responses.length)
    .toBeGreaterThan(responsesBefore);
  const revalidationRequest = probe.requests[requestsBefore];
  expect(revalidationRequest?.agentId).toBe("default");
  expect(revalidationRequest?.ifNoneMatch).toBe(etag);
  const revalidation = probe.responses[responsesBefore];
  expect(revalidation?.agentId).toBe("default");
  expect(revalidation?.status).toBe(304);
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
