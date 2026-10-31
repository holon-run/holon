import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const reads = vi.hoisted(() => ({ state: vi.fn(), workItems: vi.fn(), detail: vi.fn(), timers: vi.fn(), recentTasks: vi.fn() }));
vi.mock("./client", async (original) => {
  const actual = await original<typeof import("./client")>();
  return {
    ...actual,
    createRuntimeClient: (...args: Parameters<typeof actual.createRuntimeClient>) => ({
      ...actual.createRuntimeClient(...args),
      getAgentState: reads.state,
      getAgentWorkItems: reads.workItems,
      getAgentDetail: reads.detail,
      getAgentTimers: reads.timers,
      getAgentRecentTasks: reads.recentTasks,
    }),
  };
});

import { AgentResourceSupersededError, useRuntimeStore } from "./runtime-store";
import { RuntimeHttpError } from "./client";
import type { AgentSummary, WorkItemSummary } from "./types";

const initial = useRuntimeStore.getState();
const work: WorkItemSummary = { id: "w1", objective: "Current objective", state: "open", current: true };
function agent(overrides: Partial<AgentSummary> = {}): AgentSummary {
  return {
    id: "agent-a", incarnation: 1, badge: "A", profile: "default", lifecycle: "asleep",
    focusSummary: "", workspace: "", attention: "", model: "default", footer: "", subtitle: "",
    lastBrief: "", lastTurnTime: "", pending: 0, activeTaskCount: 0, waitingCount: 0,
    posture: "", postureReason: "", tasks: [], workItems: [], ...overrides,
  };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
const current = () => useRuntimeStore.getState();
const selected = () => current().bootstrap.agents[0];
const session = () => current().sessionsByAgentId["agent-a"];

beforeEach(() => {
  reads.state.mockReset().mockResolvedValue(agent());
  reads.workItems.mockReset().mockResolvedValue([]);
  reads.detail.mockReset();
  reads.timers.mockReset().mockResolvedValue([]);
  reads.recentTasks.mockReset().mockResolvedValue([]);
  useRuntimeStore.setState({
    ...initial,
    bootstrap: { ...initial.bootstrap, agents: [agent()] },
    sessionsByAgentId: {},
  }, true);
});
afterEach(() => { useRuntimeStore.setState(initial, true); });

describe("selected Agent resource reads", () => {
  it.each(["timers", "recentTasks"] as const)("rejects replaced %s reads instead of settling with an empty list", async (resource) => {
    const pending = deferred<[]>();
    reads[resource].mockReturnValueOnce(pending.promise);
    const abort = new AbortController();
    const method = resource === "timers" ? current().queryAgentTimers : current().queryAgentRecentTasks;
    const read = method("agent-a", { incarnation: 1, signal: abort.signal });
    expect(reads[resource]).toHaveBeenCalledWith("agent-a", { incarnation: 1, signal: abort.signal });
    useRuntimeStore.setState({ bootstrap: { ...current().bootstrap, agents: [agent({ incarnation: 2 })] } });
    const rejected = expect(read).rejects.toBeInstanceOf(AgentResourceSupersededError);
    pending.resolve([]);
    await rejected;
  });

  it.each(["timers", "recentTasks"] as const)("does not admit %s after its request is aborted", async (resource) => {
    const pending = deferred<[]>();
    reads[resource].mockReturnValueOnce(pending.promise);
    const abort = new AbortController();
    const method = resource === "timers" ? current().queryAgentTimers : current().queryAgentRecentTasks;
    const read = method("agent-a", { incarnation: 1, signal: abort.signal });
    abort.abort();
    const rejected = expect(read).rejects.toBeInstanceOf(AgentResourceSupersededError);
    pending.resolve([]);
    await rejected;
  });

  it("settles failed detail placeholders without requiring a successful identity", async () => {
    reads.detail.mockResolvedValueOnce({
      agent: agent({ incarnation: undefined }), source: "fixture", timeline: [], error: "HTTP 503",
    });
    await current().refreshAgentDetail("agent-a");
    expect(session()).toMatchObject({ loading: false, syncStatus: "error", error: "HTTP 503" });
    reads.detail.mockResolvedValueOnce({ agent: agent(), source: "http", timeline: [] });
    await current().refreshAgentDetail("agent-a");
    expect(session()).toMatchObject({ loading: false, syncStatus: "idle", error: undefined });
  });

  it("settles a successful detail with the wrong identity as an error", async () => {
    reads.detail.mockResolvedValueOnce({ agent: agent({ incarnation: 2 }), source: "http", timeline: [] });
    await current().refreshAgentDetail("agent-a");
    expect(session()).toMatchObject({ loading: false, syncStatus: "error" });
    expect(selected().incarnation).toBe(1);
  });

  it("binds the list to the selected incarnation before roster observes a replacement", async () => {
    reads.workItems.mockResolvedValueOnce([work]);
    await current().refreshAgentWorkItems("agent-a", { incarnation: 1 });
    reads.workItems.mockRejectedValueOnce(
      new RuntimeHttpError("GET", "/agents/agent-a/work-items", 409, "Identity changed", "agent_incarnation_mismatch"),
    );
    await current().refreshAgentWorkItems("agent-a", { incarnation: 1 });
    expect(reads.workItems).toHaveBeenLastCalledWith("agent-a", {
      limit: 50, signal: undefined, incarnation: 1,
    });
    expect(selected()).toMatchObject({ incarnation: 1, workItems: [work] });
    expect(session().resourceRefresh?.workItems).toMatchObject({ loading: false, stale: true });
  });

  it("keeps authoritative empty list membership and cleared focus through later state and roster merges", async () => {
    reads.state.mockResolvedValueOnce(agent({ workItems: [work], currentWork: work }));
    await current().refreshAgentState("agent-a");
    reads.workItems.mockResolvedValueOnce([]);
    await current().refreshAgentWorkItems("agent-a");
    reads.state.mockResolvedValueOnce(agent({ workItems: [work], currentWork: undefined }));
    await current().refreshAgentState("agent-a");
    expect(selected().workItems).toEqual([]);
    expect(selected().currentWork).toBeUndefined();
    expect(session().detail?.agent).toEqual(selected());
  });

  it("state clears active tasks and focus even if a later list still marks the previous item current", async () => {
    reads.state.mockResolvedValueOnce(agent({
      currentWork: work, workItems: [work], activeTaskCount: 1,
      tasks: [{ id: "t1", kind: "command", status: "running", summary: "running task" }],
    }));
    await current().refreshAgentState("agent-a");
    reads.workItems.mockResolvedValueOnce([work]);
    await current().refreshAgentWorkItems("agent-a");
    await current().refreshAgentState("agent-a");
    reads.workItems.mockResolvedValueOnce([{ ...work, objective: "Updated objective" }]);
    await current().refreshAgentWorkItems("agent-a");
    expect(selected()).toMatchObject({ activeTaskCount: 0, tasks: [], currentWork: undefined });
    expect(selected().workItems).toEqual([{ ...work, objective: "Updated objective", current: false }]);
  });

  it("retains last-good data and scopes errors without declaring the conversation offline", async () => {
    reads.workItems.mockResolvedValueOnce([work]);
    await current().refreshAgentWorkItems("agent-a");
    const lastSuccess = session().resourceRefresh?.workItems?.lastSuccessAt;
    const error = new Error("work-items temporarily unavailable");
    reads.workItems.mockRejectedValueOnce(error);
    await expect(current().refreshAgentWorkItems("agent-a", { throwOnError: true })).rejects.toBe(error);
    expect(selected().workItems).toEqual([work]);
    expect(session().resourceRefresh?.workItems).toMatchObject({
      loading: false, stale: true, error: error.message, lastSuccessAt: lastSuccess,
    });
    expect(session().error).toBeUndefined();
    expect(session().syncError).toBeUndefined();
    expect(session().syncStatus).not.toBe("error");
    await current().refreshAgentState("agent-a");
    expect(session().resourceRefresh?.state?.stale).toBe(false);
    expect(session().resourceRefresh?.workItems?.stale).toBe(true);
  });

  it("single-flights reads and prevents an old finally from deleting the replacement request", async () => {
    const old = deferred<AgentSummary>();
    const replacement = deferred<AgentSummary>();
    reads.state.mockReturnValueOnce(old.promise).mockReturnValueOnce(replacement.promise);
    const abort = new AbortController();
    const first = current().refreshAgentState("agent-a", { signal: abort.signal });
    await Promise.resolve();
    abort.abort();
    const second = current().refreshAgentState("agent-a");
    await Promise.resolve();
    old.resolve(agent({ name: "Obsolete" }));
    await first;
    const joined = current().refreshAgentState("agent-a");
    expect(reads.state).toHaveBeenCalledTimes(2);
    replacement.resolve(agent({ name: "Replacement" }));
    await Promise.all([second, joined]);
    expect(selected().name).toBe("Replacement");
    expect(session().resourceRefresh?.state?.loading).toBe(false);
  });

  it("rejects a mismatching state identity and ignores old-incarnation lists", async () => {
    reads.state.mockResolvedValueOnce(agent({ incarnation: 2 }));
    await expect(current().refreshAgentState("agent-a", { incarnation: 1, throwOnError: true }))
      .rejects.toMatchObject({ status: 409, code: "agent_incarnation_mismatch" });
    expect(selected().incarnation).toBe(1);
    const old = deferred<WorkItemSummary[]>();
    reads.workItems.mockReturnValueOnce(old.promise);
    const pending = current().refreshAgentWorkItems("agent-a", { incarnation: 1 });
    await Promise.resolve();
    useRuntimeStore.setState({ bootstrap: { ...current().bootstrap, agents: [agent({ incarnation: 2 })] } });
    old.resolve([work]);
    await pending;
    expect(selected().workItems).toEqual([]);
  });

  it("rejects results from a previous authorization realm", async () => {
    const old = deferred<WorkItemSummary[]>();
    reads.workItems.mockReturnValueOnce(old.promise);
    const pending = current().refreshAgentWorkItems("agent-a");
    await Promise.resolve();
    useRuntimeStore.setState({ currentUser: { authMethod: "token", userId: "new-user" } });
    old.resolve([work]);
    await pending;
    expect(selected().workItems).toEqual([]);
  });

  it("does not reuse the previous incarnation's list snapshot for the replacement Agent", async () => {
    reads.workItems.mockResolvedValueOnce([work]);
    await current().refreshAgentWorkItems("agent-a");
    useRuntimeStore.setState({ bootstrap: { ...current().bootstrap, agents: [agent({ incarnation: 2 })] } });
    reads.state.mockResolvedValueOnce(agent({ incarnation: 2 }));
    await current().refreshAgentState("agent-a", { incarnation: 2 });
    expect(selected().workItems).toEqual([]);
    expect(session().detail?.agent.incarnation).toBe(2);
    expect(session().resourceRefresh?.workItems).toBeUndefined();
  });

  it("resolves canonical focus from the list when state omits the embedded record", async () => {
    reads.state.mockResolvedValueOnce(agent({ currentWorkItemId: work.id }));
    await current().refreshAgentState("agent-a");
    reads.workItems.mockResolvedValueOnce([work]);
    await current().refreshAgentWorkItems("agent-a");
    expect(selected().currentWork).toEqual(work);
    reads.state.mockResolvedValueOnce(agent({ currentWorkItemId: null, workItems: [work] }));
    await current().refreshAgentState("agent-a");
    expect(selected().currentWork).toBeUndefined();
    expect(selected().workItems?.[0].current).toBe(false);
  });

  it("keeps a list read that finishes before state for an admitted Agent outside the roster", async () => {
    useRuntimeStore.setState({ bootstrap: { ...current().bootstrap, agents: [] } });
    reads.workItems.mockResolvedValueOnce([work]);
    await current().refreshAgentWorkItems("agent-a", { incarnation: 1 });
    await current().refreshAgentState("agent-a", { incarnation: 1 });
    expect(session().detail?.agent.workItems).toEqual([{ ...work, current: false }]);
    expect(current().bootstrap.agents).toEqual([]);
  });

  it("does not let a slower full-detail read overwrite newer state or list commits", async () => {
    const old = deferred<{ agent: AgentSummary; source: "http"; timeline: []; events: [] }>();
    reads.detail.mockReturnValueOnce(old.promise);
    const pending = current().refreshAgentDetail("agent-a");
    await Promise.resolve();
    await current().refreshAgentState("agent-a");
    reads.workItems.mockResolvedValueOnce([]);
    await current().refreshAgentWorkItems("agent-a");
    old.resolve({ agent: agent({ currentWork: work, workItems: [work], activeTaskCount: 9 }),
      source: "http", timeline: [], events: [] });
    await pending;
    expect(selected()).toMatchObject({ currentWork: undefined, workItems: [], activeTaskCount: 0 });
  });

  it("reports a displaced read as superseded so the scheduler retries instead of coasting", async () => {
    const slow = deferred<WorkItemSummary[]>();
    reads.workItems.mockReturnValueOnce(slow.promise);
    const slowRead = current().refreshAgentWorkItems("agent-a", { incarnation: 1, throwOnError: true });
    await Promise.resolve();
    // A context change (for example a resume generation bump) displaces the in-flight read.
    useRuntimeStore.setState({ currentUser: { authMethod: "token", userId: "user-2" } });
    const fast = deferred<WorkItemSummary[]>();
    reads.workItems.mockReturnValueOnce(fast.promise);
    const fastRead = current().refreshAgentWorkItems("agent-a", { incarnation: 1, throwOnError: true });
    slow.resolve([work]);
    await expect(slowRead).rejects.toBeInstanceOf(AgentResourceSupersededError);
    fast.resolve([]);
    await expect(fastRead).resolves.toBeUndefined();
    expect(selected().workItems).toEqual([]);
  });
});
