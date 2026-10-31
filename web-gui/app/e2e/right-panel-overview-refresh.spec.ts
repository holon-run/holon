import { expect, test, type APIRequestContext, type BrowserContext, type Page, type Request, type TestInfo } from "@playwright/test";
import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const agentPath = `/api/agents/${agentId}`;
const createdAt = "2026-10-31T00:00:00.000Z";
const staleText = "Detail is stale · showing last known data";
const forbiddenEvents = /^\/api\/(?:agents\/[^/]+\/events(?:\/stream)?|events\/stream)\/?$/;
const work = {
  id: "overview-refresh-work", agent_id: agentId, objective: "Keep overview reads scoped",
  state: "open", plan_status: "ready", revision: 1, created_at: createdAt, updated_at: createdAt,
  scheduling_state: "runnable", is_current: true, readiness: "ready", focus: "current",
};
const timer = (overrides = {}) => ({
  id: "external-timer", agent_id: agentId, summary: "Timer created outside this browser",
  status: "active", created_at: createdAt, duration_ms: 60_000, interval_ms: 60_000,
  repeat: true, fire_count: 0, next_fire_at: "2026-10-31T04:00:00.000Z", last_fired_at: null,
  ...overrides,
});
const recentTask = (summary = "Externally completed task") => ({
  id: "external-terminal-task", agent_id: agentId, kind: "command_task", status: "completed",
  summary, created_at: createdAt, updated_at: createdAt,
});
const skill = (name: string) => ({
  skill_id: name, name, description: `External ${name}`, scope: "agent", path: `/test/skills/${name}`,
});

test.setTimeout(60_000);
const observed = new WeakMap<Page, { rawEvents: string[]; identityErrors: string[]; navigations: number }>();
test.beforeEach(async ({ page }) => {
  const trace = { rawEvents: [] as string[], identityErrors: [] as string[], navigations: 0 };
  observed.set(page, trace);
  const fenced = (url: URL) => url.pathname === `${agentPath}/timers`
    || (url.pathname === `${agentPath}/tasks` && url.searchParams.get("status") === "terminal");
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (forbiddenEvents.test(url.pathname)) trace.rawEvents.push(url.pathname);
    if (fenced(url) && request.headers()["x-holon-expected-incarnation"] !== "1") {
      trace.identityErrors.push(`request ${url.pathname}`);
    }
  });
  page.on("response", (response) => {
    const url = new URL(response.url());
    if (!fenced(url) || !response.ok()) return;
    const headers = response.headers();
    if (headers["x-holon-agent-id"] !== agentId || headers["x-holon-agent-incarnation"] !== "1") {
      trace.identityErrors.push(`response ${url.pathname}`);
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
  const session = sessionFor(info, "right-panel-overview-refresh");
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  const update = async (body: Record<string, unknown>) => {
    expect((await request.post(control("/__e2e__/right-panel"), { data: { agentId, ...body } })).ok()).toBe(true);
  };
  const pending = new Set<Request>();
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (path.startsWith(`${agentPath}/`) || path.startsWith("/api/workspaces/")
      || path === `/api/control/agents/${agentId}/detail`) {
      if (!path.endsWith("/stream")) pending.add(request);
    }
  });
  page.on("requestfinished", (request) => pending.delete(request));
  page.on("requestfailed", (request) => pending.delete(request));
  const settle = () => expect.poll(() => pending.size).toBe(0);
  const advance = async (milliseconds: number) => {
    // HTTP is real; settle it between virtual seconds to avoid fake timeouts.
    for (let remaining = milliseconds; remaining > 0; remaining -= 1000) {
      await page.clock.runFor(Math.min(remaining, 1000));
      await settle();
    }
  };
  const counts = async () => {
    const { requests } = await (await request.get(control("/__e2e__/requests"))).json() as { requests: string[] };
    const reads = requests.filter((entry) => entry.startsWith("GET ")).map((entry) => new URL(entry.slice(4), "http://fixture"));
    return {
      timers: reads.filter((url) => url.pathname === `${agentPath}/timers`).length,
      recent: reads.filter((url) => url.pathname === `${agentPath}/tasks` && url.searchParams.get("status") === "terminal").length,
      skills: reads.filter((url) => url.pathname === `${agentPath}/skills`).length,
      workItemDetail: reads.filter((url) => url.pathname === `${agentPath}/work-items/${work.id}`).length,
      files: reads.filter((url) => url.pathname.startsWith("/api/workspaces/overview-files/files")).length,
      readMarkerPosts: requests.filter((entry) => /^POST [^ ?]*\/read-marker(?:\?|$)/.test(entry)).length,
      controlWrites: requests.filter((entry) => /^(?:POST|PUT|PATCH|DELETE) \/api\/control\//.test(entry)).length,
    };
  };
  const conversation = () => page.evaluate((id) => window.__HOLON_E2E__?.conversation(id), agentId);
  const checkpoint = async (body: Record<string, unknown>) => {
    const previous = (await conversation())?.throughSeq ?? 0;
    await update({ ...body, notify: "checkpoint" });
    await expect.poll(async () => (await conversation())?.throughSeq ?? 0).toBeGreaterThan(previous);
  };
  const visibility = (value: "hidden" | "visible") => page.evaluate((state) => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: state });
    document.dispatchEvent(new Event("visibilitychange"));
  }, value);
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  await update({
    workItems: [work], currentWorkItemId: work.id, stateWorkItems: [], tasks: [],
    timers: [], recentTasks: [], skills: [], workItemDetailsById: { [work.id]: work }, ...initial,
  });
  expect((await request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [{
      turn_id: "overview-baseline", key: { turn_id: "overview-baseline", turn_index: 1 }, revision: 1,
      presentation_class: "operator", inputs: [{ message_id: "overview-input", preview: "Keep resource reads separate from conversation writes" }],
      execution: { kind: "terminal", outcome: "completed" }, settled: true,
      result: { kind: "none", reason: { kind: "reducer_only", reason: "done" } },
      attention: null, detail_coverage: { kind: "complete" }, brief_ids: [],
    }],
  } })).ok()).toBe(true);
  await page.clock.install({ time: new Date("2026-10-31T03:00:00.000Z") });
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(`/agents/${agentId}/conversation`);
  await expect(page.locator('[data-turn-id="overview-baseline"]')).toBeVisible();
  const panel = page.locator(".side-panel");
  const togglePanel = () => page.getByRole("button", { name: "Context side panel", exact: true }).click();
  if (!(await panel.isVisible())) await togglePanel();
  await expect.poll(async () => (await (await request.get(control("/__e2e__/right-panel"))).json()).streamAgentIds).toContain(agentId);
  await expect.poll(async () => (await counts()).timers).toBeGreaterThan(0);
  await page.clock.pauseAt(await page.evaluate(() => Date.now()) + 1000);
  await advance(5000);
  const baseline = await counts();
  const timers = panel.locator(".agent-timers-card");
  const tasks = panel.locator("details.context-card").filter({ has: page.getByText("Tasks", { exact: true }) });
  const recent = tasks.locator("details.inspector-details-list");
  const skills = panel.locator("details.context-card").filter({ has: page.locator(".eyebrow", { hasText: /^Skills/ }) });
  const openRecent = async () => {
    await recent.locator(":scope > summary").click();
    // Native toggle is asynchronous; commit the visible section before ticking its scheduler.
    await expect(recent.getByRole("button", { name: "Refresh recent tasks", exact: true })).toBeVisible();
    await advance(1);
  };
  const healthy = async () => {
    await expect(page.locator(".conversation-sync-status .is-error, .conversation-sync-status .is-reconnecting")).toHaveCount(0);
    expect((await conversation())?.turnIds).toEqual(["overview-baseline"]);
    await expect(page.locator(".composer textarea")).toBeVisible();
    const latest = await counts();
    expect(latest.readMarkerPosts - baseline.readMarkerPosts).toBe(0);
    expect(latest.controlWrites - baseline.controlWrites).toBe(0);
  };
  return { update, checkpoint, counts, advance, settle, conversation, visibility, panel, timers, tasks, recent, skills, togglePanel, healthy, openRecent };
}

test("external Timer creation, firing and cancellation follow checkpoint hints; silent creation uses one 30-second fallback", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info);
  const before = await ui.counts();
  await ui.checkpoint({ timers: [timer()] });
  await ui.advance(6000);
  await expect(ui.timers.locator(".agent-timer-row")).toContainText(timer().summary);
  expect((await ui.counts()).timers - before.timers).toBe(1);

  const firedAt = "2026-10-31T03:00:10.000Z";
  await ui.checkpoint({ timers: [timer({ fire_count: 1, last_fired_at: firedAt })] });
  await ui.advance(6000);
  await expect(ui.timers.locator(`time[datetime="${firedAt}"]`)).toBeVisible();
  await expect(ui.timers.locator(".agent-timer-meta dd").filter({ hasText: /^1$/ })).toHaveCount(1);
  expect((await ui.counts()).timers - before.timers).toBe(2);

  await ui.checkpoint({ timers: [timer({ status: "cancelled" })] });
  await ui.advance(6000);
  await expect(ui.timers.locator(".agent-timer-row")).toHaveCount(0);
  expect((await ui.counts()).timers - before.timers).toBe(3);

  const head = (await ui.conversation())?.throughSeq;
  await ui.update({ timers: [timer({ summary: "Silent external timer" })] });
  await ui.advance(30_000);
  await expect(ui.timers).toContainText("Silent external timer");
  const after = await ui.counts();
  expect(after.timers - before.timers).toBe(4);
  expect(after.recent).toBe(0);
  expect((await ui.conversation())?.throughSeq).toBe(head);
  await ui.healthy();
});

test("Timer and recent-task reads stop when their sections, overview or side panel are closed", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info, { timers: [timer()], recentTasks: [recentTask()] });
  expect((await ui.counts()).recent).toBe(0);
  await ui.openRecent();
  await expect(ui.recent).toContainText(recentTask().summary);
  const opened = await ui.counts();
  expect(opened.recent).toBe(1);
  await ui.update({ recentTasks: [recentTask("Refreshed completed task")] });
  await ui.advance(30_000);
  await expect(ui.recent).toContainText("Refreshed completed task");
  expect((await ui.counts()).recent - opened.recent).toBe(1);

  await ui.timers.locator(":scope > summary").click();
  await ui.tasks.locator(":scope > summary").click();
  const collapsed = await ui.counts();
  await ui.checkpoint({ timers: [timer({ summary: "Changed while collapsed" })] });
  await ui.advance(60_000);
  expect((await ui.counts()).timers).toBe(collapsed.timers);
  expect((await ui.counts()).recent).toBe(collapsed.recent);
  await ui.timers.locator(":scope > summary").click();
  await ui.tasks.locator(":scope > summary").click();
  await ui.advance(1);
  await expect(ui.timers).toContainText("Changed while collapsed");
  const reopened = await ui.counts();
  expect(reopened.timers - collapsed.timers).toBe(1);
  expect(reopened.recent - collapsed.recent).toBe(1);

  await ui.panel.locator(".work-item-button").filter({ hasText: work.objective }).click();
  await expect(ui.panel.locator(".work-item-detail")).toBeVisible();
  await ui.advance(1);
  const detail = await ui.counts();
  await ui.checkpoint({});
  await ui.advance(60_000);
  expect((await ui.counts()).timers).toBe(detail.timers);
  expect((await ui.counts()).recent).toBe(detail.recent);
  await ui.panel.locator(".panel-sections").getByRole("button", { name: "Overview", exact: true }).click();
  await ui.advance(1);
  await ui.togglePanel();
  await expect(ui.panel).not.toBeVisible();
  const closed = await ui.counts();
  await ui.checkpoint({});
  await ui.advance(60_000);
  expect((await ui.counts()).timers).toBe(closed.timers);
  expect((await ui.counts()).recent).toBe(closed.recent);
  await ui.togglePanel();
  await ui.advance(1);
  expect((await ui.counts()).timers - closed.timers).toBe(1);
  await ui.healthy();
});

test("hidden overview resources stay idle and merge checkpoint, visibility, focus and online recovery", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info, { timers: [timer()], recentTasks: [recentTask()] });
  await ui.openRecent();
  await ui.visibility("hidden");
  const hidden = await ui.counts();
  // Hidden tabs suspend the conversation stream, so the hint is observed on return.
  await ui.update({ timers: [timer({ summary: "Timer changed while hidden" })], recentTasks: [recentTask("Task changed while hidden")], notify: "checkpoint" });
  await ui.advance(60_000);
  expect(await ui.counts()).toEqual(hidden);
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    document.dispatchEvent(new Event("visibilitychange"));
    window.dispatchEvent(new Event("focus"));
    window.dispatchEvent(new Event("online"));
    window.dispatchEvent(new Event("pageshow"));
  });
  await ui.advance(6000);
  await expect(ui.timers).toContainText("Timer changed while hidden");
  await expect(ui.recent).toContainText("Task changed while hidden");
  const restored = await ui.counts();
  expect(restored.timers - hidden.timers).toBe(1);
  expect(restored.recent - hidden.recent).toBe(1);
  await ui.healthy();
});

for (const status of [429, 503]) {
  test(`${status} stays local to Timer and recent tasks, preserves good data and permits explicit recovery`, async ({ page, context, request }, info) => {
    const ui = await scenario(page, context, request, info, { timers: [timer()], recentTasks: [recentTask()] });
    await ui.openRecent();
    const before = await ui.counts();
    await ui.checkpoint({ failureStatusByResource: { timers: status, "recent-tasks": status } });
    // Stop before any jittered automatic retry can enter the budget.
    await ui.advance(5000);
    await expect(ui.timers).toContainText(timer().summary);
    await expect(ui.timers).toContainText(staleText);
    await expect(ui.recent).toContainText(recentTask().summary);
    await expect(ui.recent).toContainText(staleText);
    await expect(ui.recent.getByRole("alert")).toContainText("unavailable");
    const failed = await ui.counts();
    expect(failed.timers - before.timers).toBe(1);
    expect(failed.recent - before.recent).toBe(1);
    await ui.healthy();

    await ui.update({
      failureStatusByResource: {},
      timers: [timer({ summary: "Recovered timer" })], recentTasks: [recentTask("Recovered recent task")],
    });
    await ui.timers.getByRole("button", { name: "Refresh detail", exact: true }).click();
    await ui.recent.getByRole("button", { name: "Refresh recent tasks", exact: true }).click();
    await ui.advance(1);
    await expect(ui.timers).toContainText("Recovered timer");
    await expect(ui.recent).toContainText("Recovered recent task");
    await expect(ui.timers.getByText(staleText, { exact: true })).toHaveCount(0);
    await expect(ui.recent.getByText(staleText, { exact: true })).toHaveCount(0);
    const recovered = await ui.counts();
    expect(recovered.timers - failed.timers).toBe(1);
    expect(recovered.recent - failed.recent).toBe(1);
    await ui.healthy();
  });
}

for (const { status, resource, count } of [
  { status: 401, resource: "timers", count: "timers" },
  { status: 403, resource: "recent-tasks", count: "recent" },
] as const) {
  test(`${status} on ${resource} stops automatic reads at the existing access boundary until explicit retry`, async ({ page, context, request }, info) => {
    const ui = await scenario(page, context, request, info, { timers: [timer()], recentTasks: [recentTask()] });
    if (count === "recent") {
      await ui.openRecent();
      await expect(ui.recent).toContainText(recentTask().summary);
    }
    const before = await ui.counts();
    await ui.checkpoint({ failureStatusByResource: { [resource]: status } });
    await ui.advance(5000);
    await expect(ui.panel.locator(".agent-overview-placeholder")).toContainText("Authentication required to open this agent");
    const rejected = await ui.counts();
    expect(rejected[count] - before[count]).toBe(1);
    await ui.advance(60_000);
    expect((await ui.counts())[count]).toBe(rejected[count]);

    await ui.update({ failureStatusByResource: {} });
    await ui.panel.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(ui.timers).toBeVisible();
    await ui.advance(1);
    if (count === "recent") {
      await ui.openRecent();
      await expect(ui.recent).toContainText(recentTask().summary);
    }
    expect((await ui.counts())[count] - rejected[count]).toBe(1);
    await expect(page.locator(".composer textarea")).toBeVisible();
    expect((await ui.counts()).controlWrites - before.controlWrites).toBe(0);
  });
}

test("a first recent-task failure has its own retry and does not disturb Timer or conversation reads", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info, {
    timers: [timer()], recentTasks: [recentTask()], failureStatusByResource: { "recent-tasks": 503 },
  });
  const before = await ui.counts();
  await ui.openRecent();
  await expect(ui.recent.getByRole("alert")).toContainText("unavailable");
  await expect(ui.recent.locator(".task-button")).toHaveCount(0);
  await ui.update({ failureStatusByResource: {} });
  await ui.recent.getByRole("button", { name: "Refresh recent tasks", exact: true }).click();
  await ui.advance(1);
  await expect(ui.recent).toContainText(recentTask().summary);
  const after = await ui.counts();
  expect(after.recent - before.recent).toBe(2);
  expect(after.timers).toBe(before.timers);
  await ui.healthy();
});

test("Skill catalogs do not poll or react to checkpoints, but reopen revalidates after sixty seconds", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info, { skills: [skill("original-skill")] });
  await ui.skills.locator(":scope > summary").click();
  await ui.advance(1);
  await expect(ui.skills.locator(".agent-skill-list")).toContainText("original-skill");
  await ui.skills.locator(":scope > summary").click();
  await ui.togglePanel();
  await expect(ui.panel).not.toBeVisible();
  const before = await ui.counts();
  await ui.checkpoint({ skills: [skill("external-skill")] });
  await ui.advance(65_000);
  expect((await ui.counts()).skills).toBe(before.skills);
  await ui.togglePanel();
  await expect(ui.panel).toBeVisible();
  await ui.skills.locator(":scope > summary").click();
  await ui.advance(1);
  await expect(ui.skills.locator(".agent-skill-list")).toContainText("external-skill");
  expect((await ui.counts()).skills - before.skills).toBe(1);
  const reopened = await ui.counts();
  await ui.advance(120_000);
  expect((await ui.counts()).skills).toBe(reopened.skills);
  await ui.healthy();
});

test("refreshed plan artifact hashes flag the preserved file snapshot until explicit reload", async ({ page, context, request }, info) => {
  const workspace = {
    workspace_id: "overview-files", workspace_alias: "Overview files", workspace_anchor: "/test",
    execution_root_id: "overview-root",
  };
  const detail = (revision: number) => ({
    ...work, revision,
    plan_artifact: {
      path: "/test/plan.md", relative_path: "plan.md", workspace_id: workspace.workspace_id,
      hash: `hash-${revision}`, preview: `Plan revision ${revision}`, preview_complete: true,
    },
  });
  const directory = {
    type: "directory", workspace_id: workspace.workspace_id, execution_root_id: workspace.execution_root_id,
    path: "", absolute_path: "/test", root_kind: "git_worktree_root", kind: "directory",
    entries: [{ name: "plan.md", type: "file", size: 32, mime_type: "text/markdown" }],
  };
  const file = (content: string) => ({
    type: "file", workspace_id: workspace.workspace_id, execution_root_id: workspace.execution_root_id,
    path: "plan.md", absolute_path: "/test/plan.md", root_kind: "git_worktree_root", kind: "file",
    size: content.length, mime_type: "text/markdown", content,
  });
  const ui = await scenario(page, context, request, info, {
    activeWorkspaceEntry: workspace, workItems: [detail(1)], workItemDetailsById: { [work.id]: detail(1) },
    filesByPath: { "": directory, "plan.md": file("# Original plan") },
  });
  await ui.panel.locator(".work-item-button").filter({ has: page.locator("strong", { hasText: work.objective }) }).click();
  await expect(ui.panel.locator(".work-item-detail")).toBeVisible();
  await ui.advance(1);
  await expect(ui.panel.locator(".detail-plan-preview")).toContainText("Plan revision 1");
  await ui.panel.locator(".workspace-path-link").click();
  const browser = ui.panel.locator(".file-browser");
  await expect(browser.locator(".file-browser-markdown")).toContainText("Original plan");
  const opened = await ui.counts();

  await ui.update({
    workItems: [detail(2)], notify: "checkpoint",
    workItemDetailsById: { [work.id]: detail(2) },
    filesByPath: { "": directory, "plan.md": file("# Updated plan") },
  });
  await ui.advance(6000);
  const hint = browser.locator(".file-browser-update-hint");
  await expect(hint).toContainText("File may have changed.");
  await expect(browser.locator(".file-browser-markdown")).toContainText("Original plan");
  expect((await ui.counts()).files).toBe(opened.files);
  expect((await ui.counts()).workItemDetail).toBe(opened.workItemDetail);
  await hint.getByRole("button", { name: "Reload", exact: true }).click();
  await expect(browser.locator(".file-browser-markdown")).toContainText("Updated plan");
  await expect(hint).toHaveCount(0);
  expect((await ui.counts()).files - opened.files).toBe(1);
  await ui.healthy();
});

test("directory and open-file contents change only after explicit refresh, not checkpoint or fallback", async ({ page, context, request }, info) => {
  const workspace = {
    workspace_id: "overview-files", workspace_alias: "Overview files", workspace_anchor: "/test",
    execution_root_id: "overview-root",
  };
  const directory = (names: string[]) => ({
    type: "directory", workspace_id: workspace.workspace_id, execution_root_id: workspace.execution_root_id,
    path: "", absolute_path: "/test", root_kind: "git_worktree_root", kind: "directory",
    entries: names.map((name) => ({ name, type: "file", size: 32, mime_type: "text/markdown" })),
  });
  const file = (content: string) => ({
    type: "file", workspace_id: workspace.workspace_id, execution_root_id: workspace.execution_root_id,
    path: "README.md", absolute_path: "/test/README.md", root_kind: "git_worktree_root", kind: "file",
    size: content.length, mime_type: "text/markdown", content,
  });
  const originalFile = file("# Original file content");
  const ui = await scenario(page, context, request, info, {
    activeWorkspaceEntry: workspace,
    filesByPath: { "": directory(["README.md"]), "README.md": originalFile },
  });
  await ui.panel.locator(".panel-sections").getByRole("button", { name: "Files", exact: true }).click();
  const browser = ui.panel.locator(".file-browser");
  await expect(browser.getByRole("button", { name: "README.md", exact: false })).toBeVisible();
  const opened = await ui.counts();
  // The CI dev fixture remounts effects under StrictMode; static builds do not.
  expect(opened.files).toBe(process.env.HOLON_E2E_PRODUCTION === "1" ? 1 : 2);
  await ui.checkpoint({ filesByPath: { "": directory(["README.md", "external.md"]), "README.md": originalFile } });
  await ui.advance(60_000);
  expect((await ui.counts()).files).toBe(opened.files);
  await expect(browser.getByRole("button", { name: "external.md", exact: false })).toHaveCount(0);
  await browser.getByRole("button", { name: "File actions", exact: true }).click();
  await page.getByRole("button", { name: "Refresh directory", exact: true }).click();
  await ui.settle();
  await expect(browser.getByRole("button", { name: "external.md", exact: false })).toBeVisible();
  expect((await ui.counts()).files - opened.files).toBe(1);

  await browser.getByRole("button", { name: "README.md", exact: false }).click();
  await expect(browser.locator(".file-browser-markdown")).toContainText("Original file content");
  const preview = await ui.counts();
  expect(preview.files - opened.files).toBe(2);
  await ui.checkpoint({
    filesByPath: { "": directory(["README.md", "external.md"]), "README.md": file("# Externally changed file content") },
  });
  await ui.advance(60_000);
  expect((await ui.counts()).files).toBe(preview.files);
  await expect(browser.locator(".file-browser-markdown")).toContainText("Original file content");
  await browser.getByRole("button", { name: "File actions", exact: true }).click();
  await page.getByRole("button", { name: "Refresh file content", exact: true }).click();
  await ui.settle();
  await expect(browser.locator(".file-browser-markdown")).toContainText("Externally changed file content");
  expect((await ui.counts()).files - preview.files).toBe(1);
  await ui.healthy();
});
