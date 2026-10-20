// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  identity: vi.fn(),
  state: vi.fn(),
  connection: { mode: "local", baseUrl: "/api" },
  user: { authMethod: "token", userId: "alice" },
  userLoaded: true,
  token: undefined as string | undefined,
  cached: vi.fn(),
  persist: vi.fn(),
  remove: vi.fn(),
}));
vi.mock("./runtime-store", () => ({
  getRuntimeConnectionConfig: () => ({ mode: "local", token: mocks.token }),
  useRuntimeStore: (select: (state: unknown) => unknown) => select({ bootstrap: { connection: mocks.connection, agents: [] }, currentUser: mocks.user, currentUserLoaded: mocks.userLoaded }),
}));
vi.mock("./client", async (original) => ({
  ...await original<typeof import("./client")>(),
  createRuntimeClient: () => ({ getAgentIdentity: mocks.identity, getAgentState: mocks.state }),
}));
vi.mock("./agent-presentation-cache", async (original) => ({
  ...await original<typeof import("./agent-presentation-cache")>(),
  readCachedAgents: mocks.cached, writeCachedAgents: mocks.persist, removeCachedAgent: mocks.remove,
}));

import { RuntimeHttpError } from "./client";
import { ConversationHttpError } from "@holon/conversation-sdk";
import { useAgentAccess } from "./useAgentAccess";
import { AGENT_ACCESS_KEEP_ALIVE, agentAccessRecordCount, clearAgentAccessRecords } from "./agent-access-records";
import { resolveConversationScopeKey, useConversationScopeStore } from "./conversation-scope-store";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root;
let container: HTMLDivElement;
let current: ReturnType<typeof useAgentAccess>;
function childDetail(supervision: Record<string, unknown> = {}, resolution = "resolved") {
  return {
    identity: { agent_id: "private-child", incarnation: 1, status: "active", parent_agent_id: "legacy-parent", delegated_from_task_id: "legacy-task" },
    canonical_relations: {
      agent_id: "private-child", resolution,
      supervision: {
        supervision_id: "supervision-1", supervisor_agent_id: "parent", child_agent_id: "private-child",
        delegated_from_task_id: "task-1", state: "active", ...supervision,
      },
    },
  };
}
function withChild(detail: unknown, parent: unknown = { identity: { agent_id: "parent", status: "active" } }) {
  mocks.identity.mockImplementation(async (id: string) => id === "parent" ? parent : detail);
}
function Probe({ id, incarnation }: { id: string; incarnation?: number }) {
  current = useAgentAccess(id, incarnation);
  return null;
}
async function render(id = "private-child", incarnation?: number) {
  await act(async () => { root.render(createElement(Probe, { id, incarnation })); });
}
function warm(id = "private-child", incarnation = 1) {
  const key = resolveConversationScopeKey("local", id, mocks.user, incarnation);
  useConversationScopeStore.setState((state) => ({ scopes: { ...state.scopes, [key]: {
    status: { kind: "paused" }, view: null, version: 1,
    displayView: { scope: {}, turns: [{ turn_id: "cached" }], pending_inputs: [] } as never,
  } } }));
  return key;
}
function deferred<T = unknown>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
beforeEach(() => {
  vi.useFakeTimers();
  clearAgentAccessRecords();
  mocks.user = { authMethod: "token", userId: "alice" };
  mocks.userLoaded = true;
  mocks.connection = { mode: "local", baseUrl: "/api" };
  mocks.token = undefined;
  mocks.cached.mockReset().mockResolvedValue([]);
  mocks.persist.mockReset().mockResolvedValue(undefined);
  mocks.remove.mockReset().mockResolvedValue(undefined);
  useConversationScopeStore.setState({ scopes: {} });
  mocks.identity.mockReset();
  withChild(childDetail());
  mocks.state.mockReset().mockImplementation(async (id: string) => ({ id, incarnation: 1, acceptsInput: true }));
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  clearAgentAccessRecords();
  container.remove();
  vi.useRealTimers();
});

describe("route-selected agent admission", () => {
  it.each([
    [401, undefined, "unauthorized"], [403, undefined, "unauthorized"],
    [404, undefined, "missing"], [409, "agent_incarnation_mismatch", "replaced"],
  ] as const)("revokes warm access on a definitive prompt rejection %s/%s", async (status, code, rejection) => {
    await render();
    let key = "";
    act(() => { key = warm(); });
    await render("other");
    const identity = deferred();
    mocks.identity.mockReturnValue(identity.promise);
    await render();
    expect(current.status).toBe("revalidating");
    const signal = mocks.state.mock.calls.at(-1)?.[1] as AbortSignal;
    act(() => current.rejectInput(new RuntimeHttpError("POST", "/prompt", status, "Rejected", code)));
    expect(current.status).toBe(rejection);
    expect(current.agent).toBeUndefined();
    expect(signal.aborted).toBe(true);
    expect(useConversationScopeStore.getState().scopes[key]?.displayView).toBeUndefined();
    await act(async () => identity.resolve(childDetail()));
    expect(current.status).toBe(rejection);
  });

  it.each([429, 503, 409])("does not revoke read access for a non-identity prompt failure %s", async (status) => {
    await render();
    act(() => current.rejectInput(new RuntimeHttpError("POST", "/prompt", status)));
    expect(current.status).toBe("available");
    expect(current.agent?.incarnation).toBe(1);
  });

  it.each([429, 503, "timeout", "network"] as const)(
    "retains only continuous online admission through a transient background %s and clears its notice on recovery",
    async (failure) => {
      await render();
      expect(current.status).toBe("available");
      const error = typeof failure === "number" ? new RuntimeHttpError("GET", "/state", failure)
        : failure === "timeout" ? new DOMException("Request timed out", "AbortError") : new TypeError("Failed to fetch");
      mocks.state.mockRejectedValue(error);
      await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
      expect(current.status).toBe("available");
      expect(current.agent?.acceptsInput).toBe(true);
      expect(current.error).toBeTruthy();
      mocks.state.mockResolvedValue({ id: "private-child", incarnation: 1, acceptsInput: true });
      await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
      expect(current.status).toBe("available");
      expect(current.error).toBeUndefined();
    },
  );

  it("never grants initial admission from cached presentation after a transient state failure", async () => {
    mocks.cached.mockResolvedValue([{ id: "private-child", incarnation: 1, acceptsInput: false }]);
    mocks.state.mockRejectedValue(new RuntimeHttpError("GET", "/state", 503));
    await render();
    expect(current.status).toBe("error");
    expect(current.agent?.acceptsInput).toBe(false);
  });

  it("does not retain admission across route reentry even for a transient refresh error", async () => {
    await render();
    await render("other");
    mocks.state.mockRejectedValue(new RuntimeHttpError("GET", "/state", 503));
    await render();
    expect(current.status).toBe("error");
  });

  it("does not disguise an invalid identity response as a transient background refresh failure", async () => {
    await render();
    mocks.identity.mockResolvedValue({ identity: { agent_id: "private-child", incarnation: 0, status: "active" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(current.status).toBe("error");
  });

  it.each([401, 403, 404])("revokes previously admitted access immediately after a background HTTP %s", async (status) => {
    await render();
    mocks.state.mockRejectedValue(new RuntimeHttpError("GET", "/state", status));
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(current.status).toBe(status === 404 ? "missing" : "unauthorized");
    expect(current.agent).toBeUndefined();
    expect(mocks.remove).toHaveBeenCalledTimes(2);
    const calls = mocks.state.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
    expect(mocks.state).toHaveBeenCalledTimes(calls);
  });

  it("backs off consecutive transient failures instead of polling every five seconds", async () => {
    await render();
    mocks.state.mockRejectedValue(new RuntimeHttpError("GET", "/state", 503));
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(mocks.state).toHaveBeenCalledTimes(3);
    await act(async () => { await vi.advanceTimersByTimeAsync(9999); });
    expect(mocks.state).toHaveBeenCalledTimes(3);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(mocks.state).toHaveBeenCalledTimes(4);
  });

  it("restores a persisted pinned summary without a warm in-memory scope or online responses", async () => {
    mocks.cached.mockResolvedValue([{ id: "private-child", incarnation: 1, name: "Cached child", acceptsInput: false }]);
    mocks.identity.mockReturnValue(new Promise(() => {}));
    mocks.state.mockReturnValue(new Promise(() => {}));
    await render();
    expect(current.status).toBe("revalidating");
    expect(current.agent?.name).toBe("Cached child");
    expect(current.agent?.acceptsInput).toBe(false);
    expect(mocks.identity).toHaveBeenCalledTimes(1);
    expect(mocks.state).toHaveBeenCalledTimes(1);
    expect(mocks.persist).not.toHaveBeenCalled();
  });

  it("does not restore a summary from another explicitly linked incarnation", async () => {
    mocks.cached.mockResolvedValue([{ id: "private-child", incarnation: 1 }]);
    mocks.identity.mockReturnValue(new Promise(() => {}));
    mocks.state.mockReturnValue(new Promise(() => {}));
    await render("private-child", 2);
    expect(current.status).toBe("loading");
    expect(current.agent).toBeUndefined();
  });

  it("ignores cached presentation that arrives after online admission", async () => {
    const cached = deferred();
    mocks.cached.mockReturnValue(cached.promise);
    await render();
    expect(current.status).toBe("available");
    await act(async () => { cached.resolve([{ id: "private-child", incarnation: 1, name: "Stale" }]); });
    expect(current.status).toBe("available");
    expect(current.agent?.name).not.toBe("Stale");
    expect(mocks.persist).toHaveBeenCalled();
  });

  it.each([401, 403, 404])("ignores late cached presentation after HTTP %s and purges both summary buckets", async (status) => {
    const cached = deferred();
    mocks.cached.mockReturnValue(cached.promise);
    mocks.identity.mockRejectedValue(new RuntimeHttpError("GET", "/detail", status, "denied"));
    mocks.state.mockReturnValue(new Promise(() => {}));
    await render();
    await act(async () => { cached.resolve([{ id: "private-child", incarnation: 1 }]); });
    expect(current.agent).toBeUndefined();
    expect(current.status).toBe(status === 404 ? "missing" : "unauthorized");
    expect(mocks.remove).toHaveBeenCalledTimes(2);
  });

  it("pins cached incarnation before state resolves and revokes it on replacement", async () => {
    const identity = deferred();
    mocks.cached.mockResolvedValue([{ id: "private-child", incarnation: 1 }]);
    mocks.identity.mockReturnValue(identity.promise);
    mocks.state.mockReturnValue(new Promise(() => {}));
    await render();
    expect(current.status).toBe("revalidating");
    await act(async () => { identity.resolve({ identity: { agent_id: "private-child", incarnation: 2, status: "active" } }); });
    expect(current.status).toBe("replaced");
    expect(current.agent).toBeUndefined();
    expect(mocks.remove).toHaveBeenCalledTimes(2);
  });

  it("revokes admission immediately when the conversation identity fence rejects", async () => {
    await render();
    act(() => {
      const key = warm();
      useConversationScopeStore.setState((state) => ({ scopes: { ...state.scopes, [key]: {
        ...state.scopes[key],
        status: { kind: "terminal_error", error: new ConversationHttpError(403, {
          ok: false, code: "conversation_identity_changed", error: "replaced",
        }) },
      } } }));
    });
    expect(current.status).toBe("replaced");
    expect(current.agent).toBeUndefined();
    const count = mocks.identity.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(10000); });
    expect(mocks.identity).toHaveBeenCalledTimes(count);
  });

  it("shares child and parent in-flight requests for concurrent known pinned links", async () => {
    const parent = deferred();
    mocks.identity.mockImplementation(async (id: string) => id === "parent" ? parent.promise : childDetail());
    await render();
    const second = createRoot(document.createElement("div"));
    try {
      await act(async () => second.render(createElement(Probe, { id: "private-child", incarnation: 1 })));
      expect(mocks.state).toHaveBeenCalledTimes(1);
      expect(mocks.identity.mock.calls.filter(([id]) => id === "parent")).toHaveLength(1);
      expect(current.status).toBe("available");
      await act(async () => parent.resolve({ identity: { agent_id: "parent", status: "active" } }));
      expect(current.parentAgentId).toBe("parent");
    } finally {
      act(() => second.unmount());
    }
  });

  it.each([[401, "unauthorized"], [403, "unauthorized"], [404, "missing"]] as const)(
    "handles terminal state HTTP %s without waiting for a hung identity", async (code, status) => {
      mocks.identity.mockReturnValue(new Promise(() => {}));
      mocks.state.mockRejectedValue(new RuntimeHttpError("GET", "/state", code));
      await render();
      expect(current.status).toBe(status);
      expect(mocks.identity.mock.calls[0][1].aborted).toBe(true);
    },
  );

  it.each([
    [false, "deleting", 409], [false, "deleted", 410], [false, "replaced", 410],
    [true, "deleting", 409], [true, "deleted", 410], [true, "replaced", 410],
    [false, "deleted", 500],
  ] as const)("resolves state failure through identity (warm=%s, %s, HTTP %s)", async (isWarm, status, code) => {
    let key: string | undefined;
    if (isWarm) {
      await render();
      act(() => { key = warm(); });
      await render("other");
    }
    const identity = deferred();
    mocks.identity.mockImplementation(async (id: string) => id === "parent"
      ? { identity: { agent_id: "parent", status: "active" } } : identity.promise);
    mocks.state.mockRejectedValue(new RuntimeHttpError("GET", "/state", code));
    await render("private-child", 1);
    expect(current.status).toBe(isWarm ? "revalidating" : "loading");
    const signal = mocks.state.mock.calls.at(-1)?.[1] as AbortSignal;
    expect(signal.aborted).toBe(false);
    const detail = childDetail({ state: "cleanup_required" });
    detail.identity.status = status === "replaced" ? "active" : status;
    detail.identity.incarnation = status === "replaced" ? 2 : 1;
    await act(async () => identity.resolve(detail));
    expect(current.status).toBe(status);
    expect(current.agent).toBeUndefined();
    expect(current.parentAgentId).toBe(status === "replaced" ? undefined : "parent");
    expect(signal.aborted).toBe(true);
    if (key) expect(useConversationScopeStore.getState().scopes[key]?.displayView).toBeUndefined();
    const childCalls = () => mocks.identity.mock.calls.filter(([id]) => id === "private-child").length;
    const count = childCalls();
    await act(async () => { await vi.advanceTimersByTimeAsync(15000); });
    expect(childCalls()).toBe(count);
  });

  it.each([[401, "unauthorized"], [403, "unauthorized"], [404, "missing"]] as const)(
    "preserves identity HTTP %s after state 410 as %s", async (code, status) => {
      const identity = deferred();
      mocks.identity.mockReturnValue(identity.promise);
      mocks.state.mockRejectedValue(new RuntimeHttpError("GET", "/state", 410));
      await render();
      await act(async () => identity.reject(new RuntimeHttpError("GET", "/detail", code)));
      expect(current.status).toBe(status);
      expect(current.agent).toBeUndefined();
    },
  );

  it.each([401, 403, 404])("recovers from HTTP %s on explicit retry or route reentry without automatic polling", async (code) => {
    mocks.identity.mockRejectedValue(new RuntimeHttpError("GET", "/detail", code));
    await render();
    expect(current.status).toBe(code === 404 ? "missing" : "unauthorized");
    await act(async () => { await vi.advanceTimersByTimeAsync(15000); });
    expect(mocks.state).toHaveBeenCalledTimes(1);
    withChild(childDetail());
    await act(async () => current.retry());
    expect(current.status).toBe("available");

    mocks.identity.mockRejectedValue(new RuntimeHttpError("GET", "/detail", code));
    await act(async () => current.retry());
    expect(current.status).toBe(code === 404 ? "missing" : "unauthorized");
    await render("other");
    withChild(childDetail());
    const calls = mocks.state.mock.calls.length;
    await render();
    expect(current.status).toBe("available");
    expect(mocks.state).toHaveBeenCalledTimes(calls + 1);
  });

  it.each([401, 403, 404])("retains the incarnation pin when retrying HTTP %s", async (code) => {
    await render();
    expect(current.status).toBe("available");
    mocks.identity.mockRejectedValue(new RuntimeHttpError("GET", "/detail", code));
    await act(async () => current.retry());
    expect(current.agent).toBeUndefined();
    const replacement = childDetail();
    replacement.identity.incarnation = 2;
    withChild(replacement);
    mocks.state.mockResolvedValue({ id: "private-child", incarnation: 2 });
    await act(async () => current.retry());
    expect(current.status).toBe("replaced");
    expect(current.agent).toBeUndefined();
    const calls = mocks.state.mock.calls.length;
    await act(async () => current.retry());
    await render("other");
    const afterOther = mocks.state.mock.calls.length;
    await render();
    expect(current.status).toBe("replaced");
    expect(afterOther).toBe(calls + 1);
    expect(mocks.state).toHaveBeenCalledTimes(afterOther);
  });

  it("does not infer a deleted identity from state 410 alone", async () => {
    const identity = deferred();
    mocks.identity.mockReturnValue(identity.promise);
    mocks.state.mockRejectedValue(new RuntimeHttpError("GET", "/state", 410));
    await render();
    expect(current.status).toBe("loading");
    await act(async () => identity.resolve(childDetail()));
    expect(current.status).toBe("error");
    expect(current.agent).toBeUndefined();
  });

  it.each([undefined, 0, -1, NaN, 1.5])("does not admit a malformed incarnation %s", async (incarnation) => {
    mocks.identity.mockResolvedValue({ identity: { agent_id: "private-child", incarnation, status: "active" } });
    mocks.state.mockResolvedValue({ id: "private-child", incarnation });
    await render();
    expect(current.status).toBe("error");
    expect(current.agent).toBeUndefined();
  });

  it("starts identity and state together, but never admits a state alone", async () => {
    const identity = deferred();
    mocks.identity.mockReturnValue(identity.promise);
    await render();
    expect(mocks.identity).toHaveBeenCalledTimes(1);
    expect(mocks.state).toHaveBeenCalledTimes(1);
    expect(current.status).toBe("loading");
    expect(current.agent).toBeUndefined();
    await act(async () => identity.resolve(childDetail()));
    expect(current.status).toBe("available");
  });

  it("restores pinned warm history before either revalidation finishes", async () => {
    await render();
    act(() => { warm(); });
    await render("other");
    const identity = deferred();
    const state = deferred();
    mocks.identity.mockReturnValue(identity.promise);
    mocks.state.mockReturnValue(state.promise);
    await render();
    expect(current.status).toBe("revalidating");
    expect(current.agent?.incarnation).toBe(1);
    expect(mocks.state.mock.calls.at(-1)?.[1].aborted).toBe(false);
    await act(async () => identity.resolve(childDetail()));
    expect(current.status).toBe("revalidating");
    await act(async () => state.resolve({ id: "private-child", incarnation: 1 }));
    expect(current.status).toBe("available");
  });

  it("does not use unpinned or empty history as a preview", async () => {
    act(() => { warm(); });
    mocks.identity.mockReturnValue(new Promise(() => {}));
    await render();
    expect(current.status).toBe("loading");
    expect(current.agent).toBeUndefined();
  });

  it("retains only read-only history after a transport error and retries explicitly", async () => {
    await render();
    act(() => { warm(); });
    await render("other");
    mocks.identity.mockRejectedValue(new Error("offline"));
    await render();
    expect(current.status).toBe("error");
    expect(current.agent?.incarnation).toBe(1);
    mocks.identity.mockReturnValue(new Promise(() => {}));
    await act(async () => current.retry());
    expect(current.status).toBe("revalidating");
    expect(current.agent?.incarnation).toBe(1);
  });

  it.each([401, 403, 404, "deleted", "deleting", "replaced"] as const)(
    "removes warm history and stops automatic access retries after %s", async (failure) => {
      await render();
      let key!: string;
      act(() => { key = warm(); });
      await render("other");
      if (typeof failure === "number") {
        mocks.identity.mockRejectedValue(new RuntimeHttpError("GET", "/detail", failure));
      } else {
        mocks.identity.mockResolvedValue({ identity: { agent_id: "private-child", incarnation: failure === "replaced" ? 2 : 1,
          status: failure === "replaced" ? "active" : failure } });
      }
      await render();
      expect(current.agent).toBeUndefined();
      expect(useConversationScopeStore.getState().scopes[key]?.displayView).toBeUndefined();
      const count = mocks.identity.mock.calls.length;
      await act(async () => { await vi.advanceTimersByTimeAsync(15000); });
      expect(mocks.identity).toHaveBeenCalledTimes(count);
    },
  );

  it.each(["user", "connection", "token", "unknown-user"] as const)(
    "does not preview history across a changed %s", async (change) => {
      await render();
      act(() => { warm(); });
      await render("other");
      if (change === "user") mocks.user = { ...mocks.user, userId: "bob" };
      if (change === "connection") mocks.connection = { mode: "remote", baseUrl: "http://other/api" };
      if (change === "token") mocks.token = "different";
      if (change === "unknown-user") mocks.userLoaded = false;
      mocks.identity.mockReturnValue(new Promise(() => {}));
      await render();
      expect(current.status).toBe("loading");
      expect(current.agent).toBeUndefined();
    },
  );

  it("merges mounted readers' validation and aborts only after the final release", async () => {
    mocks.identity.mockReturnValue(new Promise(() => {}));
    const secondContainer = document.createElement("div");
    const second = createRoot(secondContainer);
    await render();
    await act(async () => second.render(createElement(Probe, { id: "private-child" })));
    expect(mocks.identity).toHaveBeenCalledTimes(1);
    expect(mocks.state).toHaveBeenCalledTimes(1);
    const signal = mocks.state.mock.calls[0][1] as AbortSignal;
    await render("other");
    expect(signal.aborted).toBe(false);
    act(() => second.unmount());
    expect(signal.aborted).toBe(true);
  });

  it("ignores an aborted A response after A → B → A even for the same pinned scope", async () => {
    await render();
    act(() => { warm(); });
    await render("other");
    const old = deferred();
    mocks.identity.mockReturnValue(old.promise);
    await render();
    const oldSignal = mocks.state.mock.calls.at(-1)?.[1] as AbortSignal;
    await render("other");
    const fresh = deferred();
    mocks.identity.mockReturnValue(fresh.promise);
    await render();
    expect(oldSignal.aborted).toBe(true);
    await act(async () => old.resolve({ identity: { agent_id: "private-child", incarnation: 99, status: "active" } }));
    expect(current.status).toBe("revalidating");
    expect(current.agent?.incarnation).toBe(1);
    await act(async () => fresh.resolve(childDetail()));
    expect(current.status).toBe("available");
    expect(current.agent?.incarnation).toBe(1);
  });

  it("bounds released access records rather than retaining every visited agent", async () => {
    mocks.identity.mockImplementation(async (id: string) => ({ identity: { agent_id: id, incarnation: 1, status: "active" } }));
    for (let i = 0; i < AGENT_ACCESS_KEEP_ALIVE + 5; i++) await render(`agent-${i}`);
    expect(agentAccessRecordCount()).toBe(AGENT_ACCESS_KEEP_ALIVE + 1);
  });

  it("loads a private child without adding it to the public roster", async () => {
    await render();
    expect(current.status).toBe("available");
    expect(current.agent?.id).toBe("private-child");
    expect(current.parentAgentId).toBe("parent");
    expect(current.parentTaskId).toBe("task-1");
    expect(mocks.identity).toHaveBeenCalledWith("parent", expect.any(AbortSignal));
  });

  it.each([
    { lineage_parent_agent_id: "creator" },
    { lineage_parent_agent_id: "default", parent_agent_id: "default" },
    { profile_preset: "private_child", parent_agent_id: "creator" },
    { ownership: "parent_supervised", parent_agent_id: "creator" },
  ])("does not infer navigation from legacy identity fields: %j", async (identity) => {
    mocks.identity.mockResolvedValue({ identity: { agent_id: "private-child", incarnation: 1, status: "active", ...identity } });
    await render();
    expect(current.status).toBe("available");
    expect(current.parentAgentId).toBeUndefined();
    expect(current.parentTaskId).toBeUndefined();
    expect(mocks.identity).toHaveBeenCalledTimes(1);
  });

  it.each(["ambiguous", "contradictory", "missing_evidence"])("hides an unresolved %s supervision", async (resolution) => {
    withChild(childDetail({}, resolution));
    await render();
    expect(current.parentAgentId).toBeUndefined();
    expect(mocks.identity).toHaveBeenCalledTimes(1);
  });

  it.each([
    { state: "closed" },
    { child_agent_id: "other-child" },
    { supervisor_agent_id: "private-child" },
    { supervisor_agent_id: "default" },
  ])("rejects invalid navigation without probing a legacy route: %j", async (supervision) => {
    withChild(childDetail(supervision));
    await render();
    expect(current.parentAgentId).toBeUndefined();
    expect(mocks.identity).toHaveBeenCalledTimes(1);
  });

  it.each(["deleting", "deleted"])("hides a %s parent without disabling child input", async (status) => {
    withChild(childDetail(), { identity: { agent_id: "parent", status } });
    await render();
    expect(current.parentAgentId).toBeUndefined();
    expect(current.agent?.acceptsInput).toBe(true);
  });

  it.each([401, 403, 404, 500])("ignores parent HTTP %s independently of the child", async (code) => {
    mocks.identity.mockImplementation(async (id: string) => {
      if (id === "parent") throw new RuntimeHttpError("GET", "/parent/detail", code);
      return childDetail();
    });
    await render();
    expect(current.status).toBe("available");
    expect(current.parentAgentId).toBeUndefined();
    expect(current.agent?.acceptsInput).toBe(true);
  });

  it("does not substitute a differently resolved identity for the supervisor", async () => {
    withChild(childDetail(), { identity: { agent_id: "somewhere-else", status: "active" } });
    await render();
    expect(current.parentAgentId).toBeUndefined();
  });

  it("does not hold child admission behind a slow parent lookup", async () => {
    let resolveParent!: (value: unknown) => void;
    mocks.identity.mockImplementation(async (id: string) => id === "parent"
      ? new Promise((resolve) => { resolveParent = resolve; }) : childDetail());
    await render();
    expect(current.status).toBe("available");
    expect(current.agent?.acceptsInput).toBe(true);
    expect(current.parentAgentId).toBeUndefined();
    await act(async () => resolveParent({ identity: { agent_id: "parent", status: "active" } }));
    expect(current.parentAgentId).toBe("parent");
  });

  it("removes the return link when the parent disappears during polling", async () => {
    await render();
    withChild(childDetail(), { identity: { agent_id: "parent", status: "deleted" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(current.parentAgentId).toBeUndefined();
    expect(current.status).toBe("available");
  });

  it("keeps cleanup navigation for a deleted child while cancelling parallel state", async () => {
    const detail = childDetail({ state: "cleanup_required" });
    detail.identity.status = "deleted";
    withChild(detail);
    await render();
    expect(current.status).toBe("deleted");
    expect(current.parentAgentId).toBe("parent");
    expect(mocks.state).toHaveBeenCalledTimes(1);
    expect(mocks.state.mock.calls[0][1].aborted).toBe(true);
  });

  it("discards a late parent lookup after navigating to an independent agent", async () => {
    let resolveParent!: (value: unknown) => void;
    mocks.identity.mockImplementation(async (id: string) => id === "parent"
      ? new Promise((resolve) => { resolveParent = resolve; })
      : id === "private-child" ? childDetail() : { identity: { agent_id: id, incarnation: 1, status: "active" } });
    await render();
    await render("independent");
    await act(async () => resolveParent({ identity: { agent_id: "parent", status: "active" } }));
    expect(current.parentAgentId).toBeUndefined();
    expect(current.status).toBe("available");
  });

  it.each(["deleting", "deleted"] as const)("keeps %s distinct and cancels parallel state", async (status) => {
    mocks.identity.mockResolvedValue({ identity: { status, incarnation: 1 } });
    await render();
    expect(current.status).toBe(status);
    expect(mocks.state).toHaveBeenCalledTimes(1);
    expect(mocks.state.mock.calls[0][1].aborted).toBe(true);
  });

  it.each([[401, "unauthorized"], [403, "unauthorized"], [404, "missing"], [500, "error"]] as const)(
    "preserves HTTP %s as %s instead of redirecting", async (code, status) => {
      mocks.identity.mockRejectedValue(new RuntimeHttpError("GET", "/detail", code));
      await render();
      expect(current.status).toBe(status);
      expect(current.agent).toBeUndefined();
    },
  );

  it("rejects a stale task incarnation before opening the state or conversation", async () => {
    await render("private-child", 2);
    expect(current.status).toBe("replaced");
    expect(current.parentAgentId).toBeUndefined();
    expect(mocks.state).toHaveBeenCalledTimes(1);
    expect(mocks.state.mock.calls[0][1].aborted).toBe(true);
    expect(mocks.identity).not.toHaveBeenCalledWith("parent", expect.any(AbortSignal));
  });

  it("pins a bare URL identity across polling and retry", async () => {
    await render();
    mocks.identity.mockResolvedValue({ identity: { status: "active", incarnation: 2 } });
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(current.status).toBe("replaced");
    await act(async () => current.retry());
    expect(current.status).toBe("replaced");
    expect(mocks.state).toHaveBeenCalledTimes(2);
  });

  it("rejects an identity change between detail and state", async () => {
    mocks.state.mockResolvedValue({ id: "private-child", incarnation: 2 });
    await render();
    expect(current.status).toBe("replaced");
    expect(current.parentAgentId).toBeUndefined();
  });

  it("does not allow a late previous agent response to replace the route target", async () => {
    let resolve!: (value: unknown) => void;
    mocks.identity.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await render("old");
    mocks.identity.mockResolvedValue({ identity: { agent_id: "new", status: "active", incarnation: 1 } });
    await render("new");
    expect(current.status).toBe("available");
    await act(async () => resolve({ identity: { status: "deleted", incarnation: 1 } }));
    expect(current.status).toBe("available");
    expect(mocks.state).toHaveBeenCalledTimes(2);
  });
});
