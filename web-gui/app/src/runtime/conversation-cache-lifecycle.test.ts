import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  clearAccess: vi.fn(),
  disposeScopes: vi.fn(),
  clearCache: vi.fn(),
  notify: vi.fn(),
}));
vi.mock("./agent-access-records", () => ({ clearAgentAccessRecords: mocks.clearAccess }));
vi.mock("./conversation-scope-store", () => ({ disposeAllConversationScopes: mocks.disposeScopes }));
vi.mock("./idb-cache", () => ({ cacheClearAll: mocks.clearCache }));

import { clearConversationCaches } from "./conversation-cache-lifecycle";

beforeEach(() => {
  vi.clearAllMocks();
  mocks.clearCache.mockResolvedValue(undefined);
  vi.stubGlobal("localStorage", { setItem: mocks.notify });
});
afterEach(() => vi.unstubAllGlobals());

describe("conversation cache authentication lifecycle", () => {
  it("clears rejected or rechecked authentication locally without making sibling tabs reload again", async () => {
    await clearConversationCaches();

    expect(mocks.clearAccess).toHaveBeenCalledOnce();
    expect(mocks.disposeScopes).toHaveBeenCalledWith(false);
    expect(mocks.clearCache).toHaveBeenCalledOnce();
    expect(mocks.notify).not.toHaveBeenCalled();
  });

  it("notifies sibling tabs of an explicit shared-cookie change only after cache removal", async () => {
    let finish!: () => void;
    mocks.clearCache.mockReturnValue(new Promise<void>((resolve) => { finish = resolve; }));
    const clearing = clearConversationCaches({ notifySiblingTabs: true });

    expect(mocks.clearAccess).toHaveBeenCalledOnce();
    expect(mocks.disposeScopes).toHaveBeenCalledWith(false);
    expect(mocks.notify).not.toHaveBeenCalled();
    finish();
    await clearing;

    expect(mocks.notify).toHaveBeenCalledExactlyOnceWith("holon.conversationAuthChange.v1", expect.any(String));
  });

  it("does not announce completion if cache removal fails", async () => {
    mocks.clearCache.mockRejectedValue(new Error("cache unavailable"));

    await expect(clearConversationCaches({ notifySiblingTabs: true })).rejects.toThrow("cache unavailable");
    expect(mocks.notify).not.toHaveBeenCalled();
  });

  it("still clears caches when cross-tab notification storage is unavailable", async () => {
    mocks.notify.mockImplementationOnce(() => { throw new Error("storage unavailable"); });

    await expect(clearConversationCaches({ notifySiblingTabs: true })).resolves.toBeUndefined();
    expect(mocks.clearCache).toHaveBeenCalledOnce();
  });
});
