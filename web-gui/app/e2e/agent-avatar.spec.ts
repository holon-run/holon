import { expect, test } from "@playwright/test";
import { sessionFor } from "./test-session";

test("ID avatars match across roster/navigation and survive reload without image requests", async ({ page, context, request }, info) => {
  const session = sessionFor(info, "avatar");
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  await request.post(`/__e2e__/configure?session=${encodeURIComponent(session)}`, {
    data: { visibleAgentIds: ["bootstrap-agent", "holon-product", "holon-integrations"] },
  });
  const avatarRequests: string[] = [];
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.includes("/avatar")) avatarRequests.push(request.url());
  });
  await page.goto("/");
  const cardAvatar = page.locator(".agent-card .agent-avatar").first();
  const sidebarAvatar = page.locator(".sidebar .agent-avatar").first();
  await expect(cardAvatar).toBeVisible();
  await expect(cardAvatar).toHaveJSProperty("naturalWidth", 100);
  const src = await cardAvatar.getAttribute("src");
  expect(src).toMatch(/^data:image\/svg\+xml/);
  await expect(sidebarAvatar).toHaveAttribute("src", src!);
  await expect(sidebarAvatar).toHaveCSS("filter", "none");
  await page.reload();
  await expect(cardAvatar).toHaveAttribute("src", src!);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(cardAvatar).toBeVisible();
  await page.locator(".mobile-nav-toggle").click();
  await expect(sidebarAvatar).toBeVisible();
  await expect(sidebarAvatar).toHaveAttribute("src", src!);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  expect(avatarRequests).toEqual([]);
});

test("live status changes only the eyes in both locations, and stale data clears the expression", async ({ page, context, request }, info) => {
  const session = sessionFor(info, "avatar-state");
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  await request.post(`/__e2e__/configure?session=${encodeURIComponent(session)}`, {
    data: { visibleAgentIds: ["holon-product"] },
  });
  let status = "awake_idle";
  let posture = "idle";
  let unavailable = false;
  await page.route("**/api/agents/snapshot", async (route) => {
    if (unavailable) return route.fulfill({ status: 503, json: { error: "offline" } });
    const response = await route.fetch();
    const body = await response.json();
    for (const entry of body.agents) {
      entry.agent.status = status;
      entry.agent.scheduling_posture = { posture, reason: "avatar test" };
    }
    await route.fulfill({ response, json: body });
  });
  await page.goto("/");
  const card = page.locator(".agent-card .agent-avatar").first();
  const sidebar = page.locator(".sidebar .agent-avatar").first();
  const refresh = page.locator(".dashboard-roster-section").getByRole("button", { name: /^(Refresh|刷新)$/ });
  await expect(card).toHaveAttribute("data-expression", "neutral");
  const original = (await card.getAttribute("src"))!;
  const identity = (src: string) => decodeURIComponent(src).replace(/<g data-avatar-part="expression">.*?<\/g>/, "");
  const states = [
    ["awake_running", "active_turn", "focused"],
    ["asleep", "waiting_for_operator", "questioning"],
    ["asleep", "waiting_for_external", "waiting"],
    ["asleep", "waiting_for_task", "waiting"],
    ["asleep", "idle", "sleeping"],
    ["future_status", "unknown", "neutral"],
    ["stopped", "stopped", "neutral"],
    ["awake_idle", "idle", "neutral"],
  ];
  for (const [nextStatus, nextPosture, expression] of states) {
    status = nextStatus;
    posture = nextPosture;
    await refresh.click();
    await expect(card).toHaveAttribute("data-expression", expression);
    const src = (await card.getAttribute("src"))!;
    await expect(sidebar).toHaveAttribute("src", src);
    await expect(card).toHaveJSProperty("naturalWidth", 100);
    expect(identity(src)).toBe(identity(original));
  }
  await expect(card).toHaveAttribute("src", original);
  status = "awake_running";
  posture = "active_turn";
  await refresh.click();
  await expect(card).toHaveAttribute("data-expression", "focused");
  unavailable = true;
  await refresh.click();
  await expect(page.locator(".roster-discovery-banner.stale")).toBeVisible();
  await expect(card).toHaveAttribute("data-expression", "neutral");
  await expect(sidebar).toHaveAttribute("src", original);
  unavailable = false;
  await refresh.click();
  await expect(card).toHaveAttribute("data-expression", "focused");
  await expect(sidebar).toHaveAttribute("src", (await card.getAttribute("src"))!);
});
