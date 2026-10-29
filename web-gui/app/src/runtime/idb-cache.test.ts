import { IDBFactory, IDBKeyRange } from "fake-indexeddb";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { _resetDbPromise, CACHE_SCHEMA_VERSION, cacheClearRemoteSessions, cacheGetModelCatalog, cacheGetConversationSnapshot, cachePutConversationSnapshot, cachePutConversationBrief,
  cacheGetConversationBrief, cacheClearRemote, pruneConversationCaches, CONVERSATION_CACHE_MAX_ENTRIES,
  CONVERSATION_CACHE_MAX_AGE_MS, CONVERSATION_CACHE_MAX_BYTES } from "./idb-cache";

beforeEach(() => {
  _resetDbPromise();
  globalThis.indexedDB = new IDBFactory();
  globalThis.IDBKeyRange = IDBKeyRange;
});
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
      const request = indexedDB.open("holon-webgui-cache", 7);
      request.onsuccess = () => resolve(request.result);
      request.onblocked = () => reject(new Error("old connection did not close"));
    });
    next.close();
  });
  it("cleans legacy sessions without losing v5 briefs, snapshots or model catalogs", async () => {
    const old = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("holon-webgui-cache", 5);
      request.onupgradeneeded = () => {
        const db = request.result;
        db.createObjectStore("sessions", { keyPath: ["remoteKey", "agentId"] })
          .createIndex("byRemoteKey", "remoteKey");
        db.createObjectStore("meta", { keyPath: "remoteKey" });
        db.createObjectStore("modelCatalog", { keyPath: "remoteKey" });
        db.createObjectStore("briefs", { keyPath: ["remoteKey", "agentId", "briefId"] });
        db.createObjectStore("snapshots", { keyPath: ["remoteKey", "agentId"] });
      };
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve(request.result);
    });
    const cachedSnapshot = snapshot();
    const cachedBrief = { remoteKey: "local#user-A", agentId: "agent", briefId: "brief", schemaVersion: 1, cachedAt: Date.now(), brief: {} };
    // An obsolete session schema must not invalidate independent content caches.
    const cachedSession = { remoteKey: "local#user-A", agentId: "agent", schemaVersion: 0, cachedAt: Date.now() };
    const cachedCatalog = { remoteKey: "local#user-A", schemaVersion: CACHE_SCHEMA_VERSION, options: ["model"], cachedAt: Date.now() };
    await new Promise<void>((resolve, reject) => {
      const tx = old.transaction(["sessions", "briefs", "snapshots", "modelCatalog"], "readwrite");
      tx.objectStore("sessions").put(cachedSession);
      tx.objectStore("briefs").put(cachedBrief);
      tx.objectStore("snapshots").put(cachedSnapshot);
      tx.objectStore("modelCatalog").put(cachedCatalog);
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(tx.error);
    });
    old.close();
    expect(CACHE_SCHEMA_VERSION).toBe(5);
    await cacheClearRemoteSessions("local#user-A");
    expect(await cacheGetConversationBrief("local#user-A", "agent", "brief")).toEqual(cachedBrief);
    expect(await cacheGetConversationSnapshot("local#user-A", "agent")).toEqual(cachedSnapshot);
    expect(await cacheGetModelCatalog("local#user-A")).toEqual(cachedCatalog);
    const db = await new Promise<IDBDatabase>((resolve) => {
      const request = indexedDB.open("holon-webgui-cache");
      request.onsuccess = () => resolve(request.result);
    });
    try {
      const session = await new Promise<unknown>((resolve) => {
        const request = db.transaction("sessions").objectStore("sessions").get(["local#user-A", "agent"]);
        request.onsuccess = () => resolve(request.result);
      });
      expect(session).toBeUndefined();
    } finally { db.close(); }
  });
  it("still retires briefs and snapshots from before authenticated namespaces", async () => {
    const old = await new Promise<IDBDatabase>((resolve) => {
      const request = indexedDB.open("holon-webgui-cache", 4);
      request.onupgradeneeded = () => {
        request.result.createObjectStore("briefs", { keyPath: ["remoteKey", "agentId", "briefId"] });
        request.result.createObjectStore("snapshots", { keyPath: ["remoteKey", "agentId"] });
      };
      request.onsuccess = () => resolve(request.result);
    });
    await new Promise<void>((resolve) => {
      const tx = old.transaction(["briefs", "snapshots"], "readwrite");
      tx.objectStore("briefs").put({ remoteKey: "local#user-A", agentId: "agent", briefId: "brief", cachedAt: Date.now() });
      tx.objectStore("snapshots").put(snapshot());
      tx.oncomplete = () => resolve();
    });
    old.close();
    expect(await cacheGetConversationBrief("local#user-A", "agent", "brief")).toBeUndefined();
    expect(await cacheGetConversationSnapshot("local#user-A", "agent")).toBeUndefined();
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
