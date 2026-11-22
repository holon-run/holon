import { expect, test } from "./daemon-fixture";

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

  await page.addInitScript((token) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({ mode: "local", token }));
    localStorage.setItem("holon.webGui.languageMode.v1", "en");
  }, daemon.token);
  await page.goto(`${daemon.baseUrl}/agents/${daemon.agentId}`);
  await expect(page.locator(".composer textarea")).toBeVisible();
  const panel = page.locator(".side-panel");
  if (!await panel.isVisible()) await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  await panel.locator(".panel-sections").getByRole("button", { name: "Overview", exact: true }).click();

  const row = panel.locator(".agent-timer-row").filter({ hasText: "Original display title" });
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
