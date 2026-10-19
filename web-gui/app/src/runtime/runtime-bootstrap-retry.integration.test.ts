// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  bootstrap: vi.fn(), user: vi.fn(), read: vi.fn(),
}));
vi.mock("./client", async (original) => ({
  ...await original<typeof import("./client")>(),
  createRuntimeClient: () => ({
    getBootstrap: mocks.bootstrap, getCurrentUser: mocks.user,
    getBriefReadStates: async () => ({ agents: [] }),
  }),
}));
vi.mock("./agent-presentation-cache", async (original) => ({
  ...await original<typeof import("./agent-presentation-cache")>(),
  readCachedAgents: mocks.read, writeCachedAgents: vi.fn(),
}));

import { type BootstrapRefreshOptions, useRuntimeStore } from "./runtime-store";
import type { AgentSummary, RuntimeBootstrap } from "./types";
import { useRuntimeDashboard } from "./useRuntimeDashboard";
import { RuntimeHttpError } from "./client";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const initial = useRuntimeStore.getState();
const agent: AgentSummary = {
  id: "cached-agent", incarnation: 3, name: "Cached agent", badge: "CA", profile: "",
  lifecycle: "idle", acceptsInput: false, focusSummary: "", workspace: "", attention: "",
  model: "", footer: "", subtitle: "", lastBrief: "cached preview", lastTurnTime: "",
  pending: 0, activeTaskCount: 0, waitingCount: 0, posture: "", postureReason: "",
};
function bootstrap(source: "http" | "fixture"): RuntimeBootstrap {
  return {
    agents: source === "http" ? [agent] : [], attentionCount: 0, metrics: [],
    connection: {
      mode: "local", source, baseUrl: "/api", summary: source,
      ...(source === "fixture" ? { error: "roster unavailable" } : {}),
    },
  };
}
function Probe(): null {
  useRuntimeDashboard();
  return null;
}
let unmount: (() => void) | undefined;
function renderDashboard(): ReturnType<typeof act> {
  const root = createRoot(document.createElement("div"));
  unmount = () => act(() => root.unmount());
  return act(async () => root.render(createElement(Probe)));
}
afterEach(() => {
  unmount?.();
  unmount = undefined;
  useRuntimeStore.setState(initial, true);
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("cached reload bootstrap retry integration", () => {
  it.each([true, false])("retries a failed local bootstrap with backoff and stops on recovery (cached=%s)", async (cached) => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    mocks.user.mockResolvedValue({ authMethod: "token", userId: "alice" });
    mocks.read.mockResolvedValue(cached ? [agent] : []);
    let resolveFirst!: (value: RuntimeBootstrap) => void;
    mocks.bootstrap.mockReset()
      .mockImplementationOnce(() => new Promise<RuntimeBootstrap>((resolve) => { resolveFirst = resolve; }))
      .mockResolvedValueOnce(bootstrap("fixture"))
      .mockResolvedValueOnce(bootstrap("fixture"))
      .mockResolvedValue(bootstrap("http"));
    const retries: number[] = [];
    // Keep the real subject/cache/bootstrap action; only isolate unrelated
    // roster streams and record the hook's retry triggers.
    useRuntimeStore.setState({
      refreshBootstrap: (options?: BootstrapRefreshOptions) => {
        if (options?.trigger === "connect.retry") retries.push(Date.now());
        return initial.refreshBootstrap({ ...options, syncEvents: false });
      },
    });
    await renderDashboard();
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual(cached ? [agent] : []);
    expect(useRuntimeStore.getState().agentRosterSource).toBe(cached ? "cache" : undefined);
    expect(useRuntimeStore.getState().bootstrapLoading).toBe(true);

    await act(async () => resolveFirst(bootstrap("fixture")));
    expect(useRuntimeStore.getState().bootstrap.connection.error).toBe("roster unavailable");
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual(cached ? [agent] : []);
    for (const delay of [1_000, 2_000, 4_000]) {
      await act(async () => vi.advanceTimersByTimeAsync(delay));
    }
    expect(retries).toEqual([1_000, 3_000, 7_000]);
    expect(useRuntimeStore.getState().bootstrap.connection.error).toBeUndefined();
    expect(useRuntimeStore.getState().agentRosterSource).toBe("http");
    await act(async () => vi.advanceTimersByTimeAsync(60_000));
    expect(retries).toEqual([1_000, 3_000, 7_000]);
    expect(mocks.bootstrap).toHaveBeenCalledTimes(4);
  });

  it.each([401, 403])("does not expose caches or start bootstrap retries after strict subject rejection %s", async (status) => {
    vi.useFakeTimers();
    mocks.bootstrap.mockReset();
    mocks.read.mockReset().mockResolvedValue([agent]);
    mocks.user.mockReset().mockRejectedValue(new RuntimeHttpError("GET", "/auth/session/me", status));
    useRuntimeStore.setState({ refreshBootstrap: initial.refreshBootstrap });
    await renderDashboard();
    await act(async () => vi.advanceTimersByTimeAsync(30_000));
    expect(useRuntimeStore.getState().bootstrap.connection.authRequired).toBe(true);
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
    expect(mocks.read).not.toHaveBeenCalled();
    expect(mocks.bootstrap).not.toHaveBeenCalled();
    expect(mocks.user).toHaveBeenCalledTimes(1);
  });
});
