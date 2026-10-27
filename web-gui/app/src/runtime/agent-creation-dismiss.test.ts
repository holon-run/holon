import { describe, expect, it } from "vitest";
import type { AgentCreationProgress } from "./agent-creation";
import type { ConversationTurnGroup } from "./conversation-view-model";
import { hasSettledSuccessfulTurn, shouldDismissAgentCreation } from "./agent-creation-dismiss";

function turn(overrides: Partial<ConversationTurnGroup> = {}): ConversationTurnGroup {
  return {
    turnId: "turn-1",
    turnIndex: 1,
    revision: 1,
    presentationClass: "operator",
    inputs: [],
    execution: { kind: "terminal", outcome: "completed" },
    result: { kind: "available" },
    settled: true,
    attention: null,
    detailCoverage: { kind: "complete" },
    briefIds: ["brief-1"],
    detail: null,
    ...overrides,
  };
}

function progress(status: AgentCreationProgress["status"]): AgentCreationProgress {
  return {
    creation: {
      identity: { agent_id: "new-agent", incarnation: 1, status: "active" },
      receipt: { receipt_id: "receipt-1", agent_id: "new-agent", stage: "bootstrapped" },
    },
    status,
    observationRevision: 0,
  };
}

describe("creation banner retirement", () => {
  it("retires the banner after a settled successful turn", () => {
    expect(shouldDismissAgentCreation(progress("ready"), [turn()])).toBe(true);
  });

  it("keeps the banner when the first turn failed", () => {
    const failed = turn({
      execution: { kind: "terminal", outcome: "baseline_over_budget" },
      result: { kind: "none", reason: { kind: "reducer_only", reason: "failed" } },
      attention: { kind: "failed", outcome: "baseline_over_budget" },
      briefIds: [],
    });
    expect(shouldDismissAgentCreation(progress("ready"), [failed])).toBe(false);
  });

  it("keeps the banner while the turn is still running", () => {
    expect(shouldDismissAgentCreation(progress("ready"), [turn({ execution: { kind: "active" } })])).toBe(false);
  });

  it("keeps the banner until the successful turn settles", () => {
    expect(shouldDismissAgentCreation(progress("ready"), [turn({ settled: false })])).toBe(false);
  });

  it("keeps the banner for runtime-only receipts that never ran a model turn", () => {
    const receipt = turn({
      presentationClass: "task",
      result: { kind: "none", reason: { kind: "reducer_only", reason: "completed" } },
      briefIds: [],
    });
    expect(hasSettledSuccessfulTurn([receipt])).toBe(false);
    expect(shouldDismissAgentCreation(progress("ready"), [receipt])).toBe(false);
  });

  it("keeps the banner for degraded, uncertain, and initializing creations", () => {
    const turns = [turn()];
    expect(shouldDismissAgentCreation(progress("degraded"), turns)).toBe(false);
    expect(shouldDismissAgentCreation(progress("uncertain"), turns)).toBe(false);
    expect(shouldDismissAgentCreation(progress("initializing"), turns)).toBe(false);
  });

  it("keeps the banner when there is no creation to retire", () => {
    expect(shouldDismissAgentCreation(undefined, [turn()])).toBe(false);
    expect(shouldDismissAgentCreation(progress("ready"), [])).toBe(false);
  });
});
