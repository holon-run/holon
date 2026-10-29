import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  bootstrap: vi.fn(), user: vi.fn(), read: vi.fn(), write: vi.fn(), clear: vi.fn(),
  agentState: vi.fn(),
}));
vi.mock("./client", async (original) => ({
  ...await original<typeof import("./client")>(),
  createRuntimeClient: () => ({
    getBootstrap: mocks.bootstrap, getCurrentUser: mocks.user,
    getAgentState: mocks.agentState,
    getBriefReadStates: async () => ({ agents: [] }),
  }),
}));
vi.mock("./agent-presentation-cache", async (original) => ({
  ...await original<typeof import("./agent-presentation-cache")>(),
  readCachedAgents: mocks.read, writeCachedAgents: mocks.write,
}));
vi.mock("./conversation-cache-lifecycle", () => ({ clearConversationCaches: mocks.clear }));

import { agentPresentationCacheKey } from "./agent-presentation-cache";
import { RuntimeHttpError } from "./client";
import { useRuntimeStore } from "./runtime-store";
import type { AgentSummary, RuntimeBootstrap } from "./types";

const alice = { authMethod: "token", userId: "alice" };
const bob = { authMethod: "token", userId: "bob" };
const agent: AgentSummary = {
  id: "cached-agent", incarnation: 3, name: "Cached agent", badge: "CA", profile: "",
  lifecycle: "idle", acceptsInput: false, focusSummary: "", workspace: "", attention: "",
  model: "", footer: "", subtitle: "", lastBrief: "cached preview", lastTurnTime: "",
  pending: 0, activeTaskCount: 0, waitingCount: 0, posture: "", postureReason: "",
};
const initial = useRuntimeStore.getState();
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}
function online(agents: AgentSummary[] = []): RuntimeBootstrap {
  return { agents, metrics: [], attentionCount: 0, connection: { mode: "local", source: "http", summary: "Online" } };
}
let bootstrap: ReturnType<typeof deferred<RuntimeBootstrap>>;
let pending: Promise<void> | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  useRuntimeStore.setState(initial, true);
  bootstrap = deferred<RuntimeBootstrap>();
  mocks.bootstrap.mockReset().mockReturnValue(bootstrap.promise);
  mocks.user.mockReset().mockResolvedValue(alice);
  mocks.read.mockReset().mockResolvedValue([agent]);
  mocks.write.mockReset().mockResolvedValue(undefined);
  mocks.clear.mockReset().mockResolvedValue(undefined);
  mocks.agentState.mockReset();
});
afterEach(async () => {
  bootstrap.resolve(online());
  await pending;
  pending = undefined;
  useRuntimeStore.setState(initial, true);
  vi.clearAllTimers();
  vi.useRealTimers();
});
function refresh() {
  pending = useRuntimeStore.getState().refreshBootstrap({ syncEvents: false });
  return pending;
}

describe("authenticated reload-time agent presentation", () => {
  it("restores summaries while bootstrap is still pending, after independently confirming the subject", async () => {
    void refresh();
    await vi.waitFor(() => expect(useRuntimeStore.getState().agentRosterSource).toBe("cache"));
    expect(mocks.user).toHaveBeenCalledTimes(1);
    expect(mocks.bootstrap).toHaveBeenCalledTimes(1);
    expect(mocks.read).toHaveBeenCalledWith(agentPresentationCacheKey("local", alice));
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([agent]);
    expect(useRuntimeStore.getState().bootstrapLoading).toBe(true);
    expect(mocks.write).not.toHaveBeenCalled();
  });

  it("does not read private caches before current-session confirmation", async () => {
    const subject = deferred<typeof alice>();
    mocks.user.mockReturnValue(subject.promise);
    void refresh();
    await Promise.resolve();
    expect(mocks.read).not.toHaveBeenCalled();
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
    subject.resolve(bob);
    await vi.waitFor(() => expect(mocks.read).toHaveBeenCalledWith(agentPresentationCacheKey("local", bob)));
  });

  it("never reads persistent presentation without a confirmed user", async () => {
    mocks.user.mockResolvedValue(null);
    void refresh();
    await vi.waitFor(() => expect(useRuntimeStore.getState().currentUserLoaded).toBe(true));
    expect(mocks.read).not.toHaveBeenCalled();
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
  });

  it("does not mistake a transient subject lookup failure for a logout of the mounted session", async () => {
    useRuntimeStore.setState({
      currentUser: alice, currentUserLoaded: true, agentRosterSource: "http", bootstrap: online([agent]),
    });
    mocks.user.mockRejectedValue(new RuntimeHttpError("GET", "/auth/session/me", 503));
    void refresh();
    bootstrap.resolve(online([{ ...agent, name: "Another user's roster" }]));
    await pending;
    expect(useRuntimeStore.getState().currentUser).toEqual(alice);
    expect(useRuntimeStore.getState().bootstrap.agents).toMatchObject([agent]);
    expect(mocks.read).not.toHaveBeenCalled();
    expect(mocks.clear).not.toHaveBeenCalled();
    expect(mocks.bootstrap).not.toHaveBeenCalled();
    expect(mocks.write).not.toHaveBeenCalled();
    // Discovery updates cannot authorize writes after subject lookup failed.
    useRuntimeStore.setState({ bootstrap: online([{ ...agent, name: "Unconfirmed roster" }]) });
    expect(mocks.write).not.toHaveBeenCalled();
  });

  it.each([404, 503, "network"])("surfaces subject failure %s and retries without reading unconfirmed caches", async (failure) => {
    const error = typeof failure === "number"
      ? new RuntimeHttpError("GET", "/auth/session/me", failure) : new TypeError("Failed to fetch");
    mocks.user.mockRejectedValueOnce(error);
    await refresh();
    expect(useRuntimeStore.getState().bootstrapLoading).toBe(false);
    expect(useRuntimeStore.getState().bootstrapError).toBe(error.message);
    expect(useRuntimeStore.getState().bootstrap.connection.error).toBe(error.message);
    expect(mocks.read).not.toHaveBeenCalled();
    expect(mocks.bootstrap).not.toHaveBeenCalled();
    bootstrap.resolve(online([agent]));
    await vi.advanceTimersByTimeAsync(5_000);
    expect(mocks.user).toHaveBeenCalledTimes(2);
    expect(mocks.bootstrap).toHaveBeenCalledTimes(1);
    expect(useRuntimeStore.getState().bootstrap.agents).toMatchObject([agent]);
    expect(useRuntimeStore.getState().bootstrapError).toBeUndefined();
    expect(useRuntimeStore.getState().bootstrap.connection.error).toBeUndefined();
  });

  it.each([401, 403])("treats strict session HTTP %s as a definitive auth rejection", async (status) => {
    mocks.user.mockRejectedValue(new RuntimeHttpError("GET", "/auth/session/me", status));
    void refresh();
    bootstrap.resolve(online([agent]));
    await pending;
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
    expect(useRuntimeStore.getState().bootstrap.connection.authRequired).toBe(true);
    expect(mocks.read).not.toHaveBeenCalled();
    expect(mocks.clear).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.user).toHaveBeenCalledTimes(1);
  });

  it("consumes the scheduled subject retry when a manual refresh recovers first", async () => {
    mocks.user.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    await refresh();
    bootstrap.resolve(online([agent]));
    await refresh();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.user).toHaveBeenCalledTimes(2);
    expect(mocks.bootstrap).toHaveBeenCalledTimes(1);
    expect(useRuntimeStore.getState().bootstrap.connection.error).toBeUndefined();
  });

  it("does not retry a late subject failure after the stream has rejected authentication", async () => {
    let reject!: (error: Error) => void;
    mocks.user.mockReturnValue(new Promise((_, no) => { reject = no; }));
    void refresh();
    useRuntimeStore.setState({
      discovery: { mode: "authoritative", freshness: "unauthorized", unauthorizedReason: "test 401", retryAttempt: 0 },
    });
    reject(new TypeError("Failed to fetch"));
    await pending;
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.user).toHaveBeenCalledTimes(1);
    expect(mocks.bootstrap).not.toHaveBeenCalled();
    expect(mocks.read).not.toHaveBeenCalled();
  });

  it("does not persist a new roster under an old subject while the new subject is still pending", async () => {
    useRuntimeStore.setState({
      currentUser: alice, currentUserLoaded: true, agentRosterSource: "http", bootstrap: online([agent]),
    });
    mocks.write.mockClear();
    const subject = deferred<typeof bob>();
    mocks.user.mockReturnValue(subject.promise);
    void refresh();
    const fresh = { ...agent, name: "Bob's agent" };
    bootstrap.resolve(online([fresh]));
    await Promise.resolve();
    expect(mocks.bootstrap).not.toHaveBeenCalled();
    expect(mocks.write).not.toHaveBeenCalled();
    subject.resolve(bob);
    await pending;
    expect(mocks.write).toHaveBeenCalledWith(agentPresentationCacheKey("local", bob), [expect.objectContaining(fresh)]);
    expect(mocks.write).not.toHaveBeenCalledWith(agentPresentationCacheKey("local", alice), expect.anything());
  });

  it("fences requests from the old subject before publishing the new subject", async () => {
    useRuntimeStore.setState({
      currentUser: alice, currentUserLoaded: true, agentRosterSource: "http", bootstrap: online([agent]),
    });
    const oldState = deferred<AgentSummary>();
    mocks.agentState.mockReturnValue(oldState.promise);
    const oldRequest = useRuntimeStore.getState().refreshAgentState(agent.id);
    mocks.user.mockResolvedValue(bob);
    mocks.read.mockResolvedValue([]);
    void refresh();
    await vi.waitFor(() => expect(mocks.bootstrap).toHaveBeenCalledTimes(1));
    const fresh = { ...agent, name: "Bob's agent" };
    bootstrap.resolve(online([fresh]));
    await pending;
    mocks.write.mockClear();
    oldState.resolve({ ...agent, name: "Alice's late state" });
    await oldRequest;
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([fresh]);
    expect(useRuntimeStore.getState().sessionsByAgentId).toEqual({});
    expect(mocks.write).not.toHaveBeenCalled();
  });

  it("does not overwrite a fresh online roster with a late cached roster", async () => {
    const cache = deferred<AgentSummary[]>();
    mocks.read.mockReturnValue(cache.promise);
    void refresh();
    await vi.waitFor(() => expect(mocks.read).toHaveBeenCalled());
    const fresh = { ...agent, name: "Fresh agent" };
    bootstrap.resolve(online([fresh]));
    await pending;
    cache.resolve([agent]);
    await Promise.resolve();
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([fresh]);
    expect(useRuntimeStore.getState().agentRosterSource).toBe("http");
    expect(mocks.write).toHaveBeenCalledWith(agentPresentationCacheKey("local", alice), [fresh]);
  });

  it("treats an empty online roster as authoritative, not as a reason to resurrect cache", async () => {
    void refresh();
    await vi.waitFor(() => expect(useRuntimeStore.getState().agentRosterSource).toBe("cache"));
    bootstrap.resolve(online());
    await pending;
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
    expect(useRuntimeStore.getState().agentRosterSource).toBe("http");
  });

  it("keeps cached presentation after a transient disconnected bootstrap", async () => {
    void refresh();
    await vi.waitFor(() => expect(useRuntimeStore.getState().agentRosterSource).toBe("cache"));
    bootstrap.resolve({
      ...online(), connection: { mode: "local", source: "fixture", summary: "Offline", error: "Temporary failure" },
    });
    await pending;
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([agent]);
    expect(useRuntimeStore.getState().bootstrap.connection.error).toBe("Temporary failure");
    expect(useRuntimeStore.getState().agentRosterSource).toBe("cache");
  });

  it.each(["bootstrap", "roster"])("revokes presentation immediately on %s auth rejection", async (path) => {
    void refresh();
    await vi.waitFor(() => expect(useRuntimeStore.getState().agentRosterSource).toBe("cache"));
    if (path === "bootstrap") {
      bootstrap.resolve({
        ...online(), connection: { mode: "local", source: "fixture", summary: "Sign in", authRequired: true },
      });
      await pending;
    } else {
      useRuntimeStore.setState({ discovery: { ...useRuntimeStore.getState().discovery, freshness: "unauthorized" } });
    }
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
    expect(useRuntimeStore.getState().currentUser).toBeUndefined();
    expect(mocks.clear).toHaveBeenCalledTimes(1);
  });

  it("rejects late cache hydration after an auth rejection", async () => {
    const cache = deferred<AgentSummary[]>();
    mocks.read.mockReturnValue(cache.promise);
    void refresh();
    await vi.waitFor(() => expect(mocks.read).toHaveBeenCalled());
    useRuntimeStore.setState({
      discovery: { mode: "authoritative", freshness: "unauthorized", unauthorizedReason: "test 401", retryAttempt: 0 },
    });
    cache.resolve([agent]);
    await Promise.resolve();
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
  });

  it("rejects late cache hydration after the subject changes", async () => {
    const cache = deferred<AgentSummary[]>();
    mocks.read.mockReturnValue(cache.promise);
    void refresh();
    await vi.waitFor(() => expect(mocks.read).toHaveBeenCalled());
    useRuntimeStore.setState({ currentUser: bob });
    cache.resolve([agent]);
    await Promise.resolve();
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
  });
});
