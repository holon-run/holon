import { expect, test, type BrowserContext, type Page, type TestInfo } from "@playwright/test";
import { sessionFor } from "./test-session";

const workspaceId = "html-preview-workspace";
const executionRootId = "html-preview-root";
const token = "html-preview-e2e-token";
const source = "<!doctype html><html><body><h1>Rendered report</h1><script>parent.document.documentElement.dataset.htmlPwned = 'yes'; localStorage.setItem('htmlPwned', 'yes');</script><p>Static content</p></body></html>";
const xhtmlSource = "<?xml version=\"1.0\"?><!DOCTYPE html><html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h1>Rendered report</h1><p>Static content</p></body></html>";

async function openWorkspaceFile(
  context: BrowserContext,
  page: Page,
  info: TestInfo,
  path: string,
  mimeType: string,
  size = source.length,
  failPreview = false,
  useBearer = true,
  fileContent = source,
) {
  await context.addCookies([{
    name: "holon_e2e_session",
    value: sessionFor(info),
    domain: "127.0.0.1",
    path: "/",
  }]);
  if (useBearer) {
    await page.addInitScript((sessionToken) => {
      sessionStorage.setItem(
        "holon.webGui.activeRuntimeConnection.v1",
        JSON.stringify({ mode: "local", token: sessionToken }),
      );
    }, token);
  }

  let fileRequests = 0;
  let previewRequests = 0;
  await context.route(`**/api/workspaces/${workspaceId}/files**`, async (route) => {
    const url = new URL(route.request().url());
    const requestedPath = decodeURIComponent(url.pathname.split("/files/")[1] ?? "");
    expect(route.request().headers().authorization).toBe(useBearer ? `Bearer ${token}` : undefined);

    if (!requestedPath) {
      await route.fulfill({ json: {
        type: "directory",
        kind: "directory",
        workspace_id: workspaceId,
        execution_root_id: executionRootId,
        absolute_path: "/tmp/html-preview",
        root_kind: "canonical_root",
        path: "",
        entries: [{ name: path, type: "file", size, mime_type: mimeType }],
      } });
      return;
    }

    const file = {
      type: "file",
      kind: "file",
      workspace_id: workspaceId,
      execution_root_id: executionRootId,
      absolute_path: `/tmp/html-preview/${path}`,
      root_kind: "canonical_root",
      path,
      size,
      total_size: size,
      mime_type: mimeType,
      truncated: size > fileContent.length,
      content: fileContent,
    };
    if (url.searchParams.get("meta") === "true") {
      await route.fulfill({ json: { ...file, content: undefined } });
      return;
    }

    fileRequests += 1;
    if (fileRequests === 1) {
      await route.fulfill({ json: file });
      return;
    }

    previewRequests += 1;
    if (failPreview && !url.searchParams.has("download")) {
      await route.fulfill({ status: 403, json: { error: "preview denied" } });
      return;
    }
    await route.fulfill({
      contentType: url.searchParams.has("download") ? "application/octet-stream" : mimeType,
      body: fileContent,
    });
  });

  const search = new URLSearchParams({ workspace: workspaceId, root: executionRootId, path });
  await page.goto(`/files?${search}`);
  return {
    get fileRequests() { return fileRequests; },
    get previewRequests() { return previewRequests; },
  };
}

test("renders HTML in a sandbox and preserves source switching", async ({ page, context }, info) => {
  await openWorkspaceFile(context, page, info, "report.html", "text/html");
  const frame = page.locator("iframe.file-browser-html");
  await expect(frame).toHaveAttribute("sandbox", "");
  await expect(page.frameLocator("iframe.file-browser-html").getByRole("heading", { name: "Rendered report" })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.dataset.htmlPwned)).toBeUndefined();
  expect(await page.evaluate(() => localStorage.getItem("htmlPwned"))).toBeNull();

  await page.getByRole("button", { name: "Source", exact: true }).click();
  await expect(page.locator(".file-browser-code")).toContainText("htmlPwned");
  await expect(frame).toHaveCount(0);
});

test("recognizes .htm files by extension", async ({ page, context }, info) => {
  await openWorkspaceFile(context, page, info, "report.htm", "application/octet-stream");
  await expect(page.frameLocator("iframe.file-browser-html").getByRole("heading", { name: "Rendered report" })).toBeVisible();
});

test("recognizes XHTML MIME types without an HTML extension", async ({ page, context }, info) => {
  await openWorkspaceFile(context, page, info, "report.txt", "application/xhtml+xml", xhtmlSource.length, false, true, xhtmlSource);
  await expect(page.frameLocator("iframe.file-browser-html").getByRole("heading", { name: "Rendered report" })).toBeVisible();
  await page.getByRole("button", { name: "Source", exact: true }).click();
  await expect(page.locator(".file-browser-code")).toContainText("Static content");
});

test("offers source and download when the HTML preview request fails", async ({ page, context }, info) => {
  await openWorkspaceFile(context, page, info, "report.html", "text/html", source.length, true, false);
  const fallback = page.getByRole("alert");
  await expect(fallback).toContainText("HTML preview is unavailable");
  await fallback.getByRole("button", { name: "Source", exact: true }).click();
  await expect(page.locator(".file-browser-code")).toContainText("Rendered report");
});

test("sends cookie-authenticated HTML preview through fetch", async ({ page, context }, info) => {
  await openWorkspaceFile(context, page, info, "report.html", "text/html", source.length, false, false);
  await expect(page.frameLocator("iframe.file-browser-html").getByRole("heading", { name: "Rendered report" })).toBeVisible();
});

test("keeps oversized HTML readable and downloadable without fetching a preview blob", async ({ page, context }, info) => {
  const requests = await openWorkspaceFile(context, page, info, "report.html", "text/html", 1024 * 1024 * 1024 + 1);
  const fallback = page.getByRole("alert");
  await expect(fallback).toContainText("larger than 1 GB");
  await expect(page.locator("iframe.file-browser-html")).toHaveCount(0);
  expect(requests.fileRequests).toBe(1);
  expect(requests.previewRequests).toBe(0);

  await fallback.getByRole("button", { name: "Source", exact: true }).click();
  await expect(page.locator(".file-browser-code")).toContainText("Rendered report");
  expect(requests.previewRequests).toBe(0);

  const download = page.waitForEvent("download");
  await page.locator(".file-browser-truncated").getByRole("button", { name: "Download", exact: true }).click();
  expect((await download).suggestedFilename()).toBe("report.html");
  expect(requests.previewRequests).toBe(1);
});
