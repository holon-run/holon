import { expect, test } from "@playwright/test";
import { sessionFor } from "./test-session";

const discoveryGates = new Map<string, () => void>();
test.afterEach(async ({}, info) => {
  discoveryGates.get(info.testId)?.();
  discoveryGates.delete(info.testId);
});

test.beforeEach(async ({ page, context }, info) => {
  await context.addCookies([{ name: "holon_e2e_session", value: sessionFor(info, "panel"), domain: "127.0.0.1", path: "/" }]);
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
  await page.route("**/api/file-references/resolve", async (route) => {
    const { references } = route.request().postDataJSON();
    await route.fulfill({ json: { results: references.map((ref: any) => ({ status: "resolved", location: {
      workspace_id: "files-test", execution_root_id: "root-test", path: ref.relative_path,
      absolute_path: `/test-worktree/${ref.relative_path}`, kind: "file", root_kind: "git_worktree_root",
    } })) } });
  });
  await page.route("**/api/workspaces/files-test/files**", async (route) => {
    const path = new URL(route.request().url()).pathname.split("/files/")[1] ?? "";
    await route.fulfill({ json: path ? {
      type: "file", workspace_id: "files-test", execution_root_id: "root-test",
      absolute_path: `/test-worktree/${path}`, root_kind: "git_worktree_root", kind: "file",
      path, mime_type: "text/markdown", size: 20000,
      content: "# File reading test\n\n[Open notes](notes.md)\n\n" + Array.from({ length: 150 }, (_, i) => `Paragraph ${i}: readable content for scroll restoration.\n\n`).join(""),
    } : {
      type: "directory", workspace_id: "files-test", execution_root_id: "root-test",
      absolute_path: "/test-worktree", root_kind: "git_worktree_root", kind: "directory",
      path: "", entries: [
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
  await expect(panel.locator(".file-browser-toolbar")).toContainText("Worktree");
  await panel.getByRole("button", { name: "File information", exact: true }).click();
  await expect(panel.getByRole("region", { name: "File information" })).toContainText("root-test");
  await expect(panel.getByRole("region", { name: "File information" })).toContainText("/test-worktree/README.md");
  await panel.getByRole("button", { name: "File information", exact: true }).click();
  const discovery = page.waitForResponse("**/api/agents/snapshot");
  discoveryGates.get(info.testId)!();
  await discovery;
  await panel.getByRole("button", { name: "Expand side panel", exact: true }).click();
  await expect(panel.locator(".file-browser")).toHaveAttribute("data-split", "true");
  await expect(panel.locator(".panel-header strong")).toHaveText("README.md");
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
      // Resize transitions can still be settling after the panel reaches the
      // viewport edge; poll the geometry instead of sampling it once.
      await expect.poll(async () => {
        const main = await page.locator(".main-shell").boundingBox();
        const detail = await panel.boundingBox();
        if (!main || !detail) return false;
        return main.width >= 640 && main.x + main.width <= detail.x + 1;
      }).toBe(true);
    }
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
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
  await expect(panel.locator(".panel-header strong")).toHaveText("notes.md");
  await panel.locator(".panel-header").getByRole("button", { name: "Back to source", exact: true }).click();
  await expect(panel.locator(".panel-header strong")).toHaveText("README.md");
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
  const session = sessionFor(info, "panel");
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  const panel = page.locator(".side-panel");
  const turn = { turn_id: "live-panel", key: { turn_id: "live-panel", turn_index: 1 }, revision: 1,
    presentation_class: "operator", inputs: [{ message_id: "input-panel", preview: "Check files" }],
    execution: { kind: "active" }, result: { kind: "pending" }, settled: false,
    attention: null, detail_coverage: { kind: "complete" }, brief_ids: [] as string[] };
  const activities = ["panel-command", "second-command"].map((id, index) => ({ kind: "tool", id: `tool:${id}`, key: { event_seq: index + 1, activity_id: `tool:${id}` }, revision: 1, summary: "ExecCommand · success" }));
  await request.post(control("/__e2e__/configure"), { data: {
    toolExecutionsById: {
      "panel-command": { id: "panel-command", tool_name: "ExecCommand", status: "success", input: { cmd: "cat README.md" }, output: { stdout: "Ready", exit_status: 0 } },
      "second-command": { id: "second-command", tool_name: "ExecCommand", status: "success", input: { cmd: "echo second" }, output: { stdout: "Second output", exit_status: 0 } },
    },
    briefsById: { "panel-result": { id: "panel-result", agent_id: "bootstrap-agent", workspace_id: "files-test", kind: "result", text: "Finished while you read.", created_at: "2026-09-16T01:00:00Z", content_source: { kind: "inline" } } },
  } });
  await request.post(control("/__e2e__/conversation"), { data: { agentId: "bootstrap-agent", turns: [turn], activitiesByTurnId: { "live-panel": activities } } });
  await page.locator('[data-activity-id="tool:panel-command"] button').click();
  await expect(panel.getByText("Ready", { exact: true })).toBeVisible();
  await expect(panel.locator(".detail-meta .state-chip")).toHaveText("Completed");
  await panel.locator(".inspector-raw-detail > summary").click();
  await page.locator('[data-activity-id="tool:second-command"] button').click();
  await expect(panel.getByText("Second output", { exact: true })).toBeVisible();
  await expect(panel.locator(".inspector-raw-detail")).not.toHaveAttribute("open", "");
  await page.locator('[data-activity-id="tool:panel-command"] button').click();
  await expect(panel.locator(".inspector-raw-detail")).toHaveAttribute("open", "");
  await panel.locator(".panel-sections").getByRole("button", { name: "Files", exact: true }).click();
  await panel.getByRole("button", { name: "README.md", exact: false }).click();
  await expect(panel.locator(".file-browser-markdown")).toBeVisible();
  await panel.getByRole("button", { name: "Expand side panel", exact: true }).click();
  await request.post(control("/__e2e__/conversation"), { data: { agentId: "bootstrap-agent", turns: [{ ...turn, revision: 2, execution: { kind: "terminal", outcome: "completed" }, result: { kind: "available" }, brief_ids: ["panel-result"], settled: true }], activitiesByTurnId: { "live-panel": activities } } });
  await expect(page.locator(".conversation-brief")).toContainText("Finished while you read.");
  await expect(panel.locator(".panel-header strong")).toHaveText("README.md");
  await panel.locator(".panel-header").getByRole("button", { name: "Back to source", exact: true }).click();
  await expect(panel.getByText("Ready", { exact: true })).toBeVisible();
  await expect(panel).toHaveAttribute("data-mode", "expanded");
  await panel.getByRole("button", { name: "Back to conversation" }).click();
  await expect(page.getByText("Finished while you read.", { exact: true })).toBeVisible();
  await expect(page.locator(".conversation-detail-toggle")).toHaveAttribute("aria-expanded", "false");
});

test("work item details prioritize results and keep technical fields and plan navigation available", async ({ page, request }, info) => {
  const session = sessionFor(info, "panel");
  const work = { id: "work-detail-style", objective: "Review the release checklist", state: "open", readiness: "ready",
    revision: 7, updated_at: "2026-09-16T01:00:00Z", result_summary: "**Checks passed**\n\n- Build complete\n- Tests complete",
    plan_artifact: { path: "/test/README.md", relative_path: "README.md", workspace_id: "files-test", preview: "## Release plan\n\n- Verify artifacts", preview_complete: true },
    todo_list: [{ text: "Verify artifacts", state: "completed" }], work_refs: [] };
  await page.route("**/api/agents/bootstrap-agent/work-items**", async (route) => {
    await route.fulfill({ json: new URL(route.request().url()).pathname.endsWith(work.id) ? work : [work] });
  });
  await request.post(`/__e2e__/append-event?session=${encodeURIComponent(session)}`, { data: { envelope: {
    id: "work-written", event_seq: 1, event_log_epoch: "e2e-epoch", contract_version: 2,
    ts: "2026-09-16T01:00:00Z", agent_id: "bootstrap-agent", type: "work_item_written",
    payload_schema: "holon.runtime_event.work_item_written", payload_schema_version: 1,
    payload: { work_item: { id: work.id, objective: work.objective, state: work.state, revision: work.revision }, work_item_id: work.id },
  } } });
  await page.reload();
  const panel = page.locator(".side-panel");
  await expect(page.locator(".app-shell")).toBeVisible();
  if (!await panel.isVisible()) await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  await panel.locator(".work-item-button").filter({ hasText: work.objective }).click();
  const detail = panel.locator(".work-item-detail");
  await expect(detail.locator(".detail-title")).toHaveText(work.objective);
  await expect(detail.locator("strong").filter({ hasText: "Checks passed" })).toBeVisible();
  await expect(detail.getByRole("heading", { name: "Release plan" })).toBeVisible();
  await expect(detail.locator(".detail-technical")).not.toHaveAttribute("open", "");
  await detail.getByText("Technical details", { exact: true }).click();
  await expect(detail.getByText(work.id, { exact: true })).toBeVisible();
  await panel.getByRole("button", { name: "Expand side panel", exact: true }).click();
  await expect(detail.getByText(work.id, { exact: true })).toBeVisible();
  expect((await detail.boundingBox())!.width).toBeLessThanOrEqual(1040);
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await detail.locator(".workspace-path-link").click();
  await expect(panel.locator(".panel-header strong")).toHaveText("README.md");
});

test("panel open mode and selected file survive reload while closed stays closed", async ({ page }) => {
  const panel = page.locator(".side-panel");
  await panel.locator(".panel-sections").getByRole("button", { name: "Files", exact: true }).click();
  await panel.getByRole("button", { name: "README.md", exact: false }).click();
  await expect(panel.locator(".panel-header strong")).toHaveText("README.md");
  await panel.getByRole("button", { name: "Expand side panel", exact: true }).click();
  await page.reload();
  await expect(panel).toBeVisible();
  await expect(panel).toHaveAttribute("data-mode", "expanded");
  await expect(panel.locator(".panel-header strong")).toHaveText("README.md");
  await expect(panel.locator(".file-browser-markdown")).toContainText("File reading test");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await expect(panel).not.toBeVisible();
  await page.reload();
  await expect(panel).not.toBeVisible();
});

for (const enabled of [false, true]) {
  test(`file actions preserve locator and desktop availability (${enabled})`, async ({ page }, info) => {
    await page.route("**/api/desktop/capabilities", (route) => route.fulfill({ json: { reveal_in_finder: enabled } }));
    let reveals: unknown[] = [];
    await page.route("**/api/desktop/reveal", async (route) => {
      reveals.push(route.request().postDataJSON());
      await route.fulfill({ status: 403, json: { error: "Desktop integration was disabled" } });
    });
    const panel = page.locator(".side-panel");
    await panel.locator(".panel-sections").getByRole("button", { name: "Files", exact: true }).click();
    await panel.getByRole("button", { name: "README.md", exact: false }).click();
    await expect(panel.locator(".panel-header strong")).toHaveText("README.md");
    await expect(panel.locator(".file-browser-meta-bar")).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Back to source", exact: true })).toHaveCount(1);
    const content = panel.locator(".file-browser-markdown");
    const panelBox = (await panel.boundingBox())!;
    expect((await content.boundingBox())!.y - panelBox.y).toBeLessThan(230);
    await content.evaluate((node) => { node.scrollTop = 400; });
    await panel.getByRole("button", { name: "Browse folder", exact: true }).click();
    await expect(panel.getByRole("button", { name: "README.md", exact: false })).toHaveAttribute("data-selected", "true");
    await panel.getByRole("button", { name: "Back to preview", exact: true }).click();
    await expect.poll(() => content.evaluate((node) => node.scrollTop)).toBe(400);
    await panel.getByRole("button", { name: "File actions", exact: true }).click();
    if (enabled) {
      await panel.getByRole("button", { name: "Show in Finder", exact: true }).click();
      await expect(panel.getByRole("alert")).toContainText("Desktop integration was disabled");
      expect(reveals).toEqual([{ workspace_id: "files-test", execution_root_id: "root-test", path: "README.md" }]);
    } else {
      await expect(panel.getByRole("button", { name: "Show in Finder", exact: true })).toHaveCount(0);
    }
    await page.keyboard.press("Escape");
    await expect(panel).toBeVisible();
    await expect(panel.getByRole("button", { name: "File actions", exact: true })).toBeFocused();
    await content.evaluate((node) => { node.scrollTop = 0; });
    await panel.screenshot({ path: info.outputPath(`file-browser-${enabled}.png`) });
    if (!enabled) {
      await page.evaluate(() => localStorage.setItem("holon.webGui.languageMode.v1", "zh-CN"));
      await page.reload();
      await expect(panel.locator(".file-browser-markdown")).toBeVisible();
      await expect(panel.getByRole("button", { name: "预览", exact: true })).toBeVisible();
      await panel.screenshot({ path: info.outputPath("file-browser-zh-CN.png") });
    }
  });
}

for (const language of ["en", "zh-CN"]) {
  test(`long file locations stay compact (${language})`, async ({ page }, info) => {
    const directory = "work-items/work_very_long_identifier_1234567890abcdefghijklmnop";
    const path = `${directory}/plan.md`;
    await page.evaluate((language) => localStorage.setItem("holon.webGui.languageMode.v1", language), language);
    await page.route("**/api/workspaces/files-test/files**", async (route) => {
      const requestedPath = decodeURIComponent(new URL(route.request().url()).pathname.split("/files/")[1] ?? "");
      const file = requestedPath.endsWith(".md");
      await route.fulfill({ json: { type: file ? "file" : "directory", kind: file ? "file" : "directory", workspace_id: "files-test", execution_root_id: "root-test", root_kind: "canonical_root", path: requestedPath,
        absolute_path: `/Users/demo/.holon/agents/holon-test/${requestedPath}`, mime_type: "text/markdown", size: 100,
        content: "# 实施方案 / Implementation plan\n\n文件内容优先展示。Read the file without navigating past controls.\n\n- Review the plan\n- Run checks",
        entries: [{ name: "plan.md", type: "file", size: 100, mime_type: "text/markdown" }] } });
    });
    await page.goto(`/files?${new URLSearchParams({ workspace: "files-test", root: "root-test", path })}`);
    const browser = page.locator(".file-browser");
    const infoLabel = language === "en" ? "File information" : "文件信息";
    const parentLabel = language === "en" ? "Parent folders" : "上层目录";
    for (const width of [1200, 390, 320]) {
      await page.setViewportSize({ width, height: 900 });
      await expect(browser.locator(".file-browser-markdown")).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      expect(await browser.locator(".file-browser-controls").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
      expect((await browser.locator(".file-browser-toolbar").boundingBox())!.height).toBeLessThan(45);
      expect((await browser.locator(".file-browser-markdown").boundingBox())!.y).toBeLessThan(230);
      await browser.getByRole("button", { name: parentLabel, exact: true }).click();
      await expect(browser.locator(".file-browser-ancestors").getByRole("button", { name: "work-items", exact: true })).toBeVisible();
      await page.keyboard.press("Escape");
      await browser.getByRole("button", { name: infoLabel, exact: true }).click();
      await expect(browser.getByRole("region", { name: infoLabel })).toContainText(path);
      await browser.getByRole("button", { name: infoLabel, exact: true }).click();
      await page.screenshot({ path: info.outputPath(`file-long-${language}-${width}.png`) });
    }
  });
}
