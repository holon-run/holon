// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import { RuntimeHttpError } from "../../runtime/client";
import { useRuntimeStore } from "../../runtime/runtime-store";
import type { AgentSummary } from "../../runtime/types";
import { AgentOverviewPanel } from "./AgentOverviewPanel";

type Timer = NonNullable<AgentSummary["timers"]>[number];
const timer = (id: string, status: Timer["status"], rest: Partial<Timer> = {}): Timer => ({
  id, status, summary: id, agent_id: "agent", duration_ms: 1000, repeat: false,
  created_at: "2026-01-01T00:00:00Z", next_fire_at: null, last_fired_at: null, fire_count: 0,
  ...rest,
});
const initial = useRuntimeStore.getState();
let root: Root;
let host: HTMLDivElement;
const control = vi.fn();

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  control.mockReset().mockResolvedValue(undefined);
  useRuntimeStore.setState({
    bootstrap: { ...initial.bootstrap, connection: { ...initial.bootstrap.connection, error: undefined } },
    discovery: { ...initial.discovery, freshness: "fresh" },
    controlAgentTimer: control,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  useRuntimeStore.setState(initial, true);
  vi.unstubAllGlobals();
});

async function renderTimers(timers: Timer[]) {
  const agent: AgentSummary = {
    id: "agent", incarnation: 1, badge: "A", profile: "", lifecycle: "idle", focusSummary: "", workspace: "",
    attention: "", model: "", footer: "", subtitle: "", lastBrief: "", lastTurnTime: "",
    pending: 0, activeTaskCount: 0, waitingCount: 0, posture: "", postureReason: "", tasks: [], timers,
  };
  await act(async () => root.render(
    <AgentOverviewPanel
      agent={agent}
      onOpenWorkItemDetail={vi.fn()}
      onOpenTask={vi.fn()}
      onRefreshAgentSkills={vi.fn()}
      onDisableAgentSkill={vi.fn()}
      onOpenSkill={vi.fn()}
      onOpenSkillManager={vi.fn()}
      onBrowseFiles={vi.fn()}
    />,
  ));
}

function rows() {
  return [...host.querySelectorAll<HTMLElement>(".agent-timer-row")];
}
function buttons(row: HTMLElement) {
  return [...row.querySelectorAll("button")];
}

describe("Agent overview timer controls", () => {
  it("shows actions by server status, frozen relative delays, and calendar resume behavior", async () => {
    await renderTimers([
      timer("active", "active", { next_fire_at: "2026-01-01T00:00:01Z" }),
      timer("one shot", "paused", { paused_remaining_ms: 0 }),
      timer("fixed repeat", "paused", { interval_ms: 1000, repeat: true, paused_remaining_ms: 500 }),
      timer("calendar", "paused", { calendar: { expression: "0 * * * *", timezone: "UTC" }, repeat: true }),
      timer("completed", "completed"),
      timer("cancelled", "cancelled"),
    ]);
    const [active, oneShot, interval, calendar, completed, cancelled] = rows();
    expect(buttons(active).map((button) => button.textContent)).toEqual(["Pause", "Cancel"]);
    expect(buttons(oneShot).map((button) => button.textContent)).toEqual(["Resume", "Cancel"]);
    expect(buttons(completed)).toEqual([]);
    expect(buttons(cancelled)).toEqual([]);
    expect(active.textContent).toContain("Next run");
    expect(oneShot.textContent).toContain("Frozen delay: 0 ms");
    expect(interval.textContent).toContain("Frozen delay: 500 ms");
    expect(calendar.textContent).toContain("next calendar match");
    for (const paused of [oneShot, interval, calendar]) expect(paused.textContent).not.toContain("Next run");
    expect(host.querySelector(".agent-timers-card")?.textContent).toContain("already queued or claimed");
  });

  it("disables timer actions while pending and takes the next action from refreshed state", async () => {
    let finish!: () => void;
    control.mockReturnValueOnce(new Promise<void>((resolve) => { finish = resolve; }));
    await renderTimers([timer("one", "active")]);
    await act(async () => buttons(rows()[0])[0].click());
    expect(control).toHaveBeenNthCalledWith(1, "agent", "one", "pause", 1);
    expect(buttons(rows()[0]).map((button) => button.textContent)).toEqual(["Pausing…", "Cancel"]);
    expect(buttons(rows()[0]).every((button) => button.disabled)).toBe(true);
    await renderTimers([timer("one", "paused", { paused_remaining_ms: 900 })]);
    await act(async () => finish());
    await act(async () => buttons(rows()[0])[0].click());
    expect(control).toHaveBeenNthCalledWith(2, "agent", "one", "resume", 1);
  });

  it("reports conflicts and disables stale controls until the runtime reconnects", async () => {
    control.mockRejectedValueOnce(new RuntimeHttpError("POST", "/timers/one/pause", 409));
    await renderTimers([timer("one", "active")]);
    await act(async () => buttons(rows()[0])[0].click());
    expect(host.querySelector(".agent-timers-card [role=alert]")?.textContent).toContain("state conflicted");
    await act(async () => useRuntimeStore.setState({ discovery: { ...initial.discovery, freshness: "stale" } }));
    expect(buttons(rows()[0]).every((button) => button.disabled)).toBe(true);
    await renderTimers([timer("one", "paused", { paused_remaining_ms: 900 })]);
    await act(async () => useRuntimeStore.setState({ discovery: { ...initial.discovery, freshness: "fresh" } }));
    expect(buttons(rows()[0])[0].textContent).toBe("Resume");
    expect(buttons(rows()[0])[0].disabled).toBe(false);
  });
});
