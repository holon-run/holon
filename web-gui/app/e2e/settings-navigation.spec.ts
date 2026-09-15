import { expect, test } from "@playwright/test";

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
