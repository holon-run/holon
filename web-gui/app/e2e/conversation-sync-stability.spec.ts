import { expect, test } from "@playwright/test";

test("pause and reconnect preserve process expansion and transcript geometry without re-fetching the summary", async ({ page, context, request }, info) => {
  const agentId = "bootstrap-agent";
  const session = `sync-stability-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const current = { turn_id: "live", key: { turn_id: "live", turn_index: 1 }, revision: 1,
    presentation_class: "operator", inputs: [{ message_id: "input-live", preview: "Check sync stability" }],
    execution: { kind: "active" }, result: { kind: "pending" }, settled: false,
    attention: null, detail_coverage: { kind: "complete" }, brief_ids: [] };
  await request.post(control("/__e2e__/conversation"), { data: { agentId, turns: [current], activitiesByTurnId: { live: [
    { kind: "assistant", id: "progress", key: { event_seq: 1, activity_id: "progress" }, revision: 1,
      summary: "Checking the layout while the connection recovers." },
  ] } } });
  await page.goto(`/agents/${agentId}/conversation`);
  const process = page.locator('[data-turn-id="live"] .conversation-detail-toggle');
  const content = page.getByText("Checking the layout while the connection recovers.", { exact: true });
  await expect(content).toBeVisible();
  await expect(process).toHaveAttribute("aria-expanded", "true");
  const geometry = () => page.evaluate(() => {
    const list = document.querySelector(".message-list")!;
    const rect = document.querySelector('[data-activity-id="progress"]')!.getBoundingClientRect();
    return { y: rect.y, height: rect.height, scrollHeight: list.scrollHeight, scrollTop: list.scrollTop };
  });
  // Wait for the original expansion animation to finish.
  await expect.poll(async () => (await content.boundingBox())!.height).toBeGreaterThan(0);
  await page.waitForTimeout(300);
  const before = await geometry();
  const setVisibility = (state: string) => page.evaluate((value) => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value });
    document.dispatchEvent(new Event("visibilitychange"));
  }, state);
  await setVisibility("hidden");
  await expect(process).toContainText("Syncing");
  await page.waitForTimeout(300);
  await expect(process).toHaveAttribute("aria-expanded", "true");
  expect(await geometry()).toEqual(before);
  await setVisibility("visible");
  await expect(process).toContainText("Working");
  expect(await geometry()).toEqual(before);

  await request.post(control("/__e2e__/configure"), { data: { failConversationByAgentId: [agentId] } });
  await request.post(control("/__e2e__/disconnect-streams"));
  await expect(page.locator(".conversation-sync-status .is-reconnecting")).toBeVisible();
  await page.waitForTimeout(300);
  await expect(process).toHaveAttribute("aria-expanded", "true");
  await expect(content).toBeVisible();
  expect(await geometry()).toEqual(before);
  await page.screenshot({ path: "/tmp/holon-sync-reconnecting.png" });
  await request.post(control("/__e2e__/configure"), { data: { failConversationByAgentId: [] } });
  await expect(process).toContainText("Working", { timeout: 10000 });
  expect(await geometry()).toEqual(before);
  const diagnostics = await (await request.get(control("/__e2e__/state"))).json();
  expect(diagnostics.requests.filter((path: string) => path.startsWith(`GET /api/agents/${agentId}/conversation?`))).toHaveLength(1);

  // Force a protocol reset and hold the fresh summary: stale content stays mounted,
  // while the SDK has discarded its checkpoint and cannot advance read markers.
  let releaseSummary!: () => void;
  let summaryRequested = false;
  const gate = new Promise<void>((resolve) => { releaseSummary = resolve; });
  await page.route(`**/api/agents/${agentId}/conversation?*`, async (route) => {
    summaryRequested = true;
    await gate;
    await route.continue();
  });
  await request.post(control("/__e2e__/reset-conversation"), { data: { agentId, reason: "retention_expired" } });
  await expect.poll(() => summaryRequested).toBe(true);
  await expect(process).toContainText("Syncing");
  await expect(content).toBeVisible();
  expect(await geometry()).toEqual(before);
  releaseSummary();
  await expect(process).toContainText("Working");
  await expect(content).toBeVisible();
  await page.unroute(`**/api/agents/${agentId}/conversation?*`);

  // Manual collapse is equally stable across pause/resume.
  await process.click();
  await setVisibility("hidden");
  await expect(process).toHaveAttribute("aria-expanded", "false");
  await setVisibility("visible");
  await expect(process).toContainText("Working");
  await expect(process).toHaveAttribute("aria-expanded", "false");
  await page.setViewportSize({ width: 390, height: 844 });
  await setVisibility("hidden");
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await expect(page.locator(".conversation-sync-status")).toHaveCSS("height", "32px");
});
