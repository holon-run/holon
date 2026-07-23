import { afterEach, describe, expect, it, vi } from "vitest";

import {
  attachmentKindForFile,
  captureScrollAnchor,
  readStoredComposerDraft,
  resizeComposerTextarea,
  restoredScrollTop,
  storedComposerDraftKey,
  timelineForDisplayLevel,
  timelineLayoutRevision,
  writeStoredComposerDraft,
} from "./AgentPage";
import type { TimelineTurn } from "./timeline-utils";

class MemoryStorage implements Storage {
  private readonly items = new Map<string, string>();

  get length() {
    return this.items.size;
  }

  clear(): void {
    this.items.clear();
  }

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  key(index: number): string | null {
    return Array.from(this.items.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }

  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }
}

function installWindow(localStorage: Storage) {
  vi.stubGlobal("window", {
    localStorage,
  });
}

describe("composer draft storage", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("isolates drafts by agent id", () => {
    installWindow(new MemoryStorage());

    writeStoredComposerDraft("agent-a", "draft for a");
    writeStoredComposerDraft("agent-b", "draft for b");

    expect(readStoredComposerDraft("agent-a")).toBe("draft for a");
    expect(readStoredComposerDraft("agent-b")).toBe("draft for b");
  });

  it("removes the stored draft when the prompt is cleared", () => {
    const storage = new MemoryStorage();
    installWindow(storage);

    writeStoredComposerDraft("agent-a", "draft");
    writeStoredComposerDraft("agent-a", "");

    expect(readStoredComposerDraft("agent-a")).toBe("");
    expect(storage.getItem(storedComposerDraftKey("agent-a"))).toBeNull();
  });
});

describe("composer textarea resize", () => {
  it("expands to fit the textarea content", () => {
    const textarea = { scrollHeight: 144, style: {} } as unknown as HTMLTextAreaElement;

    resizeComposerTextarea(textarea);

    expect(textarea.style.height).toBe("144px");
    expect(textarea.style.overflowY).toBe("hidden");
  });

  it("caps very tall content and enables scrolling only past the cap", () => {
    const textarea = { scrollHeight: 420, style: {} } as unknown as HTMLTextAreaElement;

    resizeComposerTextarea(textarea);

    expect(textarea.style.height).toBe("320px");
    expect(textarea.style.overflowY).toBe("auto");
  });
});

describe("composer attachments", () => {
  it("keeps image files as image attachments", () => {
    expect(attachmentKindForFile({ type: "image/png" })).toBe("image");
  });

  it("classifies non-image files as generic file attachments", () => {
    expect(attachmentKindForFile({ type: "application/pdf" })).toBe("file");
    expect(attachmentKindForFile({ type: "" })).toBe("file");
  });
});

describe("timeline virtual layout reconciliation", () => {
  it("changes the layout revision when hydrated content replaces a preview under the same turn id", () => {
    const preview = timelineTurn("turn:assistant", "Short preview");
    const hydrated = timelineTurn("turn:assistant", "Short preview\n\nExpanded hydrated transcript body.");

    expect(timelineLayoutRevision([hydrated])).not.toBe(timelineLayoutRevision([preview]));
  });

  it("keeps the same visible turn offset after measurements change", () => {
    const anchor = captureScrollAnchor(
      [
        { key: "turn:a", index: 0, start: 0, size: 120 },
        { key: "turn:b", index: 1, start: 120, size: 200 },
      ],
      164,
    );

    expect(anchor).toEqual({ key: "turn:b", index: 1, offset: 44 });
    expect(restoredScrollTop(anchor, 1, (index) => index === 1 ? 180 : undefined, 164)).toBe(224);
  });

  it("restores an anchored turn even when it is outside the current virtual overscan", () => {
    const anchor = captureScrollAnchor([{ key: "turn:a", index: 0, start: 20, size: 80 }], 44);

    expect(restoredScrollTop(anchor, 8, (index) => index === 8 ? 1_900 : undefined, 44)).toBe(1_924);
  });

  it("accounts for history controls before the virtual wrapper when restoring an anchor", () => {
    const anchor = captureScrollAnchor([{ key: "turn:a", index: 0, start: 20, size: 80 }], 44);

    expect(restoredScrollTop(anchor, 8, (index) => index === 8 ? 1_900 : undefined, 84, 32)).toBe(1_956);
  });

  it("falls back to the original scroll top when the virtualizer cannot resolve the anchored index", () => {
    const anchor = captureScrollAnchor([{ key: "turn:a", index: 0, start: 20, size: 80 }], 44);

    expect(restoredScrollTop(anchor, 8, () => undefined, 44)).toBe(44);
  });

  it("does not capture an anchor when only overscan rows before the viewport are measured", () => {
    expect(captureScrollAnchor([{ key: "turn:a", index: 0, start: 0, size: 80 }], 120)).toBeNull();
  });
});

describe("timeline display levels", () => {
  it("keeps debug on the semantic timeline instead of replacing it with raw events", () => {
    const semanticItem = timelineTurn("turn:assistant", "Semantic result").items[0];
    const debugItem = {
      ...semanticItem,
      id: "runtime:debug",
      minDisplayLevel: "debug" as const,
      body: "Detailed runtime bookkeeping",
    };

    expect(timelineForDisplayLevel([semanticItem, debugItem], "debug", 20).map((item) => item.id))
      .toEqual(["assistant-message", "runtime:debug"]);
  });
});

function timelineTurn(turnId: string, body: string): TimelineTurn {
  return {
    id: turnId,
    kind: "runtime",
    label: "Turn",
    timestamp: "2026-07-19T00:00:00.000Z",
    items: [
      {
        id: "assistant-message",
        kind: "assistant",
        label: "Assistant",
        body,
        timestamp: "2026-07-19T00:00:00.000Z",
        meta: "assistant",
        minDisplayLevel: "info",
        sourceIds: ["event:1"],
      },
    ],
  };
}
