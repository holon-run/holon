import { describe, expect, it } from "vitest";

// skillDetailLookupId has been removed.
// Agent-scoped skill detail is now resolved via the agent-scoped endpoint
// (getAgentSkillDetail) using a composite key "agent:{agentId}:{skillDir}".
describe("AgentOverviewPanel (skill detail lookup removed)", () => {
  it("placeholder", () => {
    expect(true).toBe(true);
  });
});
