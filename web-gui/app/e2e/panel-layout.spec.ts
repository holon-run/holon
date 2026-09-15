import { expect, test } from "@playwright/test";

const discoveryGates = new Map<string, () => void>();
test.afterEach(async ({}, info) => {
  discoveryGates.get(info.testId)?.();
  discoveryGates.delete(info.testId);
});

test.beforeEach(async ({ page, context }, info) => {
  await context.addCookies([{ name: "holon_e2e_session", value: `panel-${info.testId}`, domain: "127.0.0.1", path: "/" }]);
  await page.route("**/api/agents/list", async (route) => {
    const response = await route.fetch();
    const agents = await response.json();
    for (const entry of agents) entry.active_workspace_entry = {
      workspace_id: "files-test", workspace_alias: "Test files", workspace_anchor: "/test", execution_root_id: "root-test",
    };
    await route.fulfill({ json: agents });
  });
  await page.route("**/api/agents/snapshot", async (route) => {
    const response = await route.fetch();
    const roster = await response.json();
    for (const entry of roster.agents) entry.agent.active_workspace_entry = {
      workspace_id: "files-test", workspace_alias: "Test files", workspace_anchor: "/test", execution_root_id: "root-test",
    };
    if (info.title.startsWith("late runtime")) await new Promise<void>((resolve) => discoveryGates.set(info.testId, resolve));
    await route.fulfill({ json: roster });
  });
  await page.route("**/api/agents/bootstrap-agent/state", async (route) => {
    const response = await route.fetch();
    const state = await response.json();
    state.workspace.workspaces = [{ workspace_id: "files-test", workspace_alias: "Test files", workspace_anchor: "/test", execution_root_id: "root-test", is_active: true }];
    await route.fulfill({ json: state });
  });
  await page.route("**/api/workspaces/files-test/files**", async (route) => {
    const path = new URL(route.request().url()).pathname.split("/files/")[1] ?? "";
    await route.fulfill({ json: path ? {
      type: "file", workspace_id: "files-test", path, mime_type: "text/markdown", size: 20000,
      content: "# File reading test\n\n[Open notes](notes.md)\n\n" + Array.from({ length: 150 }, (_, i) => `Paragraph ${i}: readable content for scroll restoration.\n\n`).join(""),
    } : {
      type: "directory", workspace_id: "files-test", path: "", entries: [
        { name: "README.md", type: "file", size: 20000, mime_type: "text/markdown" },
        { name: "notes.md", type: "file", size: 20000, mime_type: "text/markdown" },
      ],
    } });
  });
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto("/agents/bootstrap-agent/conversation");
  await page.getByRole("button", { name: "Context side panel", exact: true }).click();
});

test("late runtime discovery adopts the open file without remounting its browser", async ({ page }, info) => {
  const panel = page.locator(".side-panel");
  await panel.locator(".panel-sections").getByRole("button", { name: "Files", exact: true }).click();
  await panel.getByRole("button", { name: "README.md", exact: false }).click();
  await expect(panel.locator(".file-browser-markdown")).toBeVisible();
  const discovery = page.waitForResponse("**/api/agents/snapshot");
  discoveryGates.get(info.testId)!();
  await discovery;
  await panel.getByRole("button", { name: "Expand side panel", exact: true }).click();
  await expect(panel.locator(".file-browser")).toHaveAttribute("data-split", "true");
  await expect(panel.locator(".file-browser-viewer-head > strong")).toHaveText("README.md");
});

test("inspector never partially covers the conversation across viewport widths", async ({ page }) => {
  const shell = page.locator(".app-shell");
  const panel = page.locator(".side-panel");
  for (const width of [1600, 1440, 1280, 1024, 768, 390]) {
    await page.setViewportSize({ width, height: 1000 });
    await expect.poll(() => panel.evaluate((node) => node.getBoundingClientRect().right)).toBe(width);
    if (width < 1032) {
      await expect(shell).toHaveAttribute("data-panel-full", "true");
      await expect(page.locator(".main-shell")).toHaveAttribute("inert", "");
      await expect(panel.getByRole("button", { name: "Back to conversation" })).toBeVisible();
    } else {
      await expect(shell).toHaveAttribute("data-panel-full", "false");
      const main = (await page.locator(".main-shell").boundingBox())!;
      const detail = (await panel.boundingBox())!;
      expect(main.width).toBeGreaterThanOrEqual(640);
      expect(main.x + main.width).toBeLessThanOrEqual(detail.x + 1);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  }
  await panel.getByRole("button", { name: "Back to conversation" }).click();
  await expect(page.locator(".composer")).toBeVisible();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await expect(shell).toHaveAttribute("data-nav-collapsed", "false");
  await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  await panel.getByRole("separator").focus();
  await page.keyboard.press("ArrowLeft");
  await expect.poll(async () => (await panel.boundingBox())!.width).toBe(412);
  await panel.getByRole("button", { name: "Expand side panel", exact: true }).click();
  await page.keyboard.press("Escape");
  await expect.poll(async () => (await panel.boundingBox())!.width).toBe(412);
  await page.setViewportSize({ width: 1024, height: 1000 });
  await expect(shell).toHaveAttribute("data-panel-full", "true");
  await page.setViewportSize({ width: 1600, height: 1000 });
  await expect.poll(async () => (await panel.boundingBox())!.width).toBe(412);
});

test("files keep selection, preview mode and scroll through overview, maximize and restore", async ({ page }) => {
  const panel = page.locator(".side-panel");
  const files = panel.locator(".panel-sections").getByRole("button", { name: "Files", exact: true });
  await expect(files).toBeEnabled();
  await files.click();
  await panel.getByRole("button", { name: "README.md", exact: false }).click();
  const content = panel.locator(".file-browser-markdown");
  await expect(content.getByRole("heading", { name: "File reading test" })).toBeVisible();
  await content.getByRole("link", { name: "Open notes" }).click();
  await expect(panel.locator(".file-browser-viewer-head > strong")).toHaveText("notes.md");
  await panel.locator(".file-browser").getByRole("button", { name: "Back to source", exact: true }).click();
  await expect(panel.locator(".file-browser-viewer-head > strong")).toHaveText("README.md");
  await content.evaluate((node) => { node.scrollTop = 600; });
  await expect.poll(() => content.evaluate((node) => node.scrollTop)).toBe(600);
  await panel.locator(".panel-sections").getByRole("button", { name: "Overview", exact: true }).click();
  await files.click();
  await expect.poll(() => content.evaluate((node) => node.scrollTop)).toBe(600);
  await panel.getByRole("button", { name: "Expand side panel", exact: true }).click();
  await expect(panel.locator(".file-browser")).toHaveAttribute("data-split", "true");
  await expect(panel.getByRole("button", { name: "notes.md", exact: false })).toBeVisible();
  await panel.getByRole("button", { name: "Source", exact: true }).click();
  const source = panel.locator(".file-browser-code");
  await source.evaluate((node) => { node.scrollTop = 700; });
  await panel.locator(".panel-sections").getByRole("button", { name: "Overview", exact: true }).click();
  await files.click();
  await expect(source).toBeVisible();
  await expect.poll(() => source.evaluate((node) => node.scrollTop)).toBe(700);
  await page.keyboard.press("Escape");
  await expect(panel).toHaveAttribute("data-mode", "normal");
  await expect(panel.locator(".file-browser")).toHaveAttribute("data-split", "false");
  expect((await panel.boundingBox())!.width).toBe(380);
  await page.keyboard.press("Escape");
  await expect(panel).not.toBeVisible();
  await expect(page.getByRole("button", { name: "Context side panel", exact: true })).toBeFocused();
});

test("reading a maximized file keeps the live turn updating and returns to its tool detail", async ({ page, request }, info) => {
  const control = (path: string) => `${path}?session=${encodeURIComponent(`panel-${info.testId}`)}`;
  const panel = page.locator(".side-panel");
  const turn = { turn_id: "live-panel", key: { turn_id: "live-panel", turn_index: 1 }, revision: 1,
    presentation_class: "operator", inputs: [{ message_id: "input-panel", preview: "Check files" }],
    execution: { kind: "active" }, result: { kind: "pending" }, settled: false,
    attention: null, detail_coverage: { kind: "complete" }, brief_ids: [] as string[] };
  const activities = [{ kind: "tool", id: "tool:panel-command", key: { event_seq: 1, activity_id: "tool:panel-command" }, revision: 1, summary: "ExecCommand · success" }];
  await request.post(control("/__e2e__/configure"), { data: {
    toolExecutionsById: { "panel-command": { id: "panel-command", tool_name: "ExecCommand", status: "success", input: { cmd: "cat README.md" }, output: { stdout: "Ready", exit_status: 0 } } },
    briefsById: { "panel-result": { id: "panel-result", agent_id: "bootstrap-agent", workspace_id: "files-test", kind: "result", text: "Finished while you read.", created_at: "2026-09-16T01:00:00Z", content_source: { kind: "inline" } } },
  } });
  await request.post(control("/__e2e__/conversation"), { data: { agentId: "bootstrap-agent", turns: [turn], activitiesByTurnId: { "live-panel": activities } } });
  await page.locator('[data-activity-id="tool:panel-command"] button').click();
  await expect(panel.getByText("Ready", { exact: true })).toBeVisible();
  await panel.locator(".panel-sections").getByRole("button", { name: "Files", exact: true }).click();
  await panel.getByRole("button", { name: "README.md", exact: false }).click();
  await expect(panel.locator(".file-browser-markdown")).toBeVisible();
  await panel.getByRole("button", { name: "Expand side panel", exact: true }).click();
  await request.post(control("/__e2e__/conversation"), { data: { agentId: "bootstrap-agent", turns: [{ ...turn, revision: 2, execution: { kind: "terminal", outcome: "completed" }, result: { kind: "available" }, brief_ids: ["panel-result"], settled: true }], activitiesByTurnId: { "live-panel": activities } } });
  await expect(page.locator(".conversation-brief")).toContainText("Finished while you read.");
  await expect(panel.locator(".file-browser-viewer-head > strong")).toHaveText("README.md");
  await panel.locator(".file-browser").getByRole("button", { name: "Back to source", exact: true }).click();
  await expect(panel.getByText("Ready", { exact: true })).toBeVisible();
  await expect(panel).toHaveAttribute("data-mode", "expanded");
  await panel.getByRole("button", { name: "Back to conversation" }).click();
  await expect(page.getByText("Finished while you read.", { exact: true })).toBeVisible();
  await expect(page.locator(".conversation-detail-toggle")).toHaveAttribute("aria-expanded", "false");
});
