import { describe, expect, it } from "vitest";
import type { TFunction } from "i18next";

import { deriveAgentDisplayStatus } from "./agent-status";
import type { AgentSummary } from "./types";

const mockT = ((key: string, opts?: { count?: number }) => {
  if (opts?.count != null) return `${key}_${opts.count}`;
  return key;
}) as TFunction;

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
    expect(deriveAgentDisplayStatus(agent({ pending: 2, posture: "has_queued_input" }), mockT).tone).toBe("ready");
  });

  it("does not infer running from active background tasks alone", () => {
    expect(deriveAgentDisplayStatus(agent({ activeTaskCount: 1, lifecycle: "asleep", posture: "idle" }), mockT).tone).toBe("ready");
  });

  it("uses explicit runtime turn signals for running", () => {
    expect(deriveAgentDisplayStatus(agent({ posture: "active_turn" }), mockT).tone).toBe("running");
    expect(deriveAgentDisplayStatus(agent({ lifecycle: "awake-running" }), mockT).tone).toBe("running");
    expect(deriveAgentDisplayStatus(agent({ currentRunId: "run-1" }), mockT).tone).toBe("running");
  });

  it("separates operator input waits from other waits", () => {
    expect(deriveAgentDisplayStatus(agent({ posture: "waiting_for_operator" }), mockT).tone).toBe("needs-input");
    expect(deriveAgentDisplayStatus(agent({ posture: "waiting_task" }), mockT).tone).toBe("waiting");
    expect(deriveAgentDisplayStatus(agent({ posture: "blocked" }), mockT).tone).toBe("waiting");
  });

  it("keeps stopped as a top-level terminal state", () => {
    expect(deriveAgentDisplayStatus(agent({ lifecycle: "stopped", posture: "active_turn" }), mockT).tone).toBe("stopped");
  });
});
