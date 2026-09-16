import { expect, test } from "@playwright/test";
import type { ConversationActivity, ConversationTurnSummary } from "@holon/conversation-sdk";

const agentId = "bootstrap-agent";
const turn = (id: string, index: number, overrides: Partial<ConversationTurnSummary> = {}): ConversationTurnSummary => ({
  turn_id: id, key: { turn_id: id, turn_index: index }, revision: 1,
  presentation_class: "operator", inputs: [{ message_id: `input-${id}`, preview: `Message ${index}` }],
  execution: { kind: "active" }, result: { kind: "pending" }, settled: false,
  attention: null, detail_coverage: { kind: "complete" }, brief_ids: [], ...overrides,
});
const activity = (index: number, summary: string): ConversationActivity => ({
  kind: "assistant", id: `assistant:${index}`, key: { event_seq: index, activity_id: `assistant:${index}` },
  revision: 1, summary,
});

test("live process folds only after a readable result; manual expansion and failure remain visible", async ({ page, context, request }, info) => {
  const session = `presentation-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const historical = turn("history", 1, {
    execution: { kind: "terminal", outcome: "completed" }, result: { kind: "available" }, settled: true, brief_ids: ["history-brief"],
  });
  let current = turn("current", 2);
  const update = async (activities = [activity(2, "Checking the conversation layout.")]) => {
    const response = await request.post(control("/__e2e__/conversation"), {
      data: { agentId, turns: [historical, current], activitiesByTurnId: { current: activities } },
    });
    expect(response.ok()).toBe(true);
  };
  const brief = (id: string, text: string) => ({
    id, agent_id: agentId, workspace_id: "holon", kind: "result", text,
    created_at: "2026-09-15T14:30:00Z", content_source: { kind: "inline" },
  });
  await request.post(control("/__e2e__/configure"), { data: { briefsById: {
    "history-brief": brief("history-brief", "Historical result."),
    "current-brief": brief("current-brief", "The layout is ready."),
  } } });
  await update();
  await page.goto(`/agents/${agentId}/conversation`);
  const history = page.locator('[data-turn-id="history"]');
  const live = page.locator('[data-turn-id="current"]');
  const disclosure = live.locator(".conversation-detail-toggle");
  await expect(live.getByText("Checking the conversation layout.")).toBeVisible();
  await expect(history.locator(".conversation-detail-toggle")).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".side-panel")).not.toBeVisible();
  const requests = await request.get(control("/__e2e__/requests")).then((r) => r.json());
  expect(requests.requests.some((url: string) => url.includes("/turns/history/activities"))).toBe(false);

  // Execution can end before the result arrives.
  current = { ...current, revision: 2, execution: { kind: "terminal", outcome: "completed" } };
  await update();
  await expect(live.getByText("Execution ended, waiting for the result…")).toBeVisible();
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");
  // A readable brief is sufficient to fold; transport settlement can arrive later.
  current = { ...current, revision: 3, result: { kind: "available" }, settled: false, brief_ids: ["current-brief"] };
  await update();
  await expect(live.getByText("The layout is ready.")).toBeVisible();
  await expect(disclosure).toHaveAttribute("aria-expanded", "false");
  await expect(live.getByText("Finishing result delivery…")).not.toBeVisible();

  // Historical expansion is explicit and remains stable on summary updates.
  await disclosure.click();
  current = { ...current, revision: 4 };
  await update();
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");
  current = { ...current, revision: 5, execution: { kind: "terminal", outcome: "aborted" }, attention: { kind: "interrupted" } };
  await update();
  await expect(live.getByText("This turn was stopped")).toBeVisible();
  await disclosure.click();
  await expect(live.getByText("This turn was stopped")).toBeVisible();
  await expect(live.getByText("The layout is ready.")).toBeVisible();
});

test("live growth follows the bottom but preserves history reading, including on a narrow screen", async ({ page, context, request }, info) => {
  const session = `scroll-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const history = Array.from({ length: 12 }, (_, i) => turn(`history-${i}`, i + 1, {
    execution: { kind: "terminal", outcome: "completed" }, settled: true,
    result: { kind: "none", reason: { kind: "reducer_only", reason: "done" } },
  }));
  const current = turn("live", 13);
  const activities: ConversationActivity[] = [activity(100, "Reading the source.")];
  const update = () => request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [...history, current], activitiesByTurnId: { live: activities },
  } });
  await update();
  await page.goto(`/agents/${agentId}/conversation`);
  await expect(page.getByText("Reading the source.")).toBeVisible();
  const list = page.locator(".message-list");
  const bottomGap = () => list.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight);
  activities.push(activity(101, "Checking changes.\n\n".repeat(12)));
  await update();
  await expect(page.getByText("Checking changes.", { exact: true }).first()).toBeVisible();
  await expect.poll(bottomGap).toBeLessThan(5);

  await list.hover();
  await page.mouse.wheel(0, -650);
  await expect(page.getByRole("button", { name: "Back to latest" })).toBeVisible();
  // Deliver more activity while the reader is in the history.
  await expect.poll(() => list.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
  const top = await list.evaluate((el) => el.scrollTop);
  activities.push(activity(102, "More live progress.\n\n".repeat(12)));
  await update();
  await expect(page.getByText("More live progress.", { exact: true }).first()).toBeAttached();
  await expect.poll(() => list.evaluate((el) => el.scrollTop)).toBeCloseTo(top, 0);
  await page.getByRole("button", { name: "Back to latest" }).click();
  await expect.poll(bottomGap).toBeLessThan(5);

  const process = page.locator('[data-turn-id="live"] .conversation-detail-toggle');
  await process.click();
  await expect(process).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator('[data-turn-id="live"] .conversation-detail-clip')).toHaveJSProperty("offsetHeight", 0);
  const triggerTop = (await process.boundingBox())!.y;
  await process.click();
  await expect(process).toHaveAttribute("aria-expanded", "true");
  await expect.poll(async () => (await process.boundingBox())!.y).toBeCloseTo(triggerTop, 0);

  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator("form.composer")).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
  const input = page.locator(".composer textarea");
  expect((await input.boundingBox())!.width).toBeGreaterThan(200);
});

test("an invalidated active process refreshes before its brief and opens the existing tool renderer in one click", async ({ page, context, request }, info) => {
  const session = `details-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const current = turn("live-detail", 1);
  const tool: ConversationActivity = { kind: "tool", id: "tool:exec-live", key: { event_seq: 2, activity_id: "tool:exec-live" }, revision: 1, summary: "ExecCommand · success" };
  await request.post(control("/__e2e__/configure"), { data: { toolExecutionsById: {
    "exec-live": { id: "exec-live", agent_id: agentId, tool_name: "ExecCommand", status: "success",
      input: { cmd: "printf live-marker" }, output: { stdout: "live-marker", exit_status: 0 }, summary: "command completed" },
  } } });
  const update = (activities: ConversationActivity[]) => request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [current], activitiesByTurnId: { "live-detail": activities }, invalidateOnly: true,
  } });
  await update([activity(1, "Starting the live check.")]);
  await page.goto(`/agents/${agentId}/conversation`);
  await expect(page.getByText("Starting the live check.")).toBeVisible();
  await update([activity(1, "Starting the live check."), activity(3, ""), tool]);
  const toolRow = page.locator('[data-activity-id="tool:exec-live"] button');
  await expect(toolRow).toContainText("printf live-marker");
  await expect(toolRow).toContainText("ExecCommand · success");
  await expect(page.locator('[data-activity-id="assistant:3"]')).toHaveCount(0);
  await expect(page.getByText("Activity summary unavailable", { exact: true })).toHaveCount(0);
  await expect(page.locator('[data-turn-id="live-detail"] .conversation-brief')).toHaveCount(0);
  await toolRow.click();
  await expect(page.locator(".side-panel")).toBeVisible();
  await expect(page.locator(".side-panel").getByText("printf live-marker", { exact: true })).toBeVisible();
  await expect(page.locator(".side-panel").getByText("live-marker", { exact: true })).toBeVisible();
  const toolRequests = async () => (await request.get(control("/__e2e__/requests")).then((r) => r.json())).requests
    .filter((url: string) => url.includes("/tool-executions/exec-live"));
  expect(await toolRequests()).toHaveLength(1);
  // The same canonical record changes while mounted; both preview and open inspector refresh.
  await request.post(control("/__e2e__/configure"), { data: { toolExecutionsById: {
    "exec-live": { id: "exec-live", agent_id: agentId, tool_name: "ExecCommand", status: "error",
      input: { cmd: "printf live-marker" }, output: { stderr: "updated failure", exit_status: 1 }, duration_ms: 1250 },
  } } });
  await update([activity(1, "Starting the live check."), { ...activity(3, "Tool output received."), revision: 2 },
    { ...tool, revision: 2, summary: "ExecCommand · error" }]);
  await expect(page.locator('[data-activity-id="assistant:3"]')).toContainText("Tool output received.");
  await expect(toolRow).toContainText("ExecCommand · error · 1.3s");
  await expect(page.locator(".side-panel").getByText("updated failure", { exact: true })).toBeVisible();
  expect(await toolRequests()).toHaveLength(2);
  await page.getByRole("button", { name: "Close side panel", exact: true }).click();
  await page.getByText("Starting the live check.", { exact: true }).click();
  await expect(page.locator(".side-panel").getByText("Starting the live check.", { exact: true }).first()).toBeVisible();
});

test("tool summaries load only for visible expanded rows and failed loads remain inspectable", async ({ page, context, request }, info) => {
  const session = `tool-preview-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const tools: ConversationActivity[] = Array.from({ length: 10 }, (_, i) => ({
    kind: "tool", id: `tool:preview-${i}`, key: { event_seq: i, activity_id: `tool:preview-${i}` },
    revision: 1, summary: "ExecCommand · success",
  }));
  await request.post(control("/__e2e__/configure"), { data: { toolExecutionsById: Object.fromEntries(tools.slice(0, 9).map((_, i) => [
    `preview-${i}`, { id: `preview-${i}`, tool_name: "ExecCommand", status: "success", input: { cmd: `echo ${i}` } },
  ])) } });
  await request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [turn("history", 1, { execution: { kind: "terminal", outcome: "completed" }, settled: true,
      result: { kind: "none", reason: { kind: "reducer_only", reason: "done" } } })],
    activitiesByTurnId: { history: tools },
  } });
  const toolRequests = async () => (await request.get(control("/__e2e__/requests")).then((r) => r.json())).requests
    .filter((url: string) => url.includes("/tool-executions/"));
  await page.goto(`/agents/${agentId}/conversation`);
  const toggle = page.locator(".conversation-detail-toggle");
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  expect(await toolRequests()).toHaveLength(0);
  await toggle.click();
  await expect(page.getByText("echo 8", { exact: true })).toBeVisible();
  await expect.poll(async () => (await toolRequests()).length).toBe(8);
  await expect(page.locator('[data-activity-id="tool:preview-9"] button')).toBeEnabled();
  await page.getByRole("button", { name: "Show earlier activity" }).click();
  await expect(page.getByText("echo 0", { exact: true })).toBeVisible();
  expect(await toolRequests()).toHaveLength(10);
  await toggle.click();
  await expect(page.locator(".conversation-activity")).toHaveCount(0);
  await toggle.click();
  await expect(page.getByText("echo 8", { exact: true })).toBeVisible();
  expect(await toolRequests()).toHaveLength(10);
});

test("turn clock survives refresh, hides while waiting for a brief, and opens a flush-aligned process", async ({ page, context, request }, info) => {
  const session = `timing-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const start = Date.parse("2026-09-16T00:00:00Z");
  await page.clock.install({ time: new Date(start + 23000) });
  await page.clock.pauseAt(new Date(start + 24000));
  let current = turn("timed", 1, { started_at: new Date(start).toISOString() });
  const update = () => request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [current], activitiesByTurnId: { timed: [activity(1, "Checking elapsed time.")] },
  } });
  await update();
  await page.goto(`/agents/${agentId}/conversation`);
  const card = page.locator('[data-turn-id="timed"]');
  const clock = card.locator(".conversation-turn-elapsed");
  const disclosure = card.locator(".conversation-detail-toggle");
  await expect(clock).toHaveText("0:24");
  await page.clock.runFor(2000);
  await expect(clock).toHaveText("0:26");
  await page.reload();
  await expect(clock).toHaveText("0:26");
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");

  current = { ...current, revision: 2, execution: { kind: "terminal", outcome: "completed" },
    completed_at: new Date(start + 85000).toISOString(), duration_ms: 83000 };
  await update();
  await expect(disclosure).toContainText("Waiting for result");
  // Waiting for result hides the live clock entirely; nothing ticks while the brief is pending.
  await expect(clock).toHaveCount(0);
  await page.clock.runFor(10000);
  await expect(clock).toHaveCount(0);
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");

  await request.post(control("/__e2e__/configure"), { data: { briefsById: {
    "timed-brief": { id: "timed-brief", agent_id: agentId, workspace_id: "holon", kind: "result",
      text: "Timing is ready.", created_at: new Date(start + 100000).toISOString(), content_source: { kind: "inline" } },
  } } });
  current = { ...current, revision: 3, result: { kind: "available" }, settled: true, brief_ids: ["timed-brief"] };
  await update();
  await expect(card.getByText("Timing is ready.")).toBeVisible();
  await expect(disclosure).toContainText("Completed");
  // The clock returns with the frozen terminal duration, not the advanced wall clock.
  await expect(clock).toHaveText("Took 1:23");
  await expect(disclosure).toHaveAttribute("aria-expanded", "false");
  await clock.click();
  await page.clock.runFor(250);
  await expect(card.getByText("Checking elapsed time.")).toBeVisible();
  const process = (await card.locator(".conversation-detail").boundingBox())!;
  const result = (await card.locator(".conversation-brief").boundingBox())!;
  expect(process.x).toBe(result.x);
  expect(process.width).toBe(result.width);
  current = { ...current, revision: 4 };
  await update();
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");
  await page.reload();
  await expect(clock).toHaveText("Took 1:23");
  await expect(disclosure).toHaveAttribute("aria-expanded", "false");
});

test("a delivered brief replaces only the duplicate final activity, including after reload", async ({ page, context, request }, info) => {
  const session = `dedup-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  let current = turn("dedup", 1);
  const result = "The final answer is ready.";
  const progress = Array.from({ length: 8 }, (_, index) => activity(index, `Progress step ${index}.`));
  const update = () => request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [current], activitiesByTurnId: { dedup: [...progress, activity(9, result)] },
  } });
  await update();
  await page.goto(`/agents/${agentId}/conversation`);
  const card = page.locator('[data-turn-id="dedup"]');
  const disclosure = card.locator(".conversation-detail-toggle");
  const finalActivity = card.locator('[data-activity-id="assistant:9"]');
  await expect(finalActivity).toContainText(result);
  current = { ...current, revision: 2, execution: { kind: "terminal", outcome: "completed" } };
  await update();
  await expect(disclosure).toContainText("Waiting for result");
  await expect(finalActivity).toBeVisible();

  // Delay the actual brief response so result metadata alone cannot hide the output.
  let releaseBrief!: () => void;
  const briefReady = new Promise<void>((resolve) => { releaseBrief = resolve; });
  await page.route(/\/briefs(?:\/|:)/, async (route) => {
    await briefReady;
    await route.continue();
  });
  await request.post(control("/__e2e__/configure"), { data: { briefsById: {
    "dedup-brief": { id: "dedup-brief", agent_id: agentId, workspace_id: "holon", kind: "result",
      text: result, created_at: "2026-09-16T00:00:00Z", content_source: { kind: "inline" } },
  } } });
  current = { ...current, revision: 3, result: { kind: "available" }, settled: true, brief_ids: ["dedup-brief"] };
  await update();
  await expect(card.locator(".conversation-brief")).toBeVisible();
  await expect(finalActivity).toBeVisible();
  releaseBrief();
  await expect(card.locator(".conversation-brief")).toContainText(result);
  await expect(disclosure).toHaveAttribute("aria-expanded", "false");
  await disclosure.click();
  await expect(finalActivity).toHaveCount(0);
  await expect(card.getByText(result, { exact: true })).toHaveCount(1);
  // Deduplication precedes the recent-eight limit so no real progress is displaced.
  await expect(card.locator(".conversation-activity")).toHaveCount(8);
  await expect(card.getByText("Progress step 0.", { exact: true })).toBeVisible();
  await expect(card.getByRole("button", { name: "Show earlier activity" })).toHaveCount(0);
  await page.reload();
  await expect(card.locator(".conversation-brief")).toContainText(result);
  await disclosure.click();
  await expect(card.locator(".conversation-activity")).toHaveCount(8);
  await expect(finalActivity).toHaveCount(0);
  await expect(card.getByText(result, { exact: true })).toHaveCount(1);
});

test("queued task results are compact events above the latest turn while operator input stays below", async ({ page, context, request }, info) => {
  const session = `pending-events-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const pending = [
    { message_id: "a-new", revision: 1, state: "queued", presentation_class: "task", created_at: "2026-09-16T01:01:00Z", preview: "Second task finished" },
    { message_id: "z-old", revision: 1, state: "queued", presentation_class: "task", created_at: "2026-09-16T01:00:00Z", preview: "First task finished\n\n" + "Detailed command output\n".repeat(100) },
    { message_id: "operator", revision: 1, state: "queued", presentation_class: "operator", created_at: "2026-09-16T01:02:00Z", preview: "Please continue checking" },
  ];
  let current = turn("current", 1);
  const update = () => request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [current], pending_inputs: pending, activitiesByTurnId: { current: [activity(1, "Working on the request.")] },
  } });
  await update();
  await page.goto(`/agents/${agentId}/conversation`);
  const events = page.locator(".conversation-pending-events");
  await expect(events.locator(".conversation-pending-count")).toHaveText("2");
  await expect(events).not.toHaveAttribute("open", "");
  await expect(page.locator(".conversation-pending-chip")).toHaveCount(1);
  await expect(page.locator(".conversation-pending-chip")).toContainText("Please continue checking");
  const card = page.locator('[data-turn-id="current"]');
  expect((await events.boundingBox())!.y).toBeLessThan((await card.boundingBox())!.y);
  expect((await page.locator(".conversation-pending-chip").boundingBox())!.y).toBeGreaterThan((await card.boundingBox())!.y);
  await events.locator(":scope > summary").click();
  const rows = events.locator(".conversation-pending-event");
  await expect(rows.first()).toContainText("First task finished");
  await expect(rows.last()).toContainText("Second task finished");
  await rows.first().locator("summary").click();
  await expect(rows.first().locator(".conversation-event-link")).toBeVisible();
  expect((await rows.first().locator(".conversation-event-link").boundingBox())!.height).toBeLessThanOrEqual(260);
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)).toBe(false);
  // Turn assignment atomically removes the pending presentation even if an older queue snapshot remains.
  current = { ...current, revision: 2, presentation_class: "task", inputs: pending.slice(0, 2).map(({ message_id, preview }) => ({ message_id, preview })) };
  await update();
  await expect(events).toHaveCount(0);
  await expect(page.locator(".conversation-pending-chip")).toHaveCount(1);
});

test("wake and task JSON open full canonical messages in the inspector", async ({ page, context, request }, info) => {
  const session = `event-inspector-${info.testId}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const preview = JSON.stringify({ type: "text", text: 'wake hint: {"activationId":"truncated...' });
  await request.post(`/__e2e__/conversation?session=${encodeURIComponent(session)}`, { data: { agentId,
    turns: [turn("wake", 1, { presentation_class: "system", inputs: [{ message_id: "wake-message", preview }] }),
      turn("task", 2, { presentation_class: "task", inputs: [{ message_id: "task-message", preview: JSON.stringify({ type: "json", value: { summary: "Task completed", task_id: "t" } }) }] })],
  } });
  const requested: string[] = [];
  await page.route("**/api/agents/bootstrap-agent/messages:batchGet", async (route) => {
    const id = route.request().postDataJSON().message_ids[0]; requested.push(id);
    const body = { type: "json", value: { summary: id === "wake-message" ? "Pull request merged" : "Task completed", result: "Complete event tail: " + "long payload ".repeat(200) + "END OF MESSAGE" } };
    await route.fulfill({ json: { messages: [{ id, agent_id: agentId,
      body: { type: "text", text: "wake hint: truncated..." }, metadata: { wake_hint: { body } },
    }], missing_message_ids: [] } });
  });
  await page.goto(`/agents/${agentId}/conversation`);
  const source = page.locator('[data-turn-id="wake"] .conversation-source');
  await source.locator("summary").click();
  await expect(source).toContainText("Wake notification");
  await expect(source).not.toContainText("activationId");
  expect(requested).toHaveLength(0);
  await source.getByRole("button").click();
  const panel = page.locator(".side-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByRole("heading", { name: "Pull request merged", exact: true })).toBeVisible();
  await expect(panel.locator(".inspector-detail pre")).toContainText("END OF MESSAGE");
  const taskSource = page.locator('[data-turn-id="task"] .conversation-source');
  await taskSource.locator("summary").click();
  await taskSource.getByRole("button").click();
  await expect(panel.getByRole("heading", { name: "Task completed", exact: true })).toBeVisible();
  expect(requested).toEqual(["wake-message", "task-message"]);
  await page.screenshot({ path: "/tmp/holon-event-inspector.png" });
});

test("operator interjections survive admission, folding, refresh and interrupted turns in a system wake", async ({ page, context, request }, info) => {
  const session = `interjections-${info.testId}`;
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const source = { message_id: "wake-input", preview: "Scheduled deployment check", presentation_class: "system" as const };
  const steer = (id: string, seq: number, preview: string) => ({ message_id: id, preview, presentation_class: "operator" as const,
    interjected: true, activity_key: { event_seq: seq, activity_id: `operator:${id}` } });
  const first = steer("steer-a", 2, "Do not restart the service");
  const second = steer("steer-b", 4, "Check the logs first");
  let current = turn("steered", 1, { presentation_class: "system", inputs: [source], started_at: "2026-09-16T00:00:00Z" });
  const progress = [activity(1, "Inspecting configuration."), activity(3, "Checking without a restart."), activity(5, "Reading service logs.")];
  const update = (pending_inputs: unknown[] = []) => request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [current], pending_inputs, activitiesByTurnId: { steered: [...progress,
      { kind: "operator", id: "operator:steer-a", key: first.activity_key, revision: 1, summary: first.preview },
      { kind: "operator", id: "operator:steer-b", key: second.activity_key, revision: 1, summary: second.preview },
    ] },
  } });
  await request.post(control("/__e2e__/configure"), { data: { briefsById: { "steered-result": {
    id: "steered-result", agent_id: agentId, workspace_id: "holon", kind: "result", text: "Checked the logs; no restart performed.",
    created_at: "2026-09-16T00:02:00Z", content_source: { kind: "inline" },
  } } } });
  await update([{ ...first, revision: 1, state: "queued" }]);
  await page.goto(`/agents/${agentId}/conversation`);
  await expect(page.getByText(first.preview, { exact: true })).toBeVisible();
  const live = page.locator('[data-turn-id="steered"]');
  current = { ...current, revision: 2, inputs: [source, first, second] };
  await update();
  await expect(page.locator(".conversation-pending-chip")).toHaveCount(0);
  await expect(page.getByText(first.preview, { exact: true })).toHaveCount(1);
  await expect(live.locator(".conversation-input-line")).toHaveCount(2);
  const order = () => live.locator(".conversation-activities > li").allTextContents();
  await expect.poll(order).toEqual([progress[0].summary, first.preview, progress[1].summary, second.preview, progress[2].summary]);
  await expect(live.locator(".conversation-detail-toggle")).toHaveCount(1);
  await page.reload();
  await expect.poll(order).toEqual([progress[0].summary, first.preview, progress[1].summary, second.preview, progress[2].summary]);
  current = { ...current, revision: 3, execution: { kind: "terminal", outcome: "completed" }, result: { kind: "available" },
    brief_ids: ["steered-result"], settled: true, completed_at: "2026-09-16T00:02:00Z", duration_ms: 120000 };
  await update();
  await expect(live.getByText("Checked the logs; no restart performed.")).toBeVisible();
  await expect(live.locator(".conversation-detail-toggle")).toHaveAttribute("aria-expanded", "false");
  await expect.poll(order).toEqual([first.preview, second.preview]);
  await page.reload();
  await expect.poll(order).toEqual([first.preview, second.preview]);
  await live.locator(".conversation-detail-toggle").click();
  await expect.poll(order).toEqual([progress[0].summary, first.preview, progress[1].summary, second.preview, progress[2].summary]);
  current = { ...current, revision: 4, execution: { kind: "terminal", outcome: "interrupted" }, attention: { kind: "interrupted" } };
  await update();
  await live.locator(".conversation-detail-toggle").click();
  await expect.poll(order).toEqual([first.preview, second.preview]);
  await expect(live.getByText("This turn was stopped")).toBeVisible();
  await page.screenshot({ path: "/tmp/holon-interjections-collapsed.png", fullPage: true });
  await live.locator(".conversation-detail-toggle").click();
  await page.screenshot({ path: "/tmp/holon-interjections-expanded.png", fullPage: true });
});
