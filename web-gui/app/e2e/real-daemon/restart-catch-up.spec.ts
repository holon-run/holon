import type { Page } from "@playwright/test";

import { expect, test } from "./daemon-fixture";

interface LedgerSnapshot {
  eventLogEpoch: string;
  eventSeqs: number[];
  ingestedThroughSeq: number;
}

async function installLocalToken(page: Page, token: string): Promise<void> {
  await page.addInitScript((value) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local",
      token: value,
    }));
  }, token);
}

async function ledger(page: Page, agentId: string): Promise<LedgerSnapshot | null> {
  return await page.evaluate(
    async (id) => {
      const diagnostics = window.__HOLON_E2E__;
      if (!diagnostics) return null;
      const [value, partitions] = await Promise.all([
        diagnostics.ledger(id),
        diagnostics.ledgerPartitions(id),
      ]);
      if (!value) return null;
      const partition = partitions.find((candidate) =>
        candidate.runtimeId === value.runtimeId
        && candidate.visibilityScopeId === value.visibilityScopeId
        && candidate.eventLogEpoch === value.eventLogEpoch
      );
      return {
        eventLogEpoch: value.eventLogEpoch,
        eventSeqs: partition?.eventSeqs ?? [],
        ingestedThroughSeq: value.ingestedThroughSeq,
      };
    },
    agentId,
  );
}

test("same database restart reconnects, catches up, preserves epoch, and deduplicates events", async ({
  daemonFactory,
  page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await installLocalToken(page, daemon.token);
  await page.goto(daemon.baseUrl);

  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot()))
    .toMatchObject({
      bootstrapLoading: false,
      globalStreamStatus: "streaming",
      discovery: { mode: "authoritative", freshness: "fresh" },
    });

  await daemon.enqueue("before restart");
  await expect.poll(async () => ledger(page, daemon.agentId))
    .toMatchObject({ eventLogEpoch: expect.any(String), ingestedThroughSeq: expect.any(Number) });
  const before = await ledger(page, daemon.agentId);
  expect(before?.eventLogEpoch).toBeTruthy();

  await daemon.stop();
  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot().globalStreamStatus))
    .toBe("reconnecting");

  let releaseReconnect!: () => void;
  const reconnectBlocked = new Promise<void>((resolve) => {
    releaseReconnect = resolve;
  });
  let observeReconnect!: () => void;
  const reconnectObserved = new Promise<void>((resolve) => {
    observeReconnect = resolve;
  });
  let finishReconnect!: () => void;
  const reconnectFinished = new Promise<void>((resolve) => {
    finishReconnect = resolve;
  });
  let blockNextReconnect = true;
  await page.route("**/api/events/stream**", async (route) => {
    if (blockNextReconnect) {
      blockNextReconnect = false;
      observeReconnect();
      await reconnectBlocked;
    }
    await route.continue();
    finishReconnect();
  });

  await daemon.start();
  await reconnectObserved;
  await daemon.enqueue("during restart");
  releaseReconnect();
  await reconnectFinished;
  await page.unroute("**/api/events/stream**");

  await expect.poll(async () => page.evaluate(() => window.__HOLON_E2E__?.snapshot().globalStreamStatus))
    .toBe("streaming");
  await expect.poll(async () => {
    const value = await ledger(page, daemon.agentId);
    return value?.ingestedThroughSeq ?? 0;
  }).toBeGreaterThan(before?.ingestedThroughSeq ?? 0);

  const after = await ledger(page, daemon.agentId);
  expect(after).not.toBeNull();
  expect(after?.eventLogEpoch).toBe(before?.eventLogEpoch);
  expect(new Set(after!.eventSeqs).size).toBe(after!.eventSeqs.length);
});
