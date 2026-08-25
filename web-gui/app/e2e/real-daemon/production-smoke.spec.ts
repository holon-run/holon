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

test("production assets authenticate and use real snapshot, SSE, and route refresh", async ({
  daemonFactory,
  page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist" });
  const requests: string[] = [];
  page.on("request", (request) => {
    if (request.url().startsWith(`${daemon.baseUrl}/api/`)) {
      requests.push(`${request.method()} ${new URL(request.url()).pathname}`);
    }
  });

  const unauthorized = await page.request.get(`${daemon.baseUrl}/api/handshake`);
  expect(unauthorized.status()).toBe(403);

  await installLocalToken(page, daemon.token);
  await page.goto(daemon.baseUrl);
  await expect(page.locator('aside[aria-label="Holon navigation"]')).toBeVisible();
  await expect(page.getByText(/\d+ agents ·/)).toBeVisible();
  await expect.poll(() => requests).toEqual(expect.arrayContaining([
    "GET /api/handshake",
    "GET /api/agents/snapshot",
    "GET /api/events/stream",
  ]));
  expect(await page.evaluate(() => window.__HOLON_E2E__)).toBeUndefined();

  await daemon.enqueue("production SSE marker");
  await page.goto(`${daemon.baseUrl}/agents/${daemon.agentId}`);
  await expect(page).toHaveURL(new RegExp(`/agents/${daemon.agentId}$`));
  await expect(page.locator("main")).toBeVisible();
  await page.reload();
  await expect(page).toHaveURL(new RegExp(`/agents/${daemon.agentId}$`));
  await expect(page.locator('aside[aria-label="Holon navigation"]')).toBeVisible();
});
