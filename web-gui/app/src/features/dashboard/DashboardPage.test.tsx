import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import "../../i18n";
import statusFixture from "../../../../../tests/fixtures/agent-status-display.json";
import type { AgentStatusInput } from "../../runtime/agent-status";
import type { AgentSummary } from "../../runtime/types";
import { DashboardPage } from "./DashboardPage";

function renderDashboard(input: AgentStatusInput, stateIsCurrent: boolean, error?: string) {
  const agent: AgentSummary = {
    id: "agent", badge: "A", profile: "", focusSummary: "Keep the brief preview",
    workspace: "", attention: "input", model: "", footer: "", subtitle: "",
    lastBrief: "", lastTurnTime: "", postureReason: "",
    ...statusFixture.defaults, ...input,
    lifecycle: input.lifecycle ?? "", posture: input.posture ?? "",
  };
  return renderToStaticMarkup(
    <DashboardPage
      agents={[agent]}
      metrics={[]}
      connection={{ mode: "local", source: "http", summary: "", error }}
      discovery={{ mode: "authoritative", freshness: stateIsCurrent ? "fresh" : "stale", retryAttempt: 0 }}
      loading={false}
      onRefresh={() => {}}
      onOpenAgent={() => {}}
    />,
  );
}

describe("Dashboard agent status", () => {
  it.each(statusFixture.cases)("$name renders one primary status from the shared fixture", ({ input, expected }) => {
    const { stateIsCurrent, ...summary } = { ...statusFixture.defaults, ...input };
    const markup = renderDashboard(summary, stateIsCurrent);
    expect(markup.match(/data-status-kind=/g)).toHaveLength(1);
    expect(markup).toContain(`data-status-kind="${expected.kind}"`);
    expect(markup).toContain(`data-status-icon="${expected.icon}"`);
    expect(markup).toContain(`data-tone="${expected.tone}"`);
    expect(markup.includes("agent-status-spinner")).toBe(expected.kind === "running");
    expect(markup).toContain("Keep the brief preview");
  });

  it("does not advertise cached activity when disconnected", () => {
    const markup = renderDashboard({ posture: "active_turn" }, true, "Offline");
    expect(markup).toContain('data-status-kind="unknown"');
    expect(markup).toContain("Cached last-known state");
    expect(markup).not.toContain("agent-status-spinner");
  });

  it("suppresses idle boilerplate, resolves human-readable workspace names, and surfaces pending input reminders", () => {
    const idleAgent: AgentSummary = {
      id: "alpha-idle",
      badge: "A",
      profile: "public",
      lifecycle: "accepts_external_messages",
      posture: "idle",
      focusSummary: "no queued input, active turn, runnable work, or active waits",
      workspace: "ws-b93fdd1aae3d40d3",
      workspaceSummary: { id: "ws-b93fdd1aae3d40d3", name: "holon-repo", anchor: "/work/holon-repo" },
      attention: "none",
      model: "openai/gpt-5.4",
      footer: "accepts_external_messages · idle",
      subtitle: "",
      lastBrief: "",
      lastTurnTime: "",
      postureReason: "no queued input, active turn, runnable work, or active waits",
      pending: 0,
      activeTaskCount: 0,
      waitingCount: 0,
    };
    const attentionAgent: AgentSummary = {
      ...idleAgent,
      id: "zeta-attention",
      name: "Android 发布助手",
    };
    const markup = renderToStaticMarkup(
      <DashboardPage
        agents={[idleAgent, attentionAgent]}
        metrics={[]}
        pendingNotifications={[
          {
            id: "notif-1",
            kind: "operator_input_required",
            created_at: "2026-11-26T08:00:00Z",
            target: { agent_id: "zeta-attention", work_item_id: "work_release_1" },
            title: "Verify Android release keystore",
            summary: "Confirm upload key alias before signing bundle",
          },
        ]}
        connection={{ mode: "local", source: "http", summary: "" }}
        discovery={{ mode: "authoritative", freshness: "fresh", retryAttempt: 0 }}
        loading={false}
        onRefresh={() => {}}
        onOpenAgent={() => {}}
      />,
    );
    expect(markup).not.toContain('class="agent-card-summary"');
    expect(markup).toContain("<footer><span>Ready</span>");
    expect(markup).not.toContain("Ready for new operator input");
    expect(markup).toContain("holon-repo");
    expect(markup).toContain("Needs your action");
    expect(markup).toContain("Verify Android release keystore");
    expect(markup).toContain("Confirm upload key alias before signing bundle");
    // Roster card order stays decoupled from async notifications so cards do not jump on load.
    const rosterStart = markup.indexOf('class="agent-roster"');
    expect(markup.indexOf("alpha-idle", rosterStart)).toBeLessThan(markup.indexOf("Android 发布助手", rosterStart));
  });

  it("surfaces multi-workspace +N badge and deduplicated external workspace filter options", () => {
    const multiWsAgent: AgentSummary = {
      id: "holon-ios",
      badge: "H",
      profile: "public",
      lifecycle: "ready",
      posture: "idle",
      focusSummary: "idle",
      workspace: "holon-ios",
      workspaceSummary: {
        id: "agent_home:holon-ios",
        name: "holon-ios",
        anchor: "/Users/dev/.holon/agents/holon-ios",
      },
      attachedWorkspaces: [
        { workspaceId: "ws-holon", name: "holon", anchor: "/Users/dev/src/holon-run/holon" },
        { workspaceId: "ws-inbox", name: "agentinbox", anchor: "/Users/dev/src/holon-run/agentinbox" },
      ],
      attention: "none",
      model: "openai/gpt-5.4",
      footer: "ready · idle",
      subtitle: "",
      lastBrief: "",
      lastTurnTime: "",
      postureReason: "idle",
      pending: 0,
      activeTaskCount: 0,
      waitingCount: 0,
    };
    const standaloneAgent: AgentSummary = {
      ...multiWsAgent,
      id: "factorio",
      workspace: "factorio",
      workspaceSummary: {
        id: "agent_home:factorio",
        name: "factorio",
        anchor: "/Users/dev/.holon/agents/factorio",
      },
      attachedWorkspaces: [],
    };

    const markup = renderToStaticMarkup(
      <DashboardPage
        agents={[multiWsAgent, standaloneAgent]}
        metrics={[]}
        connection={{ mode: "local", source: "http", summary: "" }}
        discovery={{ mode: "authoritative", freshness: "fresh", retryAttempt: 0 }}
        loading={false}
        onRefresh={() => {}}
        onOpenAgent={() => {}}
      />,
    );

    expect(markup).not.toContain("Needs your action");
    expect(markup).toContain("+1");
    expect(markup).toContain("holon (1)");
    expect(markup).toContain("agentinbox (1)");
    expect(markup).toContain("Standalone agents (1)");
  });

  it("does not surface autonomous timer or task waits in Needs your action", () => {
    const timerWaitingAgent: AgentSummary = {
      id: "timer-agent",
      badge: "T",
      profile: "public",
      lifecycle: "asleep",
      posture: "waiting-for-external",
      focusSummary: "waiting for daily digest timer",
      workspace: "timer-agent",
      attention: "waiting",
      model: "openai/gpt-5.4",
      footer: "asleep · waiting-for-external",
      subtitle: "",
      lastBrief: "",
      lastTurnTime: "",
      postureReason: "work_item_timer_wait",
      pending: 0,
      activeTaskCount: 0,
      waitingCount: 1,
      waitingReason: "awaiting_timer",
    };

    const markup = renderToStaticMarkup(
      <DashboardPage
        agents={[timerWaitingAgent]}
        metrics={[]}
        pendingNotifications={[]}
        connection={{ mode: "local", source: "http", summary: "" }}
        discovery={{ mode: "authoritative", freshness: "fresh", retryAttempt: 0 }}
        loading={false}
        onRefresh={() => {}}
        onOpenAgent={() => {}}
      />,
    );

    expect(markup).not.toContain("Needs your action");
    expect(markup).toContain('data-status-kind="waiting"');
  });

  it("surfaces Pending and Recent notification tabs and collapses Recent by default when idle", () => {
    const agent: AgentSummary = {
      id: "holon-web",
      name: "Web 助手",
      badge: "W",
      profile: "public",
      lifecycle: "ready",
      posture: "idle",
      focusSummary: "idle",
      workspace: "holon",
      attention: "none",
      model: "openai/gpt-5.4",
      footer: "ready · idle",
      subtitle: "",
      lastBrief: "",
      lastTurnTime: "",
      postureReason: "idle",
      pending: 0,
      activeTaskCount: 0,
      waitingCount: 0,
    };

    const withBoth = renderToStaticMarkup(
      <DashboardPage
        agents={[agent]}
        metrics={[]}
        pendingNotifications={[
          {
            id: "wait-1",
            kind: "operator_input_required",
            created_at: "2026-11-26T10:00:00Z",
            target: { agent_id: "holon-web", work_item_id: "work_1" },
            title: "Confirm deployment target",
            summary: "Should we deploy to staging or production?",
          },
        ]}
        recentNotifications={[
          {
            id: "brief_1",
            kind: "work_item_completed",
            created_at: "2026-11-26T09:00:00Z",
            target: { agent_id: "holon-web", work_item_id: "work_0" },
            title: "Build release bundle",
            summary: "All checks passed",
          },
        ]}
        connection={{ mode: "local", source: "http", summary: "" }}
        discovery={{ mode: "authoritative", freshness: "fresh", retryAttempt: 0 }}
        loading={false}
        onRefresh={() => {}}
        onOpenAgent={() => {}}
      />,
    );

    expect(withBoth).toContain("Needs your action");
    expect(withBoth).toContain("Recent");
    expect(withBoth).toContain("Confirm deployment target");

    const recentOnly = renderToStaticMarkup(
      <DashboardPage
        agents={[agent]}
        metrics={[]}
        pendingNotifications={[]}
        recentNotifications={[
          {
            id: "brief_1",
            kind: "work_item_completed",
            created_at: "2026-11-26T09:00:00Z",
            target: { agent_id: "holon-web", work_item_id: "work_0" },
            title: "Build release bundle",
            summary: "All checks passed",
          },
        ]}
        connection={{ mode: "local", source: "http", summary: "" }}
        discovery={{ mode: "authoritative", freshness: "fresh", retryAttempt: 0 }}
        loading={false}
        onRefresh={() => {}}
        onOpenAgent={() => {}}
      />,
    );

    expect(recentOnly).toContain("Recent");
    expect(recentOnly).toContain("Show");
    // Collapsed when idle so historical notifications do not push the roster down.
    expect(recentOnly).not.toContain("Build release bundle");
  });
});
