import { afterEach, describe, expect, it, vi } from "vitest";

import { ResumeReconciliationCoordinator } from "./resume-reconciliation";

const scheduler = {
  setTimeout: (callback: () => void, delayMs: number) =>
    setTimeout(callback, delayMs) as unknown as number,
  clearTimeout: (timer: number) => clearTimeout(timer),
};

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("ResumeReconciliationCoordinator", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("coalesces visibility, pageshow, and online notifications into one reconciliation", async () => {
    vi.useFakeTimers();
    const reconcile = vi.fn(async () => undefined);
    const coordinator = new ResumeReconciliationCoordinator(reconcile, scheduler, 100);

    coordinator.schedule();
    coordinator.schedule();
    coordinator.schedule();
    await vi.advanceTimersByTimeAsync(100);

    expect(reconcile).toHaveBeenCalledTimes(1);
    coordinator.dispose();
  });

  it("never overlaps reconciliations and runs one trailing reconciliation when needed", async () => {
    vi.useFakeTimers();
    const first = deferred();
    const reconcile = vi
      .fn<() => Promise<void>>()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue(undefined);
    const coordinator = new ResumeReconciliationCoordinator(reconcile, scheduler, 100);

    coordinator.schedule();
    await vi.advanceTimersByTimeAsync(100);
    coordinator.schedule();
    await vi.advanceTimersByTimeAsync(100);
    expect(reconcile).toHaveBeenCalledTimes(1);

    first.resolve();
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(100);

    expect(reconcile).toHaveBeenCalledTimes(2);
    coordinator.dispose();
  });

  it("can schedule another reconciliation after a failed attempt", async () => {
    vi.useFakeTimers();
    const reconcile = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue(undefined);
    const coordinator = new ResumeReconciliationCoordinator(reconcile, scheduler, 100);

    coordinator.schedule();
    await vi.advanceTimersByTimeAsync(100);
    coordinator.schedule();
    await vi.advanceTimersByTimeAsync(100);

    expect(reconcile).toHaveBeenCalledTimes(2);
    coordinator.dispose();
  });
});
