// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  acquire: vi.fn(),
  release: vi.fn(),
  state: {
    currentUser: { authMethod: "token", userId: "alice" },
    currentUserLoaded: true,
    bootstrap: { connection: { mode: "local", baseUrl: "/api", authRequired: false } },
    sessionsByAgentId: {},
    pruneOperatorPrompts: vi.fn(),
  },
}));
vi.mock("./runtime-store", () => ({
  getRuntimeConnectionConfig: () => ({ mode: "local" }),
  retryPendingReadMarker: vi.fn(),
  useRuntimeStore: (select: (state: typeof mocks.state) => unknown) => select(mocks.state),
}));
vi.mock("./conversation-scope-store", async (original) => ({
  ...await original<typeof import("./conversation-scope-store")>(),
  acquireConversationScope: mocks.acquire,
  releaseConversationScope: mocks.release,
}));

import { conversationCacheKey, resolveConversationScopeKey } from "./conversation-scope-store";
import { useConversationSession } from "./useConversationSession";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root;
let container: HTMLDivElement;
let current: ReturnType<typeof useConversationSession>;
function Probe({ id, incarnation }: { id?: string; incarnation?: number }) {
  current = useConversationSession(id, undefined, incarnation);
  return null;
}
async function render(id?: string, incarnation?: number) {
  await act(async () => { root.render(createElement(Probe, { id, incarnation })); });
}
beforeEach(() => {
  mocks.acquire.mockReset();
  mocks.release.mockReset();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("conversation session identity binding", () => {
  it("binds controller acquisition and persistent storage to the admitted incarnation", async () => {
    await render("child", 1);
    const oldKey = resolveConversationScopeKey("local", "child", mocks.state.currentUser, 1);
    expect(current.scopeKey).toBe(oldKey);
    expect(mocks.acquire).toHaveBeenLastCalledWith(expect.objectContaining({
      key: oldKey, agentId: "child", incarnation: 1,
      cacheKey: conversationCacheKey("local", mocks.state.currentUser, 1),
    }));
    await render("child", 2);
    expect(mocks.release).toHaveBeenCalledWith(oldKey);
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
