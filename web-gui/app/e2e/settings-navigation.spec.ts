import { expect, test } from "@playwright/test";

test("settings starts with General and remembers mouse and keyboard selections", async ({ page }) => {
  await page.goto("/settings");
  const general = page.locator("#settings-tab-general");
  const models = page.locator("#settings-tab-models");
  await expect(general).toHaveAttribute("aria-selected", "true");
  await models.click();
  await page.reload();
  await expect(models).toHaveAttribute("aria-selected", "true");
  await models.focus();
  await page.keyboard.press("End");
  await page.reload();
  await expect(page.locator("#settings-tab-advanced")).toHaveAttribute("aria-selected", "true");
});

test("settings ignores stale saved tabs", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("holon.webGui.settingsTab.v1", "obsolete"));
  await page.goto("/settings");
  await expect(page.locator("#settings-tab-general")).toHaveAttribute("aria-selected", "true");
});

test("explicit tab overrides storage and follows later selections on reload", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("holon.webGui.settingsTab.v1", "advanced"));
  await page.goto("/settings?tab=models");
  await expect(page.locator("#settings-tab-models")).toHaveAttribute("aria-selected", "true");
  await page.locator("#settings-tab-general").click();
  await expect(page).toHaveURL(/\/settings\?tab=general$/);
  await page.reload();
  await expect(page.locator("#settings-tab-general")).toHaveAttribute("aria-selected", "true");
});

test("invalid explicit tab falls back to the remembered tab", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("holon.webGui.settingsTab.v1", "vision"));
  await page.goto("/settings?tab=invalid");
  await expect(page.locator("#settings-tab-vision")).toHaveAttribute("aria-selected", "true");
});

test("settings sections support keyboard navigation with a single tab stop", async ({ page }) => {
  await page.goto("/settings");
  const tabs = page.getByRole("tab");
  const panel = page.getByRole("tabpanel");
  await tabs.first().click();
  await expect(panel).toHaveAttribute("aria-labelledby", "settings-tab-general");
  await page.keyboard.press("ArrowRight");
  await expect(tabs.nth(1)).toBeFocused();
  await expect(tabs.nth(1)).toHaveAttribute("aria-selected", "true");
  await expect(tabs.first()).toHaveAttribute("tabindex", "-1");
  await page.keyboard.press("End");
  await expect(tabs.last()).toBeFocused();
  await expect(panel).toHaveAttribute("aria-labelledby", "settings-tab-advanced");
  await page.keyboard.press("ArrowRight");
  await expect(tabs.first()).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await expect(tabs.last()).toBeFocused();
  await page.keyboard.press("Home");
  await expect(tabs.first()).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(panel).toBeFocused();
});
