// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { type BootstrapRefreshOptions, useRuntimeStore } from "./runtime-store";
import type { RuntimeBootstrap, RuntimeConnection } from "./types";
import { useRuntimeDashboard } from "./useRuntimeDashboard";

// The repo has no @testing-library/react; drive the hook through the smallest
// possible createRoot + act harness instead.
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const initialStoreState = useRuntimeStore.getState();

let retryTimestamps: number[] = [];
let activeUnmount: (() => void) | null = null;

function disconnectedConnection(outage: number): RuntimeConnection {
  return {
    mode: "local",
    summary: `unreachable ${outage}`,
    baseUrl: "http://127.0.0.1:7878",
    source: "http",
    error: `ECONNREFUSED attempt ${outage}`,
  };
}

function connectedConnection(): RuntimeConnection {
  return {
    mode: "local",
    summary: "connected",
    baseUrl: "http://127.0.0.1:7878",
    source: "http",
  };
}

function bootstrapFor(connection: RuntimeConnection): RuntimeBootstrap {
  return { attentionCount: 0, connection, metrics: [], agents: [] };
}

function setBootstrap(bootstrap: RuntimeBootstrap): void {
  act(() => {
    useRuntimeStore.setState({ bootstrap });
  });
}

// Replace the store action so each `connect.retry` refresh can simulate the
// store behavior under test (bootstrap replacement, busy skip, recovery).
function installRefreshMock(onRetry: () => void) {
  const refresh = vi.fn(async (options?: BootstrapRefreshOptions) => {
    if (options?.trigger === "connect.retry") {
      retryTimestamps.push(Date.now());
      onRetry();
    }
  });
  useRuntimeStore.setState({ refreshBootstrap: refresh });
  return refresh;
}

function Probe(): null {
  useRuntimeDashboard();
  return null;
}

function renderDashboardHook(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(createElement(Probe));
  });
  activeUnmount = () => {
    act(() => {
      root.unmount();
    });
    container.remove();
    activeUnmount = null;
  };
}

async function advance(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  // Fake timers keep the real wall clock as the origin; anchor it so
  // Date.now() inside the retry mock reports the virtual elapsed time.
  vi.setSystemTime(0);
  retryTimestamps = [];
});

afterEach(() => {
  activeUnmount?.();
  vi.useRealTimers();
  useRuntimeStore.setState(initialStoreState, true);
});

describe("useRuntimeDashboard bootstrap connect retry", () => {
  it("grows the retry delay across consecutive disconnected bootstrap replacements", async () => {
    setBootstrap(bootstrapFor(disconnectedConnection(0)));
    installRefreshMock(() => {
      // Sustained outage: every refresh response replaces the bootstrap with
      // a fresh disconnected object (the store's replacement path).
      useRuntimeStore.setState({
        bootstrap: bootstrapFor(disconnectedConnection(retryTimestamps.length)),
      });
    });
    renderDashboardHook();

    await advance(1_000);
    await advance(2_000);
    await advance(4_000);
    await advance(8_000);
    await advance(15_000);

    expect(retryTimestamps).toEqual([1_000, 3_000, 7_000, 15_000, 30_000]);
  });

  it("keeps retrying with a growing delay after a projection_busy-style skip", async () => {
    setBootstrap(bootstrapFor(disconnectedConnection(0)));
    installRefreshMock(() => {
      // projection_busy: the store clears loading only and replaces no
      // bootstrap object.
    });
    renderDashboardHook();

    await advance(1_000);
    await advance(2_000);
    await advance(4_000);
    await advance(8_000);

    expect(retryTimestamps).toEqual([1_000, 3_000, 7_000, 15_000]);
  });

  it("stops retrying on recovery and restarts the backoff ladder on a fresh outage", async () => {
    setBootstrap(bootstrapFor(disconnectedConnection(0)));
    installRefreshMock(() => {
      if (retryTimestamps.length < 2) {
        useRuntimeStore.setState({
          bootstrap: bootstrapFor(disconnectedConnection(retryTimestamps.length)),
        });
      } else {
        useRuntimeStore.setState({
          bootstrap: bootstrapFor(connectedConnection()),
        });
      }
    });
    renderDashboardHook();

    await advance(3_000);
    expect(retryTimestamps).toEqual([1_000, 3_000]);

    // Long idle window after recovery: the chain must not fire again.
    await advance(60_000);
    expect(retryTimestamps).toEqual([1_000, 3_000]);

    // A fresh outage starts a new ladder at the 1s floor.
    setBootstrap(bootstrapFor(disconnectedConnection(99)));
    await advance(1_000);
    expect(retryTimestamps).toEqual([1_000, 3_000, 64_000]);
  });

  it("cancels the pending retry when the dashboard unmounts", async () => {
    setBootstrap(bootstrapFor(disconnectedConnection(0)));
    const refresh = installRefreshMock(() => {
      useRuntimeStore.setState({
        bootstrap: bootstrapFor(disconnectedConnection(retryTimestamps.length)),
      });
    });
    renderDashboardHook();
    activeUnmount?.();

    await advance(60_000);

    expect(retryTimestamps).toEqual([]);
    // Only the mount-time bootstrap refresh ran; no connect.retry followed.
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("does not retry when the error state carries no baseUrl", async () => {
    const noBaseUrl = { ...disconnectedConnection(0), baseUrl: undefined };
    setBootstrap(bootstrapFor(noBaseUrl));
    const refresh = installRefreshMock(() => {});
    renderDashboardHook();

    await advance(30_000);

    expect(retryTimestamps).toEqual([]);
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});
