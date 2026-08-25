import { expect, test } from "@playwright/test";

interface DiagnosticsSnapshot {
  bootstrapLoading: boolean;
  bootstrapError?: string;
  globalStreamStatus: string;
  discovery: { mode: string; freshness: string };
  agentIds: string[];
}

test("cold start bootstraps and applies a live roster event", async ({ page, request }, testInfo) => {
  const session = `${testInfo.workerIndex}-${testInfo.repeatEachIndex}-${testInfo.retry}`;
  const controlPath = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await page.context().addCookies([{
    name: "holon_e2e_session",
    value: session,
    domain: "127.0.0.1",
    path: "/",
  }]);
  await page.goto("/");

  await expect(page.locator('aside[aria-label="Holon navigation"]')).toBeVisible();
  await expect(page.locator('nav[aria-label="Global navigation"]')).toBeVisible();
  await expect(page.getByText("1 agents · all clear", { exact: true })).toBeVisible();

  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot()))
    .toMatchObject({
      bootstrapLoading: false,
      globalStreamStatus: "streaming",
      discovery: { mode: "authoritative", freshness: "fresh" },
      agentIds: ["bootstrap-agent"],
    } satisfies Partial<DiagnosticsSnapshot>);

  const initialRequests = await request.get(controlPath("/__e2e__/requests"))
    .then((response) => response.json());
  expect(initialRequests.requests).toEqual(expect.arrayContaining([
    "GET /api/handshake",
    "GET /api/agents/list",
    "GET /api/agents/snapshot",
    "GET /api/events/stream",
  ]));
  expect(initialRequests.requests.indexOf("GET /api/handshake"))
    .toBeLessThan(initialRequests.requests.indexOf("GET /api/agents/list"));

  await request.post(controlPath("/__e2e__/emit-agent"));

  await expect(page.getByText("2 agents · all clear", { exact: true })).toBeVisible();
  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot().agentIds))
    .toEqual(["bootstrap-agent", "e2e-agent"]);

  await expect.poll(async () => {
    const finalRequests = await request.get(controlPath("/__e2e__/requests"))
      .then((response) => response.json());
    return finalRequests.requests;
  }).toEqual(expect.arrayContaining([
    "GET /api/agents/snapshot",
    "GET /api/agents/e2e-agent/projection-snapshot",
    "GET /api/agents/e2e-agent/events?after_seq=1&limit=100&order=asc",
    "GET /api/agents/e2e-agent/events?limit=100&order=desc",
  ]));

  const finalRequests = await request.get(controlPath("/__e2e__/requests"))
    .then((response) => response.json());
  expect(finalRequests.requests.filter((entry: string) => entry === "GET /api/agents/snapshot")).toHaveLength(2);
  expect(finalRequests.requests.filter((entry: string) => entry === "GET /api/events/stream")).toHaveLength(1);
  expect(finalRequests.requests.filter(
    (entry: string) => entry === "GET /api/agents/e2e-agent/projection-snapshot",
  )).toHaveLength(1);
  expect(finalRequests.requests.filter(
    (entry: string) => entry === "GET /api/agents/e2e-agent/events?after_seq=1&limit=100&order=asc",
  )).toHaveLength(1);
  expect(finalRequests.requests.filter(
    (entry: string) => entry === "GET /api/agents/e2e-agent/events?limit=100&order=desc",
  )).toHaveLength(1);
});
