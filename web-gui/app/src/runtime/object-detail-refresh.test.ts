import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const reads = vi.hoisted(() => ({ work: vi.fn(), status: vi.fn(), output: vi.fn(), tool: vi.fn(), state: vi.fn() }));
vi.mock("./client", async (original) => {
  const actual = await original<typeof import("./client")>();
  return { ...actual, createRuntimeClient: (...args: Parameters<typeof actual.createRuntimeClient>) => ({
    ...actual.createRuntimeClient(...args),
    getAgentWorkItem: reads.work, getTaskStatus: reads.status, getTaskOutput: reads.output, getToolExecution: reads.tool,
    getAgentState: reads.state,
  }) };
});

import { AgentResourceSupersededError, useRuntimeStore } from "./runtime-store";
import { RuntimeHttpError } from "./client";
import type { AgentSummary, WorkItemSummary } from "./types";

const initial = useRuntimeStore.getState();
const work: WorkItemSummary = { id: "w1", objective: "First objective", state: "open" };
const agent = { id: "a", incarnation: 1, tasks: [], workItems: [work] } as unknown as AgentSummary;
const store = () => useRuntimeStore.getState();
const session = () => store().sessionsByAgentId.a;
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-31T00:00:00Z"));
  reads.work.mockReset().mockResolvedValue(work);
  reads.status.mockReset().mockResolvedValue({ status: "running" });
  reads.output.mockReset().mockResolvedValue({ status: "running", task: { status: "running" } });
  reads.tool.mockReset().mockResolvedValue({ status: "success", tool_name: "ExecCommand" });
  reads.state.mockReset().mockResolvedValue(agent);
  useRuntimeStore.setState({ ...initial, bootstrap: { ...initial.bootstrap, agents: [agent] }, sessionsByAgentId: {} }, true);
});
afterEach(() => {
  useRuntimeStore.setState(initial, true);
  vi.useRealTimers();
});

describe("scoped object detail reads", () => {
  it("does not lose a replacement detail when the matching core state settles later", async () => {
    await store().loadAgentWorkItemDetail("a", "w1");
    const replacement = { ...agent, incarnation: 2 };
    useRuntimeStore.setState({ bootstrap: { ...store().bootstrap, agents: [replacement] } });
    reads.work.mockResolvedValue({ ...work, objective: "New incarnation detail" });
    reads.state.mockResolvedValue(replacement);
    await store().loadAgentWorkItemDetail("a", "w1", { force: true });
    await store().refreshAgentState("a", { incarnation: 2 });
    expect(session().workItemDetailsById.w1.workItem?.objective).toBe("New incarnation detail");
  });

  it("revalidates cached WorkItem details after TTL and allows immediate force", async () => {
    await store().loadAgentWorkItemDetail("a", "w1");
    await store().loadAgentWorkItemDetail("a", "w1");
    expect(reads.work).toHaveBeenCalledTimes(1);
    reads.work.mockResolvedValue({ ...work, objective: "New objective", todoList: [] });
    await store().loadAgentWorkItemDetail("a", "w1", { force: true });
    expect(session().workItemDetailsById.w1.workItem?.objective).toBe("New objective");
    vi.advanceTimersByTime(30_000);
    await store().loadAgentWorkItemDetail("a", "w1");
    expect(reads.work).toHaveBeenCalledTimes(3);
    expect(reads.work).toHaveBeenLastCalledWith("a", "w1", { incarnation: 1, signal: undefined });
  });

  it("shares one in-flight request even for concurrent forced Task reads", async () => {
    const slow = deferred<{ status: string }>();
    reads.status.mockReturnValueOnce(slow.promise);
    const first = store().loadAgentTaskDetail("a", "t1", true);
    const second = store().loadAgentTaskDetail("a", "t1", true);
    await Promise.resolve();
    expect(reads.status).toHaveBeenCalledTimes(1);
    expect(reads.output).toHaveBeenCalledTimes(1);
    slow.resolve({ status: "running" });
    await Promise.all([first, second]);
    expect(session().taskDetailsById.t1).toMatchObject({ loading: false, refreshing: false, stale: false });
  });

  it("keeps last-known-good WorkItem data on error and clears stale on retry", async () => {
    await store().loadAgentWorkItemDetail("a", "w1");
    const lastSuccessAt = session().workItemDetailsById.w1.lastSuccessAt;
    reads.work.mockRejectedValueOnce(new RuntimeHttpError("GET", "/work-items/w1", 503, "Unavailable"));
    await expect(store().loadAgentWorkItemDetail("a", "w1", { force: true, throwOnError: true })).rejects.toThrow();
    expect(session().workItemDetailsById.w1).toMatchObject({ stale: true, loading: false, workItem: work, lastSuccessAt });
    expect(session().error).toBeUndefined();
    await store().loadAgentWorkItemDetail("a", "w1", { force: true });
    expect(session().workItemDetailsById.w1).toMatchObject({ stale: false, error: undefined });
  });

  it("publishes partial Task status but keeps output stale until a complete retry", async () => {
    await store().loadAgentTaskDetail("a", "t1", true);
    reads.status.mockResolvedValue({ status: "completed" });
    reads.output.mockRejectedValueOnce(new Error("Output unavailable"));
    await expect(store().loadAgentTaskDetail("a", "t1", true, { throwOnError: true })).rejects.toThrow("Output unavailable");
    expect(session().taskDetailsById.t1).toMatchObject({
      status: { status: "completed" }, output: { status: "running" }, stale: true, error: "Output unavailable",
    });
    reads.output.mockResolvedValue({ status: "completed", task: { status: "completed" } });
    await store().loadAgentTaskDetail("a", "t1", true);
    expect(session().taskDetailsById.t1).toMatchObject({ output: { status: "completed" }, stale: false });
  });

  it("discards old-incarnation data and does not let its finally release the new single-flight", async () => {
    const oldRead = deferred<WorkItemSummary>();
    const newRead = deferred<WorkItemSummary>();
    reads.work.mockReturnValueOnce(oldRead.promise).mockReturnValueOnce(newRead.promise);
    const old = store().loadAgentWorkItemDetail("a", "w1", { throwOnError: true });
    const rejected = expect(old).rejects.toBeInstanceOf(AgentResourceSupersededError);
    await Promise.resolve();
    useRuntimeStore.setState({ bootstrap: { ...store().bootstrap, agents: [{ ...agent, incarnation: 2 }] } });
    const fresh = store().loadAgentWorkItemDetail("a", "w1", { force: true });
    await Promise.resolve();
    oldRead.resolve({ ...work, objective: "Old incarnation" });
    await rejected;
    const deduped = store().loadAgentWorkItemDetail("a", "w1", { force: true });
    expect(reads.work).toHaveBeenCalledTimes(2);
    newRead.resolve({ ...work, objective: "Replacement incarnation" });
    await Promise.all([fresh, deduped]);
    expect(session().workItemDetailsById.w1.workItem?.objective).toBe("Replacement incarnation");
  });

  it("rejects a completion after authorization context changes", async () => {
    const slow = deferred<WorkItemSummary>();
    reads.work.mockReturnValueOnce(slow.promise);
    const request = store().loadAgentWorkItemDetail("a", "w1", { throwOnError: true });
    const rejected = expect(request).rejects.toBeInstanceOf(AgentResourceSupersededError);
    await Promise.resolve();
    useRuntimeStore.setState({ discovery: { ...store().discovery, freshness: "unauthorized" } });
    slow.resolve(work);
    await rejected;
    expect(session().workItemDetailsById.w1.workItem).toBeUndefined();
  });

  it.each([404, 429, 503])("records failed Tool revision on HTTP %i without losing explicit retry", async (status) => {
    reads.tool.mockRejectedValue(new RuntimeHttpError("GET", "/tool-executions/e1", status, "Tool unavailable"));
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, 2);
    expect(session().toolExecutionDetailsById.e1).toMatchObject({
      loading: false, stale: true, conversationRevision: 2,
    });
    expect(session().toolExecutionDetailsById.e1.lastSuccessAt).toBeUndefined();
    reads.tool.mockResolvedValue({ status: "success", tool_name: "ExecCommand" });
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, undefined, { force: true });
    expect(reads.tool).toHaveBeenCalledTimes(2);
    expect(session().toolExecutionDetailsById.e1).toMatchObject({ stale: false, conversationRevision: 2 });
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, 3);
    expect(reads.tool).toHaveBeenCalledTimes(3);
  });

  it("forces Tool revalidation, keeps good data on failures, and respects conversation revision", async () => {
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, 1);
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, 1);
    expect(reads.tool).toHaveBeenCalledTimes(1);
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, 2);
    expect(reads.tool).toHaveBeenCalledTimes(2);
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, undefined, { force: true });
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, 2);
    expect(reads.tool).toHaveBeenCalledTimes(3);
    reads.tool.mockRejectedValueOnce(new Error("Tool unavailable"));
    await store().loadAgentToolExecutionDetail("a", "e1", undefined, undefined, { force: true });
    expect(session().toolExecutionDetailsById.e1).toMatchObject({ stale: true, toolExecution: { status: "success" } });
  });
});
