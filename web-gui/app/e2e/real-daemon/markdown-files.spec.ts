import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { expect, test } from "./daemon-fixture";

test("Markdown App URLs open real authenticated static apps without file resolution", async ({ daemonFactory, page, context }) => {
  test.setTimeout(60_000);
  const daemon = await daemonFactory({ webDist: "dist" });
  const workspace = `agent_home:${daemon.agentId}`;
  const info = await daemon.api(`/workspaces/${workspace}/files`).then((response) => response.json());
  const appId = "markdown-links";
  const appDir = path.join(info.absolute_path, "apps", appId);
  await mkdir(appDir, { recursive: true });
  await writeFile(path.join(appDir, "manifest.json"), JSON.stringify({ id: appId, name: "Markdown links", version: "1", entry: "index.html" }));
  await writeFile(path.join(appDir, "index.html"), "<!doctype html><title>App navigation fixture</title><h1>Real App destination</h1>");
  const app = `/apps/${daemon.agentId}/${appId}/?mode=audition#sample`;
  await writeFile(path.join(info.absolute_path, "app-links.md"), `[Open App](${app} "Audition")\n\n[Reference][app]\n\n[app]: ${app}\n\n${app}，结束。\n\n\`${app}\`\n\n\`\`\`\n${app}\n\`\`\``);
  const resolution = await daemon.api("/file-references/resolve", { method: "POST", body: JSON.stringify({ references: [{ type: "workspace_uri", workspace_uri: `workspace://${workspace}/app-links.md` }] }) }).then((response) => response.json());
  expect(resolution.results[0].status).toBe("resolved");
  const root = resolution.results[0].location.execution_root_id;
  let resolverRequests = 0;
  let appRequests = 0;
  page.on("request", (request) => {
    if (new URL(request.url()).pathname === "/api/file-references/resolve") resolverRequests++;
    if (new URL(request.url()).pathname.startsWith("/apps/")) appRequests++;
  });
  const gui = `${daemon.baseUrl}/files?${new URLSearchParams({ workspace, root, path: "app-links.md" })}`;
  await page.goto(gui);
  await page.locator("#login-token").fill(daemon.token);
  await page.getByRole("button", { name: /Sign in|Connect/i }).click();
  const content = page.locator(".file-browser-markdown");
  await expect(content.getByRole("link", { name: "Open App", exact: true })).toHaveAttribute("href", app);
  await expect(content.getByRole("link")).toHaveCount(3);
  await expect(content.locator("code")).toHaveCount(2);
  expect(resolverRequests).toBe(0);
  expect(appRequests).toBe(0);
  await expect(content.locator("iframe")).toHaveCount(0);

  // Cookie authentication in a new context tab; no token in the URL.
  const popupPromise = context.waitForEvent("page");
  await content.getByRole("link", { name: "Open App", exact: true }).click();
  const popup = await popupPromise;
  try {
    await expect(popup.getByRole("heading", { name: "Real App destination" })).toBeVisible();
    expect(popup.url()).toBe(`${daemon.baseUrl}${app}`);
    expect(await popup.evaluate(() => window.opener === null)).toBe(true);
    expect(page.url()).toBe(gui);
  } finally { await popup.close(); }
  const anonymous = await fetch(`${daemon.baseUrl}${app}`, { redirect: "manual" });
  expect(anonymous.status).toBe(401);
});

// Seed only this fixture's registry: there is intentionally no public root-registration API.
function registerRoot(home: string, entry: Record<string, unknown>) {
  execFileSync("python3", ["-c", `import sqlite3,json,sys
entry=json.loads(sys.argv[2])
with sqlite3.connect(sys.argv[1]) as db:
 db.execute("INSERT OR REPLACE INTO execution_root_entries (execution_root_id,workspace_id,filesystem_path,root_kind,created_at,removed_at,payload_json) VALUES (?,?,?,?,?,?,?)", [entry[k] for k in ['execution_root_id','workspace_id','filesystem_path','root_kind','created_at','removed_at']] + [json.dumps(entry)])
`, path.join(home, "state", "runtime.sqlite"), JSON.stringify(entry)]);
}

test("real roots, literal filenames, authenticated images/download, login restoration and tombstones", async ({ daemonFactory, page, browser }) => {
  test.setTimeout(90_000);
  const daemon = await daemonFactory({ webDist: "dist" });
  const canonical = path.join(daemon.home, "test-canonical");
  const worktree = path.join(daemon.home, "test-worktree");
  await mkdir(canonical);
  execFileSync("git", ["init", canonical]);
  execFileSync("git", ["-C", canonical, "-c", "user.name=E2E", "-c", "user.email=e2e@example.invalid", "commit", "--allow-empty", "-m", "fixture"]);
  execFileSync("git", ["-C", canonical, "worktree", "add", "-b", "fixture-worktree", worktree]);
  const attached = await daemon.api(`/control/agents/${daemon.agentId}/workspace/attach`, { method: "POST", body: JSON.stringify({ path: canonical, authority_class: "operator_instruction" }) });
  expect(attached.ok).toBe(true);
  const { workspace_id: workspace } = await attached.json();
  const root = `git_worktree_root:${workspace}:${worktree}`;
  const entry = { execution_root_id: root, workspace_id: workspace, filesystem_path: worktree, root_kind: "git_worktree_root", created_at: new Date().toISOString(), removed_at: null as string | null };
  await daemon.stop();
  registerRoot(daemon.home, entry);
  await daemon.start();
  const name = "中文 空格(1)%20#?.md";
  await writeFile(path.join(canonical, "same.md"), "# Canonical contents");
  await writeFile(path.join(worktree, "same.md"), "# Worktree contents");
  await writeFile(path.join(worktree, name), "# Only worktree\n\n" + "Text before the target.\n\n".repeat(90) + "## Target\n\nFound the correct file.");
  await mkdir(path.join(worktree, "folder"));
  await writeFile(path.join(worktree, "image.png"), Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=", "base64"));
  const absolute = path.join(worktree, name).split("/").map(encodeURIComponent).join("/");
  const uri = `workspace://${workspace}/${encodeURIComponent(name)}?root=${root}`;
  const homeInfo = await daemon.api(`/workspaces/agent_home:${daemon.agentId}/files`).then((response) => response.json());
  await writeFile(path.join(homeInfo.absolute_path, "plan.md"), "# Agent home plan");
  await writeFile(path.join(worktree, "base.md"), `[Absolute](${absolute}#target)\n\n[Historical](${uri})\n\n[Relative](./${encodeURIComponent(name)}#target)\n\n[Worktree same](same.md)\n\n[Canonical](workspace://${workspace}/same.md)\n\n[Home](workspace://agent_home:${daemon.agentId}/plan.md)\n\n[Directory](folder/)\n\n[Missing](missing.md)\n\n[Forbidden](../outside.md)\n\n[Unknown root](workspace://${workspace}/same.md?root=unknown)\n\n![Authenticated chart](image.png)`);
  const gui = (file: string, fragment = "") => `${daemon.baseUrl}/files?${new URLSearchParams({ workspace, root, path: file })}${fragment}`;
  await page.addInitScript((token) => sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({ mode: "local", token })), daemon.token);
  await page.goto(gui("base.md"));
  const content = page.locator(".file-browser-markdown");
  await expect(content.getByRole("link", { name: "Historical", exact: true })).toHaveAttribute("href", new URL(gui(name)).pathname + new URL(gui(name)).search);
  await expect(content.getByRole("img", { name: "Authenticated chart" })).toHaveAttribute("src", /^blob:/);
  await expect(content.locator(".file-reference-unresolved")).toHaveCount(3);
  await page.screenshot({ path: test.info().outputPath("file-preview.png") });
  await content.getByRole("link", { name: "Historical", exact: true }).click();
  await expect(content).toContainText("Only worktree");
  await page.reload();
  await expect(content).toContainText("Only worktree");
  const downloadPromise = page.waitForEvent("download");
  await page.getByRole("button", { name: "File actions", exact: true }).click();
  await page.getByRole("button", { name: "Download", exact: true }).click();
  const download = await downloadPromise;
  expect(await readFile((await download.path())!, "utf8")).toContain("Only worktree");
  await page.goto(gui("base.md"));
  await content.getByRole("link", { name: "Relative", exact: true }).click();
  await expect(content.getByRole("heading", { name: "Target", exact: true })).toBeInViewport();
  for (const [label, expected] of [["Canonical", "Canonical contents"], ["Worktree same", "Worktree contents"], ["Home", "Agent home plan"]]) {
    await page.goto(gui("base.md"));
    await content.getByRole("link", { name: label, exact: true }).click();
    await expect(content).toContainText(expected);
  }
  await page.goto(gui("base.md"));
  await content.getByRole("link", { name: "Directory", exact: true }).click();
  await expect(page.locator(".file-browser-breadcrumb")).toContainText("folder");
  // A genuinely fresh tab has no Bearer token; login must restore the exact locator and fragment.
  const fresh = await browser.newContext();
  try {
    const login = await fresh.newPage();
    await login.goto(gui(name, "#target"));
    await login.locator("#login-token").fill(daemon.token);
    await login.getByRole("button", { name: /Sign in|Connect/i }).click();
    await expect(login.locator(".file-browser-markdown")).toContainText("Only worktree");
    expect(new URL(login.url()).searchParams.get("root")).toBe(root);
    expect(new URL(login.url()).hash).toBe("#target");
    await login.reload();
    await expect(login.locator(".file-browser-markdown")).toContainText("Only worktree");
    const cookieDownload = login.waitForEvent("download");
    await login.getByRole("button", { name: "File actions", exact: true }).click();
    await login.getByRole("button", { name: "Download", exact: true }).click();
    expect(await readFile((await (await cookieDownload).path())!, "utf8")).toContain("Only worktree");
  } finally { await fresh.close(); }
  // The bytes still exist: a removed root must not fall back to canonical same.md.
  await daemon.stop();
  registerRoot(daemon.home, { ...entry, removed_at: new Date().toISOString() });
  await daemon.start();
  await page.goto(gui("same.md"));
  await expect(page.getByRole("alert")).toContainText(/removed|not found|unknown/i);
  await expect(page.locator(".file-browser-markdown")).toHaveCount(0);
  const response = await daemon.api("/file-references/resolve", { method: "POST", body: JSON.stringify({ references: [{ type: "absolute_path", absolute_path: path.join(worktree, "same.md") }, { type: "workspace_uri", workspace_uri: `workspace://${workspace}/same.md?root=${root}` }] }) });
  expect((await response.json()).results.map((result: any) => result.reason)).toEqual(["root_removed", "root_removed"]);
});
