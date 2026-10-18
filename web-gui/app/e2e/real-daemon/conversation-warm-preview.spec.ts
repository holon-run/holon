import { expect, test } from "./daemon-fixture";

test("warm history is visible before real identity/state responses and stays read-only", async ({
  daemonFactory, page,
}, testInfo) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await page.addInitScript((token) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local", token,
    }));
  }, daemon.token);
  const buddyId = "warm-preview-buddy";
  const created = await daemon.api(`/control/agents/${buddyId}/create`, {
    method: "POST",
    body: JSON.stringify({ authority_class: "operator_instruction", template: null }),
  });
  expect(created.ok).toBe(true);
  const marker = "real daemon warm-cache revalidation marker";
  await daemon.enqueue(marker);
  const coldStarted = performance.now();
  await page.goto(`${daemon.baseUrl}/agents/default/conversation`);
  await expect(page.locator(".message-list")).toContainText(marker);
  const coldHistoryVisibleMs = performance.now() - coldStarted;
  await expect(page.locator(".composer textarea")).toBeEnabled();
  await expect.poll(() => page.evaluate(
    () => window.__HOLON_E2E__?.snapshot().conversationStatus?.kind,
  )).toBe("ready");
  await page.locator(`.agent-row[title*="${buddyId}"]`).click();
  await expect(page.locator(".composer textarea")).toBeEnabled();

  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  let identityRequests = 0;
  let stateRequests = 0;
  const readWrites: string[] = [];
  let preview = true;
  page.on("request", (request) => {
    if (preview && request.method() === "POST"
      && request.url().includes("/agents/default/brief-read-cursor")) {
      readWrites.push(request.url());
    }
  });
  await page.route("**/api/control/agents/default/detail", async (route) => {
    identityRequests++;
    await gate;
    await route.continue();
  });
  await page.route("**/api/agents/default/state", async (route) => {
    stateRequests++;
    await gate;
    await route.continue();
  });
  try {
    const warmStarted = performance.now();
    await page.locator('.agent-row[title*="default"]').click();
    await expect(page.locator(".message-list")).toContainText(marker);
    const warmHistoryVisibleMs = performance.now() - warmStarted;
    await expect(page.getByText("Loading agent…", { exact: true })).toHaveCount(0);
    await expect(page.locator(".composer textarea")).toBeEnabled();
    await page.locator(".composer textarea").fill("Draft before real access resolves");
    await expect(page.locator(".composer .send-button")).toBeDisabled();
    // Real responses have not been substituted: only their delivery is held.
    // Waiting for both requests also guards against identity -> state serialization.
    await expect.poll(() => identityRequests).toBeGreaterThan(0);
    await expect.poll(() => stateRequests).toBeGreaterThan(0);
    expect(readWrites).toEqual([]);
    await testInfo.attach("warm-preview-measurements", {
      body: JSON.stringify({
        coldHistoryVisibleMs, warmHistoryVisibleMs,
        identityRequests, stateRequests, readMarkerWrites: readWrites.length,
        // Local fixture and assertion overhead are included; this is not an SLA.
        responsesHeldUntilAfterHistoryVisible: true,
      }, null, 2),
      contentType: "application/json",
    });
    preview = false;
    release();
    await expect(page.locator(".composer .send-button")).toBeEnabled();
    await expect(page.locator(".composer textarea")).toHaveValue("Draft before real access resolves");
    await expect(page.locator(".message-list")).toContainText(marker);
  } finally {
    release();
  }
});
