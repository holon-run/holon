import { describe, expect, it } from "vitest";
import type { TFunction } from "i18next";

import { deriveAgentAvatarExpression, deriveAgentDisplayStatus, deriveAgentStatusProjection } from "./agent-status";
import type { AgentSummary } from "./types";
import statusFixture from "../../../../tests/fixtures/agent-status-display.json";

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
  it.each(statusFixture.cases)("$name matches the shared client contract", ({ input, expected }) => {
    const { stateIsCurrent, ...summary } = { ...statusFixture.defaults, ...input };
    expect(deriveAgentStatusProjection(summary, stateIsCurrent)).toEqual(expected);
    expect(deriveAgentDisplayStatus(summary, mockT, stateIsCurrent)).toMatchObject(expected);
  });

  it("keeps deletion separate from scheduler activity", () => {
    expect(deriveAgentDisplayStatus(agent({ lifecycle: "deleting", posture: "active_turn" }), mockT)).toMatchObject({
      kind: "deleting", icon: "circle-x", tone: "danger", label: "badge.deleting",
    });
  });

  it.each(["offline", "unloaded"])("treats %s lifecycle as unavailable freshness", (lifecycle) => {
    const summary = agent({
      lifecycle, posture: "active_turn", currentRunId: "stale-run",
      pending: 1, activeTaskCount: 1, waitingCount: 1,
    });
    expect(deriveAgentStatusProjection(summary)).toEqual({
      kind: "unknown", icon: "circle-help", tone: "muted",
    });
    expect(deriveAgentAvatarExpression(summary)).toBe("neutral");
  });

  it("treats deleted lifecycle as stopped despite an active posture", () => {
    expect(deriveAgentStatusProjection(agent({ lifecycle: "deleted", posture: "active_turn" }))).toEqual({
      kind: "stopped", icon: "circle-x", tone: "muted",
    });
  });

  it("includes the label in tooltips and marks stale details as cached", () => {
    const status = deriveAgentDisplayStatus(agent({ posture: "active_turn" }), mockT, false);
    expect(status.label).toBe("badge.unknown");
    expect(status.title).toContain("badge.unknown");
    expect(status.title).toContain("statusDetail.cached");
    expect(status.title).toContain("active_turn");
  });
});

describe("deriveAgentAvatarExpression", () => {
  const cases: [Partial<AgentSummary>, string][] = [
    [{ lifecycle: "awake_idle", posture: "idle" }, "neutral"],
    [{ posture: "active_turn" }, "focused"],
    [{ lifecycle: "awake_running", posture: "unknown" }, "focused"],
    [{ currentRunId: "run-1", posture: "unknown" }, "focused"],
    [{ posture: "waiting_for_operator", waitingCount: 1 }, "questioning"],
    [{ posture: "waiting_for_task" }, "waiting"],
    [{ posture: "waiting_for_external" }, "waiting"],
    [{ posture: "blocked" }, "waiting"],
    [{ waitingCount: 1, posture: "unknown" }, "waiting"],
    [{ activeTaskCount: 1 }, "neutral"],
    [{ lifecycle: "asleep", posture: "idle" }, "sleeping"],
    [{ pending: 1 }, "neutral"],
    [{ posture: "has_runnable_work" }, "neutral"],
    [{ posture: "has_queued_input" }, "neutral"],
    [{ lifecycle: "asleep", posture: "unknown" }, "neutral"],
    [{ lifecycle: "unrecognized", posture: "unknown" }, "neutral"],
    [{ lifecycle: "stopped", posture: "active_turn" }, "neutral"],
    [{ lifecycle: "archived", posture: "waiting_for_operator" }, "neutral"],
    [{ lifecycle: "deleting", currentRunId: "run-1" }, "neutral"],
    [{ lifecycle: "awake_running", posture: "waiting_for_operator" }, "questioning"],
    [{ lifecycle: "awake_running", posture: "idle", currentRunId: "stale-run" }, "neutral"],
  ];
  it.each(cases)("maps explicit runtime signals %j to %s", (overrides, expression) => {
    expect(deriveAgentAvatarExpression(agent(overrides))).toBe(expression);
  });

  it.each(cases)("uses neutral eyes when the state is stale or disconnected: %j", (overrides) => {
    expect(deriveAgentAvatarExpression(agent(overrides), false)).toBe("neutral");
  });

  it("does not interpret a failed tool or attention marker as Agent failure", () => {
    expect(deriveAgentAvatarExpression(agent({ lifecycle: "awake_idle", attention: "error" }))).toBe("neutral");
  });
});
