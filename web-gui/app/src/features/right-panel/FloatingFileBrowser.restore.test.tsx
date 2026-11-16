// @vitest-environment happy-dom
import { StrictMode, act, type ComponentProps, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import i18n from "../../i18n/config";
import { ClipboardProvider } from "../../components/ClipboardProvider";
import { useSelectedAgentRefreshContext } from "../../runtime/selected-agent-refresh-context";
import type { AgentSummary, WorkspaceDirectoryListing, WorkspaceFileContent, WorkspaceFileLocation } from "../../runtime/types";
import { RightSidePanel } from "./RightSidePanel";

const mocks = vi.hoisted(() => ({
  browse: vi.fn<(location: WorkspaceFileLocation) => Promise<WorkspaceDirectoryListing>>(),
  read: vi.fn<(location: WorkspaceFileLocation) => Promise<WorkspaceFileContent>>(),
  fetchPath: vi.fn<(location: WorkspaceFileLocation) => Promise<WorkspaceFileContent>>(),
  workDetail: vi.fn(),
}));

vi.mock("../../runtime/runtime-store", async () => {
  const { create } = await import("zustand");
  return {
    getRuntimeConnectionConfig: () => ({ mode: "local" }),
    useRuntimeStore: create(() => ({
      sessionsByAgentId: {},
      browseWorkspaceDir: mocks.browse,
      readWorkspaceFile: mocks.read,
      fetchWorkspacePath: mocks.fetchPath,
      loadAgentWorkItemDetail: mocks.workDetail,
      workspaceFileUrl: ({ workspaceId, executionRootId, path }: WorkspaceFileLocation) =>
        `/api/workspaces/${workspaceId}/file?root=${executionRootId}&path=${path}`,
    })),
  };
});
vi.mock("../../runtime/useSelectedAgentRefresh", () => ({ refreshSelectedAgentResources: vi.fn() }));
vi.mock("../../runtime/client", () => ({
  createRuntimeClient: () => ({ desktopCapabilities: async () => ({ reveal_in_finder: false }) }),
  resolveRuntimeApiBase: () => "",
}));
vi.mock("../../components/file-references/use-references", () => ({ useFileIdentity: () => "runtime-identity" }));
vi.mock("./FileBrowserPopover", () => ({
  FileBrowserPopover: ({ children }: { children: ReactNode }) => <div>{children}</div>,
}));
vi.mock("../../components/MarkdownContent", () => ({
  MarkdownContent: ({ text }: { text: string }) => <p>{text}</p>,
  WorkspaceImage: () => <img alt="" />,
}));

const agent: AgentSummary = {
  id: "web", incarnation: 1, badge: "W", profile: "", lifecycle: "idle",
  focusSummary: "", workspace: "", attention: "", model: "", footer: "",
  subtitle: "", lastBrief: "", lastTurnTime: "", pending: 0, activeTaskCount: 0,
  waitingCount: 0, posture: "idle", postureReason: "",
};
const noop = () => {};
const base: ComponentProps<typeof RightSidePanel> = {
  agent, connection: { mode: "local", summary: "", source: "http" },
  view: { kind: "file_browser", agentId: "web", workspaceId: "ws-a", executionRootId: "root-a", initialFilePath: "README.md" },
  open: true, mode: "normal", full: false, width: 400,
  onResize: noop, onToggleMode: noop, onOpenWorkItemDetail: noop, onOpenTask: noop,
  onRefreshAgentSkills: noop, onRefreshAvailableSkills: noop, onEnableAgentSkill: noop,
  onDisableAgentSkill: noop, onOpenSkill: noop, onShowAgentOverview: noop,
  onNavigateBack: noop, onSelectView: noop, onBrowseFiles: noop, onClose: noop,
};

function content(target: WorkspaceFileLocation, text: string): WorkspaceFileContent {
  return {
    ...target, executionRootId: target.executionRootId ?? "root-a",
    absolutePath: `/roots/${target.executionRootId}/${target.path}`,
    rootKind: "worktree", kind: "file", type: "file", mimeType: "text/markdown",
    size: text.length, truncated: false, content: text,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

describe("floating file browser restore target", () => {
  let host: HTMLDivElement;
  let root: Root;
  const resizeCallbacks: ((entries: ResizeObserverEntry[], observer: ResizeObserver) => void)[] = [];

  beforeEach(() => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("ResizeObserver", class {
      constructor(callback: (entries: ResizeObserverEntry[], observer: ResizeObserver) => void) { resizeCallbacks.push(callback); }
      observe() {} disconnect() {}
    });
    mocks.browse.mockReset().mockImplementation(async (target) => ({
      ...target, executionRootId: target.executionRootId ?? "root-a", absolutePath: "/roots/root-a",
      rootKind: "worktree", kind: "directory", type: "directory",
      entries: [
        { name: "README.md", type: "file", size: 11, mimeType: "text/markdown" },
        { name: "notes.md", type: "file", size: 10, mimeType: "text/markdown" },
      ],
    }));
    mocks.read.mockReset().mockImplementation(async (target) => content(target, "README body"));
    mocks.fetchPath.mockReset().mockImplementation(async (target) => content(target, "README body"));
    mocks.workDetail.mockReset().mockResolvedValue(undefined);
    useSelectedAgentRefreshContext.setState({ current: null });
    resizeCallbacks.length = 0;
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  });
  afterEach(async () => {
    await act(async () => root.unmount());
    useSelectedAgentRefreshContext.setState({ current: null });
    host.remove();
    vi.unstubAllGlobals();
  });

  async function render() {
    if (!i18n.isInitialized) {
      await act(async () => { await new Promise<void>((resolve) => i18n.on("initialized", () => resolve())); });
    }
    // StrictMode keeps the effect remount in the loop: a restore read that only
    // survives the first mount pass would leave the target loading forever.
    await act(async () => root.render(<StrictMode><ClipboardProvider><RightSidePanel {...base} /></ClipboardProvider></StrictMode>));
    await act(async () => { await Promise.resolve(); });
  }
  const dialog = () => document.body.querySelector<HTMLDivElement>('[role="dialog"]');
  const floatingTitle = () => dialog()?.querySelector(".file-browser-standalone-title strong")?.textContent;
  const panelTitle = () => host.querySelector(".panel-header strong")?.textContent;
  async function click(node: Element | null | undefined) {
    expect(node, "clickable node").toBeTruthy();
    await act(async () => { (node as HTMLElement).click(); });
  }
  function entry(scope: ParentNode, name: string) {
    return [...scope.querySelectorAll("button.file-browser-entry")]
      .find((node) => node.querySelector(".file-browser-entry-name")?.textContent === name);
  }
  const openFloating = () => host.querySelector('button[aria-label="Open in floating window"]');
  async function closeFloating() {
    await act(async () => { dialog()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });
  }
  // The floating window only lists its directory in the split layout, which
  // needs a wide measured browser.
  async function setWide() {
    await act(async () => {
      for (const callback of resizeCallbacks) callback([{ contentRect: { width: 1200 } } as ResizeObserverEntry], {} as ResizeObserver);
    });
  }

  it("restores the selected path when the window closed before its content loaded", async () => {
    const pending = deferred<WorkspaceFileContent>();
    mocks.read.mockImplementation(async (target) =>
      target.path === "notes.md" ? pending.promise : content(target, "README body"));
    await render();
    expect(panelTitle()).toBe("README.md");

    await click(openFloating());
    await setWide();
    expect(dialog()).not.toBeNull();
    await click(entry(dialog()!, "notes.md"));
    // The title follows the selection while the content read is still open.
    expect(floatingTitle()).toBe("notes.md");
    await closeFloating();
    expect(dialog()).toBeNull();

    // Reopening keeps the interrupted selection as the restore target and
    // reads the file again instead of reviving the pending read.
    await click(openFloating());
    expect(floatingTitle()).toBe("notes.md");
    expect(mocks.read.mock.calls.filter(([target]) => target.path === "notes.md").length).toBe(2);
    await act(async () => { pending.resolve(content({ workspaceId: "ws-a", executionRootId: "root-a", path: "notes.md" }, "Notes body")); await pending.promise; });
    expect(dialog()!.textContent).toContain("Notes body");
    // The side panel keeps its own file.
    expect(panelTitle()).toBe("README.md");
  });

  it("keeps the last selected path when an earlier read resolves late", async () => {
    const pending = deferred<WorkspaceFileContent>();
    mocks.read.mockImplementation(async (target) =>
      target.path === "notes.md" ? pending.promise : content(target, "README body"));
    await render();
    await click(openFloating());
    await setWide();
    await click(entry(dialog()!, "notes.md"));
    await click(entry(dialog()!, "README.md"));
    expect(floatingTitle()).toBe("README.md");
    await act(async () => { pending.resolve(content({ workspaceId: "ws-a", executionRootId: "root-a", path: "notes.md" }, "Notes body")); await pending.promise; });
    expect(floatingTitle()).toBe("README.md");
    expect(dialog()!.textContent).toContain("README body");
    expect(dialog()!.textContent).not.toContain("Notes body");
  });

  it("keeps the restore target and the error state when the read fails", async () => {
    mocks.read.mockImplementation(async (target) => {
      if (target.path === "notes.md") throw new Error("read failed");
      return content(target, "README body");
    });
    await render();
    await click(openFloating());
    await setWide();
    await click(entry(dialog()!, "notes.md"));
    expect(dialog()!.querySelector(".inspector-error")?.textContent).toContain("read failed");
    await closeFloating();
    await click(openFloating());
    expect(floatingTitle()).toBe("notes.md");
    expect(dialog()!.querySelector(".inspector-error")?.textContent).toContain("read failed");
    expect(panelTitle()).toBe("README.md");
  });
});
