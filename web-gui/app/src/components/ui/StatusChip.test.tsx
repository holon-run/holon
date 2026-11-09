import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import "../../i18n";
import { StatusBadge, AgentStateBadge, AgentStatusIndicator } from "./StatusChip";
import { deriveAgentDisplayStatus } from "../../runtime/agent-status";
import i18next from "i18next";
import statusFixture from "../../../../../tests/fixtures/agent-status-display.json";

function renderBadge(kind: Parameters<typeof StatusBadge>[0]["kind"], value: string): string {
  return renderToStaticMarkup(<StatusBadge kind={kind} value={value} />);
}

function hasSpin(markup: string): boolean {
  return markup.includes("animate-spin");
}

function hasIcon(markup: string, icon: string): boolean {
  return markup.includes(`lucide-${icon}`);
}

describe("AgentStatusIndicator shared projection", () => {
  it.each(statusFixture.cases)("$name stays consistent in compact rows and headers", ({ input, expected }) => {
    const { stateIsCurrent, ...summary } = { ...statusFixture.defaults, ...input };
    const status = deriveAgentDisplayStatus(summary, i18next.t, stateIsCurrent);
    for (const className of ["agent-row-status-dot", "header-status"]) {
      const markup = renderToStaticMarkup(<AgentStatusIndicator status={status} className={className} />);
      expect(markup).toContain(`data-status-kind="${expected.kind}"`);
      expect(markup).toContain(`data-status-icon="${expected.icon}"`);
      expect(markup).toContain(`data-tone="${expected.tone}"`);
      const icon = expected.icon === "spinner" ? "loader-circle"
        : expected.icon === "circle-help" ? "circle-question-mark" : expected.icon;
      expect(hasIcon(markup, icon)).toBe(true);
      expect(markup).toContain(`aria-label="${status.label}"`);
      expect(markup).toContain('role="img"');
      expect(markup).toContain('title="');
      expect(markup).toContain('data-tooltip="');
      expect(markup).toContain('aria-hidden="true"');
      expect(hasSpin(markup)).toBe(expected.kind === "running");
      expect(markup).not.toContain(`<span>${status.label}</span>`);
    }
  });

  it("can show the same label without replacing the primary icon", () => {
    const status = deriveAgentDisplayStatus({ posture: "has_queued_input" }, i18next.t);
    const markup = renderToStaticMarkup(<AgentStatusIndicator status={status} showLabel />);
    expect(markup).toContain(`<span>${status.label}</span>`);
    expect(hasIcon(markup, "circle-arrow-right")).toBe(true);
    expect(hasSpin(markup)).toBe(false);
  });
});

describe("StatusBadge task kind", () => {
  const cases: Array<{ status: string; spins: boolean; icon: string }> = [
    { status: "queued", spins: false, icon: "clock" },
    { status: "running", spins: true, icon: "loader-circle" },
    { status: "cancelling", spins: true, icon: "loader-circle" },
    { status: "completed", spins: false, icon: "circle-check" },
    { status: "failed", spins: false, icon: "circle-x" },
    { status: "cancelled", spins: false, icon: "ban" },
    { status: "interrupted", spins: false, icon: "square" },
  ];

  for (const { status, spins, icon } of cases) {
    it(`task "${status}" renders ${icon} and ${spins ? "spins" : "does not spin"}`, () => {
      const markup = renderBadge("task", status);
      expect(hasIcon(markup, icon)).toBe(true);
      expect(hasSpin(markup)).toBe(spins);
    });
  }
});

describe("StatusBadge agent kind - posture mapping", () => {
  const cases: Array<{ state: string; spins: boolean }> = [
    { state: "active_turn", spins: true },
    { state: "has_queued_input", spins: false },
    { state: "has_runnable_work", spins: false },
    { state: "waiting_for_task", spins: false },
    { state: "waiting_for_external", spins: false },
    { state: "waiting_for_operator", spins: false },
    { state: "blocked", spins: false },
    { state: "idle", spins: false },
    { state: "archived", spins: false },
  ];

  for (const { state, spins } of cases) {
    it(`agent "${state}" ${spins ? "spins" : "does not spin"}`, () => {
      const markup = renderBadge("agent", state);
      expect(hasSpin(markup)).toBe(spins);
    });
  }
});

describe("StatusBadge work kind - readiness mapping", () => {
  const cases: Array<{ state: string; spins: boolean }> = [
    { state: "runnable", spins: false },
    { state: "yielded", spins: false },
    { state: "waiting_for_operator", spins: false },
    { state: "blocked", spins: false },
    { state: "completed", spins: false },
    { state: "open", spins: false },
    { state: "in_progress", spins: true },
  ];

  for (const { state, spins } of cases) {
    it(`work "${state}" ${spins ? "spins" : "does not spin"}`, () => {
      const markup = renderBadge("work", state);
      expect(hasSpin(markup)).toBe(spins);
    });
  }
});

describe("StatusBadge terminal states never spin", () => {
  it("task completed does not spin", () => {
    expect(hasSpin(renderBadge("task", "completed"))).toBe(false);
  });
  it("task failed does not spin", () => {
    expect(hasSpin(renderBadge("task", "failed"))).toBe(false);
  });
  it("task cancelled does not spin", () => {
    expect(hasSpin(renderBadge("task", "cancelled"))).toBe(false);
  });
  it("work blocked does not spin", () => {
    expect(hasSpin(renderBadge("work", "blocked"))).toBe(false);
  });
  it("agent idle does not spin", () => {
    expect(hasSpin(renderBadge("agent", "idle"))).toBe(false);
  });
});

describe("AgentStateBadge posture priority", () => {
  it("keeps stopped lifecycle authoritative over stale posture", () => {
    const markup = renderToStaticMarkup(<AgentStateBadge lifecycle="stopped" posture="active_turn" />);
    expect(markup).toContain('data-status-kind="stopped"');
    expect(hasSpin(markup)).toBe(false);
  });

  it("prefers posture over lifecycle", () => {
    const markup = renderToStaticMarkup(
      <AgentStateBadge lifecycle="asleep" posture="active_turn" />,
    );
    expect(hasSpin(markup)).toBe(true);
  });

  it("falls back to lifecycle when posture is absent", () => {
    const markup = renderToStaticMarkup(
      <AgentStateBadge lifecycle="stopped" posture={null} />,
    );
    expect(hasSpin(markup)).toBe(false);
  });
});
