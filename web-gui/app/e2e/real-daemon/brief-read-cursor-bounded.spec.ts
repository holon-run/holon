import { expect, test } from "./daemon-fixture";

// Regression bound for #3851: while the open conversation belongs to a
// producing agent, read-cursor advances are debounced to a quiet window, so
// the write traffic must not track the event rate.
test("brief-read-cursor advance POSTs stay bounded while the agent keeps producing", async ({
  daemonFactory, page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await page.addInitScript((token) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local", token,
    }));
  }, daemon.token);

  const readCursorPosts: number[] = [];
  page.on("request", (request) => {
    if (request.method() === "POST"
      && request.url().includes("/agents/default/brief-read-cursor")) {
      readCursorPosts.push(Date.now());
    }
  });

  await page.goto(`${daemon.baseUrl}/agents/default/conversation`);
  await expect(page.locator(".composer textarea")).toBeEnabled();

  // Burst of operator inputs: every enqueue appends events, so the observed
  // head keeps moving well inside the 2s quiet window.
  for (let i = 0; i < 6; i += 1) {
    await daemon.enqueue(`read-cursor bound churn ${i}`);
  }

  // Churn (~1s) plus one quiet window (2s) plus fixture slack: the trailing
  // advance must have fired, and pre-fix behavior would have issued one POST
  // per observed event batch instead.
  await page.waitForTimeout(8_000);
  expect(readCursorPosts.length).toBeGreaterThanOrEqual(1);
  expect(readCursorPosts.length).toBeLessThanOrEqual(3);

  // Quiet and caught up: no further write traffic for the read cursor.
  const settled = readCursorPosts.length;
  await page.waitForTimeout(4_000);
  expect(readCursorPosts.length).toBe(settled);
});
