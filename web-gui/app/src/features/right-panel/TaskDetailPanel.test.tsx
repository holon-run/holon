// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import { ClipboardProvider } from "../../components/ClipboardProvider";
import type { TaskSummary } from "../../runtime/types";
import { TaskDetailPanel } from "./TaskDetailPanel";

beforeEach(() => vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true));
afterEach(() => vi.unstubAllGlobals());

describe("TaskDetailPanel conversation entry", () => {
  it("keeps the structured destination available even when output cannot load", async () => {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    const task: TaskSummary = {
      id: "child-task", kind: "child_agent", status: "completed", summary: "Child task",
      target: { kind: "child_agent", agent_id: "private-child", incarnation: 9 },
    };
    try {
      await act(async () => root.render(
        <ClipboardProvider>
          <TaskDetailPanel agentId="parent" task={task} detailState={{ error: "output unavailable" }} />
        </ClipboardProvider>,
      ));
      expect(host.querySelector(".task-conversation-open")?.getAttribute("href")).toBe("/agents/private-child/conversation?incarnation=9&from=parent");
      expect(host.textContent).toContain("output unavailable");
    } finally {
      await act(async () => root.unmount());
      host.remove();
    }
  });
});

describe("TaskDetailPanel conversation activation", () => {
  const task: TaskSummary = {
    id: "child-task", kind: "child_agent", status: "completed", summary: "Child task",
    target: { kind: "child_agent", agent_id: "private-child", incarnation: 9 },
  };

  it("hands plain link clicks to in-app navigation", async () => {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    const onOpenConversation = vi.fn();
    try {
      await act(async () => root.render(
        <ClipboardProvider>
          <TaskDetailPanel agentId="parent" task={task} onOpenConversation={onOpenConversation} />
        </ClipboardProvider>,
      ));
      const link = host.querySelector(".task-conversation-open") as HTMLAnchorElement;
      const event = new MouseEvent("click", { bubbles: true, cancelable: true });
      link.dispatchEvent(event);
      expect(onOpenConversation).toHaveBeenCalledWith({ kind: "child_agent", agent_id: "private-child", incarnation: 9 });
      expect(event.defaultPrevented).toBe(true);
    } finally {
      await act(async () => root.unmount());
      host.remove();
    }
  });
});
