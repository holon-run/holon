// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import type { TaskSummary, TaskTargetRef } from "../../runtime/types";
import { AgentTasksPanel } from "./AgentTasksPanel";

const task = (id: string, overrides: Partial<TaskSummary> = {}): TaskSummary => ({
  id,
  kind: "command_task",
  status: "running",
  summary: `Task ${id}`,
  ...overrides,
});

const roots: Root[] = [];
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
});
afterEach(async () => {
  for (const root of roots.splice(0)) await act(async () => root.unmount());
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

async function renderPanel(
  tasks: TaskSummary[] = [],
  onOpenConversation?: (target: TaskTargetRef) => void,
  activeTaskCount = tasks.length,
) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  roots.push(root);
  const onOpenTask = vi.fn();
  await act(async () => root.render(
    <AgentTasksPanel
      agentId="parent"
      incarnation={1}
      tasks={tasks}
      activeTaskCount={activeTaskCount}
      onOpenTask={onOpenTask}
      onOpenConversation={onOpenConversation}
    />,
  ));
  return { host, root, onOpenTask };
}

describe("AgentTasksPanel", () => {
  it("renders a quiet idle summary with an empty body and no history drawer when no tasks are active", async () => {
    const { host } = await renderPanel([]);
    expect(host.textContent).toContain("0 active");
    expect(host.querySelector(".inspector-details-list")).toBeNull();
    expect(host.querySelector(".collapsible-inspector-body")?.children).toHaveLength(0);
  });

  it("opens task detail on click and leaves command rows without conversation links", async () => {
    const active = task("build", { command: "cargo check --all-targets", workdir: "/Users/test/src/holon" });
    const { host, onOpenTask } = await renderPanel([active]);
    expect(host.textContent).toContain("Task build");
    expect(host.textContent).toContain("cargo check --all-targets");
    expect(host.textContent).toContain("src/holon");
    expect(host.querySelector("a")).toBeNull();
    await act(async () => (host.querySelector(".task-button") as HTMLButtonElement).click());
    expect(onOpenTask).toHaveBeenCalledWith(active);
  });

  it("shows bounded active truncation notice when activeTaskCount exceeds visible tasks", async () => {
    const { host } = await renderPanel([task("one")], undefined, 5);
    expect(host.textContent).toContain("Showing 1 of 5 active items.");
  });

  it("links active structured targets with their identity", async () => {
    const child: TaskSummary = {
      ...task("active"),
      kind: "child_agent",
      target: { kind: "child_agent", agent_id: "private child", incarnation: 7 },
    };
    const peerWait: TaskSummary = {
      ...task("wait"),
      kind: "agent_message_wait",
      target: { kind: "agent_message_wait", agent_id: "peer", incarnation: 3 },
    };
    const { host } = await renderPanel([child, peerWait]);
    expect(Array.from(host.querySelectorAll("a"), (link) => link.getAttribute("href"))).toEqual([
      "/agents/private%20child/conversation?incarnation=7&from=parent",
      "/agents/peer/conversation?incarnation=3&from=parent",
    ]);
  });

  it("routes plain link clicks through in-app conversation navigation", async () => {
    const active: TaskSummary = {
      ...task("active"),
      kind: "child_agent",
      target: { kind: "child_agent", agent_id: "private child", incarnation: 7 },
    };
    const onOpenConversation = vi.fn();
    const { host } = await renderPanel([active], onOpenConversation);
    const link = host.querySelector(".task-conversation-open") as HTMLAnchorElement;
    const event = new MouseEvent("click", { bubbles: true, cancelable: true });
    link.dispatchEvent(event);
    expect(onOpenConversation).toHaveBeenCalledWith({ kind: "child_agent", agent_id: "private child", incarnation: 7 });
    expect(event.defaultPrevented).toBe(true);
  });

  it("leaves modified link activations to the browser default", async () => {
    const active: TaskSummary = {
      ...task("active"),
      kind: "child_agent",
      target: { kind: "child_agent", agent_id: "private child", incarnation: 7 },
    };
    const onOpenConversation = vi.fn();
    const { host } = await renderPanel([active], onOpenConversation);
    const link = host.querySelector(".task-conversation-open") as HTMLAnchorElement;
    const event = new MouseEvent("click", { bubbles: true, cancelable: true, ctrlKey: true });
    link.dispatchEvent(event);
    expect(onOpenConversation).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
  });

  it("does not derive a destination from a summary or rebind a legacy identity", async () => {
    const legacy: TaskSummary = {
      ...task("legacy"),
      kind: "child_agent",
      summary: "InvokeAgent child_guess",
      target: { kind: "child_agent", agent_id: "legacy-child" },
    };
    const { host } = await renderPanel([legacy, { ...task("unknown"), summary: "InvokeAgent child_guess" }]);
    expect(host.querySelector("a")).toBeNull();
    expect(host.textContent).toContain("legacy-child");
  });
});
