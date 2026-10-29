import type { Page } from "@playwright/test";

import { expect, test } from "./daemon-fixture";

interface ConversationProbe {
  status: string;
  eventLogEpoch?: string;
  throughSeq?: number;
  turnIds: string[];
  pendingInputIds: string[];
}

async function installLocalToken(page: Page, token: string): Promise<void> {
  await page.addInitScript((value) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local",
      token: value,
    }));
  }, token);
}

async function conversation(page: Page, agentId: string): Promise<ConversationProbe | null> {
  return await page.evaluate((id) => window.__HOLON_E2E__?.conversation(id) ?? null, agentId);
}

test("same database restart recovers the conversation, preserves the epoch, and deduplicates turns", async ({
  daemonFactory,
  page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await installLocalToken(page, daemon.token);
  await page.goto(`${daemon.baseUrl}/agents/${daemon.agentId}/conversation`);

  await expect.poll(() => conversation(page, daemon.agentId), { timeout: 30_000 })
    .toMatchObject({ status: "ready" });

  await daemon.enqueue("before restart");
  await expect.poll(async () => (await conversation(page, daemon.agentId))?.turnIds.length ?? 0, {
    timeout: 30_000,
  }).toBeGreaterThan(0);
  const before = await conversation(page, daemon.agentId);
  expect(before?.eventLogEpoch).toBeTruthy();

  await daemon.stop();

  await daemon.start();
  await daemon.enqueue("during restart");

  // The durable event ledger is retired (#3808): after a daemon restart the
  // user-visible conversation recovers through the roster snapshot plus the
  // conversation summary read model, exercised here by reloading the app.
  await page.reload();

  await expect.poll(async () => {
    const value = await conversation(page, daemon.agentId);
    return value !== null && value.status === "ready"
      && value.turnIds.length > (before?.turnIds.length ?? 0);
  }, { timeout: 30_000 }).toBe(true);
  const after = await conversation(page, daemon.agentId);
  expect(after?.eventLogEpoch).toBe(before?.eventLogEpoch);
  expect(after!.turnIds).toEqual(expect.arrayContaining(before!.turnIds));
  expect(new Set(after!.turnIds).size).toBe(after!.turnIds.length);
  expect(after!.throughSeq ?? 0).toBeGreaterThanOrEqual(before!.throughSeq ?? 0);
  // Every recovered turn renders exactly once: catch-up must not duplicate UI history.
  for (const turnId of after!.turnIds) {
    await expect(page.locator(`[data-turn-id="${turnId}"]`)).toHaveCount(1);
  }
});
