import type { Page } from "@playwright/test";
import { expect, test } from "./daemon-fixture";

async function openTimers(page: Page, daemon: { baseUrl: string; agentId: string; token: string }) {
  await page.addInitScript((token) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({ mode: "local", token }));
    localStorage.setItem("holon.webGui.languageMode.v1", "en");
  }, daemon.token);
  await page.goto(`${daemon.baseUrl}/agents/${daemon.agentId}`);
  await expect(page.locator(".composer textarea")).toBeVisible();
  const panel = page.locator(".side-panel");
  if (!await panel.isVisible()) await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  await panel.locator(".panel-sections").getByRole("button", { name: "Overview", exact: true }).click();
  return panel;
}

test("the production timer editor saves a partial update without changing the schedule", async ({
  daemonFactory,
  page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist" });
  const agentPath = `/agents/${daemon.agentId}`;
  const controlPath = `/control/agents/${daemon.agentId}/timers`;
  const created = await daemon.api(controlPath, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      duration_ms: 600_000,
      title: "Original display title",
      summary: "Legacy trigger instruction",
    }),
  });
  expect(created.ok, `timer creation returned HTTP ${created.status}`).toBe(true);
  const timer = await created.json();

  const panel = await openTimers(page, daemon);
  const row = panel.locator(".agent-timer-row").filter({ hasText: "Original display title" });
  await row.getByRole("button", { name: "More actions for Original display title" }).click();
  await row.getByRole("button", { name: "Edit timer content" }).click();
  await expect(row.getByRole("textbox", { name: "Trigger instruction" })).toHaveValue("Legacy trigger instruction");
  await row.getByRole("textbox", { name: "Display title" }).fill("");
  const saving = page.waitForResponse((response) =>
    response.url() === `${daemon.baseUrl}/api${controlPath}/${timer.id}` && response.request().method() === "PATCH");
  await row.getByRole("button", { name: "Save", exact: true }).click();
  const saved = await saving;
  expect(saved.ok()).toBe(true);
  expect(saved.request().postDataJSON()).toEqual({ title: null });
  await expect(panel.locator(".agent-timer-row strong").filter({ hasText: "Legacy trigger instruction" })).toBeVisible();
  const reloaded = await daemon.api(`${agentPath}/timers/${timer.id}`);
  expect(reloaded.ok).toBe(true);
  expect(await reloaded.json()).toEqual({ ...timer, title: null });
});

test("compact timers fit a narrow panel and only the confirmed cancel moves them into history", async ({
  daemonFactory,
  page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist" });
  const agentPath = `/agents/${daemon.agentId}`;
  const controlPath = `/control${agentPath}/timers`;
  const title = "Daily maintenance with a deliberately long display title for narrow panels";
  const instruction = "Run the report and keep this instruction in the expanded details.";
  const created = await daemon.api(controlPath, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ duration_ms: 86_400_000, interval_ms: 86_400_000, title, instruction }),
  });
  expect(created.ok).toBe(true);
  const { id } = await created.json();
  const readStatus = async () => {
    const response = await daemon.api(`${agentPath}/timers/${id}`);
    expect(response.ok).toBe(true);
    return (await response.json()).status;
  };

  await page.setViewportSize({ width: 1600, height: 1000 });
  const panel = await openTimers(page, daemon);
  const divider = panel.getByRole("separator");
  await divider.focus();
  for (let i = 0; i < 12; i += 1) await divider.press("ArrowRight");
  const row = panel.locator(".agent-timer-row").filter({ has: page.getByRole("button", { name: title, exact: true }) });
  const disclosure = row.getByRole("button", { name: title, exact: true });
  const toggle = row.getByRole("switch", { name: `${title} enabled` });
  const more = row.getByRole("button", { name: `More actions for ${title}` });
  await expect(toggle).toBeChecked();
  await expect(row.getByText("Every 24 hours", { exact: true })).toBeVisible();
  await expect(row.getByText("Next run", { exact: true })).toBeVisible();
  await expect(row.getByText(instruction)).toBeHidden();
  expect(await panel.evaluate((node) => node.getBoundingClientRect().width)).toBeLessThan(380);
  expect(await row.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true);
  await disclosure.click();
  await expect(row.getByText(instruction)).toBeVisible();
  await expect(row.getByText("Last run", { exact: true })).toBeVisible();
  await expect(row.getByText("Runs", { exact: true })).toBeVisible();
  await disclosure.click();

  await toggle.press("Space");
  await expect(toggle).not.toBeChecked();
  await expect(row.getByText("Paused", { exact: true })).toBeVisible();
  await expect(row.getByText("Next run", { exact: true })).toBeHidden();
  expect(await readStatus()).toBe("paused");
  await toggle.click();
  await expect(toggle).toBeChecked();
  await expect(row.getByText("Next run", { exact: true })).toBeVisible();
  expect(await readStatus()).toBe("active");

  await more.click();
  const cancel = row.getByRole("button", { name: "Cancel timer", exact: true });
  const cancelPrompt = page.waitForEvent("dialog");
  const declineClick = cancel.click();
  const prompt = await cancelPrompt;
  expect(prompt.message()).toContain("cannot be resumed");
  expect(prompt.message()).toContain("already queued or claimed");
  await prompt.dismiss();
  await declineClick;
  expect(await readStatus()).toBe("active");
  await expect(toggle).toBeChecked();

  page.once("dialog", (dialog) => dialog.accept());
  await cancel.click();
  const history = panel.locator(".agent-timer-history");
  await expect(history.getByText("History", { exact: true })).toBeVisible();
  await expect(row).toBeHidden();
  await history.getByText("History", { exact: true }).click();
  await expect(row.getByText("Cancelled", { exact: true })).toBeVisible();
  await expect(toggle).toHaveCount(0);
  expect(await readStatus()).toBe("cancelled");
});
