// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MarkdownReference } from "./file-references/markdown";

const mocks = vi.hoisted(() => ({
  fetchBlob: vi.fn(),
  resolveImages: true,
}));

vi.mock("../runtime/runtime-store", () => ({
  useRuntimeStore: (select: (state: unknown) => unknown) => select({ fetchWorkspaceFileBlob: mocks.fetchBlob }),
}));
vi.mock("./file-references/use-references", () => ({
  useFileIdentity: () => "scope",
  useReferences: (references: Map<string, MarkdownReference>) => ({
    results: new Map([...references].flatMap(([key, reference]) =>
      mocks.resolveImages && reference.value.kind === "file" ? [[key, {
        status: "resolved",
        location: { workspaceId: "ws", executionRootId: "root", path: reference.raw, kind: "file" },
      }]] : [],
    )),
  }),
}));

import { MarkdownContent } from "./MarkdownContent";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.resolveImages = true;
  mocks.fetchBlob.mockReset().mockResolvedValue(new Blob(["image"]));
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:image");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("MarkdownContent image lifecycle", () => {
  it("retains loaded images across reference updates and preview rerenders", async () => {
    const text = "[![first](workspace://ws/one.png)](https://example.com)";
    await act(async () => { root.render(<MarkdownContent text={text} />); });
    const image = container.querySelector("img");
    const link = container.querySelector("a");
    expect(image).not.toBeNull();
    expect(mocks.fetchBlob).toHaveBeenCalledTimes(1);

    await act(async () => {
      root.render(<MarkdownContent text={`${text}\n\n![second](workspace://ws/two.png)`} />);
    });
    expect(container.querySelector("img")).toBe(image);
    expect(container.querySelector("a")).toBe(link);
    expect(mocks.fetchBlob).toHaveBeenCalledTimes(2);
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();

    await act(async () => {
      root.render(<MarkdownContent text={`${text}\n\n![second](workspace://ws/two.png)`} compact />);
    });
    expect(container.querySelector("img")).toBe(image);
    expect(mocks.fetchBlob).toHaveBeenCalledTimes(2);
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
  });

  it("uses updated resolver results and cleans up when an image target changes", async () => {
    mocks.resolveImages = false;
    const text = "![first](workspace://ws/one.png)";
    await act(async () => { root.render(<MarkdownContent text={text} />); });
    expect(mocks.fetchBlob).not.toHaveBeenCalled();

    mocks.resolveImages = true;
    await act(async () => { root.render(<MarkdownContent text={text} compact />); });
    expect(container.querySelector("img")?.alt).toBe("first");
    expect(mocks.fetchBlob).toHaveBeenCalledTimes(1);

    await act(async () => {
      root.render(<MarkdownContent text="![replacement](workspace://ws/two.png)" />);
    });
    expect(container.querySelector("img")?.alt).toBe("replacement");
    expect(mocks.fetchBlob).toHaveBeenCalledTimes(2);
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1);
  });

  it("retains links while using the latest open-file callback", async () => {
    const text = "[file](workspace://ws/note.md)";
    const previousOpen = vi.fn();
    const nextOpen = vi.fn();
    await act(async () => { root.render(<MarkdownContent text={text} onOpenFile={previousOpen} />); });
    const link = container.querySelector("a")!;
    await act(async () => { root.render(<MarkdownContent text={text} onOpenFile={nextOpen} />); });
    expect(container.querySelector("a")).toBe(link);
    act(() => { link.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })); });
    expect(previousOpen).not.toHaveBeenCalled();
    expect(nextOpen).toHaveBeenCalledTimes(1);
  });
});
