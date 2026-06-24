import { describe, expect, it } from "vitest";

import type { SkillCatalogEntry, SkillCatalogState } from "../../runtime/types";
import { skillDetailLookupId } from "./AgentOverviewPanel";

const agentSkill: SkillCatalogEntry = {
  skillId: "agent_home:holon-pm:e9cc6901ba10:agentinbox",
  rootId: "agent_home:holon-pm:e9cc6901ba10",
  skillDir: "agentinbox",
  legacyId: "agent:agentinbox",
  name: "agentinbox",
  description: "Agent inbox integration",
  path: "/Users/jolestar/.holon/agents/holon-pm/skills/agentinbox/SKILL.md",
  scope: "agent",
};

describe("skillDetailLookupId", () => {
  it("maps agent-linked skills to a global catalog detail id", () => {
    const availableSkillCatalog: SkillCatalogState = {
      source: "http",
      catalog: [
        {
          skillId: "user_global:jolestar:agentinbox",
          rootId: "user_global:jolestar",
          skillDir: "agentinbox",
          legacyId: "user:agentinbox",
          name: "agentinbox",
          description: "Agent inbox integration",
          path: "/Users/jolestar/.agents/skills/agentinbox/SKILL.md",
          scope: "user_global",
        },
      ],
    };

    expect(skillDetailLookupId(agentSkill, availableSkillCatalog)).toBe("user_global:jolestar:agentinbox");
  });

  it("falls back to the user legacy id instead of the agent instance id", () => {
    expect(skillDetailLookupId(agentSkill)).toBe("user:agentinbox");
  });
});
