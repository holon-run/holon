/**
 * Thin IndexedDB wrapper for client-side session caching.
 *
 * All operations are defensive: if IndexedDB is unavailable (private mode,
 * quota exceeded, etc.) they resolve to empty/no-op results so the caller
 * can silently fall back to a memory-only mode.
 */

import type { BriefRecord } from "@holon/conversation-sdk";

const DB_NAME = "holon-webgui-cache";
const DB_VERSION = 6;
export const CACHE_SCHEMA_VERSION = 5;
export const BRIEF_CACHE_SCHEMA_VERSION = 1;
export const SNAPSHOT_CACHE_SCHEMA_VERSION = 1;
export const AGENT_PRESENTATION_CACHE_SCHEMA_VERSION = 2;
const SESSIONS_STORE = "sessions";
const META_STORE = "meta";
const MODEL_CATALOG_STORE = "modelCatalog";
const BRIEFS_STORE = "briefs";
const SNAPSHOTS_STORE = "snapshots";
const AGENT_PRESENTATIONS_STORE = "agentPresentations";
const CACHE_TIMEOUT_MS = 500;
export const CONVERSATION_CACHE_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;
export const CONVERSATION_CACHE_MAX_BYTES = 20 * 1024 * 1024;
export const CONVERSATION_CACHE_MAX_ENTRIES = 500;
let pruneTimer: ReturnType<typeof setTimeout> | undefined;
let clearGeneration = 0;

export interface CachedModelCatalog {
  remoteKey: string;
  schemaVersion: number;
  options: unknown[];
  cachedAt: number;
}

export interface CachedAgentPresentations {
  remoteKey: string;
  schemaVersion: number;
  agents: unknown[];
  cachedAt: number;
}

export interface CachedConversationBrief {
  remoteKey: string;
  agentId: string;
  briefId: string;
  schemaVersion: number;
  brief: BriefRecord;
  cachedAt: number;
}

export interface CachedConversationSnapshot {
  remoteKey: string;
  agentId: string;
  schemaVersion: number;
  etag: string | null;
  summary: unknown;
  cachedAt: number;
}

let dbPromise: Promise<IDBDatabase | null> | null = null;
let database: IDBDatabase | null = null;

function openDB(): Promise<IDBDatabase | null> {
  if (dbPromise) return dbPromise;

  dbPromise = new Promise<IDBDatabase | null>((resolve) => {
    if (typeof indexedDB === "undefined") {
      resolve(null);
      return;
    }
    let settled = false;
    const finish = (db: IDBDatabase | null) => {
      if (settled) { db?.close(); return; }
      settled = true;
      database = db;
      clearTimeout(timer);
      resolve(db);
    };
    const timer = setTimeout(() => finish(null), CACHE_TIMEOUT_MS);
    let request: IDBOpenDBRequest;
    try {
      request = indexedDB.open(DB_NAME, DB_VERSION);
    } catch {
      finish(null);
      return;
    }
    request.onblocked = () => finish(null);
    request.onupgradeneeded = (event) => {
      const db = request.result;
      // Retire snapshots/briefs written before authenticated cache namespaces.
      if (event.oldVersion < 5) {
        for (const name of [BRIEFS_STORE, SNAPSHOTS_STORE]) {
          if (db.objectStoreNames.contains(name)) db.deleteObjectStore(name);
        }
      }
      if (!db.objectStoreNames.contains(SESSIONS_STORE)) {
        const store = db.createObjectStore(SESSIONS_STORE, { keyPath: ["remoteKey", "agentId"] });
        store.createIndex("byRemoteKey", "remoteKey", { unique: false });
      }
      if (!db.objectStoreNames.contains(META_STORE)) {
        db.createObjectStore(META_STORE, { keyPath: "remoteKey" });
      }
      if (!db.objectStoreNames.contains(MODEL_CATALOG_STORE)) {
        db.createObjectStore(MODEL_CATALOG_STORE, { keyPath: "remoteKey" });
      }
      if (!db.objectStoreNames.contains(BRIEFS_STORE)) {
        db.createObjectStore(BRIEFS_STORE, { keyPath: ["remoteKey", "agentId", "briefId"] });
      }
      if (!db.objectStoreNames.contains(SNAPSHOTS_STORE)) {
        db.createObjectStore(SNAPSHOTS_STORE, { keyPath: ["remoteKey", "agentId"] });
      }
      if (!db.objectStoreNames.contains(AGENT_PRESENTATIONS_STORE)) {
        db.createObjectStore(AGENT_PRESENTATIONS_STORE, { keyPath: "remoteKey" });
      }
    };
    request.onsuccess = () => {
      const db = request.result;
      db.onversionchange = () => { db.close(); database = null; dbPromise = null; };
      finish(db);
    };
    request.onerror = () => finish(null);
  });

  return dbPromise;
}

function runRequest<T>(
  db: IDBDatabase,
  storeName: string,
  mode: IDBTransactionMode,
  fn: (store: IDBObjectStore) => IDBRequest,
): Promise<T | undefined> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(storeName, mode);
    const timer = setTimeout(() => {
      try { tx.abort(); } catch { /* already finished */ }
      resolve(undefined);
    }, CACHE_TIMEOUT_MS);
    let result: T | undefined;
    tx.oncomplete = () => { clearTimeout(timer); resolve(result); };
    tx.onabort = tx.onerror = () => { clearTimeout(timer); reject(tx.error); };
    try {
      const request = fn(tx.objectStore(storeName));
      request.onsuccess = () => { if (request.readyState === "done") result = request.result as T; };
    } catch (error) {
      clearTimeout(timer);
      try { tx.abort(); } catch { /* already finished */ }
      reject(error);
    }
  });
}

export async function cacheDeleteSession(remoteKey: string, agentId: string): Promise<void> {
  const db = await openDB();
  if (!db) return;
  try {
    await runRequest(db, SESSIONS_STORE, "readwrite", (store) => store.delete([remoteKey, agentId]));
  } catch {
    // Silent fallback.
  }
}

/**
 * Delete every cached agent session for one remote without touching the
 * model-catalog cache or any other store. Used to retire the legacy
 * conversation-content cache namespace after the conversation read-model
 * cutover.
 */
export async function cacheClearRemoteSessions(
  remoteKey: string,
  isCurrent: () => boolean = () => true,
): Promise<void> {
  const db = await openDB();
  if (!db || !isCurrent()) return;
  try {
    await new Promise<void>((resolve, reject) => {
      const store = db.transaction(SESSIONS_STORE, "readwrite").objectStore(SESSIONS_STORE);
      const request = store.index("byRemoteKey").openCursor(IDBKeyRange.only(remoteKey));
      request.onsuccess = () => {
        const cursor = request.result;
        if (cursor) {
          cursor.delete();
          cursor.continue();
        } else {
          resolve();
        }
      };
      request.onerror = () => reject(request.error);
    });
  } catch {
    // Silent fallback.
  }
}

export async function cachePutModelCatalog(catalog: CachedModelCatalog): Promise<void> {
  const db = await openDB();
  if (!db) return;
  try {
    await runRequest(db, MODEL_CATALOG_STORE, "readwrite", (store) => store.put(catalog));
  } catch {
    // Silent fallback — cache is best-effort.
  }
}

export async function cacheGetModelCatalog(remoteKey: string): Promise<CachedModelCatalog | undefined> {
  const db = await openDB();
  if (!db) return undefined;
  try {
    const catalog = await runRequest<CachedModelCatalog>(db, MODEL_CATALOG_STORE, "readonly", (store) =>
      store.get(remoteKey),
    );
    return catalog?.schemaVersion === CACHE_SCHEMA_VERSION ? catalog : undefined;
  } catch {
    return undefined;
  }
}

export async function cachePutAgentPresentations(entry: CachedAgentPresentations): Promise<void> {
  const generation = clearGeneration;
  const db = await openDB();
  if (!db || generation !== clearGeneration) return;
  try {
    await runRequest(db, AGENT_PRESENTATIONS_STORE, "readwrite", (store) => store.put(entry));
  } catch { /* storage unavailable */ }
}

export async function cacheGetAgentPresentations(remoteKey: string): Promise<unknown> {
  const generation = clearGeneration;
  const db = await openDB();
  if (!db || generation !== clearGeneration) return undefined;
  try {
    const entry = await runRequest(db, AGENT_PRESENTATIONS_STORE, "readonly", (store) => store.get(remoteKey));
    return generation === clearGeneration ? entry : undefined;
  } catch { return undefined; }
}

/** Keep read/modify/write atomic so concurrent list updates cannot be overwritten. */
export async function cacheRemoveAgentPresentation(remoteKey: string, agentId: string): Promise<void> {
  const generation = clearGeneration;
  const db = await openDB();
  if (!db || generation !== clearGeneration) return;
  try {
    await runRequest(db, AGENT_PRESENTATIONS_STORE, "readwrite", (store) => {
      const request = store.get(remoteKey);
      request.addEventListener("success", () => {
        if (generation !== clearGeneration) return;
        const entry = request.result;
        if (!entry || !Array.isArray(entry.agents)) return;
        store.put({
          ...entry,
          agents: entry.agents.filter((agent: unknown) =>
            !agent || typeof agent !== "object" || !("id" in agent) || agent.id !== agentId),
        });
      });
      return request;
    });
  } catch { /* storage unavailable */ }
}

export async function cachePutConversationBrief(
  entry: CachedConversationBrief,
): Promise<void> {
  const generation = clearGeneration;
  const db = await openDB();
  if (!db || generation !== clearGeneration) return;
  try {
    await runRequest(db, BRIEFS_STORE, "readwrite", (store) => store.put(entry));
    scheduleConversationPrune();
  } catch {
    // Silent fallback — cache is best-effort.
  }
}

export async function cacheGetConversationBrief(
  remoteKey: string,
  agentId: string,
  briefId: string,
): Promise<CachedConversationBrief | undefined> {
  const db = await openDB();
  if (!db) return undefined;
  try {
    const entry = await runRequest<CachedConversationBrief>(db, BRIEFS_STORE, "readonly", (store) => store.get([remoteKey, agentId, briefId]));
    return entry && Date.now() - entry.cachedAt <= CONVERSATION_CACHE_MAX_AGE_MS ? entry : undefined;
  } catch {
    return undefined;
  }
}

export async function cachePutConversationSnapshot(
  entry: CachedConversationSnapshot,
): Promise<void> {
  const generation = clearGeneration;
  const db = await openDB();
  if (!db || generation !== clearGeneration) return;
  try {
    await runRequest(db, SNAPSHOTS_STORE, "readwrite", (store) => store.put(entry));
    scheduleConversationPrune();
  } catch {
    // Silent fallback — cache is best-effort.
  }
}

export async function cacheGetConversationSnapshot(
  remoteKey: string,
  agentId: string,
): Promise<CachedConversationSnapshot | undefined> {
  const db = await openDB();
  if (!db) return undefined;
  try {
    const entry = await runRequest<CachedConversationSnapshot>(
      db,
      SNAPSHOTS_STORE,
      "readonly",
      (store) => store.get([remoteKey, agentId]),
    );
    return entry && Date.now() - entry.cachedAt <= CONVERSATION_CACHE_MAX_AGE_MS ? entry : undefined;
  } catch {
    return undefined;
  }
}

function belongsToRemote(key: string, remote: string): boolean {
  return key === remote || key.startsWith(`${remote}#`);
}

async function deleteMatching(storeNames: string[], matches: (entry: { remoteKey: string; agentId?: string }) => boolean): Promise<void> {
  clearGeneration += 1;
  const db = await openDB();
  if (!db) return;
  await Promise.all(storeNames.map(async (name) => {
    try {
      await runRequest(db, name, "readwrite", (store) => {
        const request = store.openCursor();
        request.addEventListener("success", () => {
          const cursor = request.result;
          if (!cursor) return;
          if (matches(cursor.value)) cursor.delete();
          cursor.continue();
        });
        return request;
      });
    } catch { /* best-effort cache cleanup */ }
  }));
}

export async function cacheClearConversationScope(remoteKey: string, agentId: string): Promise<void> {
  await deleteMatching([BRIEFS_STORE, SNAPSHOTS_STORE], (entry) => entry.remoteKey === remoteKey && entry.agentId === agentId);
}

export async function cacheClearRemoteBriefs(remoteKey: string): Promise<void> {
  await deleteMatching([BRIEFS_STORE], (entry) => belongsToRemote(entry.remoteKey, remoteKey));
}

/** Delete every cached conversation snapshot for one remote (connection switch, sign-out). */
export async function cacheClearRemoteSnapshots(remoteKey: string): Promise<void> {
  await deleteMatching([SNAPSHOTS_STORE], (entry) => belongsToRemote(entry.remoteKey, remoteKey));
}

export async function cacheClearRemote(remoteKey: string): Promise<void> {
  await deleteMatching([SESSIONS_STORE, MODEL_CATALOG_STORE, BRIEFS_STORE, SNAPSHOTS_STORE, AGENT_PRESENTATIONS_STORE],
    (entry) => belongsToRemote(entry.remoteKey, remoteKey));
}

/** Logout clears this origin's cache, including legacy namespaces. */
export async function cacheClearAll(): Promise<void> {
  await deleteMatching([SESSIONS_STORE, MODEL_CATALOG_STORE, BRIEFS_STORE, SNAPSHOTS_STORE, AGENT_PRESENTATIONS_STORE], () => true);
}

function scheduleConversationPrune(): void {
  if (pruneTimer !== undefined) return;
  pruneTimer = setTimeout(() => {
    pruneTimer = undefined;
    void pruneConversationCaches();
  }, 1000);
}

/** Bounded by age, total serialized bytes and entry count across both stores. */
export async function pruneConversationCaches(): Promise<void> {
  const db = await openDB();
  if (!db) return;
  try {
    await new Promise<void>((resolve) => {
      const tx = db.transaction([BRIEFS_STORE, SNAPSHOTS_STORE], "readwrite");
      const timeout = setTimeout(() => {
        try { tx.abort(); } catch { /* already finished */ }
        resolve();
      }, CACHE_TIMEOUT_MS);
      tx.oncomplete = tx.onabort = tx.onerror = () => { clearTimeout(timeout); resolve(); };
      const entries: Array<{ store: string; key: IDBValidKey; cachedAt: number; bytes: number }> = [];
      let remaining = 2;
      for (const name of [BRIEFS_STORE, SNAPSHOTS_STORE]) {
        const request = tx.objectStore(name).openCursor();
        request.onsuccess = () => {
          const cursor = request.result;
          if (cursor) {
            const value = cursor.value;
            entries.push({ store: name, key: cursor.primaryKey, cachedAt: value.cachedAt ?? 0, bytes: JSON.stringify(value).length * 2 });
            cursor.continue();
          } else if (--remaining === 0) {
            entries.sort((a, b) => b.cachedAt - a.cachedAt);
            let bytes = 0;
            let count = 0;
            for (const entry of entries) {
              if (Date.now() - entry.cachedAt > CONVERSATION_CACHE_MAX_AGE_MS
                || count >= CONVERSATION_CACHE_MAX_ENTRIES || bytes + entry.bytes > CONVERSATION_CACHE_MAX_BYTES) {
                tx.objectStore(entry.store).delete(entry.key);
              } else { count++; bytes += entry.bytes; }
            }
          }
        };
      }
    });
  } catch { /* storage unavailable */ }
}

/** Reset the DB promise (test utility). */
export function _resetDbPromise(): void {
  database?.close();
  database = null;
  if (pruneTimer !== undefined) clearTimeout(pruneTimer);
  pruneTimer = undefined;
  dbPromise = null;
}
