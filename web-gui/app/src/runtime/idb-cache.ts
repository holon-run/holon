/**
 * Thin IndexedDB wrapper for client-side session caching.
 *
 * All operations are defensive: if IndexedDB is unavailable (private mode,
 * quota exceeded, etc.) they resolve to empty/no-op results so the caller
 * can silently fall back to a memory-only mode.
 */

const DB_NAME = "holon-webgui-cache";
const DB_VERSION = 1;
export const CACHE_SCHEMA_VERSION = 2;
const SESSIONS_STORE = "sessions";
const META_STORE = "meta";

export interface CachedAgentSession {
  remoteKey: string;
  agentId: string;
  schemaVersion: number;
  eventLogEpoch?: string;
  eventsBySeq: Record<number, unknown>;
  eventSeqs: number[];
  messagesById: Record<string, unknown>;
  transcriptEntriesById: Record<string, unknown>;
  briefRecordsById: Record<string, unknown>;
  newestSeq?: number;
  oldestSeq?: number;
  cachedAt: number;
}

let dbPromise: Promise<IDBDatabase | null> | null = null;

function openDB(): Promise<IDBDatabase | null> {
  if (dbPromise) return dbPromise;

  dbPromise = new Promise<IDBDatabase | null>((resolve) => {
    if (typeof indexedDB === "undefined") {
      resolve(null);
      return;
    }
    let request: IDBOpenDBRequest;
    try {
      request = indexedDB.open(DB_NAME, DB_VERSION);
    } catch {
      resolve(null);
      return;
    }
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(SESSIONS_STORE)) {
        const store = db.createObjectStore(SESSIONS_STORE, { keyPath: ["remoteKey", "agentId"] });
        store.createIndex("byRemoteKey", "remoteKey", { unique: false });
      }
      if (!db.objectStoreNames.contains(META_STORE)) {
        db.createObjectStore(META_STORE, { keyPath: "remoteKey" });
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => resolve(null);
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
    const request = fn(tx.objectStore(storeName));
    request.onsuccess = () => resolve(request.result as T);
    request.onerror = () => reject(request.error);
  });
}

export async function cachePutSession(session: CachedAgentSession): Promise<void> {
  const db = await openDB();
  if (!db) return;
  try {
    await runRequest(db, SESSIONS_STORE, "readwrite", (store) => store.put(session));
  } catch {
    // Silent fallback — cache is best-effort.
  }
}

export async function cacheGetSession(remoteKey: string, agentId: string): Promise<CachedAgentSession | undefined> {
  const db = await openDB();
  if (!db) return undefined;
  try {
    return await runRequest<CachedAgentSession>(db, SESSIONS_STORE, "readonly", (store) =>
      store.get([remoteKey, agentId]),
    );
  } catch {
    return undefined;
  }
}

export async function cacheGetAllSessions(remoteKey: string): Promise<CachedAgentSession[]> {
  const db = await openDB();
  if (!db) return [];
  try {
    const index = db.transaction(SESSIONS_STORE, "readonly").objectStore(SESSIONS_STORE).index("byRemoteKey");
    return await new Promise<CachedAgentSession[]>((resolve, reject) => {
      const result: CachedAgentSession[] = [];
      const request = index.openCursor(IDBKeyRange.only(remoteKey));
      request.onsuccess = () => {
        const cursor = request.result;
        if (cursor) {
          result.push(cursor.value as CachedAgentSession);
          cursor.continue();
        } else {
          resolve(result);
        }
      };
      request.onerror = () => reject(request.error);
    });
  } catch {
    return [];
  }
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

export async function cacheClearRemote(remoteKey: string): Promise<void> {
  const db = await openDB();
  if (!db) return;
  try {
    const store = db.transaction(SESSIONS_STORE, "readwrite").objectStore(SESSIONS_STORE);
    const index = store.index("byRemoteKey");
    const range = IDBKeyRange.only(remoteKey);
    await new Promise<void>((resolve, reject) => {
      const request = index.openCursor(range);
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

/**
 * Check if the cache schema is compatible. If any cached entry has a
 * mismatched schema version, wipe the entire database and return false.
 */
export async function ensureCacheSchemaVersion(): Promise<boolean> {
  const db = await openDB();
  if (!db) return false;
  try {
    const store = db.transaction(SESSIONS_STORE, "readonly").objectStore(SESSIONS_STORE);
    const incompatible = await new Promise<boolean>((resolve, reject) => {
      const request = store.openCursor();
      let foundIncompatible = false;
      request.onsuccess = () => {
        const cursor = request.result;
        if (cursor) {
          const entry = cursor.value as CachedAgentSession;
          if (entry.schemaVersion !== CACHE_SCHEMA_VERSION) {
            foundIncompatible = true;
            resolve(true);
            return;
          }
          cursor.continue();
        } else {
          resolve(foundIncompatible);
        }
      };
      request.onerror = () => reject(request.error);
    });
    if (incompatible) {
      db.close();
      dbPromise = null;
      await new Promise<void>((resolve) => {
        try {
          const req = indexedDB.deleteDatabase(DB_NAME);
          req.onsuccess = () => resolve();
          req.onerror = () => resolve();
          req.onblocked = () => resolve();
        } catch {
          resolve();
        }
      });
      return false;
    }
    return true;
  } catch {
    return false;
  }
}

/** Reset the DB promise (test utility). */
export function _resetDbPromise(): void {
  dbPromise = null;
}
