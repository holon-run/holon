import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { RuntimeHttpError } from "./client";
import {
  createSelectedAgentRefreshScheduler,
  type SelectedAgentRefreshOptions,
} from "./selected-agent-refresh";

function deferred() {
  let resolve!: () => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<void>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

function httpError(status: number, retryAfterSeconds?: number) {
  return new RuntimeHttpError(
    "GET", "/agents/selected/state", status,
    undefined, undefined, undefined, undefined, retryAfterSeconds,
  );
}

describe("selected agent refresh scheduler", () => {
  const schedulers: ReturnType<typeof createSelectedAgentRefreshScheduler>[] = [];

  function create(options: SelectedAgentRefreshOptions) {
    const scheduler = createSelectedAgentRefreshScheduler({ random: () => 0.5, ...options });
    schedulers.push(scheduler);
    return scheduler;
  }

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
  });

  afterEach(() => {
    for (const scheduler of schedulers.splice(0)) scheduler.dispose();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("starts inactive and coalesces initial/resume/manual triggers in one turn", async () => {
    const refresh = vi.fn(async () => {});
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.hint();
    scheduler.resume();
    scheduler.refreshNow();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(refresh).not.toHaveBeenCalled();

    scheduler.setActive(true);
    scheduler.setActive(true);
    scheduler.resume();
    scheduler.refreshNow();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(29_999);
    expect(refresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("uses a fixed 300ms hint deadline so continuous hints cannot starve reads", async () => {
    const starts: number[] = [];
    const refresh = vi.fn(async () => { starts.push(Date.now()); });
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(6_000);

    scheduler.hint();
    for (let index = 0; index < 7; index += 1) {
      await vi.advanceTimersByTimeAsync(100);
      scheduler.hint();
    }
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(starts).toEqual([0, 6_300]);
    await vi.advanceTimersByTimeAsync(4_599);
    expect(refresh).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(refresh).toHaveBeenCalledTimes(3);
  });

  it("waits for the automatic floor before an early-settling request's trailing read", async () => {
    const first = deferred();
    const refresh = vi.fn<() => Promise<void>>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValue(undefined);
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    scheduler.hint();
    await vi.advanceTimersByTimeAsync(1_000);
    first.resolve();
    await vi.advanceTimersByTimeAsync(3_999);
    expect(refresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("limits automatic hints to one read per resource per five seconds", async () => {
    const refresh = vi.fn(async () => {});
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    for (let index = 0; index < 49; index += 1) {
      scheduler.hint();
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(refresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(100);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("keeps an in-flight resource single-flight and performs one trailing dirty read", async () => {
    const first = deferred();
    const refresh = vi.fn<() => Promise<void>>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValue(undefined);
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    scheduler.hint();
    scheduler.hint();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(refresh).toHaveBeenCalledTimes(1);

    first.resolve();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(29_999);
    expect(refresh).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(refresh).toHaveBeenCalledTimes(3);
  });

  it("does not turn a clean completion into a trailing/self-refresh loop", async () => {
    const refresh = vi.fn(async () => {});
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => Infinity } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(300_000);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("revalidates a same-head resume immediately without a hint and bypasses only the normal floor", async () => {
    const refresh = vi.fn(async () => {});
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(1_000);
    scheduler.resume();
    scheduler.resume();
    scheduler.refreshNow();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1_000);
    scheduler.refreshNow();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(3);
  });

  it("coalesces explicit triggers during an older request into one trailing validation", async () => {
    const first = deferred();
    const refresh = vi.fn<() => Promise<void>>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValue(undefined);
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(1_000);
    scheduler.resume();
    scheduler.resume();
    scheduler.refreshNow();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(1);
    first.resolve();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("pauses hint/poll timers while inactive and immediately validates on activation", async () => {
    const refresh = vi.fn(async () => {});
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 5_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    scheduler.hint();
    scheduler.setActive(false);
    scheduler.hint();
    scheduler.resume();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
    scheduler.setActive(true);
    scheduler.resume();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("settles an existing request without launching hidden trailing work", async () => {
    const first = deferred();
    const refresh = vi.fn<() => Promise<void>>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValue(undefined);
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 5_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    scheduler.hint();
    scheduler.setActive(false);
    first.resolve();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(refresh).toHaveBeenCalledTimes(1);
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it.each(["resolve", "reject"] as const)("ignores a disposed request's late %s, including finally reads", async (outcome) => {
    const first = deferred();
    const refresh = vi.fn(() => first.promise);
    const intervalMs = vi.fn(() => 5_000);
    const now = vi.fn(() => Date.now());
    const onError = vi.fn();
    const scheduler = create({ resources: { state: { refresh, intervalMs } }, now, onError });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    scheduler.dispose();
    intervalMs.mockClear();
    now.mockClear();
    if (outcome === "resolve") first.resolve();
    else first.reject(new Error("late failure"));
    await vi.advanceTimersByTimeAsync(60_000);
    scheduler.hint();
    scheduler.resume();
    scheduler.refreshNow();
    scheduler.setActive(true);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(intervalMs).not.toHaveBeenCalled();
    expect(now).not.toHaveBeenCalled();
    expect(onError).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps failures and retry budgets independent between resources", async () => {
    const error = new Error("state unavailable");
    const state = vi.fn<() => Promise<void>>().mockRejectedValue(error);
    const workitems = vi.fn(async () => {});
    const onError = vi.fn();
    const scheduler = create({
      resources: {
        state: { refresh: state, intervalMs: () => 5_000 },
        workitems: { refresh: workitems, intervalMs: () => 5_000 },
      },
      onError,
    });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(state).toHaveBeenCalledTimes(2);
    expect(workitems).toHaveBeenCalledTimes(3);
    expect(onError).toHaveBeenNthCalledWith(1, "state", error);
    expect(onError).toHaveBeenNthCalledWith(2, "state", error);
  });

  it("uses capped exponential backoff and does not let checkpoints reset it", async () => {
    const starts: number[] = [];
    const refresh = vi.fn(async () => {
      starts.push(Date.now());
      throw new Error("transient");
    });
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 5_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    for (let index = 0; index < 950; index += 1) {
      scheduler.hint();
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(starts).toEqual([0, 5_000, 15_000, 35_000, 65_000, 95_000]);
  });

  it("applies jitter without exceeding the local 30-second retry cap", async () => {
    const starts: number[] = [];
    const scheduler = create({
      resources: {
        state: {
          refresh: async () => {
            starts.push(Date.now());
            throw new Error("transient");
          },
          intervalMs: () => 30_000,
        },
      },
      random: () => 1,
    });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(72_000);
    expect(starts).toEqual([0, 6_000, 18_000, 42_000, 72_000]);
  });

  it.each([429, 503])("honors %i Retry-After even across resume/manual/hidden transitions", async (status) => {
    const refresh = vi.fn<() => Promise<void>>()
      .mockRejectedValueOnce(httpError(status, 40))
      .mockResolvedValue(undefined);
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 5_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    for (let index = 0; index < 39; index += 1) {
      scheduler.hint();
      scheduler.resume();
      scheduler.refreshNow();
      scheduler.setActive(false);
      scheduler.setActive(true);
      await vi.advanceTimersByTimeAsync(1_000);
    }
    expect(refresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("lets a manual retry override the local backoff while resume keeps it", async () => {
    const refresh = vi.fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error("transient"))
      .mockResolvedValue(undefined);
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(1);

    scheduler.resume();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(1);

    scheduler.refreshNow();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("resets local retry backoff only after a successful validation", async () => {
    const refresh = vi.fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error("first"))
      .mockRejectedValueOnce(new Error("second"))
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error("after success"))
      .mockResolvedValue(undefined);
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 30_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(15_000);
    expect(refresh).toHaveBeenCalledTimes(3);
    scheduler.refreshNow();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(4);
    await vi.advanceTimersByTimeAsync(4_999);
    expect(refresh).toHaveBeenCalledTimes(4);
    await vi.advanceTimersByTimeAsync(1);
    expect(refresh).toHaveBeenCalledTimes(5);
  });

  it.each([401, 403, 404, 410])("stops automatic retries after %i until an explicit manual retry", async (status) => {
    const refresh = vi.fn<() => Promise<void>>()
      .mockRejectedValueOnce(httpError(status))
      .mockResolvedValue(undefined);
    const scheduler = create({ resources: { state: { refresh, intervalMs: () => 5_000 } } });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(60_000);
    scheduler.hint();
    scheduler.resume();
    scheduler.setActive(false);
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(refresh).toHaveBeenCalledTimes(1);
    scheduler.refreshNow();
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("re-evaluates a dynamic polling interval and keeps resource cadences independent", async () => {
    let interval = 5_000;
    const state = vi.fn(async () => {});
    const workitems = vi.fn(async () => {});
    const scheduler = create({
      resources: {
        state: { refresh: state, intervalMs: () => interval },
        workitems: { refresh: workitems, intervalMs: () => 30_000 },
      },
    });
    scheduler.setActive(true);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(state).toHaveBeenCalledTimes(2);
    expect(workitems).toHaveBeenCalledTimes(1);
    interval = 30_000;
    await vi.advanceTimersByTimeAsync(25_000);
    expect(state).toHaveBeenCalledTimes(2);
    expect(workitems).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(state).toHaveBeenCalledTimes(3);
  });
});
