/**
 * Database open/upgrade for the observer-sync event ledger.
 *
 * New, independent database: never reuses the legacy `holon-webgui-cache`
 * object stores or key namespaces. DB_VERSION manages future schema
 * upgrades inside this database only.
 */

import { LedgerUnavailableError } from "./errors";

export const LEDGER_DB_NAME = "holon.webGui.eventLedger.v1";
export const LEDGER_DB_VERSION = 2;

export const RUNTIME_SCOPES_STORE = "runtime_scopes";
export const AGENT_SESSIONS_STORE = "agent_sessions";
export const RAW_EVENTS_STORE = "raw_events";
export const PENDING_HYDRATION_STORE = "pending_hydration";
export const CANONICAL_RECORDS_STORE = "canonical_records";
export const READ_STATES_STORE = "read_states";
export const MIGRATION_META_STORE = "migration_meta";

export const LEDGER_STORES = [
  RUNTIME_SCOPES_STORE,
  AGENT_SESSIONS_STORE,
  RAW_EVENTS_STORE,
  PENDING_HYDRATION_STORE,
  CANONICAL_RECORDS_STORE,
  READ_STATES_STORE,
  MIGRATION_META_STORE,
] as const;

export type LedgerStoreName = (typeof LEDGER_STORES)[number];

/** Index on agent-scoped stores covering the 5-part scope prefix. */
export const BY_SCOPE_INDEX = "byScope";
/** Index on runtime_scopes covering (remoteKey, runtimeId, visibilityScopeId). */
export const BY_REMOTE_RUNTIME_INDEX = "byRemoteRuntime";
/** Index on agent_sessions covering (remoteKey, agentId) across scopes. */
export const BY_AGENT_INDEX = "byAgent";

/**
 * Apply schema migrations inside one upgrade transaction. Kept as an
 * explicit function so every future version bump has one obvious home.
 */
export function applyLedgerUpgrade(
  db: IDBDatabase,
  oldVersion: number,
  upgradeTransaction?: IDBTransaction,
): void {
  if (oldVersion < 1) {
    const runtimeScopes = db.createObjectStore(RUNTIME_SCOPES_STORE, {
      keyPath: ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch"],
    });
    runtimeScopes.createIndex(
      BY_REMOTE_RUNTIME_INDEX,
      ["remoteKey", "runtimeId", "visibilityScopeId"],
      { unique: false },
    );

    const agentSessions = db.createObjectStore(AGENT_SESSIONS_STORE, {
      keyPath: ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId"],
    });
    agentSessions.createIndex(
      BY_SCOPE_INDEX,
      ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId"],
      { unique: true },
    );

    const rawEvents = db.createObjectStore(RAW_EVENTS_STORE, {
      keyPath: ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId", "eventSeq"],
    });
    rawEvents.createIndex(
      BY_SCOPE_INDEX,
      ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId"],
      { unique: false },
    );

    const pendingHydration = db.createObjectStore(PENDING_HYDRATION_STORE, {
      keyPath: ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId", "jobId"],
    });
    pendingHydration.createIndex(
      BY_SCOPE_INDEX,
      ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId"],
      { unique: false },
    );

    const canonicalRecords = db.createObjectStore(CANONICAL_RECORDS_STORE, {
      keyPath: [
        "remoteKey",
        "runtimeId",
        "visibilityScopeId",
        "eventLogEpoch",
        "agentId",
        "recordKind",
        "recordId",
      ],
    });
    canonicalRecords.createIndex(
      BY_SCOPE_INDEX,
      ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId"],
      { unique: false },
    );

    const readStates = db.createObjectStore(READ_STATES_STORE, {
      keyPath: ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId"],
    });
    readStates.createIndex(
      BY_SCOPE_INDEX,
      ["remoteKey", "runtimeId", "visibilityScopeId", "eventLogEpoch", "agentId"],
      { unique: true },
    );

    db.createObjectStore(MIGRATION_META_STORE, { keyPath: "metaKey" });
  }
  if (oldVersion < 2) {
    // v2: per-agent secondary index so recovery lookups stop scanning every
    // runtime scope's sessions for one agent id.
    const store = upgradeTransaction?.objectStore(AGENT_SESSIONS_STORE);
    if (store && !store.indexNames.contains(BY_AGENT_INDEX)) {
      store.createIndex(BY_AGENT_INDEX, ["remoteKey", "agentId"], { unique: false });
    }
  }
}

export interface OpenLedgerDatabaseEvents {
  /**
   * Called when another connection upgrades the schema; this handle closed
   * itself so the upgrade is not blocked.
   */
  onSuperseded?: () => void;
}

/**
 * Open the ledger database at LEDGER_DB_VERSION.
 *
 * Rejects with LedgerUnavailableError on hard open failures. A `blocked`
 * upgrade keeps waiting: the open completes once the blocking connection
 * closes, which is the correct multi-tab behavior.
 */
export function openLedgerDatabase(events: OpenLedgerDatabaseEvents = {}): Promise<IDBDatabase> {
  return new Promise<IDBDatabase>((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new LedgerUnavailableError("no_indexeddb", "memory_only"));
      return;
    }
    let request: IDBOpenDBRequest;
    try {
      request = indexedDB.open(LEDGER_DB_NAME, LEDGER_DB_VERSION);
    } catch (error) {
      reject(new LedgerUnavailableError("open_error", "memory_only", error));
      return;
    }
    request.onupgradeneeded = (event) => {
      const oldVersion = (event as IDBVersionChangeEvent).oldVersion;
      applyLedgerUpgrade(request.result, oldVersion, request.transaction ?? undefined);
    };
    request.onsuccess = () => {
      const db = request.result;
      db.onversionchange = () => {
        db.close();
        events.onSuperseded?.();
      };
      resolve(db);
    };
    request.onerror = () => {
      reject(new LedgerUnavailableError("open_error", "memory_only", request.error));
    };
    request.onblocked = () => {
      // Keep waiting: the open proceeds when the blocking connection closes.
    };
  });
}

/** Test/utility helper: fully delete the ledger database. */
export function deleteLedgerDatabase(): Promise<void> {
  return new Promise<void>((resolve) => {
    if (typeof indexedDB === "undefined") {
      resolve();
      return;
    }
    const request = indexedDB.deleteDatabase(LEDGER_DB_NAME);
    request.onsuccess = () => resolve();
    request.onerror = () => resolve();
    request.onblocked = () => resolve();
  });
}
