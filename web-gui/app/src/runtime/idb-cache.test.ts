import { IDBFactory } from "fake-indexeddb";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { _resetDbPromise, cacheGetConversationSnapshot, cachePutConversationSnapshot, cachePutConversationBrief,
  cacheGetConversationBrief, cacheClearRemote, pruneConversationCaches, CONVERSATION_CACHE_MAX_ENTRIES,
  CONVERSATION_CACHE_MAX_AGE_MS, CONVERSATION_CACHE_MAX_BYTES } from "./idb-cache";

beforeEach(() => { _resetDbPromise(); globalThis.indexedDB = new IDBFactory(); });
afterEach(() => _resetDbPromise());
const snapshot = (remoteKey = "local#user-A", agentId = "agent", cachedAt = Date.now()) => ({
  remoteKey, agentId, schemaVersion: 1, etag: "cached-etag", summary: {}, cachedAt,
});

describe("conversation cache lifecycle", () => {
  it("falls back immediately if an older tab blocks the schema upgrade", async () => {
    const old = await new Promise<IDBDatabase>((resolve) => {
      const request = indexedDB.open("holon-webgui-cache", 4);
      request.onsuccess = () => resolve(request.result);
    });
    try {
      const outcome = await Promise.race([
        cacheGetConversationSnapshot("local", "agent").then(() => "fallback"),
        new Promise((resolve) => setTimeout(() => resolve("hung"), 200)),
      ]);
      expect(outcome).toBe("fallback");
    } finally { old.close(); }
  });
  it("closes its connection when another tab upgrades the database", async () => {
    await cacheGetConversationSnapshot("local", "agent");
    const next = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("holon-webgui-cache", 6);
      request.onsuccess = () => resolve(request.result);
      request.onblocked = () => reject(new Error("old connection did not close"));
    });
    next.close();
  });
  it("clears both caches and identity namespaces without touching other remotes", async () => {
    await cachePutConversationSnapshot(snapshot());
    await cachePutConversationSnapshot(snapshot("other#user"));
    await cachePutConversationBrief({ remoteKey: "local#user-A", agentId: "agent", briefId: "brief", schemaVersion: 1, cachedAt: Date.now(), brief: {} as never });
    await cacheClearRemote("local");
    expect(await cacheGetConversationSnapshot("local#user-A", "agent")).toBeUndefined();
    expect(await cacheGetConversationBrief("local#user-A", "agent", "brief")).toBeUndefined();
    expect(await cacheGetConversationSnapshot("other#user", "agent")).toBeDefined();
  });
  it("bounds entries and expires old snapshots", async () => {
    await cachePutConversationSnapshot(snapshot("local", "expired", Date.now() - CONVERSATION_CACHE_MAX_AGE_MS - 1));
    expect(await cacheGetConversationSnapshot("local", "expired")).toBeUndefined();
    for (let i = 0; i <= CONVERSATION_CACHE_MAX_ENTRIES; i++) {
      await cachePutConversationSnapshot(snapshot("local", `agent-${i}`, Date.now() + i));
    }
    await pruneConversationCaches();
    expect(await cacheGetConversationSnapshot("local", "agent-0")).toBeUndefined();
    expect(await cacheGetConversationSnapshot("local", `agent-${CONVERSATION_CACHE_MAX_ENTRIES}`)).toBeDefined();
  });
  it("rejects entries that would exceed the total byte budget", async () => {
    await cachePutConversationSnapshot({ ...snapshot(), summary: { text: "x".repeat(CONVERSATION_CACHE_MAX_BYTES / 2) } });
    await pruneConversationCaches();
    expect(await cacheGetConversationSnapshot("local#user-A", "agent")).toBeUndefined();
  });
});
