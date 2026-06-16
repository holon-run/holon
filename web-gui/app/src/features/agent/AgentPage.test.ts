import { afterEach, describe, expect, it, vi } from "vitest";

import { readStoredComposerDraft, storedComposerDraftKey, writeStoredComposerDraft } from "./AgentPage";

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
