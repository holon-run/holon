import { describe, expect, it } from "vitest";

// skillDetailLookupId has been removed. Agent-scoped skill detail is now
// resolved by passing skill.skillId directly to the existing skill_detail
// endpoint, which parses the skill_id prefix to determine scope.
describe("AgentOverviewPanel (skill detail lookup removed)", () => {
  it("placeholder", () => {
    expect(true).toBe(true);
  });
});
