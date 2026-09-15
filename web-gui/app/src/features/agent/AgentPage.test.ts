import { afterEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import i18next from "i18next";

import "../../i18n";
import {
  attachmentKindForFile,
  composerPrimaryAction,
  isScrollKey,
  looksLikeProgrammaticBottomScroll,
  readStoredComposerDraft,
  resizeComposerTextarea,
  storedComposerDraftKey,
  SyncRecoveryStatus,
  writeStoredComposerDraft,
  resolveModelSwitchReasoningEffort,
} from "./AgentPage";
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

describe("composer primary action", () => {
  it("switches the send button to stop-run while a turn is running and the input is empty", () => {
    expect(composerPrimaryAction({ currentRunId: "run-1", hasDraft: false, sendingPrompt: false })).toBe("stop-run");
  });

  it("reverts to send as soon as the operator types a draft", () => {
    expect(composerPrimaryAction({ currentRunId: "run-1", hasDraft: true, sendingPrompt: false })).toBe("send");
  });

  it("returns to stop-run once the draft is cleared while the turn still runs", () => {
    expect(composerPrimaryAction({ currentRunId: "run-1", hasDraft: false, sendingPrompt: false })).toBe("stop-run");
  });

  it("keeps the send action when no run is active", () => {
    expect(composerPrimaryAction({ currentRunId: null, hasDraft: false, sendingPrompt: false })).toBe("send");
    expect(composerPrimaryAction({ currentRunId: undefined, hasDraft: true, sendingPrompt: false })).toBe("send");
  });

  it("keeps the send action while a prompt submission is in flight", () => {
    expect(composerPrimaryAction({ currentRunId: "run-1", hasDraft: false, sendingPrompt: true })).toBe("send");
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


describe("scroll stick intent", () => {
  it("treats a scroll event as programmatic only while auto-scroll is active, no user intent exists, and the position is near the bottom", () => {
    expect(
      looksLikeProgrammaticBottomScroll({ autoScrollActive: true, userScrollIntent: false, nearBottom: true }),
    ).toBe(true);
  });

  it("does not force stick when the user is scrolling even inside the auto-scroll window", () => {
    expect(
      looksLikeProgrammaticBottomScroll({ autoScrollActive: true, userScrollIntent: true, nearBottom: false }),
    ).toBe(false);
    expect(
      looksLikeProgrammaticBottomScroll({ autoScrollActive: true, userScrollIntent: true, nearBottom: true }),
    ).toBe(false);
  });

  it("does not force stick when the position left the bottom, even without user intent", () => {
    expect(
      looksLikeProgrammaticBottomScroll({ autoScrollActive: true, userScrollIntent: false, nearBottom: false }),
    ).toBe(false);
  });

  it("never forces stick outside the auto-scroll window", () => {
    expect(
      looksLikeProgrammaticBottomScroll({ autoScrollActive: false, userScrollIntent: false, nearBottom: true }),
    ).toBe(false);
  });

  it("recognizes keys that can scroll the message list", () => {
    for (const key of ["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " ", "Spacebar"]) {
      expect(isScrollKey(key)).toBe(true);
    }
  });

  it("ignores keys that cannot scroll the message list", () => {
    for (const key of ["Enter", "Tab", "a", "Escape", ""]) {
      expect(isScrollKey(key)).toBe(false);
    }
  });
});

