// @vitest-environment happy-dom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { TurnInputSummary } from "@holon/conversation-sdk";

import { ClipboardProvider } from "../../components/ClipboardProvider";
import { ConversationInputLine } from "./ConversationTimeline";
import "../../i18n";

const mocks = vi.hoisted(() => ({
  write: vi.fn<(text: string) => Promise<boolean>>(),
}));

vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);

vi.mock("../../lib/clipboard", () => ({ writeClipboardText: mocks.write }));
vi.mock("../../components/MarkdownContent", () => ({
  MarkdownContent: ({ text }: { text: string }) => <p>{text}</p>,
}));

let container: HTMLElement | undefined;
let root: Root | undefined;

afterEach(() => {
  root?.unmount();
  container?.remove();
  container = undefined;
  mocks.write.mockReset();
});

function render(node: ReactNode) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => {
    root!.render(<ClipboardProvider>{node}</ClipboardProvider>);
  });
}

function input(preview: string): TurnInputSummary {
  return { message_id: "m-1", preview };
}

function truncatedPreview(): string {
  const text = `Truncated operator prompt with a quoted "value" and a path\\name. ${"detail. ".repeat(400)}`;
  return JSON.stringify({ type: "text", text }).slice(0, 2048);
}

async function clickCopy() {
  const button = document.querySelector<HTMLButtonElement>(".conversation-input-actions button");
  expect(button).not.toBeNull();
  await act(async () => {
    button!.click();
  });
}

describe("ConversationInputLine copy action", () => {
  it("copies the displayed text without fetching the full message", async () => {
    const onLoadMessageText = vi.fn<() => Promise<string | null>>();
    mocks.write.mockResolvedValue(true);
    render(<ConversationInputLine input={input('{"type":"text","text":"帮我检查发布"}')} onLoadMessageText={onLoadMessageText} />);
    expect(document.querySelector(".conversation-input-actions button")?.getAttribute("aria-label")).toBe("Copy message");

    await clickCopy();

    expect(onLoadMessageText).not.toHaveBeenCalled();
    expect(mocks.write).toHaveBeenCalledWith("帮我检查发布");
    expect(document.querySelector(".conversation-input-actions button")?.getAttribute("aria-label")).toBe("Message copied");
  });

  it("upgrades a byte-truncated preview to the fetched full text", async () => {
    const onLoadMessageText = vi.fn<(messageId: string) => Promise<string | null>>()
      .mockResolvedValue("full message text");
    render(<ConversationInputLine input={input(truncatedPreview())} onLoadMessageText={onLoadMessageText} />);

    await clickCopy();

    expect(onLoadMessageText).toHaveBeenCalledWith("m-1");
    expect(mocks.write).toHaveBeenCalledWith("full message text");
  });

  it.each([
    ["rejects", () => Promise.reject(new Error("offline"))],
    ["resolves null", () => Promise.resolve(null)],
  ])("falls back to the bounded preview when the fetch %s", async (_name, fetchImpl) => {
    mocks.write.mockResolvedValue(true);
    render(<ConversationInputLine input={input(truncatedPreview())} onLoadMessageText={fetchImpl} />);

    await clickCopy();

    expect(mocks.write).toHaveBeenCalledTimes(1);
    const copied = mocks.write.mock.calls[0][0];
    expect(copied).toContain("Truncated operator prompt");
    expect(copied).toMatch(/…$/);
  });

  it("hides the copy action for preview-empty messages", () => {
    render(<ConversationInputLine input={input("")} />);
    expect(document.querySelector(".conversation-input-actions")).toBeNull();
  });
});
