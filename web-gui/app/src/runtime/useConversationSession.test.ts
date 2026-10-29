// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  acquire: vi.fn(),
  release: vi.fn(),
  peek: vi.fn(),
  state: {
    currentUser: { authMethod: "token", userId: "alice" },
    currentUserLoaded: true,
    bootstrap: { connection: { mode: "local", baseUrl: "/api", authRequired: false } },
    discovery: { freshness: "fresh" },
    briefReadStateByAgentId: {},
    sessionsByAgentId: {},
    pruneOperatorPrompts: vi.fn(),
  },
}));
vi.mock("./runtime-store", () => ({
  getRuntimeConnectionConfig: () => ({ mode: "local" }),
  refreshBriefReadStatesInView: vi.fn(),
  retryPendingReadMarker: vi.fn(),
  useRuntimeStore: (select: (state: typeof mocks.state) => unknown) => select(mocks.state),
}));
vi.mock("./conversation-scope-store", async (original) => ({
  ...await original<typeof import("./conversation-scope-store")>(),
  acquireConversationScope: mocks.acquire,
  releaseConversationScope: mocks.release,
  peekConversationScope: mocks.peek,
}));

import { conversationCacheKey, resolveConversationScopeKey, useConversationScopeStore } from "./conversation-scope-store";
import { refreshBriefReadStatesInView, retryPendingReadMarker } from "./runtime-store";
import { useConversationSession } from "./useConversationSession";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root;
let container: HTMLDivElement;
let current: ReturnType<typeof useConversationSession>;
function Probe({ id, incarnation, readOnly }: { id?: string; incarnation?: number; readOnly?: boolean }) {
  current = useConversationSession(id, undefined, incarnation, readOnly);
  return null;
}
async function render(id?: string, incarnation?: number, readOnly?: boolean) {
  await act(async () => { root.render(createElement(Probe, { id, incarnation, readOnly })); });
}
beforeEach(() => {
  mocks.acquire.mockReset();
  mocks.release.mockReset();
  mocks.peek.mockReset().mockReturnValue(null);
  vi.mocked(retryPendingReadMarker).mockClear();
  vi.mocked(refreshBriefReadStatesInView).mockClear();
  useConversationScopeStore.setState({ scopes: {} });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("conversation session identity binding", () => {
  it("blocks preview network actions and pending read-marker writes even with a ready cached view", async () => {
    const actions = {
      loadOlderHistory: vi.fn(), loadNewerHistory: vi.fn(), loadDetail: vi.fn(),
      loadOlderActivities: vi.fn(), loadBrief: vi.fn(), locateSource: vi.fn(),
      returnToLatest: vi.fn(), retry: vi.fn(),
    };
    const restoreCachedBrief = vi.fn();
    mocks.peek.mockReturnValue({
      ...actions, restoreCachedBrief, browseState: () => null, historyState: () => ({ kind: "idle" }),
      view: () => ({ reset_reason: null }),
    });
    const key = resolveConversationScopeKey("local", "child", mocks.state.currentUser, 1);
    const view = { scope: {}, turns: [], pending_inputs: [], details: [], through_seq: 1,
      next_before_cursor: null, has_more: false, checkpoint: null, reset_reason: null } as never;
    useConversationScopeStore.setState({ scopes: { [key]: {
      status: { kind: "ready" }, view, displayView: view, version: 1,
    } } });
    await render("child", 1, true);
    act(() => {
      current.loadOlderHistory(); current.loadNewerHistory(); current.loadDetail("t");
      current.loadOlderActivities("t"); current.loadBrief("b"); current.locateSource();
      current.returnToLatest(); current.retry();
    });
    for (const action of Object.values(actions)) expect(action).not.toHaveBeenCalled();
    expect(restoreCachedBrief).toHaveBeenCalledWith("b");
    expect(retryPendingReadMarker).not.toHaveBeenCalled();
    expect(refreshBriefReadStatesInView).not.toHaveBeenCalled();
    expect(current.conversationReady).toBe(false);
    await render("child", 1, false);
    expect(current.conversationReady).toBe(true);
    expect(retryPendingReadMarker).toHaveBeenCalledWith("child");
    vi.mocked(retryPendingReadMarker).mockClear();
    await act(async () => {
      useConversationScopeStore.setState({ scopes: { [key]: {
        status: { kind: "ready" }, view: {
          ...useConversationScopeStore.getState().scopes[key].view!, through_seq: 2,
        },
        displayView: view, version: 2,
      } } });
    });
    expect(retryPendingReadMarker).toHaveBeenCalledWith("child");
    act(() => current.loadOlderHistory());
    expect(actions.loadOlderHistory).toHaveBeenCalledTimes(1);
  });

  it("revalidates unread state on a hidden conversation head update without a roster refresh", async () => {
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    const key = resolveConversationScopeKey("local", "child", mocks.state.currentUser, 1);
    const view = { scope: {}, turns: [], pending_inputs: [], details: [], through_seq: 1,
      next_before_cursor: null, has_more: false, checkpoint: null, reset_reason: null } as never;
    try {
      useConversationScopeStore.setState({ scopes: { [key]: {
        status: { kind: "ready" }, view, displayView: view, version: 1,
      } } });
      await render("child", 1, false);
      vi.mocked(refreshBriefReadStatesInView).mockClear();
      await act(async () => {
        const nextView = { ...useConversationScopeStore.getState().scopes[key].view!, through_seq: 2 };
        useConversationScopeStore.setState({ scopes: { [key]: {
          status: { kind: "ready" }, view: nextView, displayView: nextView, version: 2,
        } } });
      });
      expect(refreshBriefReadStatesInView).toHaveBeenCalledExactlyOnceWith(true);
      await render("child", 1, false);
      expect(refreshBriefReadStatesInView).toHaveBeenCalledTimes(1);
    } finally {
      visibility.mockRestore();
    }
  });

  it("attaches previews explicitly read-only and does not expose readiness or network actions", async () => {
    await render("child", 1, true);
    expect(mocks.acquire).toHaveBeenLastCalledWith(expect.objectContaining({ readOnly: true, incarnation: 1 }));
    expect(current.conversationReady).toBe(false);
    expect(current.controller).toBeNull();
    act(() => {
      current.retry();
      current.loadOlderHistory();
      current.loadNewerHistory();
      current.loadDetail("turn");
      current.loadOlderActivities("turn");
      current.loadBrief("brief");
      current.locateSource();
      current.returnToLatest();
    });
    const key = current.scopeKey;
    await render("child", 1, false);
    expect(mocks.release).toHaveBeenCalledWith(key, true);
    expect(mocks.acquire).toHaveBeenLastCalledWith(expect.objectContaining({ readOnly: false, key }));
  });

  it("binds controller acquisition and persistent storage to the admitted incarnation", async () => {
    await render("child", 1);
    const oldKey = resolveConversationScopeKey("local", "child", mocks.state.currentUser, 1);
    expect(current.scopeKey).toBe(oldKey);
    expect(mocks.acquire).toHaveBeenLastCalledWith(expect.objectContaining({
      key: oldKey, agentId: "child", incarnation: 1,
      cacheKey: conversationCacheKey("local", mocks.state.currentUser, 1),
    }));
    await render("child", 2);
    expect(mocks.release).toHaveBeenCalledWith(oldKey, false);
    expect(current.scopeKey).not.toBe(oldKey);
    expect(mocks.acquire).toHaveBeenLastCalledWith(expect.objectContaining({
      incarnation: 2,
      cacheKey: conversationCacheKey("local", mocks.state.currentUser, 2),
    }));
  });

  it("does not start before admission and keeps legacy unpinned callers compatible", async () => {
    await render();
    expect(current.scopeKey).toBeNull();
    expect(mocks.acquire).not.toHaveBeenCalled();
    await render("child");
    expect(mocks.acquire).toHaveBeenLastCalledWith(expect.objectContaining({
      incarnation: undefined,
      key: resolveConversationScopeKey("local", "child", mocks.state.currentUser),
    }));
  });
});
