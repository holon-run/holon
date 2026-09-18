import { expect, test, type Page } from "@playwright/test";
import { sessionFor } from "./test-session";

test.use({ launchOptions: { args: ["--host-resolver-rules=MAP holon-http.test 127.0.0.1"] } });

test.beforeEach(async ({ context }, info) => {
  for (const domain of ["127.0.0.1", "holon-http.test"]) {
    await context.addCookies([{ name: "holon_e2e_session", value: sessionFor(info), domain, path: "/" }]);
  }
});
const copyButton = (page: Page) => page.getByRole("button", { name: "Copy agent ID: bootstrap-agent", exact: true });

async function observeLegacyCopy(page: Page, result: "native" | "false" | "throw" = "native") {
  await page.addInitScript((mode) => {
    const nativeCopy = document.execCommand.bind(document);
    document.execCommand = (command) => {
      const field = document.activeElement as HTMLTextAreaElement;
      (window as any).__legacyCopy = { command, value: field.value, selected: field.value.slice(field.selectionStart, field.selectionEnd) };
      if (mode === "throw") throw new Error("Copy denied");
      return mode === "false" ? false : nativeCopy(command);
    };
  }, result);
}

test("HTTP host without Clipboard API copies through the native selection fallback", async ({ page, baseURL }) => {
  await observeLegacyCopy(page);
  const url = new URL(baseURL!); url.hostname = "holon-http.test";
  await page.goto(url.href);
  expect(await page.evaluate(() => ({ secure: isSecureContext, clipboard: typeof navigator.clipboard }))).toEqual({ secure: false, clipboard: "undefined" });
  await copyButton(page).click();
  await expect(page.locator(".clipboard-toast")).toHaveText("Copied");
  expect(await page.evaluate(() => (window as any).__legacyCopy)).toEqual({ command: "copy", value: "bootstrap-agent", selected: "bootstrap-agent" });
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.locator('textarea[tabindex="-1"]')).toHaveCount(0);
});

test("secure-origin Clipboard API succeeds without invoking the fallback", async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (text: string) => { (window as any).__modernCopy = text; } } });
    document.execCommand = () => { throw new Error("Unexpected fallback"); };
  });
  await page.goto("/");
  await copyButton(page).click();
  await expect(page.locator(".clipboard-toast")).toHaveText("Copied");
  expect(await page.evaluate(() => (window as any).__modernCopy)).toBe("bootstrap-agent");
});

test("denied Clipboard API falls back and restores input focus and selection", async ({ page }) => {
  await observeLegacyCopy(page);
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async () => { throw new DOMException("Denied", "NotAllowedError"); } } });
  });
  await page.goto("/");
  const filter = page.getByRole("textbox", { name: "Filter agents" });
  await filter.fill("bootstrap");
  await filter.evaluate((input: HTMLInputElement) => input.setSelectionRange(1, 5, "backward"));
  // Keep the input focused while retaining a real user gesture for legacy copy.
  await copyButton(page).evaluate((button) => button.addEventListener("mousedown", (event) => event.preventDefault(), { once: true }));
  await copyButton(page).click();
  await expect(page.locator(".clipboard-toast")).toHaveText("Copied");
  expect(await filter.evaluate((input: HTMLInputElement) => ({ focused: document.activeElement === input, start: input.selectionStart, end: input.selectionEnd, direction: input.selectionDirection }))).toEqual({ focused: true, start: 1, end: 5, direction: "backward" });
});

for (const mode of ["false", "throw"] as const) {
  test(`failed fallback (${mode}) shows selected text for manual copy and closes with Escape`, async ({ page }) => {
    await observeLegacyCopy(page, mode);
    await page.addInitScript(() => { Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined }); });
    await page.goto("/");
    await copyButton(page).click();
    const dialog = page.getByRole("dialog", { name: "Could not copy automatically" });
    await expect(dialog).toBeVisible();
    const text = dialog.getByRole("textbox", { name: "Text to copy" });
    await expect(text).toHaveValue("bootstrap-agent");
    expect(await text.evaluate((input: HTMLTextAreaElement) => input.value.slice(input.selectionStart, input.selectionEnd))).toBe("bootstrap-agent");
    await expect(page.locator(".clipboard-toast")).toHaveCount(0);
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(page.locator(".agent-id-copy")).toBeFocused();
  });
}

test("one- and two-line agents align status with copy without hover movement", async ({ page, context, request }, info) => {
  await request.post(`/__e2e__/configure?session=${encodeURIComponent(sessionFor(info))}`, { data: { visibleAgentIds: ["bootstrap-agent", "agent-ecosystem-researcher"] } });
  await context.route(/\/api\/agents\/(list|snapshot|[^/]+\/state)$/, async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    const rename = (value: any) => {
      if (!value || typeof value !== "object") return;
      if (value.identity?.agent_id === "agent-ecosystem-researcher") value.identity.name = "Agent调研员";
      for (const child of Object.values(value)) rename(child);
    };
    rename(data);
    await route.fulfill({ response, json: data });
  });
  await page.goto("/");
  const rows = page.locator(".agent-list-entry");
  await expect(rows).toHaveCount(2);
  await expect(rows.filter({ has: page.getByRole("button", { name: "Copy agent ID: bootstrap-agent", exact: true }) }).locator(".agent-row-meta")).toHaveCount(0);
  await expect(rows.filter({ hasText: "Agent调研员" }).locator(".agent-row-meta")).toHaveText("agent-ecosystem-researcher");
  for (const row of await rows.all()) {
    const status = row.locator(".agent-row-status-dot");
    const before = await status.boundingBox();
    await row.hover();
    await expect(row.locator(".agent-id-copy")).toHaveCSS("opacity", "1");
    const after = await status.boundingBox();
    const copy = await row.locator(".agent-id-copy").boundingBox();
    expect(after).toEqual(before);
    expect(Math.abs(after!.y + after!.height / 2 - copy!.y - copy!.height / 2)).toBeLessThan(1);
    expect(after!.x + after!.width).toBeLessThanOrEqual(copy!.x);
  }
  await page.screenshot({ path: "/tmp/holon-sidebar-copy-alignment.png" });
});
