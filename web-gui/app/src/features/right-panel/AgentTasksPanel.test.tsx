// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import type { TaskSummary } from "../../runtime/types";
import { AgentTasksPanel, RECENT_TASK_LIMIT } from "./AgentTasksPanel";

const task = (id: string): TaskSummary => ({
  id, kind: "command_task", status: "completed", summary: `Task ${id}`,
});

const roots: Root[] = [];
beforeEach(() => vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true));
afterEach(async () => {
  for (const root of roots.splice(0)) await act(async () => root.unmount());
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

async function renderPanel(loadRecentTasks: (agentId: string) => Promise<TaskSummary[]>, tasks: TaskSummary[] = []) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  roots.push(root);
  const onOpenTask = vi.fn();
  await act(async () => root.render(
    <AgentTasksPanel agentId="parent" tasks={tasks} activeTaskCount={tasks.length} loadRecentTasks={loadRecentTasks} onOpenTask={onOpenTask} />,
  ));
  const recentDetails = host.querySelector(".inspector-details-list") as HTMLDetailsElement;
  async function openRecent() {
    await act(async () => {
      recentDetails.open = true;
      recentDetails.dispatchEvent(new Event("toggle"));
    });
  }
  return { host, root, openRecent, recentDetails, onOpenTask };
}

describe("AgentTasksPanel", () => {
  it("queries only when recently completed is expanded, even with no active tasks", async () => {
    const load = vi.fn().mockResolvedValue([task("done")]);
    const { host, openRecent, onOpenTask } = await renderPanel(load);
    expect(load).not.toHaveBeenCalled();
    await openRecent();
    expect(load).toHaveBeenCalledWith("parent");
    expect(host.textContent).toContain("Task done");
    await act(async () => (host.querySelector(".task-button") as HTMLButtonElement).click());
    expect(onOpenTask).toHaveBeenCalledWith(task("done"));
  });

  it("bounds recent rows to 20 and leaves command/legacy rows without navigation", async () => {
    const load = vi.fn().mockResolvedValue(Array.from({ length: 25 }, (_, index) => task(String(index))));
    const { host, openRecent } = await renderPanel(load);
    await openRecent();
    expect(host.querySelectorAll(".task-button")).toHaveLength(RECENT_TASK_LIMIT);
    expect(host.querySelector("a")).toBeNull();
  });

  it("shows fetch failures and supports an explicit retry", async () => {
    const load = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce([task("retried")]);
    const { host, openRecent } = await renderPanel(load);
    await openRecent();
    expect(host.querySelector("[role=alert]")?.textContent).toContain("offline");
    await act(async () => (host.querySelector(".text-button") as HTMLButtonElement).click());
    expect(load).toHaveBeenCalledTimes(2);
    expect(host.querySelector("[role=alert]")).toBeNull();
    expect(host.textContent).toContain("Task retried");
  });

  it("links active and recent structured targets with their identity", async () => {
    const active: TaskSummary = {
      ...task("active"), kind: "child_agent", status: "running",
      target: { kind: "child_agent", agent_id: "private child", incarnation: 7 },
    };
    const recent: TaskSummary = {
      ...task("recent"), kind: "agent_message_wait",
      target: { kind: "agent_message_wait", agent_id: "peer", incarnation: 3 },
    };
    const { host, openRecent } = await renderPanel(vi.fn().mockResolvedValue([recent]), [active]);
    expect(host.querySelector("a")?.getAttribute("href")).toBe("/agents/private%20child/conversation?incarnation=7");
    await openRecent();
    expect(Array.from(host.querySelectorAll("a"), (link) => link.getAttribute("href")))
      .toEqual(["/agents/private%20child/conversation?incarnation=7", "/agents/peer/conversation?incarnation=3"]);
  });

  it("does not derive a destination from a summary or rebind a legacy identity", async () => {
    const legacy: TaskSummary = {
      ...task("legacy"), kind: "child_agent", summary: "InvokeAgent child_guess",
      target: { kind: "child_agent", agent_id: "legacy-child" },
    };
    const { host } = await renderPanel(vi.fn().mockResolvedValue([]), [legacy, {
      ...task("unknown"), summary: "InvokeAgent child_guess",
    }]);
    expect(host.querySelector("a")).toBeNull();
    expect(host.textContent).toContain("legacy-child");
  });

  it("does not publish a request that completes after the section closes", async () => {
    let resolve!: (tasks: TaskSummary[]) => void;
    const load = vi.fn(() => new Promise<TaskSummary[]>((done) => { resolve = done; }));
    const { host, openRecent, recentDetails } = await renderPanel(load);
    await openRecent();
    await act(async () => {
      recentDetails.open = false;
      recentDetails.dispatchEvent(new Event("toggle"));
    });
    await act(async () => resolve([task("stale")]));
    expect(host.textContent).not.toContain("Task stale");
  });

  it("resets recent history when switching agents and ignores the previous agent's response", async () => {
    let resolve!: (tasks: TaskSummary[]) => void;
    const load = vi.fn(() => new Promise<TaskSummary[]>((done) => { resolve = done; }));
    const { host, root, openRecent, onOpenTask } = await renderPanel(load);
    await openRecent();
    await act(async () => root.render(
      <AgentTasksPanel agentId="other" tasks={[]} activeTaskCount={0} loadRecentTasks={load} onOpenTask={onOpenTask} />,
    ));
    await act(async () => resolve([task("parent-only")]));
    expect(host.textContent).not.toContain("Task parent-only");
    expect((host.querySelector(".inspector-details-list") as HTMLDetailsElement).open).toBe(false);
    expect(load).toHaveBeenCalledTimes(1);
  });
});
