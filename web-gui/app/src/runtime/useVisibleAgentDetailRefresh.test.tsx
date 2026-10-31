// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const conversation = vi.hoisted(() => ({ head: 1, listeners: new Set<() => void>() }));
vi.mock("./conversation-scope-store", () => ({
  conversationScopeSnapshot: () => ({
    status: { kind: "ready" },
    view: { reset_reason: null, scope: "scope", through_seq: conversation.head },
  }),
  subscribeConversationScope: (_key: string, listener: () => void) => {
    conversation.listeners.add(listener);
    return () => conversation.listeners.delete(listener);
  },
}));

import { emptyAgentSession } from "./conversation-store";
import { useRuntimeStore } from "./runtime-store";
import { useVisibleAgentDetailRefresh } from "./useVisibleAgentDetailRefresh";
import { refreshSelectedAgentResources } from "./selected-agent-resource-invalidation";
import { RuntimeHttpError } from "./client";
import type { AgentSummary, TaskDetailState } from "./types";

const initial = useRuntimeStore.getState();
const errors = vi.fn();
let root: Root;
let host: HTMLDivElement;
let hidden = false;
const work = { id: "w1", objective: "Work", state: "open" };
const workRead = vi.fn();
const taskRead = vi.fn();
const toolRead = vi.fn();
function Harness({ enabled = true }: { enabled?: boolean }) {
  useVisibleAgentDetailRefresh("a", 1, "scope", enabled, "context", errors);
  return null;
}
async function advance(ms: number) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}
async function hint() {
  await act(async () => {
    conversation.head++;
    conversation.listeners.forEach((notify) => notify());
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => hidden ? "hidden" : "visible" });
  hidden = false;
  conversation.head = 1;
  conversation.listeners.clear();
  workRead.mockReset().mockResolvedValue(undefined);
  errors.mockReset();
  taskRead.mockReset().mockResolvedValue(undefined);
  toolRead.mockReset().mockResolvedValue(undefined);
  useRuntimeStore.setState({
    ...initial,
    bootstrap: { ...initial.bootstrap, agents: [{ id: "a", incarnation: 1, workItems: [work] } as AgentSummary] },
    sessionsByAgentId: { a: emptyAgentSession() },
    rightPanelOpen: true,
    rightPanelView: { kind: "work_item_detail", agentId: "a", workItem: work },
    loadAgentWorkItemDetail: workRead,
    loadAgentTaskDetail: taskRead,
    loadAgentToolExecutionDetail: toolRead,
  }, true);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  useRuntimeStore.setState(initial, true);
  Reflect.deleteProperty(document, "visibilityState");
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("visible object refresh ownership", () => {
  it("keeps an object 404 local and stopped until an explicit retry", async () => {
    workRead.mockRejectedValueOnce(new RuntimeHttpError("GET", "/agents/a/work-items/w1", 404, "Object missing"));
    await act(async () => root.render(<Harness />));
    await advance(0);
    expect(errors).not.toHaveBeenCalled();
    await hint();
    await advance(60_000);
    expect(workRead).toHaveBeenCalledTimes(1);
    await act(async () => { refreshSelectedAgentResources("a"); });
    await advance(0);
    expect(workRead).toHaveBeenCalledTimes(2);
  });

  it("merges opening triggers, throttles checkpoint hints, and uses a 30s silent fallback", async () => {
    await act(async () => root.render(<Harness />));
    await advance(0);
    expect(workRead).toHaveBeenCalledTimes(1);
    await hint();
    await hint();
    await advance(4_999);
    expect(workRead).toHaveBeenCalledTimes(1);
    await advance(1);
    expect(workRead).toHaveBeenCalledTimes(2);
    await act(async () => conversation.listeners.forEach((notify) => notify()));
    await advance(29_999);
    expect(workRead).toHaveBeenCalledTimes(2);
    await advance(1);
    expect(workRead).toHaveBeenCalledTimes(3);
  });

  it("pauses hidden, closed and unavailable views and rechecks on return", async () => {
    await act(async () => root.render(<Harness />));
    await advance(0);
    hidden = true;
    await act(async () => document.dispatchEvent(new Event("visibilitychange")));
    await hint();
    await advance(60_000);
    expect(workRead).toHaveBeenCalledTimes(1);
    hidden = false;
    await act(async () => document.dispatchEvent(new Event("visibilitychange")));
    await advance(0);
    expect(workRead).toHaveBeenCalledTimes(2);
    await act(async () => useRuntimeStore.setState({ rightPanelOpen: false }));
    await advance(60_000);
    expect(workRead).toHaveBeenCalledTimes(2);
    await act(async () => useRuntimeStore.setState({ rightPanelOpen: true }));
    await advance(0);
    expect(workRead).toHaveBeenCalledTimes(3);
    await act(async () => root.render(<Harness enabled={false} />));
    await advance(60_000);
    expect(workRead).toHaveBeenCalledTimes(3);
  });

  it("rechecks the new target and routes manual refresh through the same owner", async () => {
    await act(async () => root.render(<Harness />));
    await advance(0);
    await act(async () => useRuntimeStore.setState({
      rightPanelView: { kind: "work_item_detail", agentId: "a", workItem: { ...work, id: "w2" } },
    }));
    await advance(0);
    expect(workRead).toHaveBeenLastCalledWith("a", "w2", expect.objectContaining({ force: true, incarnation: 1 }));
    await act(async () => { refreshSelectedAgentResources("a"); });
    await advance(0);
    expect(workRead).toHaveBeenCalledTimes(3);
  });

  it("keeps polling after terminal status until final Task output is also present", async () => {
    await act(async () => useRuntimeStore.setState({
      rightPanelView: { kind: "task_detail", agentId: "a", task: { id: "t1", kind: "command_task", status: "running", summary: "Task" } },
    }));
    let count = 0;
    taskRead.mockImplementation(async () => {
      count++;
      const session = useRuntimeStore.getState().sessionsByAgentId.a;
      const detail = {
        status: { status: count >= 2 ? "completed" : "running" },
        output: { status: count >= 3 ? "completed" : "running" },
        stale: false,
      } as TaskDetailState;
      useRuntimeStore.setState({ sessionsByAgentId: { a: { ...session, taskDetailsById: { t1: detail } } } });
    });
    await act(async () => root.render(<Harness />));
    await advance(0);
    await advance(5_000);
    expect(taskRead).toHaveBeenCalledTimes(2);
    await advance(5_000);
    expect(taskRead).toHaveBeenCalledTimes(3);
    await advance(60_000);
    expect(taskRead).toHaveBeenCalledTimes(3);
    expect(workRead).not.toHaveBeenCalled();
  });

  it("stops periodic Tool reads after the terminal record and invalidates changed list metadata", async () => {
    await act(async () => root.render(<Harness />));
    await advance(0);
    await act(async () => useRuntimeStore.setState({
      bootstrap: { ...useRuntimeStore.getState().bootstrap, agents: [{
        id: "a", incarnation: 1, workItems: [{ ...work, updatedAt: "new-version" }],
      } as AgentSummary] },
    }));
    await advance(5_000);
    expect(workRead).toHaveBeenCalledTimes(2);
    await act(async () => useRuntimeStore.setState({
      rightPanelView: { kind: "tool_execution_detail", agentId: "a", toolExecutionId: "e1" },
    }));
    toolRead.mockImplementation(async () => {
      const session = useRuntimeStore.getState().sessionsByAgentId.a;
      useRuntimeStore.setState({ sessionsByAgentId: { a: {
        ...session, toolExecutionDetailsById: { e1: { toolExecution: { status: "success" } } },
      } } });
    });
    await advance(0);
    await advance(60_000);
    expect(toolRead).toHaveBeenCalledTimes(1);
    expect(workRead).toHaveBeenCalledTimes(2);
  });
});
