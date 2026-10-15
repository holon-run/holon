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
      expect(host.querySelector("a")?.getAttribute("href")).toBe("/agents/private-child/conversation?incarnation=9");
      expect(host.textContent).toContain("output unavailable");
    } finally {
      await act(async () => root.unmount());
      host.remove();
    }
  });
});
