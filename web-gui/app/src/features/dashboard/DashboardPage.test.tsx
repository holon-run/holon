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
});
