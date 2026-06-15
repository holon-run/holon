import { describe, expect, it } from "vitest";

import { deriveAgentDisplayStatus } from "./agent-status";
import type { AgentSummary } from "./types";

const baseAgent: AgentSummary = {
  id: "agent",
  badge: "A",
  profile: "default",
  lifecycle: "asleep",
  focusSummary: "",
  workspace: "",
  attention: "",
  model: "",
  footer: "",
  subtitle: "",
  lastBrief: "",
  lastTurnTime: "",
  pending: 0,
  activeTaskCount: 0,
  waitingCount: 0,
  posture: "idle",
  postureReason: "",
};

function agent(overrides: Partial<AgentSummary>): AgentSummary {
  return { ...baseAgent, ...overrides };
}

describe("deriveAgentDisplayStatus", () => {
  it("treats queued input as ready instead of needing input", () => {
    expect(deriveAgentDisplayStatus(agent({ pending: 2, posture: "has_queued_input" })).label).toBe("Ready");
  });

  it("does not infer running from active background tasks alone", () => {
    expect(deriveAgentDisplayStatus(agent({ activeTaskCount: 1, lifecycle: "asleep", posture: "idle" })).label).toBe("Ready");
  });

  it("uses explicit runtime turn signals for running", () => {
    expect(deriveAgentDisplayStatus(agent({ posture: "active_turn" })).label).toBe("Running");
    expect(deriveAgentDisplayStatus(agent({ lifecycle: "awake-running" })).label).toBe("Running");
    expect(deriveAgentDisplayStatus(agent({ currentRunId: "run-1" })).label).toBe("Running");
  });

  it("separates operator input waits from other waits", () => {
    expect(deriveAgentDisplayStatus(agent({ posture: "waiting_for_operator" })).label).toBe("Waiting for input");
    expect(deriveAgentDisplayStatus(agent({ posture: "waiting_task" })).label).toBe("Waiting");
    expect(deriveAgentDisplayStatus(agent({ posture: "blocked" })).label).toBe("Waiting");
  });

  it("keeps stopped as a top-level terminal state", () => {
    expect(deriveAgentDisplayStatus(agent({ lifecycle: "stopped", posture: "active_turn" })).label).toBe("Stopped");
  });
});
