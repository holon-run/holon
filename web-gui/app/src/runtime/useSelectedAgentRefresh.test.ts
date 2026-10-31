// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  factory: vi.fn(),
  scheduler: { hint: vi.fn(), resume: vi.fn(), setActive: vi.fn(), refreshNow: vi.fn(), dispose: vi.fn() },
  state: {
    currentUser: { authMethod: "token", userId: "alice" },
    currentUserLoaded: true,
    bootstrap: { connection: { mode: "local", baseUrl: "/api", authRequired: false } },
    discovery: { freshness: "fresh", identity: undefined },
    sessionsByAgentId: {},
    refreshAgentState: vi.fn().mockResolvedValue(undefined),
    refreshAgentWorkItems: vi.fn().mockResolvedValue(undefined),
  },
}));
vi.mock("./runtime-store", () => ({
  getRuntimeConnectionConfig: () => ({ mode: "local" }),
  useRuntimeStore: Object.assign(
    (select: (state: typeof mocks.state) => unknown) => select(mocks.state),
    { getState: () => mocks.state },
  ),
}));
vi.mock("./selected-agent-refresh", () => ({ createSelectedAgentRefreshScheduler: mocks.factory }));

import { useConversationScopeStore } from "./conversation-scope-store";
import { refreshSelectedAgentResources, useSelectedAgentRefresh } from "./useSelectedAgentRefresh";
import type { AgentSummary } from "./types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root;
let container: HTMLDivElement;
const rejected = vi.fn();
function Probe({ id = "a", incarnation = 1, enabled = true }: { id?: string; incarnation?: number; enabled?: boolean }) {
  useSelectedAgentRefresh({ id, incarnation } as AgentSummary, "scope", enabled, rejected);
  return null;
}
function seed(seq: number, version: number, status: "ready" | "reconnecting" = "ready") {
  const view = { through_seq: seq, scope: { runtime_id: "r", visibility_scope_id: "v", event_log_epoch: "e" },
    reset_reason: null } as never;
  useConversationScopeStore.setState({ scopes: { scope: {
    view, displayView: view, version,
    status: status === "ready" ? { kind: "ready" } : { kind: "reconnecting", attempt: 1, delayMs: 100 },
  } } });
}
async function render(props: Parameters<typeof Probe>[0] = {}) {
  await act(async () => { root.render(createElement(Probe, props)); });
}
beforeEach(() => {
  vi.clearAllMocks();
  mocks.factory.mockReturnValue(mocks.scheduler);
  mocks.state.currentUser = { authMethod: "token", userId: "alice" };
  mocks.state.discovery.freshness = "fresh";
  Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
  useConversationScopeStore.setState({ scopes: {} });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => { act(() => root.unmount()); container.remove(); });

describe("selected Agent refresh wiring", () => {
  it("uses committed checkpoint-only advancement, not history/brief/version changes", async () => {
    seed(10, 1);
    await render();
    mocks.scheduler.hint.mockClear();
    act(() => seed(10, 2));
    expect(mocks.scheduler.hint).not.toHaveBeenCalled();
    act(() => seed(11, 3));
    expect(mocks.scheduler.hint).toHaveBeenCalledTimes(1);
    act(() => seed(11, 4));
    expect(mocks.scheduler.hint).toHaveBeenCalledTimes(1);
  });

  it("revalidates same-head reconnection and visible/online restoration", async () => {
    seed(10, 1);
    await render();
    mocks.scheduler.resume.mockClear();
    act(() => seed(10, 2, "reconnecting"));
    act(() => seed(10, 3));
    expect(mocks.scheduler.resume).toHaveBeenCalledTimes(1);
    Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
    expect(mocks.scheduler.setActive).toHaveBeenLastCalledWith(false);
    const resumes = mocks.scheduler.resume.mock.calls.length;
    window.dispatchEvent(new Event("online"));
    expect(mocks.scheduler.resume).toHaveBeenCalledTimes(resumes);
    Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
    expect(mocks.scheduler.setActive).toHaveBeenLastCalledWith(true);
    expect(mocks.scheduler.resume).toHaveBeenCalledTimes(resumes + 1);
  });

  it("pins reads and cancels old work on route, incarnation or authorization change", async () => {
    await render();
    const options = mocks.factory.mock.calls[0][0];
    await options.resources.state.refresh();
    await options.resources.workItems.refresh();
    const binding = mocks.state.refreshAgentState.mock.calls[0] as unknown as [string, { incarnation: number; signal: AbortSignal }];
    expect(binding[0]).toBe("a");
    expect(binding[1].incarnation).toBe(1);
    expect(mocks.state.refreshAgentWorkItems).toHaveBeenCalledWith("a", binding[1]);
    await render({ id: "b", incarnation: 2 });
    expect(binding[1].signal.aborted).toBe(true);
    expect(mocks.scheduler.dispose).toHaveBeenCalledTimes(1);
    refreshSelectedAgentResources("a");
    expect(mocks.scheduler.refreshNow).not.toHaveBeenCalled();
    refreshSelectedAgentResources("b");
    expect(mocks.scheduler.refreshNow).toHaveBeenCalledTimes(1);
    mocks.state.currentUser = { authMethod: "token", userId: "bob" };
    await render({ id: "b", incarnation: 2 });
    expect(mocks.scheduler.dispose).toHaveBeenCalledTimes(2);
  });

  it("does not read for an unadmitted route and stops on resource access rejection", async () => {
    await render({ enabled: false });
    expect(mocks.factory).not.toHaveBeenCalled();
    await render();
    const { RuntimeHttpError } = await import("./client");
    const error = new RuntimeHttpError("GET", "/state", 403);
    mocks.factory.mock.calls[0][0].onError("state", error);
    expect(mocks.scheduler.setActive).toHaveBeenLastCalledWith(false);
    expect(rejected).toHaveBeenCalledWith(error);
  });
});
