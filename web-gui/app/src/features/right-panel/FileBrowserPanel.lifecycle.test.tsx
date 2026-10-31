// @vitest-environment happy-dom
import { act, type ComponentProps, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ClipboardProvider } from "../../components/ClipboardProvider";
import { useRuntimeStore } from "../../runtime/runtime-store";
import type { WorkspaceDirectoryListing, WorkspaceFileContent, WorkspaceFileLocation } from "../../runtime/types";
import { FileBrowserPanel, type FileBrowserSnapshot, type FileRevision } from "./FileBrowserPanel";

const mocks = vi.hoisted(() => ({
  browse: vi.fn<(location: WorkspaceFileLocation) => Promise<WorkspaceDirectoryListing>>(),
  read: vi.fn<(location: WorkspaceFileLocation) => Promise<WorkspaceFileContent>>(),
  fetchPath: vi.fn(),
  readImage: vi.fn(),
}));

vi.mock("../../runtime/runtime-store", async () => {
  const { create } = await import("zustand");
  return {
    getRuntimeConnectionConfig: () => ({ mode: "local" }),
    useRuntimeStore: create(() => ({
      browseWorkspaceDir: mocks.browse,
      readWorkspaceFile: mocks.read,
      fetchWorkspacePath: mocks.fetchPath,
      workspaceFileUrl: ({ workspaceId, executionRootId, path }: WorkspaceFileLocation) =>
        `/api/workspaces/${workspaceId}/file?root=${executionRootId}&path=${path}`,
    })),
  };
});
vi.mock("../../runtime/client", () => ({
  createRuntimeClient: () => ({ desktopCapabilities: async () => ({ reveal_in_finder: false }) }),
  resolveRuntimeApiBase: () => "",
}));
vi.mock("../../components/file-references/use-references", () => ({ useFileIdentity: () => "runtime-identity" }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string, options?: { defaultValue?: string }) => options?.defaultValue ?? key }),
}));
vi.mock("./FileBrowserPopover", () => ({
  FileBrowserPopover: ({ children }: { children: ReactNode }) => <div>{children}</div>,
}));
vi.mock("../../components/MarkdownContent", async () => {
  const { useEffect } = await import("react");
  return {
    MarkdownContent: ({ text }: { text: string }) => <p>{text}</p>,
    WorkspaceImage: ({ workspaceId, executionRootId, path }: WorkspaceFileLocation) => {
      useEffect(() => { mocks.readImage({ workspaceId, executionRootId, path }); }, [workspaceId, executionRootId, path]);
      return <img alt={path} />;
    },
  };
});

const location = { workspaceId: "ws-a", executionRootId: "root-a", path: "work-items/wi/plan.md" };
const revision = (hash: string): FileRevision => ({ ...location, hash });
function content(target: WorkspaceFileLocation, text = "Original plan"): WorkspaceFileContent {
  return { ...target, executionRootId: target.executionRootId ?? "root-a", absolutePath: `/roots/${target.executionRootId}/${target.path}`,
    rootKind: "worktree", kind: "file", type: "file", mimeType: "text/markdown", size: text.length, truncated: false, content: text };
}

describe("FileBrowserPanel explicit refresh", () => {
  let host: HTMLDivElement;
  let root: Root;
  let snapshot: FileBrowserSnapshot | undefined;
  let props: ComponentProps<typeof FileBrowserPanel>;

  beforeEach(() => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
    vi.useFakeTimers();
    mocks.browse.mockReset().mockImplementation(async (target) => ({
      ...target, executionRootId: target.executionRootId ?? "root-a", absolutePath: `/roots/${target.executionRootId}/${target.path}`,
      rootKind: "worktree", kind: "directory", type: "directory",
      entries: [{ name: "plan.md", type: "file", size: 13, mimeType: "text/markdown" }],
    }));
    mocks.read.mockReset().mockImplementation(async (target) => content(target));
    mocks.fetchPath.mockReset();
    mocks.readImage.mockReset();
    snapshot = undefined;
    props = { workspaceId: location.workspaceId, executionRootId: location.executionRootId, initialFilePath: location.path,
      fileRevisions: [revision("hash-a")], onSnapshot: (value) => { snapshot = value; } };
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  });
  afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });
  async function render(overrides: Partial<typeof props> = {}) {
    props = { ...props, ...overrides };
    await act(async () => root.render(<ClipboardProvider><FileBrowserPanel {...props} /></ClipboardProvider>));
  }
  async function clickButton(text: string) {
    const button = [...host.querySelectorAll("button")].find((node) => node.textContent === text);
    expect(button, text).toBeDefined();
    await act(async () => button!.click());
  }
  async function refreshFromMenu(text: string) {
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="fileBrowser.fileActions"]')!.click());
    await clickButton(text);
  }

  it("does not poll or reread on checkpoint renders; revision hints preserve body, scroll, and selection", async () => {
    await render();
    const body = host.querySelector<HTMLElement>(".file-browser-markdown")!;
    const paragraph = body.querySelector("p")!;
    body.scrollTop = 86;
    body.scrollLeft = 12;
    const range = document.createRange();
    range.selectNodeContents(paragraph);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);

    await act(async () => {
      useRuntimeStore.setState({ selectedAgentId: "checkpoint-updated-agent" });
      await vi.advanceTimersByTimeAsync(120_000);
    });
    await render({ fileRevisions: [revision("hash-a")] });
    await render({ fileRevisions: [revision("hash-b")] });

    expect(mocks.browse).toHaveBeenCalledTimes(1);
    expect(mocks.read).toHaveBeenCalledTimes(1);
    expect(host.querySelector(".file-browser-markdown")).toBe(body);
    expect(body.querySelector("p")).toBe(paragraph);
    expect(body.scrollTop).toBe(86);
    expect(body.scrollLeft).toBe(12);
    expect(selection.toString()).toBe("Original plan");
    expect(host.querySelector(".file-browser-update-hint")?.textContent).toContain("File may have changed.");

    mocks.read.mockImplementation(async (target) => content(target, "Updated plan"));
    await clickButton("Reload");
    expect(mocks.read).toHaveBeenLastCalledWith(location);
    expect(mocks.read).toHaveBeenCalledTimes(2);
    expect(mocks.browse).toHaveBeenCalledTimes(1);
    expect(host.textContent).toContain("Updated plan");
    expect(host.querySelector(".file-browser-update-hint")).toBeNull();
    expect(snapshot?.selectedFile?.revisionHash).toBe("hash-b");
  });

  it("applies root-less plan artifact revisions to the browsed root", async () => {
    // Work item plan artifacts know workspace + path but not the execution
    // root; the panel opens them without an executionRootId prop.
    props = { ...props, executionRootId: undefined, fileRevisions: [{ workspaceId: location.workspaceId, path: location.path, hash: "hash-a" }] };
    await render();
    expect(snapshot?.selectedFile?.revisionHash).toBe("hash-a");

    await render({ fileRevisions: [{ workspaceId: location.workspaceId, path: location.path, hash: "hash-b" }] });

    expect(mocks.read).toHaveBeenCalledTimes(1);
    expect(host.querySelector(".file-browser-update-hint")?.textContent).toContain("File may have changed.");
    expect(host.textContent).toContain("Original plan");
  });

  it("restores the saved revision without reading, but explicit refresh bypasses the old snapshot", async () => {
    await render();
    const saved = snapshot!;
    await act(async () => root.unmount());
    root = createRoot(host);
    await render({ snapshot: saved, fileRevisions: [revision("hash-b")] });
    expect(mocks.browse).toHaveBeenCalledTimes(1);
    expect(mocks.read).toHaveBeenCalledTimes(1);
    expect(host.querySelector(".file-browser-update-hint")).not.toBeNull();
    mocks.read.mockImplementation(async (target) => content(target, "Fresh from disk"));
    await refreshFromMenu("fileBrowser.refreshFile");
    await render();
    expect(mocks.read).toHaveBeenCalledTimes(2);
    expect(host.textContent).toContain("Fresh from disk");
    expect(host.querySelector(".file-browser-update-hint")).toBeNull();
  });

  it.each([
    { workspaceId: "ws-b" },
    { executionRootId: "root-b" },
    { path: "work-items/another/plan.md" },
  ])("ignores revisions for a different locator: %j", async (different) => {
    await render();
    await render({ fileRevisions: [{ ...revision("hash-b"), ...different }] });
    expect(mocks.read).toHaveBeenCalledTimes(1);
    expect(host.querySelector(".file-browser-update-hint")).toBeNull();
  });

  it.each([{ workspaceId: "ws-b" }, { executionRootId: "root-b" }])("rejects a snapshot from another scope: %j", async (different) => {
    await render();
    const saved = snapshot!;
    mocks.read.mockImplementation(async (target) => content(target, `Scope ${target.workspaceId}/${target.executionRootId}`));
    await render({ ...different, snapshot: saved });
    expect(mocks.read).toHaveBeenCalledTimes(2);
    expect(mocks.read).toHaveBeenLastCalledWith({ ...location, ...different });
    expect(host.textContent).not.toContain("Original plan");
  });

  it("does not restore a different initial file from the same workspace and root", async () => {
    await render();
    const saved = snapshot!;
    mocks.fetchPath.mockImplementation(async (target: WorkspaceFileLocation) => content(target));
    mocks.read.mockImplementation(async (target) => content(target, "Another file"));
    await render({ initialFilePath: "work-items/wi/other.md", snapshot: saved });
    expect(mocks.read).toHaveBeenLastCalledWith({ ...location, path: "work-items/wi/other.md" });
    expect(host.textContent).toContain("Another file");
    expect(host.textContent).not.toContain("Original plan");
  });

  it("baselines a detail that becomes available after opening without rereading", async () => {
    await render({ fileRevisions: [] });
    await render({ fileRevisions: [revision("hash-a")] });
    expect(host.querySelector(".file-browser-update-hint")).toBeNull();
    await render({ fileRevisions: [revision("hash-b")] });
    expect(host.querySelector(".file-browser-update-hint")).not.toBeNull();
    expect(mocks.read).toHaveBeenCalledTimes(1);
  });

  it("keeps a newer revision hint if the artifact changes during an explicit read", async () => {
    await render();
    await render({ fileRevisions: [revision("hash-b")] });
    let finish!: (value: WorkspaceFileContent) => void;
    mocks.read.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await clickButton("Reload");
    await render({ fileRevisions: [revision("hash-c")] });
    await act(async () => finish(content(location, "Read hash-b")));
    expect(host.textContent).toContain("Read hash-b");
    expect(host.querySelector(".file-browser-update-hint")).not.toBeNull();
    expect(snapshot?.selectedFile?.revisionHash).toBe("hash-b");
    expect(mocks.read).toHaveBeenCalledTimes(2);
  });

  it("discards an old refresh response after switching execution roots", async () => {
    await render();
    await render({ fileRevisions: [revision("hash-b")] });
    let finish!: (value: WorkspaceFileContent) => void;
    mocks.read.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await clickButton("Reload");
    mocks.read.mockImplementation(async (target) => content(target, "New root content"));
    await render({ executionRootId: "root-b" });
    await act(async () => finish(content(location, "Obsolete old root content")));
    expect(host.textContent).toContain("New root content");
    expect(host.textContent).not.toContain("Obsolete old root content");
    expect(mocks.read).toHaveBeenLastCalledWith({ ...location, executionRootId: "root-b" });
  });

  it("explicit directory refresh reads again instead of restoring its listing snapshot", async () => {
    await render({ initialFilePath: undefined, initialPath: "work-items/wi" });
    const saved = snapshot!;
    await act(async () => root.unmount());
    root = createRoot(host);
    await render({ snapshot: saved });
    expect(mocks.browse).toHaveBeenCalledTimes(1);
    mocks.browse.mockImplementation(async (target) => ({
      ...saved.listing!, ...target, entries: [{ name: "new-file.md", type: "file", size: 1 }],
    }));
    await refreshFromMenu("fileBrowser.refreshDir");
    expect(mocks.browse).toHaveBeenCalledTimes(2);
    expect(mocks.browse).toHaveBeenLastCalledWith({ ...location, path: "work-items/wi" });
    expect(host.textContent).toContain("new-file.md");
    expect(mocks.read).not.toHaveBeenCalled();
  });

  it("remounts an image only for explicit refresh, not metadata updates", async () => {
    mocks.browse.mockImplementation(async (target) => ({
      ...target, executionRootId: "root-a", absolutePath: "/roots/root-a/images", rootKind: "worktree", kind: "directory", type: "directory",
      entries: [{ name: "chart.png", type: "file", mimeType: "image/png", size: 1 }],
    }));
    await render({ initialFilePath: "images/chart.png", fileRevisions: [] });
    await render();
    expect(mocks.readImage).toHaveBeenCalledTimes(1);
    await refreshFromMenu("fileBrowser.refreshFile");
    expect(mocks.readImage).toHaveBeenCalledTimes(2);
    expect(mocks.read).not.toHaveBeenCalled();
  });

  it.each([
    { name: "clip.mp4", mimeType: "video/mp4", selector: "video" },
    { name: "sound.mp3", mimeType: "audio/mpeg", selector: "audio" },
  ])("invalidates the media URL only on explicit refresh: $name", async ({ name, mimeType, selector }) => {
    mocks.browse.mockImplementation(async (target) => ({
      ...target, executionRootId: "root-a", absolutePath: "/roots/root-a/media", rootKind: "worktree", kind: "directory", type: "directory",
      entries: [{ name, type: "file", mimeType, size: 1 }],
    }));
    await render({ initialFilePath: `media/${name}`, fileRevisions: [] });
    const before = host.querySelector(selector)!.getAttribute("src");
    await render();
    expect(host.querySelector(selector)!.getAttribute("src")).toBe(before);
    await refreshFromMenu("fileBrowser.refreshFile");
    const firstRefresh = host.querySelector(selector)!.getAttribute("src");
    expect(firstRefresh).not.toBe(before);
    expect(firstRefresh).toContain("root=root-a");
    await refreshFromMenu("fileBrowser.refreshFile");
    expect(host.querySelector(selector)!.getAttribute("src")).not.toBe(firstRefresh);
    expect(mocks.read).not.toHaveBeenCalled();
  });
});
