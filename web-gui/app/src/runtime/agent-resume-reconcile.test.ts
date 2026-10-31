// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const reads = vi.hoisted(() => ({ state: vi.fn(), workItems: vi.fn() }));
vi.mock("./client", async (original) => {
  const actual = await original<typeof import("./client")>();
  return {
    ...actual,
    createRuntimeClient: (...args: Parameters<typeof actual.createRuntimeClient>) => ({
      ...actual.createRuntimeClient(...args),
      getAgentState: reads.state,
      getAgentWorkItems: reads.workItems,
    }),
  };
});

import { useRuntimeStore } from "./runtime-store";
import type { AgentSummary, WorkItemSummary } from "./types";

const initial = useRuntimeStore.getState();
const work: WorkItemSummary = { id: "w1", objective: "Resumed objective", state: "open", current: true };
function agent(overrides: Partial<AgentSummary> = {}): AgentSummary {
  return {
    id: "agent-a", incarnation: 1, badge: "A", profile: "default", lifecycle: "asleep",
    focusSummary: "", workspace: "", attention: "", model: "default", footer: "", subtitle: "",
    lastBrief: "", lastTurnTime: "", pending: 0, activeTaskCount: 0, waitingCount: 0,
    posture: "", postureReason: "", tasks: [], workItems: [], ...overrides,
  };
}

beforeEach(() => {
  reads.state.mockReset().mockResolvedValue(agent({ currentWorkItemId: "w1" }));
  reads.workItems.mockReset().mockResolvedValue([work]);
  useRuntimeStore.setState({
    ...initial,
    bootstrap: { ...initial.bootstrap, agents: [agent()] },
    selectedAgentId: "agent-a",
    sessionsByAgentId: {},
  }, true);
});
afterEach(() => { useRuntimeStore.setState(initial, true); });

describe("resume reconciliation", () => {
  it("re-hydrates state and work items for the selected Agent after the scope reset", async () => {
    await useRuntimeStore.getState().reconcileAfterResume();
    const state = useRuntimeStore.getState();
    expect(reads.state.mock.calls.some((call) => call[0] === "agent-a")).toBe(true);
    expect(reads.workItems.mock.calls.some((call) => call[0] === "agent-a")).toBe(true);
    expect(state.bootstrap.agents[0]?.workItems).toEqual([work]);
    expect(state.sessionsByAgentId["agent-a"]?.workItemsSnapshot).toEqual([work]);
  });
});
