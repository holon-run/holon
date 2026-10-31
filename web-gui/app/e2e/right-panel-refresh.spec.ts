import { expect, test, type APIRequestContext, type BrowserContext, type Page, type Request, type TestInfo } from "@playwright/test";
import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const agentPath = `/api/agents/${agentId}`;
const forbiddenEvents = /^\/api\/(?:agents\/[^/]+\/events(?:\/stream)?|events\/stream)\/?$/;
const createdAt = "2026-10-31T00:00:00.000Z";
const work = (id: string, objective: string, overrides = {}) => ({
  id, objective, state: "open", plan_status: "ready", revision: 1,
  created_at: createdAt, updated_at: createdAt, scheduling_state: "runnable",
  is_current: true, readiness: "ready", focus: "current", ...overrides,
});

const observed = new WeakMap<Page, { rawEvents: string[]; navigations: number }>();
test.beforeEach(async ({ page }) => {
  const trace = { rawEvents: [] as string[], navigations: 0 };
  observed.set(page, trace);
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (forbiddenEvents.test(path)) trace.rawEvents.push(path);
  });
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) trace.navigations += 1;
  });
});
test.afterEach(async ({ page }) => {
  expect(observed.get(page)?.rawEvents).toEqual([]);
  expect(observed.get(page)?.navigations).toBe(1);
});
test.setTimeout(60_000);

async function scenario(page: Page, context: BrowserContext, request: APIRequestContext, info: TestInfo, initial = [] as ReturnType<typeof work>[]) {
  const session = sessionFor(info, "right-panel-refresh");
  const pending = new Set<Request>();
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if ([`${agentPath}/state`, `${agentPath}/work-items`, `/api/control/agents/${agentId}/detail`].includes(path)) pending.add(request);
  });
  page.on("requestfinished", (request) => pending.delete(request));
  page.on("requestfailed", (request) => pending.delete(request));
  const advance = async (milliseconds: number) => {
    // Let real HTTP settle between virtual ticks instead of manufacturing
    // request timeouts while the fake clock races ahead of the fixture server.
    for (let remaining = milliseconds; remaining > 0; remaining -= 1000) {
      await page.clock.runFor(Math.min(remaining, 1000));
      await expect.poll(() => pending.size).toBe(0);
    }
  };
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  const update = async (body: Record<string, unknown>) => {
    expect((await request.post(control("/__e2e__/right-panel"), { data: { agentId, ...body } })).ok()).toBe(true);
  };
  const counts = async () => {
    const { requests } = await (await request.get(control("/__e2e__/requests"))).json() as { requests: string[] };
    const paths = requests.map((entry) => entry.split(" ")[1].split("?")[0]);
    return {
      state: paths.filter((path) => path === `${agentPath}/state`).length,
      workItems: paths.filter((path) => path === `${agentPath}/work-items`).length,
      readMarkerPosts: requests.filter((entry) => /^POST [^ ?]*\/read-marker(?:\?|$)/.test(entry)).length,
      controlWrites: requests.filter((entry) => /^(?:POST|PUT|PATCH|DELETE) \/api\/control\//.test(entry)).length,
    };
  };
  const conversation = () => page.evaluate((id) => window.__HOLON_E2E__?.conversation(id), agentId);
  const checkpoint = async (body: Record<string, unknown>) => {
    const previous = (await conversation())?.throughSeq ?? 0;
    await update({ ...body, notify: "checkpoint" });
    // Do not advance fake time until the real SSE batch has committed.
    await expect.poll(async () => (await conversation())?.throughSeq ?? 0).toBeGreaterThan(previous);
  };
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  await update({ workItems: initial, currentWorkItemId: initial[0]?.id ?? null, stateWorkItems: [] });
  expect((await request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [{
      turn_id: "refresh-baseline", key: { turn_id: "refresh-baseline", turn_index: 1 }, revision: 1,
      presentation_class: "operator", inputs: [{ message_id: "refresh-input", preview: "Keep this conversation unchanged" }],
      execution: { kind: "terminal", outcome: "completed" }, settled: true,
      result: { kind: "none", reason: { kind: "reducer_only", reason: "done" } },
      attention: null, detail_coverage: { kind: "complete" }, brief_ids: [],
    }],
  } })).ok()).toBe(true);
  // Install before navigation, then pause only after normal bootstrap/rendering.
  await page.clock.install({ time: new Date("2026-10-31T03:00:00.000Z") });
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(`/agents/${agentId}/conversation`);
  await expect(page.locator('[data-turn-id="refresh-baseline"]')).toBeVisible();
  const panel = page.locator(".side-panel");
  if (!(await panel.isVisible())) await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  await expect(panel).toBeVisible();
  await expect.poll(async () => (await (await request.get(control("/__e2e__/right-panel"))).json()).streamAgentIds).toContain(agentId);
  await expect.poll(async () => (await counts()).workItems).toBeGreaterThan(0);
  await page.clock.pauseAt(await page.evaluate(() => Date.now()) + 1000);
  await advance(5000);
  const bar = page.locator(".current-work-bar");
  const cards = panel.locator(".work-item-button");
  const card = (objective: string) => cards.filter({ has: page.locator("strong", { hasText: objective }) });
  const healthy = async () => {
    await expect(page.locator(".conversation-sync-status .is-error, .conversation-sync-status .is-reconnecting")).toHaveCount(0);
    expect((await conversation())?.turnIds).toEqual(["refresh-baseline"]);
    await expect(page.locator(".composer textarea")).toBeVisible();
  };
  return { update, counts, checkpoint, conversation, panel, bar, cards, card, healthy, advance };
}

test("checkpoint-only hints update the empty list, objective, status and current work without a reload", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info);
  await expect(ui.cards).toHaveCount(0);
  await expect(ui.bar).toHaveCount(0);

  const first = work("refresh-first", "First external work item");
  await ui.checkpoint({ workItems: [first], currentWorkItemId: first.id });
  await ui.advance(5500);
  await expect(ui.card(first.objective)).toBeVisible();
  await expect(ui.card(first.objective)).toHaveClass(/featured/);
  await expect(ui.bar).toContainText(first.objective);

  // Deliberately stale state.work_items cannot override the authoritative list.
  const blocked = work(first.id, "Updated objective from the work-item API", {
    revision: 2, scheduling_state: "waiting_operator", readiness: "blocked",
  });
  const before = await ui.counts();
  for (let revision = 2; revision <= 7; revision += 1) {
    await ui.checkpoint({ workItems: [{ ...blocked, revision }], stateWorkItems: [first] });
  }
  await ui.advance(4900);
  const burst = await ui.counts();
  expect(burst.state - before.state).toBeLessThanOrEqual(1);
  expect(burst.workItems - before.workItems).toBeLessThanOrEqual(1);
  await ui.advance(600);
  await expect(ui.card(blocked.objective)).toBeVisible();
  await expect(ui.card(first.objective)).toHaveCount(0);
  await expect(ui.bar).toContainText(blocked.objective);
  await expect(ui.bar).toHaveAttribute("data-state", "needsInput");

  const second = work("refresh-second", "New current work item");
  const notCurrent = { ...blocked, is_current: false, focus: "background" };
  await ui.checkpoint({ workItems: [notCurrent, second], currentWorkItemId: second.id });
  await ui.advance(5500);
  await expect(ui.card(second.objective)).toHaveClass(/featured/);
  await expect(ui.card(blocked.objective)).not.toHaveClass(/featured/);
  await expect(ui.bar).toContainText(second.objective);
  await expect(ui.bar).toHaveAttribute("data-state", "ready");

  await ui.checkpoint({ currentWorkItemId: null, workItems: [notCurrent, second].map((item) => ({
    ...item, state: "completed", readiness: "completed", is_current: false, focus: "background",
  })) });
  await ui.advance(5500);
  await expect(ui.panel.locator(".current-work .inspector-details-list > summary")).toHaveText("2 completed");
  await expect(ui.cards.filter({ has: page.locator("small", { hasText: "Current" }) })).toHaveCount(0);
  await ui.advance(8100);
  await expect(ui.bar).toHaveCount(0);
  await ui.healthy();
});

test("silent changes converge within the 30-second fallback; hidden tabs pause and catch up on return", async ({ page, context, request }, info) => {
  const ui = await scenario(page, context, request, info);
  const head = (await ui.conversation())?.throughSeq;
  const first = work("silent-first", "Created without any conversation notification");
  const before = await ui.counts();
  await ui.update({ workItems: [first], currentWorkItemId: first.id });
  await ui.advance(30_000);
  await expect(ui.card(first.objective)).toBeVisible();
  await expect(ui.bar).toContainText(first.objective);
  const fallback = await ui.counts();
  expect(fallback.state - before.state).toBe(1);
  expect(fallback.workItems - before.workItems).toBe(1);
  // Bootstrap may legitimately mark the conversation read; only refresh deltas must stay zero.
  expect(fallback.readMarkerPosts - before.readMarkerPosts).toBe(0);
  expect(fallback.controlWrites - before.controlWrites).toBe(0);
  expect((await ui.conversation())?.throughSeq).toBe(head);

  const visibility = (value: string) => page.evaluate((state) => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: state });
    document.dispatchEvent(new Event("visibilitychange"));
  }, value);
  await visibility("hidden");
  const hidden = await ui.counts();
  const updated = work(first.id, "Changed while the tab was hidden", { revision: 2 });
  await ui.update({ workItems: [updated] });
  await ui.advance(60_000);
  expect(await ui.counts()).toEqual(hidden);
  await expect(ui.card(first.objective)).toBeVisible();
  await visibility("visible");
  await ui.advance(5500);
  await expect(ui.card(updated.objective)).toBeVisible();
  await expect(ui.bar).toContainText(updated.objective);
  await ui.healthy();
});

test("resource failures keep last-known-good work visible without a conversation sync error", async ({ page, context, request }, info) => {
  const first = work("failure-first", "Retain this work during a refresh failure");
  const ui = await scenario(page, context, request, info, [first]);
  await expect(ui.card(first.objective)).toBeVisible();
  await expect(ui.bar).toContainText(first.objective);
  const failed: string[] = [];
  page.on("response", (response) => {
    const path = new URL(response.url()).pathname;
    if (response.status() === 503 && ["/state", "/work-items"].some((suffix) => path === agentPath + suffix)) failed.push(path);
  });
  await ui.checkpoint({ failResources: ["work-items"] });
  await ui.advance(5500);
  await expect.poll(() => failed).toContain(`${agentPath}/work-items`);
  await expect(ui.card(first.objective)).toBeVisible();
  await expect(ui.bar).toContainText(first.objective);
  await ui.healthy();

  await ui.checkpoint({ failResources: ["state", "work-items"] });
  await ui.advance(5500);
  await expect.poll(() => failed).toContain(`${agentPath}/state`);
  await expect(ui.bar).toHaveAttribute("data-state", "stale");
  await expect(ui.bar).toContainText(first.objective);
  await expect(ui.card(first.objective)).toBeVisible();
  await ui.healthy();

  const recovered = work(first.id, "Recovered right-panel refresh", { revision: 2 });
  await ui.update({ failResources: [], workItems: [recovered] });
  await ui.advance(30_000);
  await expect(ui.card(recovered.objective)).toBeVisible();
  await expect(ui.bar).toContainText(recovered.objective);
  await expect(ui.bar).toHaveAttribute("data-state", "ready");
  await ui.healthy();
});
