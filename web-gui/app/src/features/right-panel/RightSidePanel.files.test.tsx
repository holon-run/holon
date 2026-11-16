// @vitest-environment happy-dom
import { act, type ComponentProps } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import { ClipboardProvider } from "../../components/ClipboardProvider";
import type { AgentSummary, WorkItemSummary } from "../../runtime/types";
import { FileBrowserPanel, type FileBrowserSnapshot } from "./FileBrowserPanel";
import { RightSidePanel } from "./RightSidePanel";

vi.mock("../../runtime/useSelectedAgentRefresh", () => ({
  refreshSelectedAgentResources: vi.fn(),
}));
vi.mock("./FileBrowserPanel", () => ({ FileBrowserPanel: vi.fn(() => null) }));

const agent: AgentSummary = {
  id: "web", incarnation: 1, badge: "W", profile: "", lifecycle: "idle",
  focusSummary: "", workspace: "", attention: "", model: "", footer: "",
  subtitle: "", lastBrief: "", lastTurnTime: "", pending: 0, activeTaskCount: 0,
  waitingCount: 0, posture: "idle", postureReason: "",
};
const plan = (hash: string) => ({
  path: "/test/plan.md", workspaceId: "plans", relativePath: "plan.md", hash,
});
const work: WorkItemSummary = {
  id: "w", objective: "Plan", state: "open", current: true, revision: 1, planArtifact: plan("old"),
};
const noop = () => {};
const base: ComponentProps<typeof RightSidePanel> = {
  agent, connection: { mode: "local", summary: "", source: "http" },
  view: { kind: "file_browser", agentId: "web", workspaceId: "plans", initialFilePath: "plan.md" },
  open: true, mode: "normal", full: false, width: 400,
  onResize: noop, onToggleMode: noop, onOpenWorkItemDetail: noop, onOpenTask: noop,
  onRefreshAgentSkills: noop, onRefreshAvailableSkills: noop, onEnableAgentSkill: noop,
  onDisableAgentSkill: noop, onOpenSkill: noop, onShowAgentOverview: noop,
  onNavigateBack: noop, onSelectView: noop, onBrowseFiles: noop, onClose: noop,
};
let host: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  vi.mocked(FileBrowserPanel).mockClear();
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
async function render(listed: WorkItemSummary[], detailed = work) {
  await act(async () => root.render(
    <ClipboardProvider><RightSidePanel {...base} agent={{ ...agent, workItems: listed }}
      workItemDetailsById={{ w: { workItem: detailed, dataRequestId: 1 } }} /></ClipboardProvider>,
  ));
  return vi.mocked(FileBrowserPanel).mock.calls.at(-1)![0].fileRevisions;
}

describe("RightSidePanel plan revision dataflow", () => {
  it("forwards a newer list hash without reopening the paused WorkItem detail", async () => {
    expect(await render([work])).toEqual([{ workspaceId: "plans", path: "plan.md", hash: "old", workItemId: "w" }]);
    expect(await render([{ ...work, revision: 2, planArtifact: plan("new") }]))
      .toEqual([{ workspaceId: "plans", path: "plan.md", hash: "new", workItemId: "w" }]);
  });

  it("preserves newer detail descriptors and includes list-only plan files", async () => {
    expect(await render([work], { ...work, revision: 2, planArtifact: plan("new") }))
      .toEqual([{ workspaceId: "plans", path: "plan.md", hash: "new", workItemId: "w" }]);
    expect(await render([{ ...work, id: "other", planArtifact: { ...plan("other"), relativePath: "other.md" } }]))
      .toEqual([
        { workspaceId: "plans", path: "plan.md", hash: "old", workItemId: "w" },
        { workspaceId: "plans", path: "other.md", hash: "other", workItemId: "other" },
      ]);
  });
});

describe("RightSidePanel floating file browser", () => {
  const snapshot = (overrides: Partial<FileBrowserSnapshot> = {}): FileBrowserSnapshot => ({
    identity: "identity",
    currentPath: "docs",
    listing: { workspaceId: "plans", path: "docs" },
    selectedFile: { path: "docs/readme.md", loading: false },
    viewMode: "preview",
    fragment: "L3",
    showHidden: false,
    showRendered: true,
    filterText: "",
    sortKey: "name",
    sortAsc: true,
    directoryVisible: true,
    scroll: {},
    history: [],
    ...overrides,
  } as FileBrowserSnapshot);

  it("opens a modal dialog from the panel's floating action and closes it with Escape", async () => {
    await act(async () => root.render(
      <ClipboardProvider><RightSidePanel {...base} agent={{ ...agent, workItems: [] }} /></ClipboardProvider>,
    ));
    const panelProps = vi.mocked(FileBrowserPanel).mock.calls.at(-1)![0];
    expect(panelProps.onOpenFloating).toBeTypeOf("function");
    await act(async () => { panelProps.onOpenFloating!(); });
    const dialog = document.body.querySelector('[role="dialog"]');
    expect(dialog).not.toBeNull();
    expect(dialog!.getAttribute("aria-modal")).toBe("true");
    await act(async () => { dialog!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
  });

  it("starts at the panel's current file and keeps the floating position across reopen", async () => {
    await act(async () => root.render(
      <ClipboardProvider><RightSidePanel {...base} agent={{ ...agent, workItems: [] }} /></ClipboardProvider>,
    ));
    const panelProps = vi.mocked(FileBrowserPanel).mock.calls.at(-1)![0];
    await act(async () => { panelProps.onSnapshot!(snapshot()); });
    await act(async () => { panelProps.onOpenFloating!(); });
    let floatingProps = vi.mocked(FileBrowserPanel).mock.calls.at(-1)![0];
    expect(floatingProps.workspaceId).toBe("plans");
    expect(floatingProps.initialPath).toBe("docs");
    expect(floatingProps.initialFilePath).toBe("docs/readme.md");
    expect(floatingProps.initialFragment).toBe("L3");
    // Browsing inside the floating window, then closing and reopening, restores
    // the floating window's own last position instead of the panel's.
    await act(async () => {
      floatingProps.onSnapshot!(snapshot({ currentPath: "notes", selectedFile: { path: "notes/a.md", loading: false } }));
    });
    const dialog = document.body.querySelector('[role="dialog"]');
    await act(async () => { dialog!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });
    await act(async () => { panelProps.onOpenFloating!(); });
    floatingProps = vi.mocked(FileBrowserPanel).mock.calls.at(-1)![0];
    expect(floatingProps.initialFilePath).toBe("docs/readme.md");
    expect(floatingProps.snapshot?.currentPath).toBe("notes");
    expect(floatingProps.snapshot?.selectedFile?.path).toBe("notes/a.md");
  });
});
