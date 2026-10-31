// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import type { AgentSummary, WorkItemSummary } from "../../runtime/types";
import { CurrentWorkBar } from "./CurrentWorkBar";

const work: WorkItemSummary = { id: "w1", objective: "Ship the fix", state: "open" } as WorkItemSummary;
const base = {
  id: "a", incarnation: 1, lifecycle: "awake-idle", posture: "idle",
  currentRunId: null, tasks: [], waits: [],
} as unknown as AgentSummary;

const roots: Root[] = [];
beforeEach(() => vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true));
afterEach(async () => {
  for (const root of roots.splice(0)) await act(async () => root.unmount());
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

async function renderBar(root: Root, agent: AgentSummary) {
  await act(async () => root.render(
    <CurrentWorkBar agent={agent} onOpenWorkItem={vi.fn()} onOpenTask={vi.fn()} />,
  ));
}

describe("CurrentWorkBar completion flash", () => {
  it("still flashes when the completed item arrives after the current focus disappears", async () => {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    roots.push(root);

    await renderBar(root, { ...base, currentWork: work });
    expect(host.querySelector(".current-work-bar")).toBeTruthy();

    // State lands first: focus gone while the work-items list still shows the open item.
    await renderBar(root, { ...base, currentWork: undefined, workItems: [work] });
    expect(host.querySelector(".current-work-bar")).toBeNull();

    // The work-items refresh lands afterwards with the completed item.
    await renderBar(root, { ...base, currentWork: undefined, workItems: [{ ...work, state: "completed" }] });
    const bar = host.querySelector(".current-work-bar") as HTMLElement;
    expect(bar.getAttribute("data-state")).toBe("completed");
  });
});
