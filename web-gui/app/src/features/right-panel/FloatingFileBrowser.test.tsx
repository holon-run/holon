// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import { FileBrowserPanel } from "./FileBrowserPanel";
import { FloatingFileBrowser } from "./FloatingFileBrowser";

vi.mock("./FileBrowserPanel", () => ({ FileBrowserPanel: vi.fn(() => null) }));

let host: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
let closed: number;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  closed = 0;
  vi.mocked(FileBrowserPanel).mockClear();
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

async function render(overrides: Partial<Parameters<typeof FloatingFileBrowser>[0]> = {}) {
  await act(async () => root.render(
    <FloatingFileBrowser
      workspaceId="ws-a"
      executionRootId="root-a"
      initialPath="docs"
      initialFilePath="docs/readme.md"
      initialFragment="L10"
      workspaceLabel="Workspace A"
      onClose={() => { closed += 1; }}
      {...overrides}
    />,
  ));
  return document.body.querySelector<HTMLDivElement>('[role="dialog"]');
}

describe("FloatingFileBrowser dialog", () => {
  it("mounts a modal dialog in the body, focuses its close button, and forwards the location", async () => {
    const opener = document.createElement("button");
    document.body.append(opener);
    opener.focus();
    const dialog = await render();
    expect(dialog).not.toBeNull();
    expect(dialog!.getAttribute("aria-modal")).toBe("true");
    expect(document.activeElement).toBe(dialog!.querySelector(".modal-close"));
    const panelProps = vi.mocked(FileBrowserPanel).mock.calls.at(-1)![0];
    expect(panelProps.workspaceId).toBe("ws-a");
    expect(panelProps.initialFilePath).toBe("docs/readme.md");
    expect(panelProps.initialFragment).toBe("L10");
    expect(panelProps.onClose).toBeTypeOf("function");
    opener.remove();
  });

  it("closes on Escape and backdrop clicks, but not on card clicks", async () => {
    const dialog = await render();
    act(() => { dialog!.querySelector(".floating-file-browser-card")!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    expect(closed).toBe(0);
    act(() => { dialog!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });
    expect(closed).toBe(1);
    act(() => { dialog!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    expect(closed).toBe(2);
  });

  it("restores focus to the opener when unmounted", async () => {
    const opener = document.createElement("button");
    document.body.append(opener);
    opener.focus();
    await render();
    expect(document.activeElement).not.toBe(opener);
    await act(async () => root.render(<div />));
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });
});
