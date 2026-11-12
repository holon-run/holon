// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import { registerSelectedAgentRefreshContext, useSelectedAgentRefreshContext } from "../../runtime/selected-agent-refresh-context";
import type { AgentResourceReadOptions } from "../../runtime/useVisibleAgentResourceRefresh";
import type { WorkItemSummary } from "../../runtime/types";
import { AgentWorkItemsPanel, COMPLETED_WORK_ITEM_LIMIT } from "./AgentWorkItemsPanel";

const item = (id: string, state = "completed"): WorkItemSummary => ({ id, objective: `Work ${id}`, state });
let host: HTMLDivElement;
let root: Root;
const load = vi.fn<(agentId: string, options?: AgentResourceReadOptions) => Promise<WorkItemSummary[]>>();
const onSelect = vi.fn();
function admit(agentId = "parent", incarnation = 1) {
  registerSelectedAgentRefreshContext({ key: `${agentId}:${incarnation}`, agentId, incarnation, scopeKey: "test", onAccessError: vi.fn() });
}
async function advance(ms = 0) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}
async function render(workItems: WorkItemSummary[] = [], agentId = "parent", incarnation = 1, activeWorkItemCount = workItems.length) {
  await act(async () => root.render(
    <AgentWorkItemsPanel agentId={agentId} incarnation={incarnation} workItems={workItems} activeWorkItemCount={activeWorkItemCount} loadCompletedWorkItems={load} onSelect={onSelect} />,
  ));
}
async function toggle(selector: string, open: boolean) {
  await act(async () => {
    const details = host.querySelector(selector) as HTMLDetailsElement;
    details.open = open;
    details.dispatchEvent(new Event("toggle"));
  });
  await advance();
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  load.mockReset().mockResolvedValue([item("done")]);
  onSelect.mockReset();
  admit();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  useSelectedAgentRefreshContext.setState({ current: null });
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("AgentWorkItemsPanel", () => {
  it("keeps history discoverable with no active work and makes zero hidden history reads", async () => {
    await render();
    expect(host.querySelector(".inspector-details-list summary")?.textContent).toContain("Completed work items");
    await advance(120_000);
    expect(load).not.toHaveBeenCalled();
    await toggle(".inspector-details-list", true);
    expect(load).toHaveBeenCalledExactlyOnceWith("parent", { incarnation: 1, signal: expect.any(AbortSignal) });
    expect(host.textContent).toContain("Work done");
    await act(async () => (host.querySelector(".work-item-button") as HTMLButtonElement).click());
    expect(onSelect).toHaveBeenCalledWith(item("done"));
  });

  it.each([".inspector-details-list", ".collapsible-inspector-card"])("aborts pending history and stops refresh when %s collapses", async (selector) => {
    let resolve!: (items: WorkItemSummary[]) => void;
    load.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await render();
    await toggle(".inspector-details-list", true);
    const signal = load.mock.calls[0][1]?.signal;
    await toggle(selector, false);
    expect(signal?.aborted).toBe(true);
    await act(async () => resolve([item("obsolete")]));
    await advance(120_000);
    expect(load).toHaveBeenCalledTimes(1);
    expect(host.textContent).not.toContain("Work obsolete");
    await toggle(selector, true);
    expect(load).toHaveBeenCalledTimes(2);
    expect(host.textContent).toContain("Work done");
  });

  it.each([["other", 1], ["parent", 2]] as const)("isolates history across identity %s:%s", async (agentId, incarnation) => {
    await render();
    await toggle(".inspector-details-list", true);
    let resolve!: (items: WorkItemSummary[]) => void;
    load.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await act(async () => (host.querySelector(".text-button") as HTMLButtonElement).click());
    await advance();
    const signal = load.mock.calls[1][1]?.signal;
    await act(async () => admit(agentId, incarnation));
    await render([], agentId, incarnation);
    expect(signal?.aborted).toBe(true);
    expect((host.querySelector(".inspector-details-list") as HTMLDetailsElement).open).toBe(false);
    expect(host.textContent).not.toContain("Work done");
    await act(async () => resolve([item("obsolete")]));
    await advance(60_000);
    expect(host.textContent).not.toContain("Work obsolete");
    await toggle(".inspector-details-list", true);
    expect(load).toHaveBeenLastCalledWith(agentId, { incarnation, signal: expect.any(AbortSignal) });
  });

  it("updates active state without overwriting or re-fetching independent history", async () => {
    await render([item("active", "open")]);
    await toggle(".inspector-details-list", true);
    await render([item("replacement", "blocked")]);
    expect(host.textContent).toContain("Work replacement");
    expect(host.textContent).not.toContain("Work active");
    expect(host.textContent).toContain("Work done");
    expect(load).toHaveBeenCalledTimes(1);
    await render([]);
    expect(host.textContent).not.toContain("Work replacement");
    expect(host.textContent).toContain("Work done");
  });

  it("shows bounded active and history windows without claiming they are complete", async () => {
    load.mockResolvedValue(Array.from({ length: 60 }, (_, i) => item(String(i))));
    await render([item("visible", "open")], "parent", 1, 70);
    expect(host.textContent).toContain("Showing 1 of 70 active items");
    await toggle(".inspector-details-list", true);
    expect(host.querySelectorAll(".inspector-details-list .work-item-button")).toHaveLength(COMPLETED_WORK_ITEM_LIMIT);
  });

  it("keeps a history failure local and supports explicit retry", async () => {
    load.mockRejectedValueOnce(new Error("history unavailable"));
    await render([item("active", "open")]);
    await toggle(".inspector-details-list", true);
    expect(host.textContent).toContain("history unavailable");
    expect(host.textContent).toContain("Work active");
    await act(async () => (host.querySelector(".text-button") as HTMLButtonElement).click());
    await advance();
    expect(host.textContent).not.toContain("history unavailable");
    expect(host.textContent).toContain("Work done");
  });
});
