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
  current = { ...current, revision: 3, result: { kind: "available" }, settled: true, brief_ids: ["current-brief"] };
  await update();
  await expect(live.getByText("The layout is ready.")).toBeVisible();
  await expect(disclosure).toHaveAttribute("aria-expanded", "false");

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
  await update([activity(1, "Starting the live check."), tool]);
  await expect(page.getByRole("button", { name: "ExecCommand · success", exact: true })).toBeVisible();
  await expect(page.locator('[data-turn-id="live-detail"] .conversation-brief')).toHaveCount(0);
  await page.getByRole("button", { name: "ExecCommand · success", exact: true }).click();
  await expect(page.locator(".side-panel")).toBeVisible();
  await expect(page.locator(".side-panel").getByText("printf live-marker", { exact: true })).toBeVisible();
  await expect(page.locator(".side-panel").getByText("live-marker", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Close side panel", exact: true }).click();
  await page.getByText("Starting the live check.", { exact: true }).click();
  await expect(page.locator(".side-panel").getByText("Starting the live check.", { exact: true }).first()).toBeVisible();
});
