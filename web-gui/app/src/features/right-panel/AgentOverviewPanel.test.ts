import { renderToStaticMarkup } from "react-dom/server";
import { createElement } from "react";
import { describe, expect, it } from "vitest";

import "../../i18n";
import type { AgentSummary } from "../../runtime/types";
import { AgentOverviewPanel } from "./AgentOverviewPanel";

// skillDetailLookupId has been removed. Agent-scoped skill detail is now
// resolved by passing skill.skillId directly to the existing skill_detail
// endpoint, which parses the skill_id prefix to determine scope.
describe("AgentOverviewPanel (skill detail lookup removed)", () => {
  it("placeholder", () => {
    expect(true).toBe(true);
  });

  it("keeps a completed-only work item section titled and pinned in the work item position", () => {
    const agent: AgentSummary = {
      id: "web", badge: "W", profile: "", lifecycle: "idle", focusSummary: "", workspace: "",
      attention: "", model: "", footer: "", subtitle: "", lastBrief: "", lastTurnTime: "",
      pending: 0, activeTaskCount: 0, waitingCount: 0, posture: "idle", postureReason: "",
      workItems: [{ id: "done", objective: "Finished work", state: "completed" }],
    };
    const noop = () => {};
    const html = renderToStaticMarkup(createElement(AgentOverviewPanel, {
      agent,
      onLoadWorkItemDetail: noop,
      onOpenWorkItemDetail: noop,
      onOpenTask: noop,
      onRefreshAgentSkills: noop,
      onDisableAgentSkill: noop,
      onOpenSkill: noop,
      onOpenSkillManager: noop,
      onBrowseFiles: noop,
    }));
    expect(html).toContain("current-work");
    expect(html).not.toContain("past-work");
    expect(html).toContain("Work items (0)");
    expect(html).toContain("1 completed");
  });
});
