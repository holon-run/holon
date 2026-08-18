import "fake-indexeddb/auto";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  LEDGER_DB_NAME,
  LEDGER_DB_VERSION,
  LEDGER_STORES,
  type LedgerScopeKey,
  type LedgerRemoteScopeKey,
  LedgerCursorRegressionError,
  LedgerIdentityConflictError,
  LedgerQuotaError,
  LedgerTransactionAbortedError,
  LedgerUnavailableError,
} from "./index";
import {
  EventLedger,
  type LedgerHydrationJobRecord,
} from "./ledger";
import {
  LEGACY_DB_NAME,
  deleteLegacyDatabase,
  hasUnreadMigrationNoticeBeenShown,
  initializeFreshBaseline,
  markUnreadMigrationNoticeShown,
} from "./migration";

function makeScope(overrides: Partial<LedgerScopeKey> = {}): LedgerScopeKey {
  return {
    remoteKey: "http://127.0.0.1:7878",
    runtimeId: "rt_test",
    visibilityScopeId: "vis_test",
    eventLogEpoch: "epoch-1",
    agentId: "agent-1",
    ...overrides,
  };
}

function envelope(seq: number, payload: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    agent_id: "agent-1",
    contract_version: 1,
    event_log_epoch: "epoch-1",
    event_seq: seq,
    id: `evt-${seq}`,
    payload,
    payload_schema: "test",
    payload_schema_version: 1,
    provenance: {},
    ts: `2026-08-18T00:00:0${seq}Z`,
    type: "test.event",
  };
}

function remoteScopeOf(scope: LedgerScopeKey): LedgerRemoteScopeKey {
  return {
    remoteKey: scope.remoteKey,
    runtimeId: scope.runtimeId,
    visibilityScopeId: scope.visibilityScopeId,
    eventLogEpoch: scope.eventLogEpoch,
  };
}

function makeJob(scope: LedgerScopeKey, seq: number): LedgerHydrationJobRecord {
  return {
    ...scope,
    jobId: `job-${seq}`,
    recordKind: "message",
    recordId: `msg-${seq}`,
    createdByEventSeq: seq,
    createdAt: 1,
  };
}

async function openLedger(): Promise<EventLedger> {
  const result = await EventLedger.open();
  if (result.kind !== "available") {
    throw new Error(`expected available ledger, got ${result.kind}`);
  }
  return result.ledger;
}

function openRaw(name: string, version?: number): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, version);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(new Error("raw open blocked"));
  });
}

function deleteRaw(name: string): Promise<void> {
  return new Promise((resolve) => {
    const request = indexedDB.deleteDatabase(name);
    request.onsuccess = () => resolve();
    request.onerror = () => resolve();
    request.onblocked = () => resolve();
  });
}

async function countRows(db: IDBDatabase, store: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(store, "readonly");
    const req = tx.objectStore(store).count();
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

describe("event ledger open/upgrade", () => {
  beforeEach(async () => {
    await deleteRaw(LEDGER_DB_NAME);
    await deleteRaw(LEGACY_DB_NAME);
  });

  afterEach(async () => {
    vi.restoreAllMocks();
    await deleteRaw(LEDGER_DB_NAME);
    await deleteRaw(LEGACY_DB_NAME);
  });

  it("creates every normalized store on fresh upgrade", async () => {
    const ledger = await openLedger();
    const db = ledger.requireOpenDb();
    for (const store of LEDGER_STORES) {
      expect(db.objectStoreNames.contains(store)).toBe(true);
    }
    expect(ledger.durability).toBe("exact");
    ledger.close();
  });

  it("reports memory_only explicitly when indexedDB is missing", async () => {
    const original = globalThis.indexedDB;
    vi.stubGlobal("indexedDB", undefined);
    try {
      const result = await EventLedger.open();
      expect(result.kind).toBe("memory_only");
      if (result.kind === "memory_only") {
        expect(result.reason).toBe("no_indexeddb");
      }
    } finally {
      vi.stubGlobal("indexedDB", original);
      vi.unstubAllGlobals();
    }
  });

  it("supports multiple same-version connections and cross-tab visibility", async () => {
    const tabA = await openLedger();
    const tabB = await openLedger();
    const scope = makeScope();

    await tabA
      .beginWrite()
      .putRawEvent(scope, 1, envelope(1), { projectionEffect: "none" })
      .advanceIngestionCursor(scope, 1)
      .commit();

    const events = await tabB.getRawEvents(scope);
    expect(events.map((e) => e.eventSeq)).toEqual([1]);
    const session = await tabB.getAgentSession(scope);
    expect(session?.ingestedThroughSeq).toBe(1);
    expect(tabB.durability).toBe("exact");
    tabA.close();
    tabB.close();
  });

  it("closes itself and degrades when another connection upgrades the schema", async () => {
    const ledger = await openLedger();

    const upgrade = new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(LEDGER_DB_NAME, LEDGER_DB_VERSION + 1);
      request.onupgradeneeded = () => {
        // Future-version migration is not under test; nothing to add.
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
      request.onblocked = () => reject(new Error("upgrade should not be blocked"));
    });
    const upgraded = await upgrade;

    expect(ledger.durability).toBe("memory_only");
    expect(ledger.durabilityReason).toBe("superseded");
    await expect(
      ledger
        .beginWrite()
        .putRawEvent(makeScope(), 9, envelope(9), { projectionEffect: "none" })
        .commit(),
    ).rejects.toBeInstanceOf(LedgerUnavailableError);
    upgraded.close();
  });

  it("waits (blocked) when a connection that ignores versionchange holds the db", async () => {
    // Simulate an old tab that does not handle versionchange events.
    const stale = await openRaw(LEDGER_DB_NAME);

    let blockedFired = false;
    const upgrade = new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(LEDGER_DB_NAME, LEDGER_DB_VERSION + 1);
      request.onupgradeneeded = () => {
        // Nothing to add for this test's synthetic version.
      };
      request.onblocked = () => {
        blockedFired = true;
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });

    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(blockedFired).toBe(true);

    stale.close();
    const upgraded = await upgrade; // Proceeds once the blocking connection closes.
    upgraded.close();
  });
});

describe("event ledger atomic transactions", () => {
  beforeEach(async () => {
    await deleteRaw(LEDGER_DB_NAME);
  });

  afterEach(async () => {
    vi.restoreAllMocks();
    await deleteRaw(LEDGER_DB_NAME);
  });

  it("commits envelope, classification, hydration, records, cursor, and read state atomically", async () => {
    const ledger = await openLedger();
    const scope = makeScope();

    await ledger
      .beginWrite()
      .putRawEvent(scope, 1, envelope(1, { text: "hello" }), {
        projectionEffect: "display_invalidation",
        envelopeContractVersion: 3,
      })
      .putRawEvent(scope, 2, envelope(2, { ref: "msg-2" }), { projectionEffect: "none" })
      .putHydrationJob(scope, makeJob(scope, 2))
      .putCanonicalRecord(scope, "message", "msg-1", { id: "msg-1", text: "hello" }, "rev-1")
      .applyProjectionChange(scope, { projectionRevision: 7, projection: { items: 1 } })
      .advanceIngestionCursor(scope, 2)
      .putReadState(scope, { lastReadDeliverySeq: 1, lastUnreadDeliverySeq: 2 })
      .putRuntimeScope(remoteScopeOf(scope), { eventHeadSeq: 2 })
      .commit();

    const event1 = await ledger.getRawEvent(scope, 1);
    expect(event1?.classification.projectionEffect).toBe("display_invalidation");
    expect(event1?.classification.envelopeContractVersion).toBe(3);
    expect(event1?.identityFingerprint).toBeTruthy();

    const jobs = await ledger.getPendingHydrationJobs(scope);
    expect(jobs.map((j) => j.jobId)).toEqual(["job-2"]);

    const record = await ledger.getCanonicalRecord(scope, "message", "msg-1");
    expect(record?.revision).toBe("rev-1");

    const session = await ledger.getAgentSession(scope);
    expect(session?.ingestedThroughSeq).toBe(2);
    expect(session?.projectionRevision).toBe(7);

    const readState = await ledger.getReadState(scope);
    expect(readState?.lastReadDeliverySeq).toBe(1);
    expect(readState?.lastUnreadDeliverySeq).toBe(2);

    const runtimeScope = await ledger.getRuntimeScope(remoteScopeOf(scope));
    expect(runtimeScope?.eventHeadSeq).toBe(2);
    ledger.close();
  });

  it("merges multiple patches to the same session in one batch instead of overwriting", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    await ledger
      .beginWrite()
      .applyProjectionChange(scope, { projectionRevision: 4 })
      .advanceIngestionCursor(scope, 3)
      .commit();
    const session = await ledger.getAgentSession(scope);
    expect(session?.projectionRevision).toBe(4);
    expect(session?.ingestedThroughSeq).toBe(3);
    ledger.close();
  });

  it("rolls back the whole batch (including the cursor) on quota failure", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    await ledger
      .beginWrite()
      .putRawEvent(scope, 1, envelope(1), { projectionEffect: "none" })
      .advanceIngestionCursor(scope, 1)
      .commit();

    const originalPut = IDBObjectStore.prototype.put;
    const putSpy = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (
      this: IDBObjectStore,
      value: unknown,
      key?: IDBValidKey,
    ) {
      if (this.name === "agent_sessions") {
        throw new DOMException("The quota has been exceeded", "QuotaExceededError");
      }
      return originalPut.call(this, value, key);
    });

    await expect(
      ledger
        .beginWrite()
        .putRawEvent(scope, 2, envelope(2), { projectionEffect: "none" })
        .advanceIngestionCursor(scope, 2)
        .commit(),
    ).rejects.toBeInstanceOf(LedgerQuotaError);
    putSpy.mockRestore();

    expect(ledger.durability).toBe("memory_only");
    expect(ledger.durabilityReason).toBe("quota");

    // Nothing from the failed batch may be visible, including the cursor.
    const reopened = await openLedger();
    expect((await reopened.getRawEvents(scope)).map((e) => e.eventSeq)).toEqual([1]);
    expect((await reopened.getAgentSession(scope))?.ingestedThroughSeq).toBe(1);
    ledger.close();
    reopened.close();
  });

  it("marks durability uncertain on non-quota transaction failure", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    const originalPut = IDBObjectStore.prototype.put;
    const putSpy = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (
      this: IDBObjectStore,
      value: unknown,
      key?: IDBValidKey,
    ) {
      if (this.name === "raw_events") {
        throw new Error("synthetic store failure");
      }
      return originalPut.call(this, value, key);
    });

    await expect(
      ledger
        .beginWrite()
        .putRawEvent(scope, 1, envelope(1), { projectionEffect: "none" })
        .advanceIngestionCursor(scope, 1)
        .commit(),
    ).rejects.toBeInstanceOf(LedgerTransactionAbortedError);
    putSpy.mockRestore();

    expect(ledger.durability).toBe("uncertain");
    expect(ledger.durabilityReason).toBe("transaction_aborted");

    const reopened = await openLedger();
    expect(await reopened.getRawEvents(scope)).toEqual([]);
    expect(await reopened.getAgentSession(scope)).toBeUndefined();
    ledger.close();
    reopened.close();
  });

  it("treats an identical duplicate raw event as idempotent", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    const payload = envelope(1, { text: "same" });

    await ledger
      .beginWrite()
      .putRawEvent(scope, 1, payload, { projectionEffect: "none" })
      .advanceIngestionCursor(scope, 1)
      .commit();
    await ledger
      .beginWrite()
      .putRawEvent(scope, 1, payload, { projectionEffect: "none" })
      .advanceIngestionCursor(scope, 1)
      .commit();

    const events = await ledger.getRawEvents(scope);
    expect(events).toHaveLength(1);
    expect(events[0].envelope).toEqual(payload);
    expect((await ledger.getAgentSession(scope))?.ingestedThroughSeq).toBe(1);
    ledger.close();
  });

  it("hard-fails an identity conflict without overwriting the stored envelope", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    await ledger
      .beginWrite()
      .putRawEvent(scope, 1, envelope(1, { text: "original" }), { projectionEffect: "none" })
      .advanceIngestionCursor(scope, 1)
      .commit();

    const conflict = envelope(1, { text: "mutated" });
    let caught: unknown;
    try {
      await ledger
        .beginWrite()
        .putRawEvent(scope, 1, conflict, { projectionEffect: "none" })
        .advanceIngestionCursor(scope, 2)
        .commit();
    } catch (error) {
      caught = error;
    }
    expect(caught).toBeInstanceOf(LedgerIdentityConflictError);
    const conflictError = caught as LedgerIdentityConflictError;
    expect(conflictError.store).toBe("raw_events");
    expect(conflictError.existingFingerprint).not.toBe(conflictError.incomingFingerprint);

    // Stored value and cursor must be untouched.
    const stored = await ledger.getRawEvent(scope, 1);
    expect((stored?.envelope as { payload: { text: string } }).payload.text).toBe("original");
    expect((await ledger.getAgentSession(scope))?.ingestedThroughSeq).toBe(1);
    expect(ledger.durability).toBe("exact");
    ledger.close();
  });

  it("rejects cursor regression", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    await ledger.beginWrite().advanceIngestionCursor(scope, 5).commit();

    await expect(ledger.beginWrite().advanceIngestionCursor(scope, 3).commit()).rejects.toBeInstanceOf(
      LedgerCursorRegressionError,
    );
    expect((await ledger.getAgentSession(scope))?.ingestedThroughSeq).toBe(5);
    ledger.close();
  });

  it("survives close and reopen with committed data intact", async () => {
    const first = await openLedger();
    const scope = makeScope();
    await first
      .beginWrite()
      .putRawEvent(scope, 1, envelope(1), { projectionEffect: "none" })
      .putRawEvent(scope, 2, envelope(2), { projectionEffect: "none" })
      .advanceIngestionCursor(scope, 2)
      .commit();
    first.close();

    const second = await openLedger();
    expect((await second.getRawEvents(scope)).map((e) => e.eventSeq)).toEqual([1, 2]);
    expect((await second.getAgentSession(scope))?.ingestedThroughSeq).toBe(2);
    second.close();
  });
});

describe("event ledger scope partitioning", () => {
  beforeEach(async () => {
    await deleteRaw(LEDGER_DB_NAME);
  });

  afterEach(async () => {
    await deleteRaw(LEDGER_DB_NAME);
  });

  it("never stitches data across remote, runtime, visibility scope, or epoch", async () => {
    const ledger = await openLedger();
    const base = makeScope();
    await ledger
      .beginWrite()
      .putRawEvent(base, 1, envelope(1), { projectionEffect: "none" })
      .advanceIngestionCursor(base, 1)
      .putRuntimeScope(remoteScopeOf(base), { eventHeadSeq: 1 })
      .commit();

    const otherRemote = makeScope({ remoteKey: "http://other:7878" });
    const otherRuntime = makeScope({ runtimeId: "rt_other" });
    const otherVisibility = makeScope({ visibilityScopeId: "vis_other" });
    const otherEpoch = makeScope({ eventLogEpoch: "epoch-2" });
    const otherAgent = makeScope({ agentId: "agent-2" });

    for (const scope of [otherRemote, otherRuntime, otherVisibility, otherEpoch, otherAgent]) {
      expect(await ledger.getRawEvents(scope)).toEqual([]);
      expect(await ledger.getAgentSession(scope)).toBeUndefined();
      expect(await ledger.getReadState(scope)).toBeUndefined();
      expect(await ledger.getPendingHydrationJobs(scope)).toEqual([]);
    }

    // Different epochs of the same remote/runtime/visibility are separate
    // runtime_scopes entries, listed together but never merged.
    await ledger
      .beginWrite()
      .putRuntimeScope(remoteScopeOf(otherEpoch), { eventHeadSeq: 1 })
      .commit();
    const scopes = await ledger.listRuntimeScopes(
      base.remoteKey,
      base.runtimeId,
      base.visibilityScopeId,
    );
    expect(scopes.map((s) => s.eventLogEpoch).sort()).toEqual(["epoch-1", "epoch-2"]);

    const sessions = await ledger.listAgentSessions({
      remoteKey: base.remoteKey,
      runtimeId: base.runtimeId,
      visibilityScopeId: base.visibilityScopeId,
      eventLogEpoch: "epoch-1",
    });
    expect(sessions.map((s) => s.agentId)).toEqual(["agent-1"]);
    ledger.close();
  });

  it("returns seq-ordered events and bounded ranges", async () => {
    const ledger = await openLedger();
    const scope = makeScope();
    const batch = ledger.beginWrite();
    for (let seq = 1; seq <= 5; seq++) {
      batch.putRawEvent(scope, seq, envelope(seq), { projectionEffect: "none" });
    }
    await batch.advanceIngestionCursor(scope, 5).commit();

    expect((await ledger.getRawEvents(scope)).map((e) => e.eventSeq)).toEqual([1, 2, 3, 4, 5]);
    expect((await ledger.getRawEventsBetween(scope, 2, 4)).map((e) => e.eventSeq)).toEqual([
      2, 3, 4,
    ]);
    ledger.close();
  });
});

describe("event ledger legacy baseline", () => {
  beforeEach(async () => {
    await deleteRaw(LEDGER_DB_NAME);
    await deleteRaw(LEGACY_DB_NAME);
  });

  afterEach(async () => {
    await deleteRaw(LEDGER_DB_NAME);
    await deleteRaw(LEGACY_DB_NAME);
  });

  it("seeds nothing from the legacy database and records a fresh baseline", async () => {
    // Populate a legacy database the way the old cache would have.
    const legacy = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(LEGACY_DB_NAME, 1);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains("sessions")) {
          db.createObjectStore("sessions", { keyPath: ["remoteKey", "agentId"] });
        }
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    await new Promise<void>((resolve, reject) => {
      const tx = legacy.transaction("sessions", "readwrite");
      tx.objectStore("sessions").put({
        remoteKey: "http://127.0.0.1:7878",
        agentId: "agent-1",
        schemaVersion: 5,
        eventsBySeq: { 1: { seq: 1 } },
        readState: { lastReadDeliverySeq: 42, lastUnreadDeliverySeq: 43 },
      });
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
    });

    const ledger = await openLedger();
    const meta = await initializeFreshBaseline(ledger);
    expect(meta.strategy).toBe("fresh_server_authoritative");
    expect(meta.legacyImported).toBe(false);
    expect(meta.legacyDbName).toBe(LEGACY_DB_NAME);

    // The new database starts from nothing regardless of legacy content.
    const scope = makeScope(); // same remoteKey/agentId as the legacy entry
    expect(await ledger.getRawEvents(scope)).toEqual([]);
    expect(await ledger.getAgentSession(scope)).toBeUndefined();
    expect(await ledger.getReadState(scope)).toBeUndefined();

    // The legacy database itself must remain untouched for rollback.
    expect(await countRows(legacy, "sessions")).toBe(1);
    legacy.close();

    // Idempotent: a second call keeps the original decision.
    const again = await initializeFreshBaseline(ledger);
    expect(again.decidedAt).toBe(meta.decidedAt);
    ledger.close();
  });

  it("tracks the one-time unread migration notice across reopen", async () => {
    const first = await openLedger();
    expect(await hasUnreadMigrationNoticeBeenShown(first)).toBe(false);
    await markUnreadMigrationNoticeShown(first);
    expect(await hasUnreadMigrationNoticeBeenShown(first)).toBe(true);
    first.close();

    const second = await openLedger();
    expect(await hasUnreadMigrationNoticeBeenShown(second)).toBe(true);
    second.close();
  });

  it("reports legacy cleanup outcomes without throwing", async () => {
    const legacy = await openRaw(LEGACY_DB_NAME, 1).catch(() => undefined);
    legacy?.close();
    const result = await deleteLegacyDatabase();
    expect(result.ok).toBe(true);

    const held = await openRaw(LEGACY_DB_NAME, 1);
    // A connection that never receives versionchange blocks deletion.
    const blocked = await deleteLegacyDatabase();
    expect(blocked.ok).toBe(false);
    expect(blocked.error).toContain("blocked");
    held.close();
  });
});
