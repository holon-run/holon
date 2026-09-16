import { expect, test } from "@playwright/test";

const created = new Date(Date.now() - 90_000).toISOString();
const work = { id: "work-current", objective: "Restore session after login", state: "open", plan_status: "ready", revision: 1, created_at: created, updated_at: created, scheduling_state: "waiting_task", is_current: true, readiness: "blocked", focus: "current" };
const task = { id: "task-tests", agent_id: "bootstrap-agent", work_item_id: work.id, kind: "command_task", status: "running", summary: "Run login regression tests", created_at: created, updated_at: created };
const wait = { id: "wait-tests", work_item_id: work.id, kind: "task", status: "active", task_ids: [task.id], created_at: created };

test("all waiting statuses stay visible without a waiting timer", async ({ page }) => {
  let kind = "operator";
  await page.route("**/api/agents/bootstrap-agent/state", async (route) => {
    const body = await (await route.fetch()).json();
    body.agent.agent.current_work_item_id = work.id;
    body.work_items = [work]; body.tasks = [task]; body.waits = [{ ...wait, kind }];
    await route.fulfill({ json: body });
  });
  await page.goto("/agents/bootstrap-agent/conversation");
  const bar = page.locator(".current-work-bar");
  for (const [next, expected] of [["operator", "needsInput"], ["task", "waitingTask"], ["timer", "waitingTimer"], ["external", "waitingExternal"], ["system", "waitingExternal"]]) {
    kind = next;
    await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
    await expect(bar).toHaveAttribute("data-state", expected);
    await expect(bar.getByRole("status")).not.toBeEmpty();
    await expect(bar.locator(".current-work-status time")).toHaveCount(0);
  }
  await bar.getByRole("button", { name: "Expand", exact: true }).click();
  await expect(bar.locator(".current-work-task time")).toContainText("Elapsed");
});

test("current work stays visible with the panel closed and opens work/task details", async ({ page }) => {
  await page.route("**/api/agents/bootstrap-agent/state", async (route) => {
    const body = await (await route.fetch()).json();
    body.agent.agent.current_work_item_id = work.id;
    body.work_items = [work]; body.tasks = [task]; body.waits = [wait];
    await route.fulfill({ json: body });
  });
  await page.route(`**/api/agents/bootstrap-agent/work-items/${work.id}`, (route) => route.fulfill({ json: { ...work, agent_id: "bootstrap-agent", workspace_id: "test", plan_artifact: { path: "/test/plan.md", relative_path: "plan.md", workspace_id: "test", preview: "# Login repair plan", preview_complete: true, updated_at: created } } }));
  await page.route(`**/api/agents/bootstrap-agent/tasks/${task.id}`, (route) => route.fulfill({ json: { ...task, wait_policy: "none", command: { cmd: "npm test", workdir: "/test" } } }));
  await page.route(`**/api/agents/bootstrap-agent/tasks/${task.id}/output**`, (route) => route.fulfill({ json: { task_id: task.id, output: "Tests running", status: "running" } }));
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto("/agents/bootstrap-agent/conversation");
  const bar = page.getByRole("region", { name: "Current work", exact: true });
  await expect(bar).toBeVisible();
  await expect(bar).toHaveAttribute("data-state", "waitingTask");
  await expect(bar.getByRole("status")).toContainText("Waiting for Run login regression tests");
  await expect(bar.locator(".current-work-status time")).toHaveCount(0);
  await bar.getByRole("button", { name: work.objective, exact: true }).click();
  await expect(page.locator(".side-panel")).toBeVisible();
  await expect(page.locator(".detail-plan-preview")).toContainText("Login repair plan");
  await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  await bar.getByRole("button", { name: "Expand", exact: true }).click();
  await bar.locator(".current-work-task").click();
  await expect(page.locator(".side-panel")).toContainText("npm test");
  await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator(".composer textarea")).toBeVisible();
  const box = (await bar.boundingBox())!;
  expect(box.x + box.width).toBeLessThanOrEqual(390);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});

test("live state moves from waiting to result ready to running and completed without changing turn time", async ({ page }) => {
  let phase = "waiting";
  await page.route("**/api/agents/bootstrap-agent/state", async (route) => {
    const body = await (await route.fetch()).json();
    body.agent.agent.current_work_item_id = phase === "completed" ? null : work.id;
    body.session.current_run_id = phase === "running" ? "new-run" : null;
    body.work_items = [{ ...work, state: phase === "completed" ? "completed" : "open" }];
    body.tasks = phase === "waiting" ? [task] : [];
    body.waits = phase === "waiting" ? [wait] : phase === "ready" ? [{ ...wait, status: "triggered" }] : [];
    await route.fulfill({ json: body });
  });
  await page.goto("/agents/bootstrap-agent/conversation");
  const bar = page.locator(".current-work-bar");
  await expect(bar).toHaveAttribute("data-state", "waitingTask");
  for (const [next, expected] of [["ready", "resultReady"], ["running", "running"], ["completed", "completed"]]) {
    phase = next;
    // The same refresh path runs when returning to a visible tab.
    await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
    await expect(bar).toHaveAttribute("data-state", expected);
  }
  await expect(bar).toHaveCount(0, { timeout: 12000 });
  await expect(page.locator(".composer textarea")).toBeVisible();
});

test("background-only work shows ownership and refresh failures preserve visibly stale status", async ({ page }) => {
  let failed = false;
  await page.route("**/api/agents/bootstrap-agent/state", async (route) => {
    if (failed) return route.fulfill({ status: 503, json: {} });
    const body = await (await route.fetch()).json();
    body.tasks = [{ ...task, work_item_id: "other-work" }];
    await route.fulfill({ json: body });
  });
  await page.goto("/agents/bootstrap-agent/conversation");
  const bar = page.locator(".current-work-bar");
  await expect(bar).toHaveAttribute("data-state", "background");
  await expect(bar).toContainText("Background activity");
  await bar.getByRole("button", { name: "Expand", exact: true }).click();
  await expect(bar).toContainText("Other work: other-work");
  failed = true;
  await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
  await expect(bar).toHaveAttribute("data-state", "stale");
  await expect(bar).toContainText("status may be outdated");
  failed = false;
  await bar.getByRole("button", { name: "Refresh status" }).click();
  await expect(bar).toHaveAttribute("data-state", "background");
});
