import { describe, expect, it } from "vitest";

import { skillRoot } from "./SkillsPage";

describe("skillRoot", () => {
  it("returns the library root for compatible skill directories", () => {
    expect(skillRoot("/Users/jolestar/.claude/skills/ace-step/SKILL.md")).toBe("/Users/jolestar/.claude/skills");
    expect(skillRoot("/Users/jolestar/.codex/skills/github-review/SKILL.md")).toBe("/Users/jolestar/.codex/skills");
    expect(skillRoot("/Users/jolestar/.agents/skills/ghx/SKILL.md")).toBe("/Users/jolestar/.agents/skills");
  });

  it("returns the library root for non-hidden skills directories", () => {
    expect(skillRoot("/repo/skills/local-skill/SKILL.md")).toBe("/repo/skills");
  });
});
