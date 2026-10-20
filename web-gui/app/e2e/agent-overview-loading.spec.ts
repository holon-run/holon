import { expect, test, type Page } from "@playwright/test";
import { sessionFor } from "./test-session";

const gates = new Map<string, () => void>();
test.afterEach(async ({}, info) => {
  gates.get(info.testId)?.();
  gates.delete(info.testId);
});
test.beforeEach(async ({ context }, info) => {
  await context.addCookies([{ name: "holon_e2e_session", value: sessionFor(info, "overview-loading"), domain: "127.0.0.1", path: "/" }]);
});

async function holdAgentAccess(page: Page, testId: string) {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  gates.set(testId, release);
  await page.route("**/api/control/agents/bootstrap-agent/detail", async (route) => {
    await gate;
    await route.continue();
  });
  return release;
}

async function rememberOpenPanel(page: Page) {
  await page.addInitScript(() => {
    const key = "holon.webGui.contextPanel.v1";
    if (!localStorage.getItem(key)) localStorage.setItem(key, JSON.stringify({ open: true, mode: "normal" }));
  });
}

test("refresh preserves overview width, header and close control until agent access resolves", async ({ page }, info) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await rememberOpenPanel(page);
  await page.goto("/agents/bootstrap-agent/conversation");
  const panel = page.locator(".side-panel");
  await expect(panel.locator(".agent-overview-stack")).toBeVisible();
  const before = await panel.boundingBox();
  const release = await holdAgentAccess(page, info.testId);
  await page.reload();
  await expect(panel.locator(".panel-header strong")).toHaveText("Agent overview");
  await expect(panel.getByRole("button", { name: "Close side panel", exact: true })).toBeVisible();
  await expect(panel.locator(".agent-overview-stack")).toBeVisible();
  await expect(panel.locator(".agent-overview-skeleton")).toHaveCount(0);
  await expect(page.locator(".conversation-sync-status")).toBeEmpty();
  await expect(page.locator(".composer textarea")).toBeEnabled();
  await page.locator(".composer textarea").fill("Draft while verifying access");
  await expect(page.locator(".composer .send-button")).toBeEnabled();
  const loading = await panel.boundingBox();
  expect(Math.abs(loading!.width - before!.width)).toBeLessThan(1);
  await page.screenshot({ path: info.outputPath("agent-overview-loading.png") });
  release();
  await expect(page.locator(".composer .send-button")).toBeEnabled();
  await expect(page.locator(".composer textarea")).toHaveValue("Draft while verifying access");
  await expect(panel.locator(".agent-overview-stack")).toBeVisible();
  await expect(panel.locator(".agent-overview-placeholder")).toHaveCount(0);
  expect(Math.abs((await panel.boundingBox())!.width - before!.width)).toBeLessThan(1);
});

test("delayed access retains the overview and explicit sending while retry restores online access", async ({ page }) => {
  await rememberOpenPanel(page);
  let fail = true;
  await page.route("**/api/control/agents/bootstrap-agent/detail", async (route) => {
    if (fail) await route.fulfill({ status: 503, json: { error: "Agent temporarily unavailable" } });
    else await route.continue();
  });
  await page.goto("/agents/bootstrap-agent/conversation");
  const panel = page.locator(".side-panel");
  await expect(panel).toBeVisible();
  await expect(panel.locator(".agent-overview-stack")).toBeVisible();
  await expect(page.locator(".conversation-sync-status")).toContainText("Agent status refresh is delayed");
  await expect(page.locator(".composer textarea")).toBeEnabled();
  await page.locator(".composer textarea").fill("Draft while access is delayed");
  await expect(page.locator(".composer .send-button")).toBeEnabled();
  await expect(panel.getByText("Could not load this agent", { exact: true })).toHaveCount(0);
  await expect(panel.locator(".agent-overview-skeleton")).toHaveCount(0);
  fail = false;
  await page.locator(".conversation-sync-status").getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.locator(".composer .send-button")).toBeEnabled();
  await expect(page.locator(".composer textarea")).toHaveValue("Draft while access is delayed");
  await expect(page.locator(".conversation-sync-status").filter({ hasText: "cached history is read-only" })).toHaveCount(0);
  await expect(panel.locator(".agent-overview-stack")).toBeVisible();
});

for (const width of [1440, 390]) {
  test(`closing the loading panel stays closed after data arrives and reload (${width}px)`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 900 });
    await rememberOpenPanel(page);
    // Keep this a cold path; a confirmed subject may legitimately show roster previews.
    await page.route("**/api/auth/session/me", (route) => route.fulfill({ json: {} }));
    const release = await holdAgentAccess(page, info.testId);
    await page.goto("/agents/bootstrap-agent/conversation");
    const panel = page.locator(".side-panel");
    await expect(panel.getByRole("status")).toHaveText("Loading agent information…");
    await panel.getByRole("button", { name: "Close side panel", exact: true }).click();
    await expect(panel).toBeHidden();
    release();
    await expect(panel.locator(".panel-sections")).toBeAttached();
    await expect(panel).toBeHidden();
    await page.reload();
    await expect(panel.locator(".panel-sections")).toBeAttached();
    await expect(panel).toBeHidden();
  });
}

test("a mobile refresh never opens a panel without an existing open preference", async ({ page }, info) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.route("**/api/auth/session/me", (route) => route.fulfill({ json: {} }));
  const release = await holdAgentAccess(page, info.testId);
  await page.goto("/agents/bootstrap-agent/conversation");
  const panel = page.locator(".side-panel");
  await expect(panel.locator(".agent-overview-placeholder")).toBeAttached();
  await expect(panel).toBeHidden();
  release();
  await expect(panel.locator(".panel-sections")).toBeAttached();
  await expect(panel).toBeHidden();
});
