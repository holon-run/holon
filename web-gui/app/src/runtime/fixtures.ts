import type { RuntimeBootstrap } from "./types";

export const runtimeFixture: RuntimeBootstrap = {
  attentionCount: 1,
  connection: {
    mode: "local",
    summary: "connected · fixture bootstrap",
  },
  agents: [
    {
      id: "holon-pm",
      badge: "PM",
      profile: "public · project manager profile",
      lifecycle: "asleep",
      focusSummary: "standalone Web GUI implementation plan",
      workspace: "holon · web-gui-prototype",
      attention: "1 waiting signal",
      model: "gpt-5-high",
      footer: "asleep · ready for operator input",
      subtitle: "asleep · workspace holon · web-gui-prototype",
      lastBrief: "Standalone Web GUI work is split into contract, scaffold, Dashboard, Agent page, and API-gap slices.",
      lastTurnTime: "16:39",
      currentWork: {
        id: "work_57ad2568ea1307f",
        objective: "Web GUI standalone implementation plan in current worktree",
        state: "in_progress",
      },
    },
    {
      id: "holon-dev",
      badge: "DEV",
      badgeTone: "muted",
      profile: "public · implementation profile",
      lifecycle: "stopped",
      focusSummary: "no active workspace",
      workspace: "not bound",
      attention: "none",
      model: "runtime default",
      footer: "stopped · no active workspace",
      subtitle: "stopped · no active workspace",
      lastBrief: "Stored state is available when this agent becomes active.",
      lastTurnTime: "—",
    },
  ],
};
