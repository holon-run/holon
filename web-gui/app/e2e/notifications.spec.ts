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
  page.on("request", (request) => traffic.push(new URL(request.url()).pathname));
  await page.goto("/");
  const trigger = page.getByLabel(/^Notifications \(/);
  const toasts = page.getByTestId("notification-toast");
  await expect(trigger).toHaveAccessibleName("Notifications (1 needs your input)");
  await expect(toasts).toHaveCount(0);
  await trigger.press("Enter");
  await expect(page.getByRole("region", { name: "Notifications" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Recent" }).getByText("New reply")).toBeVisible();
  await page.getByRole("region", { name: "Needs your input" }).getByRole("button").press("Escape");
  await expect(trigger).toBeFocused();

  // A genuine hint over the existing SSE must not nudge the roster.
  const before = traffic.filter((url) => url === "/api/agents/list").length;
  const result = notice("result", "work_item_completed", workId);
  expect((await control({ recent: [result, old], signal: "changed" })).streams).toBe(1);
  await expect(toasts).toHaveText(/Result ready/);
  expect(traffic.filter((url) => url === "/api/agents/list")).toHaveLength(before);

  await trigger.click();
  const detail = page.waitForRequest((request) => new URL(request.url()).pathname === `/api/agents/${agentId}/work-items/${workId}`);
  await page.getByRole("region", { name: "Needs your input" }).getByRole("button").click();
  await expect(page).toHaveURL(/\/agents\/bootstrap-agent\/conversation/);
  await detail;
  await expect(trigger).toHaveAccessibleName("Notifications (1 needs your input)");
  await page.getByRole("button", { name: "Dismiss notification" }).click();
  await expect(toasts).toHaveCount(0);

  const duringConversation = notice("during-conversation", "operator_input_required", workId);
  await control({ recent: [duringConversation, result, old], signal: "changed" });
  await trigger.click();
  await expect(page.getByRole("region", { name: "Recent" }).getByText("Needs your input")).toBeVisible();
  await expect(toasts).toHaveCount(0);

  await control({ pending: [], recent: [result, old], signal: "disconnect" });
  await expect(page.getByText("Notifications may be out of date.")).toBeVisible();
  await expect(trigger).toHaveAccessibleName("Notifications (0 need your input)");
  await expect(page.getByRole("region", { name: "Recent" }).getByText("Needs your input")).toHaveCount(0);
  await expect(toasts).toHaveCount(0);
  expect(traffic.filter((url) => url === "/api/events/stream")).toHaveLength(2);

  await control({ status: 403, signal: "changed" });
  await expect(trigger).toHaveCount(0);
  await expect(page).toHaveURL(/\/agents\/bootstrap-agent\/conversation/);
});

test("hides the entry on a legacy runtime with no notification endpoints", async ({ page, context }, testInfo) => {
  await context.addCookies([{
    name: "holon_e2e_session", value: sessionFor(testInfo, "notifications"), domain: "127.0.0.1", path: "/",
  }]);
  const legacy = page.waitForResponse((response) => new URL(response.url()).pathname === "/api/notifications/pending");
  await page.goto("/");
  expect((await legacy).status()).toBe(404);
  await expect(page.getByLabel(/^Notifications \(/)).toHaveCount(0);
});
