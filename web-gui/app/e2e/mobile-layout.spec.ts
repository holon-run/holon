import { expect, test } from "@playwright/test";
import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";

test.beforeEach(async ({ page, context }, info) => {
  await context.addCookies([{ name: "holon_e2e_session", value: sessionFor(info, "mobile-layout"), domain: "127.0.0.1", path: "/" }]);
});

test("portrait phones open the full agent roster as a drawer", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/agents/${agentId}/conversation`);

  await expect(page.locator(".mobile-nav-toggle")).toBeVisible();
  await expect(page.locator(".sidebar")).toHaveCSS("transform", /matrix\(1, 0, 0, 1, -/);
  await page.locator(".mobile-nav-toggle").click();
  await expect(page.locator(".sidebar")).toHaveCSS("transform", /matrix\(1, 0, 0, 1, 0, 0\)/);
  await expect(page.locator(".sidebar .agent-filter")).toBeVisible();
  await expect(page.locator(".sidebar .agent-row-main").first()).toBeVisible();
  await expect(page.locator(".mobile-nav-scrim")).toBeVisible();
  await expect(page.locator(".sidebar .agent-row").first()).toBeVisible();
  const agentList = await page.locator(".sidebar .agent-list").boundingBox();
  const connectionStatus = await page.locator(".sidebar .connection-status").boundingBox();
  expect(agentList).not.toBeNull();
  expect(connectionStatus).not.toBeNull();
  expect(agentList!.y + agentList!.height).toBeLessThanOrEqual(connectionStatus!.y + 1);
  await page.mouse.click(380, 420);
  await expect(page.locator(".mobile-nav-scrim")).toHaveCount(0);
  await expect(page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).resolves.toBe(true);
});

test("landscape phones keep the drawer usable and keep composer actions in view", async ({ page }) => {
  await page.setViewportSize({ width: 844, height: 390 });
  await page.goto(`/agents/${agentId}/conversation`);

  await expect(page.locator(".mobile-nav-toggle")).toBeVisible();
  await page.locator(".mobile-nav-toggle").click();
  await expect(page.locator(".sidebar .agent-row-main").first()).toBeVisible();
  await page.locator(".sidebar .agent-row").first().click();
  await expect(page.locator(".mobile-nav-scrim")).toHaveCount(0);

  await expect(page.locator(".composer")).toBeVisible();
  await expect(page.locator(".composer .send-button")).toBeVisible();
  await expect(page.locator(".composer .model-button")).toBeVisible();
  const sendBox = await page.locator(".composer .send-button").boundingBox();
  expect(sendBox).not.toBeNull();
  expect(sendBox!.x + sendBox!.width).toBeLessThanOrEqual(844);
  await expect(page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).resolves.toBe(true);
});

test("the model menu stays inside the viewport on a narrow phone", async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 700 });
  await page.goto(`/agents/${agentId}/conversation`);
  await page.locator(".model-button").click();
  const menu = page.locator(".model-menu");
  await expect(menu).toBeVisible();
  const box = await menu.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(320);
  expect(box!.y + box!.height).toBeLessThanOrEqual(700);
  await expect(page.locator(".composer .send-button")).toBeVisible();
});
