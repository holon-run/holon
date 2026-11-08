// @vitest-environment happy-dom
import { act, type ComponentProps, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { Highlighter } from "shiki";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";

import type { WorkspaceDirectoryListing, WorkspaceFileContent, WorkspaceFileLocation } from "../../runtime/types";

const mocks = vi.hoisted(() => ({
  browse: vi.fn<(location: WorkspaceFileLocation) => Promise<WorkspaceDirectoryListing>>(),
  read: vi.fn<(location: WorkspaceFileLocation) => Promise<WorkspaceFileContent>>(),
  snapshot: vi.fn(),
}));

vi.mock("shiki", async (importOriginal) => {
  const actual = await importOriginal<typeof import("shiki")>();
  return { ...actual, createHighlighter: vi.fn(actual.createHighlighter) };
});
vi.mock("../../runtime/runtime-store", async () => {
  const { create } = await import("zustand");
  return {
    getRuntimeConnectionConfig: () => ({ mode: "local" }),
    useRuntimeStore: create(() => ({
      browseWorkspaceDir: mocks.browse,
      readWorkspaceFile: mocks.read,
      workspaceFileUrl: () => "/file",
    })),
  };
});
vi.mock("../../runtime/client", () => ({
  createRuntimeClient: () => ({ desktopCapabilities: async () => ({ reveal_in_finder: false }) }),
  resolveRuntimeApiBase: () => "",
}));
vi.mock("../../components/ClipboardProvider", () => ({ useCopyText: () => vi.fn() }));
vi.mock("../../components/file-references/use-references", () => ({ useFileIdentity: () => "runtime-identity" }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string, options?: { defaultValue?: string }) => options?.defaultValue ?? key }),
}));
vi.mock("./FileBrowserPopover", () => ({
  FileBrowserPopover: ({ children }: { children: ReactNode }) => <div>{children}</div>,
}));
vi.mock("../../components/MarkdownContent", () => ({
  MarkdownContent: ({ text }: { text: string }) => <p>{text}</p>,
  WorkspaceImage: () => null,
}));

const firstSource = 'const first = "<unsafe>&";\nexport default first;\n';
const secondSource = "const second = 2;\nexport default second;\n";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function fileContent(target: WorkspaceFileLocation, text: string): WorkspaceFileContent {
  return {
    ...target, executionRootId: "root-a", absolutePath: `/root/${target.path}`,
    rootKind: "worktree", kind: "file", type: "file", mimeType: "text/plain",
    size: text.length, truncated: false, content: text,
  };
}

describe("FileBrowserPanel syntax highlighting lifecycle", () => {
  let Panel: typeof import("./FileBrowserPanel").FileBrowserPanel;
  let createHighlighter: MockInstance<typeof import("shiki").createHighlighter>;
  let realCreateHighlighter: typeof import("shiki").createHighlighter;
  let host: HTMLDivElement;
  let root: Root | null;
  let props: ComponentProps<typeof Panel>;
  let highlighters: Set<Highlighter>;

  beforeEach(async () => {
    // Reset only the module cache, not the real Shiki implementation: each test
    // owns its initialization promise and the success oracle uses real tokens.
    vi.resetModules();
    const actualShiki = await vi.importActual<typeof import("shiki")>("shiki");
    const shiki = await import("shiki");
    realCreateHighlighter = actualShiki.createHighlighter;
    createHighlighter = vi.mocked(shiki.createHighlighter);
    createHighlighter.mockReset().mockImplementation(realCreateHighlighter);
    Panel = (await import("./FileBrowserPanel")).FileBrowserPanel;
    highlighters = new Set();
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
    mocks.browse.mockReset().mockImplementation(async (target) => ({
      ...target, executionRootId: "root-a", absolutePath: "/root/src",
      rootKind: "worktree", kind: "directory", type: "directory",
      entries: ["first.ts", "second.ts", "notes.unknown", "empty.ts"].map((name) => ({
        name, type: "file" as const, size: 1, mimeType: "text/plain",
      })),
    }));
    mocks.read.mockReset().mockImplementation(async (target) =>
      fileContent(target, target.path === "src/second.ts" ? secondSource : firstSource));
    mocks.snapshot.mockClear();
    props = {
      workspaceId: "ws-a", executionRootId: "root-a", initialFilePath: "src/first.ts",
      onSnapshot: mocks.snapshot,
    };
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  });

  async function unmount() {
    const mounted = root;
    root = null;
    await act(async () => mounted?.unmount());
  }

  afterEach(async () => {
    await unmount();
    host.remove();
    for (const highlighter of highlighters) highlighter.dispose();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  async function render(overrides: Partial<typeof props> = {}) {
    props = { ...props, ...overrides };
    await act(async () => root!.render(<Panel {...props} />));
  }

  async function refreshFile() {
    const actions = host.querySelector<HTMLButtonElement>('[aria-label="fileBrowser.fileActions"]');
    expect(actions).not.toBeNull();
    await act(async () => actions!.click());
    const refresh = [...host.querySelectorAll("button")].find((button) =>
      button.textContent === "fileBrowser.refreshFile");
    expect(refresh).toBeDefined();
    await act(async () => refresh!.click());
  }

  function expectSource(text: string, plain: boolean) {
    const pre = host.querySelector(".file-browser-code > pre.shiki");
    expect(pre).not.toBeNull();
    expect(pre!.classList.contains("file-browser-plain")).toBe(plain);
    expect([...pre!.querySelectorAll("code > .line")].map((line) => line.textContent))
      .toEqual(text.split("\n"));
    expect(pre!.querySelector("unsafe")).toBeNull();
  }

  async function realHighlighter() {
    const highlighter = await realCreateHighlighter({ themes: ["github-light"], langs: ["typescript"] });
    highlighters.add(highlighter);
    return highlighter;
  }

  async function finishInitialization(index = 0) {
    const result = createHighlighter.mock.results[index];
    expect(result?.type).toBe("return");
    await act(async () => {
      highlighters.add(await result.value);
    });
  }

  it("initializes real Shiki and renders colored tokens with the original source lines", async () => {
    await render();
    expect(createHighlighter).toHaveBeenCalledTimes(1);
    await finishInitialization();

    expectSource(firstSource, false);
    expect(host.querySelector(".file-browser-code .line span[style]")).not.toBeNull();
    expect(mocks.browse).toHaveBeenCalledTimes(1);
    expect(mocks.read).toHaveBeenCalledTimes(1);
  });

  it("shares pending initialization but only highlights the current file after a scope change", async () => {
    const pending = deferred<Highlighter>();
    createHighlighter.mockReturnValueOnce(pending.promise);
    await render();
    expectSource(firstSource, true);

    await render({ initialFilePath: "src/second.ts" });
    expectSource(secondSource, true);
    expect(createHighlighter).toHaveBeenCalledTimes(1);
    const highlighter = await realHighlighter();
    const codeToHtml = vi.spyOn(highlighter, "codeToHtml");
    await act(async () => pending.resolve(highlighter));

    expectSource(secondSource, false);
    expect(codeToHtml).toHaveBeenCalledExactlyOnceWith(secondSource, { lang: "typescript", theme: "github-light" });
    expect(host.textContent).not.toContain("<unsafe>");
    expect(mocks.read).toHaveBeenCalledTimes(2);
  });

  it("keeps plain source after initialization rejects and retries on content reload, not every render", async () => {
    const pending = deferred<Highlighter>();
    createHighlighter.mockReturnValueOnce(pending.promise);
    await render();
    expectSource(firstSource, true);
    await act(async () => pending.reject(new Error("WASM initialization failed")));

    expectSource(firstSource, true);
    await render();
    expect(createHighlighter).toHaveBeenCalledTimes(1);
    expect(mocks.read).toHaveBeenCalledTimes(1);

    mocks.read.mockImplementationOnce(async (target) => fileContent(target, secondSource));
    await refreshFile();
    expect(createHighlighter).toHaveBeenCalledTimes(2);
    await finishInitialization(1);
    expectSource(secondSource, false);
    expect(mocks.read).toHaveBeenCalledTimes(2);
  });

  it("falls back on tokenization failure without discarding a successfully initialized highlighter", async () => {
    const highlighter = await realHighlighter();
    const codeToHtml = vi.spyOn(highlighter, "codeToHtml")
      .mockImplementationOnce(() => { throw new Error("Tokenization failed"); });
    createHighlighter.mockResolvedValueOnce(highlighter);
    await render();

    expectSource(firstSource, true);
    expect(codeToHtml).toHaveBeenCalledExactlyOnceWith(firstSource, { lang: "typescript", theme: "github-light" });
    await render({ initialFilePath: "src/second.ts" });
    expectSource(secondSource, false);
    expect(createHighlighter).toHaveBeenCalledTimes(1);
  });

  it("does not tokenize or publish a snapshot when initialization resolves after unmount", async () => {
    const pending = deferred<Highlighter>();
    createHighlighter.mockReturnValueOnce(pending.promise);
    await render();
    expectSource(firstSource, true);
    await unmount();
    const snapshotCount = mocks.snapshot.mock.calls.length;
    const highlighter = await realHighlighter();
    const codeToHtml = vi.spyOn(highlighter, "codeToHtml");
    await act(async () => pending.resolve(highlighter));

    expect(codeToHtml).not.toHaveBeenCalled();
    expect(mocks.snapshot).toHaveBeenCalledTimes(snapshotCount);
    expect(host.childElementCount).toBe(0);
  });

  it("handles initialization rejection after unmount and allows a later mount to retry", async () => {
    const pending = deferred<Highlighter>();
    createHighlighter.mockReturnValueOnce(pending.promise);
    await render();
    await unmount();
    const snapshotCount = mocks.snapshot.mock.calls.length;
    await act(async () => pending.reject(new Error("Late initialization failure")));

    expect(mocks.snapshot).toHaveBeenCalledTimes(snapshotCount);
    expect(host.childElementCount).toBe(0);
    expect(createHighlighter).toHaveBeenCalledTimes(1);
    root = createRoot(host);
    await render({ initialFilePath: "src/second.ts" });
    expect(createHighlighter).toHaveBeenCalledTimes(2);
    await finishInitialization(1);
    expectSource(secondSource, false);
  });

  it.each([
    { path: "src/notes.unknown", text: firstSource },
    { path: "src/empty.ts", text: "" },
  ])("renders plain source without initializing Shiki for $path", async ({ path, text }) => {
    mocks.read.mockImplementationOnce(async (target) => fileContent(target, text));
    await render({ initialFilePath: path });

    expectSource(text, true);
    expect(createHighlighter).not.toHaveBeenCalled();
    expect(mocks.read).toHaveBeenCalledTimes(1);
  });
});
