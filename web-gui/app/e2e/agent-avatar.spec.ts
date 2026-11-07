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
