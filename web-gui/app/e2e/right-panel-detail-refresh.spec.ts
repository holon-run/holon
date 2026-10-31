import { expect, test, type APIRequestContext, type BrowserContext, type Locator, type Page, type Request, type TestInfo } from "@playwright/test";
import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const agentPath = `/api/agents/${agentId}`;
const workId = "refresh-detail-work";
const taskId = "refresh-detail-task";
const toolId = "refresh-detail-tool";
const workPath = `${agentPath}/work-items/${workId}`;
const taskPath = `${agentPath}/tasks/${taskId}`;
const toolPath = `${agentPath}/tool-executions/${toolId}`;
const createdAt = "2026-10-31T00:00:00.000Z";
const staleText = "Detail is stale · showing last known data";
const forbiddenEvents = /^\/api\/(?:agents\/[^/]+\/events(?:\/stream)?|events\/stream)\/?$/;
const detailPaths = new Set([workPath, taskPath, `${taskPath}/output`, toolPath]);
const observed = new WeakMap<Page, { rawEvents: string[]; identityErrors: string[]; navigations: number }>();

const work = (revision = 1) => ({
  id: workId, agent_id: agentId, objective: `Detail objective revision ${revision}`,
  state: "open", plan_status: "ready", revision, created_at: createdAt,
  updated_at: new Date(Date.parse(createdAt) + revision * 1000).toISOString(),
  scheduling_state: "runnable", is_current: true, readiness: "ready", focus: "current",
});
const workDetail = (revision = 1) => ({
  ...work(revision), result_summary: `Detail result revision ${revision}`,
  plan_artifact: { path: "/test/plan.md", relative_path: "plan.md", workspace_id: "test",
    preview: `## Detail plan revision ${revision}`, preview_complete: true },
  todo_list: [{ text: `Detail todo revision ${revision}`, state: revision === 1 ? "pending" : "completed" }],
});
const task = (status = "running") => ({
  id: taskId, agent_id: agentId, work_item_id: workId, kind: "command_task", status,
  summary: "Run detail refresh checks", created_at: createdAt, updated_at: createdAt,
});
const taskDetail = (status = "running") => ({
  ...task(status), task_id: taskId, wait_policy: "none", command: { cmd: "npm test", workdir: "/test" },
});
const taskOutput = (status = "running", output = "Initial task output") => ({
  retrieval_status: "ready", task: { task_id: taskId, kind: "command_task", status,
    summary: task().summary, output_preview: output, output_truncated: false },
});
const tool = (status = "running", output = "Initial tool output") => ({
  id: toolId, agent_id: agentId, tool_name: "ExecCommand", status,
  input: { cmd: "printf detail-refresh" }, output: { stdout: output },
});

test.setTimeout(60_000);
test.beforeEach(async ({ page }) => {
  const trace = { rawEvents: [] as string[], identityErrors: [] as string[], navigations: 0 };
  observed.set(page, trace);
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (forbiddenEvents.test(path)) trace.rawEvents.push(path);
  });
  page.on("response", (response) => {
    const path = new URL(response.url()).pathname;
    if (!detailPaths.has(path) || !response.ok()) return;
    const headers = response.headers();
    if (headers["x-holon-agent-id"] !== agentId || headers["x-holon-agent-incarnation"] !== "1") {
      trace.identityErrors.push(path);
    }
  });
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) trace.navigations += 1;
  });
});
test.afterEach(async ({ page }) => {
  expect(observed.get(page)?.rawEvents).toEqual([]);
  expect(observed.get(page)?.identityErrors).toEqual([]);
  expect(observed.get(page)?.navigations).toBe(1);
});

async function scenario(page: Page, context: BrowserContext, request: APIRequestContext, info: TestInfo, initial: Record<string, unknown> = {}) {
  const session = sessionFor(info, "right-panel-detail-refresh");
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  const update = async (body: Record<string, unknown>) => {
    expect((await request.post(control("/__e2e__/right-panel"), { data: { agentId, ...body } })).ok()).toBe(true);
  };
  const pending = new Set<Request>();
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (detailPaths.has(path) || [agentPath + "/state", agentPath + "/work-items",
      `/api/control/agents/${agentId}/detail`].includes(path)) pending.add(request);
  });
  page.on("requestfinished", (request) => pending.delete(request));
  page.on("requestfailed", (request) => pending.delete(request));
  const settle = () => expect.poll(() => pending.size).toBe(0);
  const advance = async (milliseconds: number) => {
    // Match the list-refresh fixture: settle real HTTP between virtual ticks.
    for (let remaining = milliseconds; remaining > 0; remaining -= 1000) {
      await page.clock.runFor(Math.min(remaining, 1000));
      await settle();
    }
  };
  const conversation = () => page.evaluate((id) => window.__HOLON_E2E__?.conversation(id), agentId);
  const checkpoint = async (body: Record<string, unknown>) => {
    const previous = (await conversation())?.throughSeq ?? 0;
    await update({ ...body, notify: "checkpoint" });
    await expect.poll(async () => (await conversation())?.throughSeq ?? 0).toBeGreaterThan(previous);
  };
  const counts = async () => {
    const { requests } = await (await request.get(control("/__e2e__/requests"))).json() as { requests: string[] };
    const reads = requests.filter((entry) => entry.startsWith("GET ")).map((entry) => entry.split(" ")[1].split("?")[0]);
    const count = (path: string) => reads.filter((entry) => entry === path).length;
    return {
      state: count(`${agentPath}/state`), workItems: count(`${agentPath}/work-items`),
      workDetail: count(workPath), taskStatus: count(taskPath), taskOutput: count(`${taskPath}/output`),
      toolDetail: count(toolPath),
      readMarkerPosts: requests.filter((entry) => /^POST [^ ?]*\/read-marker(?:\?|$)/.test(entry)).length,
      controlWrites: requests.filter((entry) => /^(?:POST|PUT|PATCH|DELETE) \/api\/control\//.test(entry)).length,
    };
  };
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  await update({ workItems: [work()], currentWorkItemId: workId, stateWorkItems: [], tasks: [],
    workItemDetailsById: { [workId]: workDetail() }, ...initial });
  expect((await request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [{
      turn_id: "detail-baseline", key: { turn_id: "detail-baseline", turn_index: 1 }, revision: 1,
      presentation_class: "operator", inputs: [{ message_id: "detail-input", preview: "Keep detail refresh separate from conversation sync" }],
      execution: { kind: "terminal", outcome: "completed" }, settled: true,
      result: { kind: "none", reason: { kind: "reducer_only", reason: "done" } },
      attention: null, detail_coverage: { kind: "complete" }, brief_ids: [],
    }],
    activitiesByTurnId: { "detail-baseline": initial.toolExecutionsById ? [{
      kind: "tool", id: `tool:${toolId}`, key: { event_seq: 1, activity_id: `tool:${toolId}` },
      revision: 1, summary: "ExecCommand · running",
    }] : [] },
  } })).ok()).toBe(true);
  await page.clock.install({ time: new Date("2026-10-31T03:00:00.000Z") });
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(`/agents/${agentId}/conversation`);
  await expect(page.locator('[data-turn-id="detail-baseline"]')).toBeVisible();
  const panel = page.locator(".side-panel");
  if (!(await panel.isVisible())) await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  await expect.poll(async () => (await (await request.get(control("/__e2e__/right-panel"))).json()).streamAgentIds).toContain(agentId);
  await expect.poll(async () => (await counts()).workItems).toBeGreaterThan(0);
  await page.clock.pauseAt(await page.evaluate(() => Date.now()) + 1000);
  await advance(5000);
  const baseline = await counts();
  const healthy = async () => {
    await expect(page.locator(".conversation-sync-status .is-error, .conversation-sync-status .is-reconnecting")).toHaveCount(0);
    expect((await conversation())?.turnIds).toEqual(["detail-baseline"]);
    await expect(page.locator(".composer textarea")).toBeVisible();
    const latest = await counts();
    expect(latest.readMarkerPosts - baseline.readMarkerPosts).toBe(0);
    expect(latest.controlWrites - baseline.controlWrites).toBe(0);
  };
  const openWork = async () => {
    await panel.locator(".work-item-button").filter({ has: page.locator("strong", { hasText: work().objective }) }).click();
    await expect(panel.locator(".work-item-detail")).toBeVisible();
    await advance(1);
    await settle();
  };
  const refresh = async (detail: Locator, path: string) => {
    const [read, response] = await Promise.all([
      page.waitForRequest((request) => new URL(request.url()).pathname === path),
      page.waitForResponse((response) => new URL(response.url()).pathname === path),
      (async () => {
        await detail.getByRole("button", { name: "Refresh detail", exact: true }).click();
        await advance(1);
      })(),
    ]);
    expect(read.headers()["x-holon-expected-incarnation"]).toBe("1");
    expect(response.ok()).toBe(true);
    await settle();
  };
  return { update, checkpoint, counts, conversation, panel, advance, settle, healthy, openWork, refresh };
}

async function expectWork(detail: Locator, revision: number) {
  await expect(detail.locator(".detail-title")).toHaveText(work(revision).objective);
  await expect(detail.locator(".detail-plan-preview")).toContainText(`Detail plan revision ${revision}`);
  await expect(detail).toContainText(`Detail todo revision ${revision}`);
  await expect(detail).toContainText(`Detail result revision ${revision}`);
}

test("checkpoint bursts refresh the mounted work detail without exceeding its shared request budget", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info);
  await ui.openWork();
  const detail = ui.panel.locator(".work-item-detail");
  await expectWork(detail, 1);
  const before = await ui.counts();
  for (let revision = 2; revision <= 7; revision += 1) {
    await ui.checkpoint({ workItems: [work(revision)], workItemDetailsById: { [workId]: workDetail(revision) } });
  }
  await ui.advance(4900);
  const burst = await ui.counts();
  expect(burst.workDetail - before.workDetail).toBeLessThanOrEqual(1);
  expect(burst.state - before.state).toBeLessThanOrEqual(1);
  expect(burst.workItems - before.workItems).toBeLessThanOrEqual(1);
  await ui.advance(600);
  await expectWork(detail, 7);
  await expect(detail).not.toContainText("Detail plan revision 1");
  await expect(detail).not.toContainText("Detail todo revision 1");
  await expect(detail).not.toContainText("Detail result revision 1");
  await ui.healthy();
});

test("silent work detail changes converge in 30 seconds and returning from overview uses the latest store", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info);
  await ui.openWork();
  const detail = ui.panel.locator(".work-item-detail");
  await expectWork(detail, 1);
  const head = (await ui.conversation())?.throughSeq;
  const before = await ui.counts();
  await ui.update({ workItems: [work(2)], workItemDetailsById: { [workId]: workDetail(2) } });
  await ui.advance(30_000);
  await expectWork(detail, 2);
  const fallback = await ui.counts();
  expect(fallback.workDetail - before.workDetail).toBe(1);
  expect(fallback.state - before.state).toBe(1);
  expect(fallback.workItems - before.workItems).toBe(1);
  expect((await ui.conversation())?.throughSeq).toBe(head);

  await ui.panel.locator(".panel-sections").getByRole("button", { name: "Overview", exact: true }).click();
  const away = await ui.counts();
  await ui.update({ workItems: [work(3)], workItemDetailsById: { [workId]: workDetail(3) } });
  await ui.advance(30_000);
  expect((await ui.counts()).workDetail).toBe(away.workDetail);
  await ui.panel.locator(".panel-sections").getByRole("button", { name: "Details", exact: true }).click();
  await ui.advance(5500);
  await expectWork(detail, 3);
  await expect(detail.locator(".detail-title")).not.toHaveText(work().objective);
  await ui.healthy();
});

test("first work detail failure stays local and Refresh detail retries the identity-fenced read", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info, { failResources: ["work-item-detail"] });
  await ui.openWork();
  const detail = ui.panel.locator(".work-item-detail");
  await expect(detail.getByRole("status")).toContainText(staleText);
  await expect(detail.locator(".detail-title")).toHaveText(work().objective);
  await ui.healthy();
  await ui.update({ failResources: [], workItemDetailsById: { [workId]: workDetail(2) } });
  await ui.refresh(detail, workPath);
  await expectWork(detail, 2);
  await expect(detail.getByText(staleText, { exact: true })).toHaveCount(0);
  await ui.healthy();
});

test("background work detail failure preserves the complete last-known-good detail until manual recovery", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info);
  await ui.openWork();
  const detail = ui.panel.locator(".work-item-detail");
  await expectWork(detail, 1);
  await ui.checkpoint({ failResources: ["work-item-detail"], workItemDetailsById: { [workId]: workDetail(2) } });
  await ui.advance(5500);
  await expect(detail.getByRole("status")).toContainText(staleText);
  await expectWork(detail, 1);
  await expect(detail.locator(".detail-refresh-feedback small")).not.toBeEmpty();
  await ui.healthy();
  await ui.update({ failResources: [] });
  await ui.refresh(detail, workPath);
  await expectWork(detail, 2);
  await expect(detail.getByText(staleText, { exact: true })).toHaveCount(0);
  await ui.healthy();
});

test("active task detail keeps old output on failure, reads final terminal output, then stops its five-second reads", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info, {
    tasks: [task()], taskDetailsById: { [taskId]: taskDetail() }, taskOutputsById: { [taskId]: taskOutput() },
  });
  await ui.panel.locator(".task-button").filter({ hasText: task().summary }).click();
  await ui.advance(1);
  const detail = ui.panel.locator(".task-detail");
  await expect(detail).toContainText("Initial task output");
  await ui.settle();
  const active = await ui.counts();
  await ui.update({ taskOutputsById: { [taskId]: taskOutput("running", "Updated running output") } });
  await ui.advance(5000);
  await expect(detail).toContainText("Updated running output");
  const running = await ui.counts();
  expect(running.taskStatus - active.taskStatus).toBe(1);
  expect(running.taskOutput - active.taskOutput).toBe(1);

  await ui.update({ failResources: ["task-output"], tasks: [task("completed")],
    taskDetailsById: { [taskId]: taskDetail("completed") },
    taskOutputsById: { [taskId]: taskOutput("completed", "Final task output after completion") } });
  await ui.advance(5000);
  await expect(detail.getByRole("status")).toContainText(staleText);
  await expect(detail).toContainText("Updated running output");
  await ui.healthy();

  // A terminal status must not stop retries before its final output is read.
  const failed = await ui.counts();
  await ui.update({ failResources: [] });
  await ui.advance(6500); // The first local retry includes up to 20% jitter.
  await expect(detail.locator(".task-detail-header .state-chip")).toHaveText("Completed");
  await expect(detail).toContainText("Final task output after completion");
  await expect(detail.getByText(staleText, { exact: true })).toHaveCount(0);
  const terminal = await ui.counts();
  expect(terminal.taskStatus - failed.taskStatus).toBe(1);
  expect(terminal.taskOutput - failed.taskOutput).toBe(1);
  await ui.advance(30_000);
  const idle = await ui.counts();
  expect(idle.taskStatus).toBe(terminal.taskStatus);
  expect(idle.taskOutput).toBe(terminal.taskOutput);
  await ui.update({ taskOutputsById: { [taskId]: taskOutput("completed", "Manually refreshed final task output") } });
  await ui.refresh(detail, taskPath);
  await expect(detail).toContainText("Manually refreshed final task output");
  const manual = await ui.counts();
  expect(manual.taskStatus - idle.taskStatus).toBe(1);
  expect(manual.taskOutput - idle.taskOutput).toBe(1);
  await ui.healthy();
});

test("a failed expanded tool row does not immediately retry the same conversation revision", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info, {
    toolExecutionsById: { [toolId]: tool() }, failResources: ["tool-execution"],
  });
  const toggle = page.locator('[data-turn-id="detail-baseline"] .conversation-detail-toggle');
  const before = await ui.counts();
  await toggle.click();
  await ui.advance(1000);
  expect((await ui.counts()).toolDetail - before.toolDetail).toBe(1);
  await toggle.click();
  await toggle.click();
  await ui.advance(1000);
  expect((await ui.counts()).toolDetail - before.toolDetail).toBe(1);
  await ui.healthy();
});

test("tool detail refreshes while active, retains old content on failure and supports manual recovery", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info, { toolExecutionsById: { [toolId]: tool() } });
  const turn = page.locator('[data-turn-id="detail-baseline"]');
  await turn.locator(".conversation-detail-toggle").click();
  await page.locator(`[data-activity-id="tool:${toolId}"] button`).click();
  await ui.advance(1);
  const detail = ui.panel.locator(".tool-execution-detail");
  await expect(detail.getByText("Initial tool output", { exact: true })).toBeVisible();
  await ui.settle();
  const before = await ui.counts();
  await ui.update({ toolExecutionsById: { [toolId]: tool("running", "Updated active tool output") } });
  await ui.advance(5000);
  await expect(detail.getByText("Updated active tool output", { exact: true })).toBeVisible();
  expect((await ui.counts()).toolDetail - before.toolDetail).toBe(1);

  await ui.update({ failResources: ["tool-execution"] });
  await ui.advance(5000);
  await expect(detail.getByRole("status")).toContainText(staleText);
  await expect(detail.getByText("Updated active tool output", { exact: true })).toBeVisible();
  await ui.healthy();

  await ui.update({ failResources: [], toolExecutionsById: { [toolId]: tool("success", "Final refreshed tool output") } });
  await ui.refresh(detail, toolPath);
  await expect(detail.getByText("Final refreshed tool output", { exact: true })).toBeVisible();
  await expect(detail.locator(".detail-meta .state-chip")).toHaveText("Completed");
  await expect(detail.getByText(staleText, { exact: true })).toHaveCount(0);
  const terminal = await ui.counts();
  await ui.advance(30_000);
  expect((await ui.counts()).toolDetail).toBe(terminal.toolDetail);
  await ui.healthy();
});
