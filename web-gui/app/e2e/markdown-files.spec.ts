import { expect, test, type BrowserContext } from "@playwright/test";

const root = "git_worktree_root:ws:/tmp/feature";
const locator = (path: string, executionRoot = root) => ({ workspace_id: "ws", execution_root_id: executionRoot, absolute_path: `/tmp/feature/${path}`, path, kind: path === "docs" ? "directory" : "file", root_kind: "git_worktree_root" });
const targetName = "空 格(1)%20#?.md";
const text = `[Absolute](/tmp/feature/${encodeURIComponent(targetName)}#target)\n\n[Historical](workspace://ws/${encodeURIComponent(targetName)}?root=${root})\n\n[Relative](./${encodeURIComponent(targetName)}#target)\n\n[Directory](/tmp/feature/docs)\n\n[Missing](/tmp/missing.md)\n\n[External](https://example.com)\n\n![Chart](/tmp/feature/image.png)\n\n[Local](#local-section)\n\n## Local section`;
async function mockFiles(context: BrowserContext) {
  const batches: any[][] = [];
  await context.route("**/api/file-references/resolve", async (route) => {
    const refs = route.request().postDataJSON().references;
    batches.push(refs);
    await route.fulfill({ json: { results: refs.map((ref: any) => {
      if (ref.absolute_path === "/tmp/missing.md") return { status: "unresolved", reason: "not_found", message: "File not found" };
      const path = ref.type === "absolute_path" ? ref.absolute_path.slice("/tmp/feature/".length) : ref.type === "relative_path" ? ref.relative_path.replace(/^\.\//, "") : decodeURIComponent(ref.workspace_uri.split("?", 1)[0].slice("workspace://ws/".length));
      return { status: "resolved", location: locator(path) };
    }) } });
  });
  await context.route("**/api/workspaces/ws/files**", async (route) => {
    const url = new URL(route.request().url());
    expect(url.searchParams.get("execution_root_id")).toBe(root);
    const path = decodeURIComponent(url.pathname.split("/files/")[1] ?? "");
    if (path === "image.png") { await route.fulfill({ contentType: "image/png", body: Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=", "base64") }); return; }
    const content = path === "base.md" ? text : "# Target\n\nWorktree contents";
    await route.fulfill({ json: path && path !== "docs" ? { ...locator(path), type: "file", mime_type: "text/markdown", content, size: content.length } : {
      ...locator(path), kind: "directory", type: "directory", entries: ["base.md", targetName].map((name) => ({ name, type: "file", size: 100, mime_type: "text/markdown" })),
    } });
  });
  return batches;
}
const preview = (path: string) => `/files?${new URLSearchParams({ workspace: "ws", root, path })}`;

test("Explorer shares resolver rules; native new-tab links, fragments, images and refresh preserve the root", async ({ page, context }) => {
  const batches = await mockFiles(context);
  await page.goto(preview("base.md"));
  const content = page.locator(".file-browser-markdown");
  await expect(content.getByRole("link", { name: "Absolute", exact: true })).toHaveAttribute("href", preview(targetName) + "#target");
  await expect(content.getByRole("img", { name: "Chart" })).toHaveAttribute("src", /^blob:/);
  await expect(content.getByText("File not found", { exact: false })).toBeVisible();
  expect(batches.flat().find((ref) => ref.type === "relative_path").base_file.execution_root_id).toBe(root);
  await content.getByRole("link", { name: "Local", exact: true }).click();
  await expect(content.getByRole("heading", { name: "Local section" })).toBeInViewport();
  const popupEvent = context.waitForEvent("page");
  await content.getByRole("link", { name: "Absolute", exact: true }).click({ modifiers: ["ControlOrMeta"] });
  const popup = await popupEvent;
  await expect(popup.locator(".file-browser-markdown")).toContainText("Worktree contents");
  await popup.reload();
  await expect(popup.locator(".file-browser-markdown")).toContainText("Worktree contents");
  await popup.close();
  const middleEvent = context.waitForEvent("page");
  await content.getByRole("link", { name: "Historical", exact: true }).click({ button: "middle" });
  const middle = await middleEvent;
  await expect(middle.locator(".file-browser-markdown")).toContainText("Worktree contents");
  await middle.close();
  await content.getByRole("link", { name: "Relative", exact: true }).click();
  await expect(content).toContainText("Worktree contents");
  expect(new URL(page.url()).searchParams.get("root")).toBe(root);
  await page.goto(preview(targetName) + "#missing");
  await expect(content.getByRole("status")).toContainText("was not found");
  await page.goto(preview("base.md"));
  await content.getByRole("link", { name: "Directory", exact: true }).click();
  await expect(page.locator(".file-browser")).toContainText("base.md");
  await page.getByRole("button", { name: targetName, exact: false }).click();
  await expect(content).toContainText("Worktree contents");
  expect(new URL(page.url()).searchParams.get("path")).toBe(`docs/${targetName}`);
  await page.reload();
  await expect(content).toContainText("Worktree contents");
});

test("brief and live assistant keep the same absolute references; relative references never guess an agent base", async ({ page, context, request }, info) => {
  const batches = await mockFiles(context);
  const session = `file-refs-${info.testId}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  await request.post(control("/__e2e__/configure"), { data: { visibleAgentIds: ["bootstrap-agent", "other-agent"], briefsById: { brief: {
    id: "brief", agent_id: "bootstrap-agent", workspace_id: "ws", kind: "result", text, created_at: "2026-09-17T01:00:00Z", content_source: { kind: "inline" },
  } } } });
  const turn = (id: string, index: number) => ({ turn_id: id, key: { turn_id: id, turn_index: index }, revision: 1, presentation_class: "operator", inputs: [{ message_id: id, preview: id }], execution: { kind: "active" }, result: { kind: "pending" }, settled: false, attention: null, detail_coverage: { kind: "complete" }, brief_ids: [] });
  await request.post(control("/__e2e__/conversation"), { data: { agentId: "bootstrap-agent", turns: [
    { ...turn("done", 1), execution: { kind: "terminal", outcome: "completed" }, result: { kind: "available" }, settled: true, brief_ids: ["brief"] }, turn("live", 2),
  ], activitiesByTurnId: { live: [{ kind: "assistant", id: "assistant:1", key: { event_seq: 1, activity_id: "assistant:1" }, revision: 1, summary: text }] } } });
  await page.goto("/agents/bootstrap-agent/conversation");
  for (const id of ["done", "live"]) {
    const block = page.locator(`[data-turn-id="${id}"]`);
    await expect(block.getByRole("link", { name: "Absolute", exact: true })).toHaveAttribute("href", preview(targetName) + "#target");
    await expect(block.getByText("Missing file location context", { exact: false })).toBeVisible();
  }
  expect(batches.flat().some((ref) => ref.type === "relative_path")).toBe(false);
  await page.locator(".sidebar").getByRole("button", { name: /other-agent posture:/ }).click();
  await page.locator(".sidebar").getByRole("button", { name: /bootstrap-agent posture:/ }).click();
  await expect(page.locator('[data-turn-id="done"]').getByRole("link", { name: "Historical", exact: true })).toHaveAttribute("href", preview(targetName));
  await page.locator('[data-turn-id="done"]').getByRole("link", { name: "Historical", exact: true }).click();
  await expect(page.locator(".side-panel .file-browser-markdown")).toContainText("Worktree contents");
});


test("stream additions resolve only new references and late old-content replies cannot replace them", async ({ page, context, request }, info) => {
  const batches = await mockFiles(context);
  const session = `stream-files-${info.testId}`;
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  let revision = 0;
  const update = async (summary: string) => request.post(control("/__e2e__/conversation"), { data: { agentId: "bootstrap-agent", turns: [{
    turn_id: "live", key: { turn_id: "live", turn_index: 1 }, revision: ++revision, presentation_class: "operator", inputs: [], execution: { kind: "active" }, result: { kind: "pending" }, settled: false, attention: null, detail_coverage: { kind: "complete" }, brief_ids: [],
  }], activitiesByTurnId: { live: [{ kind: "assistant", id: "assistant:1", key: { event_seq: 1, activity_id: "assistant:1" }, revision, summary }] } } });
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  let waiting = false;
  await context.route("**/api/file-references/resolve", async (route) => {
    if (route.request().postDataJSON().references.some((ref: any) => ref.absolute_path === "/tmp/feature/old.md")) {
      waiting = true; await gate;
      await route.fulfill({ json: { results: [{ status: "resolved", location: locator("old.md") }] } });
    } else await route.fallback();
  });
  try {
    await update("[Old](/tmp/feature/old.md)");
    await page.goto("/agents/bootstrap-agent/conversation");
    await expect.poll(() => waiting).toBe(true);
    await update("[Current](/tmp/feature/current.md)");
    const block = page.locator('[data-turn-id="live"]');
    await expect(block.getByRole("link", { name: "Current", exact: true })).toHaveAttribute("href", preview("current.md"));
    release();
    await update("[Current](/tmp/feature/current.md) [Next](/tmp/feature/next.md)");
    await expect(block.getByRole("link", { name: "Next", exact: true })).toHaveAttribute("href", preview("next.md"));
    await expect(block.getByRole("link", { name: "Old", exact: true })).toHaveCount(0);
    expect(batches.flat().filter((ref) => ref.absolute_path === "/tmp/feature/current.md")).toHaveLength(1);
  } finally { release(); }
});
