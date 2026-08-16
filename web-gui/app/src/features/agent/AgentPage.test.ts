import { afterEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import i18next from "i18next";

import "../../i18n";
import {
  attachmentKindForFile,
  captureScrollAnchor,
  historyLoadDecision,
  readStoredComposerDraft,
  resizeComposerTextarea,
  restoredScrollTop,
  storedComposerDraftKey,
  SyncRecoveryStatus,
  timelineForDisplayLevel,
  timelineLayoutRevision,
  writeStoredComposerDraft,
  resolveModelSwitchReasoningEffort,
} from "./AgentPage";
import { availableDisplayLevels } from "../../runtime/display-level";
import type { TimelineTurn } from "./timeline-utils";
import type { RuntimeModelOption } from "../../runtime/types";

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

describe("sync recovery status", () => {
  it("renders the failure, retry attempt, and manual recovery action", async () => {
    await i18next.changeLanguage("en");
    const markup = renderToStaticMarkup(
      createElement(SyncRecoveryStatus, {
        error: "baseline unavailable",
        retryAttempt: 3,
        onRetry: () => undefined,
      }),
    );

    expect(markup).toContain("Conversation sync recovery failed (attempt 3)");
    expect(markup).toContain("baseline unavailable");
    expect(markup).toContain("Retry sync now");
  });
});

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

describe("model switch reasoning effort", () => {
  function modelOption(supportsReasoningEffort: boolean, reasoningEffortOptions: string[]): RuntimeModelOption {
    return {
      model: "glm-5.3",
      routeRef: "bigmodel/glm-5.3",
      provider: "bigmodel",
      providerFamily: "bigmodel",
      endpoint: "default",
      routeProvider: "bigmodel",
      displayName: "GLM-5.3",
      available: true,
      supportsImageInput: true,
      supportsImageGeneration: false,
      supportsReasoningEffort,
      reasoningEffortOptions,
    };
  }

  it("falls back to auto when the target model lacks the current effort level", () => {
    expect(resolveModelSwitchReasoningEffort(modelOption(true, ["low", "high", "max"]), "medium")).toBe("auto");
  });

  it("keeps the current effort level when the target model supports it", () => {
    expect(resolveModelSwitchReasoningEffort(modelOption(true, ["low", "medium", "high"]), "medium")).toBe("medium");
  });

  it("keeps auto untouched for reasoning models", () => {
    expect(resolveModelSwitchReasoningEffort(modelOption(true, ["low", "high"]), "auto")).toBe("auto");
  });

  it("resets to auto for models without reasoning support", () => {
    expect(resolveModelSwitchReasoningEffort(modelOption(false, []), "medium")).toBe("auto");
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
  it("only exposes Debug while developer diagnostics are enabled", () => {
    expect(availableDisplayLevels(false)).toEqual(["info", "verbose"]);
    expect(availableDisplayLevels(true)).toEqual(["info", "verbose", "debug"]);
  });

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

describe("history loading", () => {
  it("expands already loaded timeline items before requesting network history", () => {
    expect(historyLoadDecision(true, true)).toBe("expand-local");
    expect(historyLoadDecision(true, false)).toBe("expand-local");
  });

  it("requests network history only after the local timeline reaches its boundary", () => {
    expect(historyLoadDecision(false, true)).toBe("load-network");
  });

  it("does nothing when neither local nor server history remains", () => {
    expect(historyLoadDecision(false, false)).toBe("none");
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
