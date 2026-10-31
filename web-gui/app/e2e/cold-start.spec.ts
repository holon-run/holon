import { expect, test } from "@playwright/test";
import { sessionFor } from "./test-session";

interface DiagnosticsSnapshot {
  bootstrapLoading: boolean;
  bootstrapError?: string;
  discovery: { mode: string; freshness: string };
  agentIds: string[];
}

test("manual agent refresh reads shared state and work items without raw events or hydration", async ({ page, context }, info) => {
  await context.addCookies([{
    name: "holon_e2e_session", value: sessionFor(info, "detail-conversation"), domain: "127.0.0.1", path: "/",
  }]);
  await page.goto("/agents/bootstrap-agent/conversation");
  const refresh = page.getByRole("button", { name: "Refresh agent detail", exact: true });
  await expect(refresh).toBeEnabled();
  const requests: string[] = [];
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (url.pathname.startsWith("/api/agents/bootstrap-agent/")) {
      requests.push(url.pathname + url.search);
    }
  });
  const stateResponse = page.waitForResponse((response) =>
    new URL(response.url()).pathname === "/api/agents/bootstrap-agent/state",
  );
  const workItemsResponse = page.waitForResponse((response) =>
    new URL(response.url()).pathname === "/api/agents/bootstrap-agent/work-items",
  );
  await refresh.click();
  expect((await stateResponse).ok()).toBe(true);
  expect((await workItemsResponse).ok()).toBe(true);
  await expect(refresh).toBeEnabled();
  expect(requests).toContain("/api/agents/bootstrap-agent/work-items?limit=50");
  expect(requests).not.toContain("/api/agents/bootstrap-agent/conversation?limit=50");
  // Neither the detail refresh nor diagnostics may resurrect raw-event recovery.
  expect(requests.filter((path) =>
    /^\/api\/agents\/bootstrap-agent\/(?:events(?:\/stream)?|projection-snapshot)(?:\?|$)/.test(path)
      || path.includes("messages:batchGet")
      || path.includes("transcript:batchGet"),
  )).toEqual([]);
});

for (const failure of [404, 503, "network"] as const) {
  test(`subject lookup ${failure} is visible and automatically recovers before bootstrap`, async ({ page, context }, info) => {
    await context.addCookies([{
      name: "holon_e2e_session", value: sessionFor(info, "subject-retry"), domain: "127.0.0.1", path: "/",
    }]);
    let fail = true;
    await page.route("**/api/auth/session/me", async (route) => {
      if (!fail) return route.continue();
      if (failure === "network") return route.abort("failed");
      return route.fulfill({ status: failure, json: { error: "Session lookup unavailable" } });
    });
    await page.goto("/");
    await expect.poll(() => page.evaluate(() => window.__HOLON_E2E__?.snapshot().bootstrapError)).toBeTruthy();
    await expect(page.locator(".connection-status")).toHaveAttribute("aria-label", /Disconnected/);
    expect(await page.evaluate(() => window.__HOLON_E2E__?.snapshot().agentIds)).toEqual([]);
    fail = false;
    await expect(page.getByText("1 agents · all clear", { exact: true })).toBeVisible({ timeout: 10_000 });
    await expect.poll(() => page.evaluate(() => window.__HOLON_E2E__?.snapshot().bootstrapError)).toBeFalsy();
    await expect(page.locator(".connection-status")).toHaveAttribute("aria-label", /Connected/);
  });
}

test("cold start refreshes the roster and loads conversation history only on selection", async ({ page, request }, testInfo) => {
  const session = sessionFor(testInfo);
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
      discovery: { mode: "authoritative", freshness: "fresh" },
      agentIds: ["bootstrap-agent"],
    } satisfies Partial<DiagnosticsSnapshot>);

  const initialRequests = await request.get(controlPath("/__e2e__/requests"))
    .then((response) => response.json());
  expect(initialRequests.requests).toEqual(expect.arrayContaining([
    "GET /api/handshake",
    "GET /api/agents/list",
    "GET /api/agents/snapshot",
  ]));
  expect(initialRequests.requests.filter((entry: string) => entry === "GET /api/events/stream")).toHaveLength(0);
  expect(initialRequests.requests.indexOf("GET /api/handshake"))
    .toBeLessThan(initialRequests.requests.indexOf("GET /api/agents/list"));

  await request.post(controlPath("/__e2e__/emit-agent"));

  // Without the dormant global event stream, roster changes surface through
  // the next explicit dashboard refresh (#3808 slice 2).
  await page.getByRole("button", { name: "Refresh", exact: true }).click();

  await expect(page.getByText("2 agents · all clear", { exact: true })).toBeVisible();
  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot().agentIds))
    .toEqual(["bootstrap-agent", "e2e-agent"]);

  const finalRequests = await request.get(controlPath("/__e2e__/requests"))
    .then((response) => response.json());
  expect(finalRequests.requests.filter((entry: string) => entry === "GET /api/agents/snapshot")).toHaveLength(2);
  expect(finalRequests.requests).toContain("GET /api/agents/brief-read-states");
  // Dashboard discovery needs neither an eager projection nor a conversation
  // controller for any agent. Selection owns the history request boundary.
  expect(finalRequests.requests.filter((entry: string) =>
    /\/api\/agents\/[^/]+\/(?:projection-snapshot|conversation)(?:[/?]|$)/.test(entry),
  )).toEqual([]);
  expect(finalRequests.requests.filter((entry: string) => entry.includes("/events"))).toEqual([]);
  expect(await page.evaluate(() => window.__HOLON_E2E__?.conversation("e2e-agent"))).toBeNull();

  // Opening an agent speaks the conversation read-model contract: one
  // capability handshake, one summary snapshot, one live conversation
  // stream — and no legacy per-agent raw event stream.
  await page.getByRole("button", { name: "Open e2e-agent", exact: true }).click();
  await expect(
    page.getByRole("region", { name: "Agent conversation" }),
  ).toBeVisible();
  await expect.poll(async () => {
    const conversationRequests = await request.get(controlPath("/__e2e__/requests"))
      .then((response) => response.json());
    return conversationRequests.requests;
  }).toEqual(expect.arrayContaining([
    "GET /api/agents/e2e-agent/conversation?limit=30",
    "GET /api/agents/e2e-agent/conversation/stream",
  ]));
  const conversationRequests = await request.get(controlPath("/__e2e__/requests"))
    .then((response) => response.json());
  expect(conversationRequests.requests.filter(
    (entry: string) => entry === "GET /api/agents/e2e-agent/conversation?limit=30",
  )).toHaveLength(1);
  expect(conversationRequests.requests.filter(
    (entry: string) => entry === "GET /api/agents/e2e-agent/conversation/stream",
  )).toHaveLength(1);
  expect(conversationRequests.requests.filter(
    (entry: string) => entry === "GET /api/agents/e2e-agent/events/stream",
  )).toHaveLength(0);
  expect(conversationRequests.requests.filter(
    (entry: string) => entry.includes("messages:batchGet")
      || entry.includes("transcript:batchGet")
      || entry.includes("/projection-snapshot")
      || entry.includes("/events")
  )).toHaveLength(0);
  await expect.poll(() => page.evaluate(() => window.__HOLON_E2E__?.conversation("e2e-agent")))
    .toMatchObject({ status: "ready", eventLogEpoch: "e2e-epoch" });
});
