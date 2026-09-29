import http from "node:http";
import type { Server } from "node:http";
import net from "node:net";
import type { Page } from "@playwright/test";

import { expect, test } from "./daemon-fixture";

async function installLocalToken(page: Page, token: string): Promise<void> {
  await page.addInitScript((value) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local",
      token: value,
    }));
  }, token);
}

async function conversationStatus(page: Page): Promise<string | undefined> {
  return await page.evaluate(
    () => window.__HOLON_E2E__?.snapshot().conversationStatus?.kind,
  );
}

async function timelineHasMarker(page: Page, marker: string): Promise<boolean> {
  return await page.locator(".message-list", { hasText: marker }).count() > 0;
}

test("operator prompt stays continuously visible from send through turn settlement", async ({
  daemonFactory,
  page,
}) => {
  test.setTimeout(120_000);
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await installLocalToken(page, daemon.token);

  await page.goto(`${daemon.baseUrl}/agents/${daemon.agentId}`);
  await expect.poll(() => conversationStatus(page)).toBe("ready");

  const marker = `prompt-visibility-${Date.now()}`;
  const composer = page.locator(".composer textarea");
  await expect(composer).toBeVisible();
  await composer.fill(marker);
  await page.locator(".composer .send-button").click();

  // The prompt must appear in the timeline immediately after send, without
  // waiting for any server round-trip beyond the enqueue itself.
  await expect
    .poll(() => timelineHasMarker(page, marker), { timeout: 5_000 })
    .toBe(true);

  // Continuously sample visibility: the marker must never drop out of the
  // timeline between the pending-input phase and the turn-input phase.
  const deadline = Date.now() + 20_000;
  let disappearances = 0;
  let landedInTurn = false;
  while (Date.now() < deadline) {
    if (!(await timelineHasMarker(page, marker))) disappearances += 1;
    if (await page.locator(`[data-turn-id]:has-text("${marker}")`).count() > 0) {
      landedInTurn = true;
      break;
    }
    await page.waitForTimeout(300);
  }
  expect(landedInTurn, "prompt landed in a turn card").toBe(true);
  expect(disappearances, "marker vanished from timeline during observation").toBe(0);
  await expect
    .poll(() => timelineHasMarker(page, marker), { timeout: 1_000 })
    .toBe(true);
});

interface HangingProvider {
  server: Server;
  baseUrl: string;
  stop(): Promise<void>;
}

async function startHangingProvider(): Promise<HangingProvider> {
  const port = await new Promise<number>((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("failed to allocate a provider port"));
        return;
      }
      server.close((error) => error ? reject(error) : resolve(address.port));
    });
  });
  const server = http.createServer((_request, response) => {
    response.writeHead(200, { "Content-Type": "text/event-stream" });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", resolve);
  });
  return {
    server,
    baseUrl: `http://127.0.0.1:${port}/v1`,
    async stop() {
      server.closeAllConnections?.();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

test("operator prompt sent while a run is active stays visible", async ({ daemonFactory, page }) => {
  test.setTimeout(120_000);
  const provider = await startHangingProvider();
  const daemon = await daemonFactory({
    webDist: "dist-e2e",
    env: {
      HOLON_OPENAI_BASE_URL: provider.baseUrl,
      OPENAI_API_KEY: "e2e-provider-key",
    },
  });
  try {
    await installLocalToken(page, daemon.token);
    await page.goto(`${daemon.baseUrl}/agents/${daemon.agentId}`);
    await expect.poll(() => conversationStatus(page)).toBe("ready");

    // Start a run that hangs on the frozen provider.
    const first = await daemon.api(`/control/agents/${daemon.agentId}/prompt`, {
      method: "POST",
      body: JSON.stringify({ text: "first turn that hangs" }),
    });
    expect(first.status).toBe(200);
    await expect.poll(async () => {
      const state = await daemon.api(`/agents/${daemon.agentId}/state`).then((response) => response.json());
      return state?.agent?.agent?.current_run_id ?? null;
    }, { timeout: 20_000 }).not.toBeNull();

    const midRunMarker = `mid-run-${Date.now()}`;
    const composer = page.locator(".composer textarea");
    await expect(composer).toBeVisible();
    await composer.fill(midRunMarker);
    await page.locator(".composer .send-button").click();

    await expect
      .poll(() => timelineHasMarker(page, midRunMarker), { timeout: 5_000 })
      .toBe(true);

    // While the run keeps hanging, the prompt must remain visible.
    const deadline = Date.now() + 10_000;
    let disappearances = 0;
    while (Date.now() < deadline) {
      if (!(await timelineHasMarker(page, midRunMarker))) disappearances += 1;
      await page.waitForTimeout(300);
    }
    expect(disappearances, "mid-run marker vanished").toBe(0);
  } finally {
    await provider.stop();
  }
});
