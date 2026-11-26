import { expect, test } from "@playwright/test";

import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const workId = "work-notification";
const target = { agent_id: agentId };
const notice = (id: string, kind: string, work_item_id?: string) => ({
  id, kind, created_at: "2026-11-22T04:00:00Z", target: { ...target, work_item_id },
});

test("reads both lists on the shared stream, does not replay, and opens the target without clearing pending", async ({
  page, context, request,
}, testInfo) => {
  const session = sessionFor(testInfo, "notifications");
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const control = async (data: Record<string, unknown>) => {
    const response = await request.post(`/__e2e__/notifications?session=${session}`, { data });
    expect(response.ok()).toBe(true);
    return await response.json() as { streams: number };
  };
  const silent = notice("silent", "operator_input_required", workId);
  const old = notice("old", "operator_first_brief");
  await control({ status: 200, pending: [silent], recent: [old] });
  const traffic: string[] = [];
  page.on("request", (req) => traffic.push(new URL(req.url()).pathname));
  await page.goto("/");

  const navBadge = page.locator(".primary-global-nav .nav-attention-badge");
  const inboxPanel = page.locator(".dashboard-triage-panel");
  const toasts = page.getByTestId("notification-toast");

  await expect(navBadge).toHaveText("1");
  await expect(inboxPanel).toBeVisible();
  await expect(toasts).toHaveCount(0);

  await inboxPanel.getByRole("tab", { name: /Recent/ }).click();
  await expect(inboxPanel.getByText("New reply")).toBeVisible();
  await inboxPanel.getByRole("tab", { name: /Needs your action/ }).click();

  // A genuine hint over the existing SSE must not nudge the roster.
  const before = traffic.filter((url) => url === "/api/agents/list").length;
  const result = notice("result", "work_item_completed", workId);
  expect((await control({ recent: [result, old], signal: "changed" })).streams).toBe(1);
  await expect(toasts).toHaveText(/Result ready/);
  expect(traffic.filter((url) => url === "/api/agents/list")).toHaveLength(before);

  const detail = page.waitForRequest((req) => new URL(req.url()).pathname === `/api/agents/${agentId}/work-items/${workId}`);
  await inboxPanel.locator(".dashboard-triage-main").first().click();
  await expect(page).toHaveURL(/\/agents\/bootstrap-agent\/conversation/);
  await detail;
  await expect(navBadge).toHaveText("1");
  await page.getByRole("button", { name: "Dismiss notification" }).click();
  await expect(toasts).toHaveCount(0);

  const duringConversation = notice("during-conversation", "operator_input_required", workId);
  await control({ recent: [duringConversation, result, old], signal: "changed" });
  await expect(toasts).toHaveCount(0);

  await page.locator(".primary-global-nav").getByRole("button", { name: "Dashboard" }).click();
  await expect(page).toHaveURL(/\/$/);
  await inboxPanel.getByRole("tab", { name: /Recent/ }).click();
  await expect(inboxPanel.locator(".dashboard-triage-list").getByText("Needs your input")).toBeVisible();

  await control({ pending: [], recent: [result, old], signal: "disconnect" });
  await expect(page.getByText("Notifications may be out of date.")).toBeVisible();
  await expect(navBadge).toHaveCount(0);
  await expect(inboxPanel.locator(".dashboard-triage-list").getByText("Needs your input")).toHaveCount(0);
  await expect(toasts).toHaveCount(0);
  expect(traffic.filter((url) => url === "/api/events/stream")).toHaveLength(2);

  await control({ status: 403, signal: "changed" });
  await expect(inboxPanel).toHaveCount(0);
});

test("hides the entry on a legacy runtime with no notification endpoints", async ({ page, context }, testInfo) => {
  await context.addCookies([{
    name: "holon_e2e_session", value: sessionFor(testInfo, "notifications"), domain: "127.0.0.1", path: "/",
  }]);
  const legacy = page.waitForResponse((response) => new URL(response.url()).pathname === "/api/notifications/pending");
  await page.goto("/");
  expect((await legacy).status()).toBe(404);
  await expect(page.locator(".dashboard-triage-panel")).toHaveCount(0);
});
