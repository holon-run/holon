import { describe, expect, it } from "vitest";
import type { AgentSummary } from "../runtime/types";
import {
  ALL_WORKSPACES_FILTER,
  STANDALONE_WORKSPACE_KEY,
  agentMatchesWorkspaceFilter,
  buildWorkspaceFilterOptions,
  extractExternalWorkspaces,
  isAgentHomeWorkspace,
  resolvePrimaryWorkspace,
} from "./agent-workspaces";

function makeAgent(overrides: Partial<AgentSummary> & { id: string }): AgentSummary {
  return {
    badge: "AG",
    profile: "public",
    lifecycle: "ready",
    focusSummary: "idle",
    workspace: "not bound",
    attention: "clear",
    model: "gpt-5.4",
    footer: "ready · idle",
    subtitle: "ready · not bound",
    lastBrief: "",
    lastTurnTime: "—",
    pending: 0,
    activeTaskCount: 0,
    waitingCount: 0,
    posture: "idle",
    postureReason: "idle",
    ...overrides,
  };
}

describe("agent-workspaces", () => {
  it("treats agent_home IDs and .holon/agents/<name> paths as private agent home workspaces", () => {
    expect(isAgentHomeWorkspace("agent_home:factorio", "/Users/dev/.holon/agents/factorio")).toBe(true);
    expect(isAgentHomeWorkspace("ws_472d42", "/Users/dev/.holon/agents/daily-life")).toBe(true);
    expect(isAgentHomeWorkspace(undefined, undefined)).toBe(true);
    expect(isAgentHomeWorkspace("ws_0c5379", "/Users/dev/src/github.com/holon-run/holon")).toBe(false);
  });

  it("extracts external workspaces and falls back when active workspace is agent_home", () => {
    const holonIos = makeAgent({
      id: "holon-ios",
      workspace: "holon-ios",
      workspaceSummary: {
        id: "agent_home:holon-ios",
        name: "holon-ios",
        anchor: "/Users/dev/.holon/agents/holon-ios",
      },
      attachedWorkspaces: [
        {
          workspaceId: "ws-inbox",
          name: "agentinbox",
          anchor: "/Users/dev/src/holon-run/agentinbox",
        },
        {
          workspaceId: "ws-holon",
          name: "holon",
          anchor: "/Users/dev/src/holon-run/holon",
        },
      ],
    });

    const external = extractExternalWorkspaces(holonIos);
    expect(external.map((w) => w.label)).toEqual(["agentinbox", "holon"]);

    const primary = resolvePrimaryWorkspace(holonIos, "Standalone");
    expect(primary.isStandalone).toBe(false);
    expect(primary.label).toBe("agentinbox");
    expect(primary.extraCount).toBe(1);

    // M:N workspace filter matches BOTH external workspaces!
    expect(agentMatchesWorkspaceFilter(holonIos, "path:/Users/dev/src/holon-run/holon")).toBe(true);
    expect(agentMatchesWorkspaceFilter(holonIos, "path:/Users/dev/src/holon-run/agentinbox")).toBe(true);
    expect(agentMatchesWorkspaceFilter(holonIos, STANDALONE_WORKSPACE_KEY)).toBe(false);
    expect(agentMatchesWorkspaceFilter(holonIos, ALL_WORKSPACES_FILTER)).toBe(true);
  });

  it("groups standalone agents without external workspaces into Standalone", () => {
    const standalone = makeAgent({
      id: "factorio",
      workspaceSummary: {
        id: "agent_home:factorio",
        name: "factorio",
        anchor: "/Users/dev/.holon/agents/factorio",
      },
    });
    const repoAgent = makeAgent({
      id: "holon-dev",
      workspaceSummary: {
        id: "ws-holon",
        name: "holon",
        anchor: "/Users/dev/src/holon-run/holon",
      },
    });

    const primary = resolvePrimaryWorkspace(standalone, "Standalone");
    expect(primary.key).toBe(STANDALONE_WORKSPACE_KEY);
    expect(primary.isStandalone).toBe(true);

    const options = buildWorkspaceFilterOptions([standalone, repoAgent], "Standalone");
    expect(options).toEqual([
      {
        key: "path:/Users/dev/src/holon-run/holon",
        label: "holon",
        anchor: "/Users/dev/src/holon-run/holon",
        count: 1,
      },
      {
        key: STANDALONE_WORKSPACE_KEY,
        label: "Standalone",
        count: 1,
      },
    ]);
  });
});
