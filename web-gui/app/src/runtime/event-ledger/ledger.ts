/**
 * Event ledger: durable raw ingestion storage with composable atomic
 * transactions (W1).
 *
 * Correctness contract:
 * - One `commit()` is one IndexedDB readwrite transaction covering every
 *   touched store. Either all of envelope/classification, hydration jobs,
 *   canonical records, projection change, read state, and the contiguous
 *   ingestion cursor land, or none do.
 * - A raw event with an existing correctness key and a different identity
 *   fingerprint is a protocol error (hard fail), never an overwrite.
 * - A duplicate raw event with identical content is idempotent.
 * - Cursor regression is a protocol error.
 * - Failures are typed errors plus an explicit durability state; nothing ever
 *   silently continues as "exact".
 */

import {
  AGENT_SESSIONS_STORE,
  BY_AGENT_INDEX,
  BY_REMOTE_RUNTIME_INDEX,
  BY_SCOPE_INDEX,
  CANONICAL_RECORDS_STORE,
  MIGRATION_META_STORE,
  PENDING_HYDRATION_STORE,
  RAW_EVENTS_STORE,
  READ_STATES_STORE,
  RUNTIME_SCOPES_STORE,
  deleteLedgerDatabase,
  openLedgerDatabase,
  type LedgerStoreName,
} from "./db";
import {
  LedgerCursorRegressionError,
  LedgerIdentityConflictError,
  LedgerQuotaError,
  LedgerTransactionAbortedError,
  LedgerUnavailableError,
  isQuotaExceeded,
  type LedgerDurability,
} from "./errors";
import {
  agentRecordKey,
  agentScopeRange,
  canonicalRecordKey,
  computeEnvelopeFingerprint,
  hydrationJobKey,
  remoteScopeRecordKey,
  rawEventKey,
  rawEventRange,
  rawEventRangeBetween,
  scopeKeyParts,
  type LedgerRecordKind,
  type LedgerRemoteScopeKey,
  type LedgerScopeKey,
} from "./keys";
import {
  isQualifyingUnreadEnvelope,
  mergeReadMarkerCandidate,
  mergeTruncationAcknowledgement,
  type ReadMarkerAdvanceResult,
} from "./read-markers";

/** Classification attached to a stored raw event (S2 contract). */
export interface RawEventClassification {
  projectionEffect: "none" | "display_invalidation";
  envelopeContractVersion?: number;
}

export interface LedgerRawEventRecord {
  remoteKey: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  agentId: string;
  eventSeq: number;
  envelope: unknown;
  identityFingerprint: string;
  classification: RawEventClassification;
  ingestedAt: number;
}

export interface LedgerHydrationJobRecord {
  remoteKey: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  agentId: string;
  jobId: string;
  recordKind: LedgerRecordKind;
  recordId: string;
  createdByEventSeq: number;
  /**
   * Minimum canonical revision that satisfies this job, when the creating
   * event names one. A canonical record (or tombstone) with a revision >=
   * this value completes the job; a fetched record without a revision cannot
   * prove satisfaction and leaves the job pending.
   */
  expectedRevision?: string | number;
  /** Durable retry bookkeeping; `failed` marks exhausted bounded retries. */
  attemptCount?: number;
  lastAttemptAt?: number;
  lastErrorKind?: string;
  state?: "pending" | "failed";
  createdAt: number;
}

export interface LedgerCanonicalRecord {
  remoteKey: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  agentId: string;
  recordKind: LedgerRecordKind;
  recordId: string;
  record: unknown;
  revision?: string | number;
  updatedAt: number;
}

export interface LedgerAgentSessionRecord {
  remoteKey: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  agentId: string;
  /** Contiguous raw ingestion cursor: every event <= this seq is durably stored. */
  ingestedThroughSeq?: number;
  /**
   * Projection readiness cursor: every display-affecting event <= this seq
   * is durably stored AND satisfied (applied directly, hydrated, or
   * tombstoned). Never exceeds ingestedThroughSeq.
   */
  projectionReadyThroughSeq?: number;
  /** Projection revision anchor after applying directly applicable events. */
  projectionRevision?: number;
  projection?: unknown;
  updatedAt: number;
}

export interface LedgerRuntimeScopeRecord {
  remoteKey: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  eventHeadSeq?: number;
  updatedAt: number;
}

export interface LedgerReadStateRecord {
  remoteKey: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  agentId: string;
  lastReadDeliverySeq?: number;
  lastUnreadDeliverySeq?: number;
  /**
   * RFC LocalReadState (observer sync): fresh unread baseline established
   * at bootstrap. Unread is counted from qualifying brief events above
   * `max(unreadBaselineSeq, readThroughEventSeq ?? 0)`.
   */
  unreadBaselineSeq?: number;
  /** Highest delivery seq the reader has consumed; absent until first read. */
  readThroughEventSeq?: number;
  /** Whether unread counts above the boundary are exact or a lower bound. */
  certainty?: "exact" | "truncated";
  /** Retention floor of the last retention-gap reset; history below is lost. */
  historyTruncatedBeforeSeq?: number;
  /** User acknowledgement of unknown history after a truncation (W5 action). */
  acknowledgedTruncationBeforeSeq?: number;
  updatedAt: number;
}

export type EventLedgerOpenResult =
  | { kind: "available"; ledger: EventLedger }
  | { kind: "memory_only"; reason: "no_indexeddb" | "open_error"; error?: unknown };

type AgentSessionPatch = Partial<
  Omit<LedgerAgentSessionRecord, keyof LedgerScopeKey | "updatedAt">
>;
type RuntimeScopePatch = Partial<
  Omit<LedgerRuntimeScopeRecord, keyof LedgerRemoteScopeKey | "updatedAt">
>;
type ReadStatePatch = Partial<
  Omit<LedgerReadStateRecord, keyof LedgerScopeKey | "updatedAt">
>;

type WriteOperation =
  | {
      kind: "raw_event";
      scope: LedgerScopeKey;
      eventSeq: number;
      envelope: unknown;
      classification: RawEventClassification;
    }
  | { kind: "hydration_job"; scope: LedgerScopeKey; job: LedgerHydrationJobRecord }
  | { kind: "hydration_job_delete"; scope: LedgerScopeKey; jobId: string }
  | {
      kind: "canonical_record";
      scope: LedgerScopeKey;
      recordKind: LedgerRecordKind;
      recordId: string;
      record: unknown;
      revision?: string | number;
    }
  | { kind: "agent_session"; scope: LedgerScopeKey; patch: AgentSessionPatch }
  | { kind: "runtime_scope"; scope: LedgerRemoteScopeKey; patch: RuntimeScopePatch }
  | { kind: "read_state"; scope: LedgerScopeKey; patch: ReadStatePatch }
  /**
   * Discard every durable record of one agent scope (raw events, hydration
   * jobs, canonical records, session). Read state survives when preserved:
   * retention resets keep the marker to record truncation against it.
   */
  | { kind: "clear_agent_scope"; scope: LedgerScopeKey; preserveReadState: boolean }
  /** Discard every agent scope and the runtime scope record itself. */
  | { kind: "clear_runtime_scope"; scope: LedgerRemoteScopeKey };

/*
 * Agent-session patches share one patch channel: per-scope patches inside one
 * batch coalesce by ordered spread and the merged value is written once. Both
 * cursors are validated monotonically against the value observed inside the
 * same transaction, so a coalesced patch can never regress either cursor.
 */
type AgentSessionCursorField = "ingestedThroughSeq" | "projectionReadyThroughSeq";

const CURSOR_FIELDS: readonly AgentSessionCursorField[] = [
  "ingestedThroughSeq",
  "projectionReadyThroughSeq",
];

/** Composable write batch; `commit()` runs as one atomic transaction. */
export class EventLedgerWriteBatch {
  private readonly ledger: EventLedger;
  private readonly ops: WriteOperation[] = [];
  private committed = false;

  constructor(ledger: EventLedger) {
    this.ledger = ledger;
  }

  /** Store one immutable raw event envelope with its classification. */
  putRawEvent(
    scope: LedgerScopeKey,
    eventSeq: number,
    envelope: unknown,
    classification: RawEventClassification,
  ): this {
    this.assertNotCommitted();
    this.ops.push({ kind: "raw_event", scope, eventSeq, envelope, classification });
    return this;
  }

  /** Queue one durable hydration job for a reference event. */
  putHydrationJob(scope: LedgerScopeKey, job: LedgerHydrationJobRecord): this {
    this.assertNotCommitted();
    this.ops.push({ kind: "hydration_job", scope, job });
    return this;
  }

  /**
   * Remove one durable hydration job. Intended to land in the same
   * transaction as the canonical record (or tombstone) that satisfies it,
   * so completion is all-or-nothing.
   */
  deleteHydrationJob(scope: LedgerScopeKey, jobId: string): this {
    this.assertNotCommitted();
    this.ops.push({ kind: "hydration_job_delete", scope, jobId });
    return this;
  }

  /** Upsert one canonical record (projection state, mutable by design). */
  putCanonicalRecord(
    scope: LedgerScopeKey,
    recordKind: LedgerRecordKind,
    recordId: string,
    record: unknown,
    revision?: string | number,
  ): this {
    this.assertNotCommitted();
    this.ops.push({ kind: "canonical_record", scope, recordKind, recordId, record, revision });
    return this;
  }

  /** Apply a directly applicable projection change to the agent session. */
  applyProjectionChange(scope: LedgerScopeKey, patch: AgentSessionPatch): this {
    this.assertNotCommitted();
    this.ops.push({ kind: "agent_session", scope, patch });
    return this;
  }

  /** Advance the contiguous ingestion cursor (monotonic only). */
  advanceIngestionCursor(scope: LedgerScopeKey, throughSeq: number): this {
    this.assertNotCommitted();
    this.applyProjectionChange(scope, { ingestedThroughSeq: throughSeq });
    return this;
  }

  /** Patch runtime-scope metadata (e.g. observed event head). */
  putRuntimeScope(scope: LedgerRemoteScopeKey, patch: RuntimeScopePatch): this {
    this.assertNotCommitted();
    this.ops.push({ kind: "runtime_scope", scope, patch });
    return this;
  }

  /** Patch browser-local read marker state. */
  putReadState(scope: LedgerScopeKey, patch: ReadStatePatch): this {
    this.assertNotCommitted();
    this.ops.push({ kind: "read_state", scope, patch });
    return this;
  }

  /**
   * Clear one agent scope: raw events, hydration jobs, canonical records,
   * and the agent session land in one transaction with everything else in
   * this batch, so a reset followed by an install is all-or-nothing. Read
   * state is preserved for retention resets and discarded for identity
   * resets, which must not migrate markers.
   */
  clearAgentScope(
    scope: LedgerScopeKey,
    options: { preserveReadState?: boolean } = {},
  ): this {
    this.assertNotCommitted();
    this.ops.push({
      kind: "clear_agent_scope",
      scope,
      preserveReadState: options.preserveReadState ?? false,
    });
    return this;
  }

  /**
   * Clear an entire runtime scope: every agent scope under it plus the
   * runtime scope record. Used for epoch and visibility resets, where no
   * old-scope data may ever join the new scope's projection.
   */
  clearRuntimeScope(scope: LedgerRemoteScopeKey): this {
    this.assertNotCommitted();
    this.ops.push({ kind: "clear_runtime_scope", scope });
    return this;
  }

  /**
   * Commit atomically. Rejects with a typed error on any failure; on
   * conflict, regression, or storage failure nothing is committed,
   * including the cursor.
   */
  async commit(): Promise<void> {
    this.assertNotCommitted();
    if (this.ops.length === 0) {
      this.committed = true;
      return;
    }
    const db = this.ledger.requireOpenDb();
    this.committed = true;

    const storeNames = new Set<LedgerStoreName>();
    for (const op of this.ops) {
      switch (op.kind) {
        case "raw_event":
          storeNames.add(RAW_EVENTS_STORE);
          storeNames.add(AGENT_SESSIONS_STORE);
          break;
        case "hydration_job":
          storeNames.add(PENDING_HYDRATION_STORE);
          break;
        case "hydration_job_delete":
          storeNames.add(PENDING_HYDRATION_STORE);
          break;
        case "canonical_record":
          storeNames.add(CANONICAL_RECORDS_STORE);
          break;
        case "agent_session":
          storeNames.add(AGENT_SESSIONS_STORE);
          break;
        case "runtime_scope":
          storeNames.add(RUNTIME_SCOPES_STORE);
          break;
        case "read_state":
          storeNames.add(READ_STATES_STORE);
          break;
        case "clear_agent_scope":
          storeNames.add(RAW_EVENTS_STORE);
          storeNames.add(PENDING_HYDRATION_STORE);
          storeNames.add(CANONICAL_RECORDS_STORE);
          storeNames.add(AGENT_SESSIONS_STORE);
          storeNames.add(READ_STATES_STORE);
          break;
        case "clear_runtime_scope":
          storeNames.add(RAW_EVENTS_STORE);
          storeNames.add(PENDING_HYDRATION_STORE);
          storeNames.add(CANONICAL_RECORDS_STORE);
          storeNames.add(AGENT_SESSIONS_STORE);
          storeNames.add(READ_STATES_STORE);
          storeNames.add(RUNTIME_SCOPES_STORE);
          break;
      }
    }

    const tx = db.transaction([...storeNames], "readwrite");
    let failure: unknown = null;

    // Coalesce per-scope patches so multiple ops in one batch against the
    // same record merge instead of overwriting each other.
    const sessionPatches = new Map<string, { scope: LedgerScopeKey; patch: AgentSessionPatch }>();
    const runtimeScopePatches = new Map<
      string,
      { scope: LedgerRemoteScopeKey; patch: RuntimeScopePatch }
    >();
    const readStatePatches = new Map<string, { scope: LedgerScopeKey; patch: ReadStatePatch }>();
    for (const op of this.ops) {
      if (op.kind === "agent_session") {
        const id = scopeKeyParts(op.scope).join("\u0000");
        const entry = sessionPatches.get(id);
        sessionPatches.set(id, {
          scope: op.scope,
          patch: { ...(entry?.patch ?? {}), ...op.patch },
        });
      } else if (op.kind === "runtime_scope") {
        const id = [
          op.scope.remoteKey,
          op.scope.runtimeId,
          op.scope.visibilityScopeId,
          op.scope.eventLogEpoch,
        ].join("\u0000");
        const entry = runtimeScopePatches.get(id);
        runtimeScopePatches.set(id, {
          scope: op.scope,
          patch: { ...(entry?.patch ?? {}), ...op.patch },
        });
      } else if (op.kind === "read_state") {
        const id = scopeKeyParts(op.scope).join("\u0000");
        const entry = readStatePatches.get(id);
        readStatePatches.set(id, {
          scope: op.scope,
          patch: { ...(entry?.patch ?? {}), ...op.patch },
        });
      }
    }

    /** Issue a request; a synchronous throw aborts the transaction. */
    const issue = <T>(make: () => IDBRequest<T>): IDBRequest<T> | null => {
      try {
        return make();
      } catch (error) {
        failure = failure ?? error;
        tryAbort(tx);
        return null;
      }
    };

    const settled = new Promise<void>((resolve, reject) => {
      tx.oncomplete = () => resolve();
      tx.onabort = () => {
        if (failure && isQuotaExceeded(failure)) {
          this.ledger.noteDurability("memory_only", "quota");
          reject(new LedgerQuotaError(failure));
          return;
        }
        if (
          failure instanceof LedgerIdentityConflictError ||
          failure instanceof LedgerCursorRegressionError
        ) {
          // Deterministic rollback with a known cause; durability unchanged.
          reject(failure);
          return;
        }
        this.ledger.noteDurability("uncertain", "transaction_aborted");
        reject(new LedgerTransactionAbortedError(failure ?? tx.error));
      };
      tx.onerror = () => {
        failure = failure ?? tx.error;
        tryAbort(tx);
      };
    });

    const now = Date.now();

    for (const op of this.ops) {
      switch (op.kind) {
        case "raw_event": {
          const events = tx.objectStore(RAW_EVENTS_STORE);
          const key = rawEventKey(op.scope, op.eventSeq);
          const keyLabel = (key as unknown[]).join("/");
          const fingerprint = computeEnvelopeFingerprint(op.envelope);
          const existing = issue<LedgerRawEventRecord | undefined>(() => events.get(key));
          if (!existing) break;
          existing.onsuccess = () => {
            const prior = existing.result;
            if (prior && prior.identityFingerprint !== fingerprint) {
              failure =
                failure ??
                new LedgerIdentityConflictError({
                  store: RAW_EVENTS_STORE,
                  key: keyLabel,
                  existingFingerprint: prior.identityFingerprint,
                  incomingFingerprint: fingerprint,
                });
              tryAbort(tx);
              return;
            }
            if (prior) return; // Idempotent duplicate: keep the stored value.
            const put = issue(() =>
              events.put({
                ...op.scope,
                eventSeq: op.eventSeq,
                envelope: op.envelope,
                identityFingerprint: fingerprint,
                classification: op.classification,
                ingestedAt: now,
              } satisfies LedgerRawEventRecord),
            );
            put?.addEventListener("error", () => {
              failure = failure ?? (put.error ?? new Error("raw event put failed"));
            });
          };
          existing.addEventListener("error", () => {
            failure = failure ?? (existing.error ?? new Error("raw event get failed"));
          });
          break;
        }
        case "hydration_job": {
          const jobs = tx.objectStore(PENDING_HYDRATION_STORE);
          const put = issue(() =>
            jobs.put({
              ...op.job,
              createdAt: op.job.createdAt ?? now,
            } satisfies LedgerHydrationJobRecord),
          );
          put?.addEventListener("error", () => {
            failure = failure ?? (put.error ?? new Error("hydration job put failed"));
          });
          break;
        }
        case "hydration_job_delete": {
          const jobs = tx.objectStore(PENDING_HYDRATION_STORE);
          const del = issue(() => jobs.delete(hydrationJobKey(op.scope, op.jobId)));
          del?.addEventListener("error", () => {
            failure = failure ?? (del.error ?? new Error("hydration job delete failed"));
          });
          break;
        }
        case "canonical_record": {
          const records = tx.objectStore(CANONICAL_RECORDS_STORE);
          const put = issue(() =>
            records.put({
              ...op.scope,
              recordKind: op.recordKind,
              recordId: op.recordId,
              record: op.record,
              revision: op.revision,
              updatedAt: now,
            } satisfies LedgerCanonicalRecord),
          );
          put?.addEventListener("error", () => {
            failure = failure ?? (put.error ?? new Error("canonical record put failed"));
          });
          break;
        }
      }
    }

    /** Range-delete agent-scoped rows through the byScope index. */
    const deleteByScopeRange = (storeName: LedgerStoreName, range: IDBKeyRange) => {
      const index = tx.objectStore(storeName).index(BY_SCOPE_INDEX);
      const scan = issue(() => index.openCursor(range));
      if (!scan) return;
      scan.onsuccess = () => {
        const cursor = scan.result;
        if (!cursor) return;
        const del = issue(() => cursor.delete());
        del?.addEventListener("error", () => {
          failure = failure ?? (del.error ?? new Error(`${storeName} scope clear failed`));
        });
        try {
          cursor.continue();
        } catch (error) {
          failure = failure ?? error;
          tryAbort(tx);
        }
      };
      scan.addEventListener("error", () => {
        failure = failure ?? (scan.error ?? new Error(`${storeName} scope scan failed`));
      });
    };

    for (const op of this.ops) {
      if (op.kind === "clear_agent_scope") {
        const scopeOnly = IDBKeyRange.only(scopeKeyParts(op.scope));
        deleteByScopeRange(RAW_EVENTS_STORE, scopeOnly);
        deleteByScopeRange(PENDING_HYDRATION_STORE, scopeOnly);
        deleteByScopeRange(CANONICAL_RECORDS_STORE, scopeOnly);
        const sessionDelete = issue(() =>
          tx.objectStore(AGENT_SESSIONS_STORE).delete(agentRecordKey(op.scope)),
        );
        sessionDelete?.addEventListener("error", () => {
          failure =
            failure ?? (sessionDelete.error ?? new Error("agent session clear failed"));
        });
        if (!op.preserveReadState) {
          const stateDelete = issue(() =>
            tx.objectStore(READ_STATES_STORE).delete(agentRecordKey(op.scope)),
          );
          stateDelete?.addEventListener("error", () => {
            failure =
              failure ?? (stateDelete.error ?? new Error("read state clear failed"));
          });
        }
      } else if (op.kind === "clear_runtime_scope") {
        const scopeRange = agentScopeRange(op.scope);
        deleteByScopeRange(RAW_EVENTS_STORE, scopeRange);
        deleteByScopeRange(PENDING_HYDRATION_STORE, scopeRange);
        deleteByScopeRange(CANONICAL_RECORDS_STORE, scopeRange);
        deleteByScopeRange(AGENT_SESSIONS_STORE, scopeRange);
        deleteByScopeRange(READ_STATES_STORE, scopeRange);
        const runtimeDelete = issue(() =>
          tx.objectStore(RUNTIME_SCOPES_STORE).delete(remoteScopeRecordKey(op.scope)),
        );
        runtimeDelete?.addEventListener("error", () => {
          failure =
            failure ?? (runtimeDelete.error ?? new Error("runtime scope clear failed"));
        });
      }
    }

    // Agent sessions: one read-modify-write per coalesced scope. Cursor
    // monotonicity is validated against the value observed inside this
    // same transaction.
    for (const entry of sessionPatches.values()) {
      const sessions = tx.objectStore(AGENT_SESSIONS_STORE);
      const get = issue<LedgerAgentSessionRecord | undefined>(() =>
        sessions.get(agentRecordKey(entry.scope)),
      );
      if (!get) continue;
      get.onsuccess = () => {
        const existing = get.result;
        for (const field of CURSOR_FIELDS) {
          const current = existing?.[field];
          const requested = entry.patch[field];
          if (
            typeof current === "number" &&
            typeof requested === "number" &&
            requested < current
          ) {
            failure = failure ?? new LedgerCursorRegressionError(current, requested);
            tryAbort(tx);
            return;
          }
        }
        // Readiness may never claim past the contiguous ingestion cursor.
        const readyThrough = entry.patch.projectionReadyThroughSeq;
        const ingestedThrough =
          entry.patch.ingestedThroughSeq ?? existing?.ingestedThroughSeq;
        if (
          typeof readyThrough === "number" &&
          typeof ingestedThrough === "number" &&
          readyThrough > ingestedThrough
        ) {
          failure = failure ?? new LedgerCursorRegressionError(ingestedThrough, readyThrough);
          tryAbort(tx);
          return;
        }
        const merged: LedgerAgentSessionRecord = {
          ...(existing ?? {}),
          ...entry.scope,
          ...entry.patch,
          updatedAt: now,
        };
        const put = issue(() => sessions.put(merged));
        put?.addEventListener("error", () => {
          failure = failure ?? (put.error ?? new Error("agent session put failed"));
        });
      };
      get.addEventListener("error", () => {
        failure = failure ?? (get.error ?? new Error("agent session get failed"));
      });
    }

    // Runtime scopes: one read-modify-write per coalesced remote scope.
    for (const entry of runtimeScopePatches.values()) {
      const scopes = tx.objectStore(RUNTIME_SCOPES_STORE);
      const scopeKey: IDBValidKey = [
        entry.scope.remoteKey,
        entry.scope.runtimeId,
        entry.scope.visibilityScopeId,
        entry.scope.eventLogEpoch,
      ];
      const get = issue<LedgerRuntimeScopeRecord | undefined>(() => scopes.get(scopeKey));
      if (!get) continue;
      get.onsuccess = () => {
        const merged: LedgerRuntimeScopeRecord = {
          ...(get.result ?? {}),
          ...entry.scope,
          ...entry.patch,
          updatedAt: now,
        };
        const put = issue(() => scopes.put(merged));
        put?.addEventListener("error", () => {
          failure = failure ?? (put.error ?? new Error("runtime scope put failed"));
        });
      };
      get.addEventListener("error", () => {
        failure = failure ?? (get.error ?? new Error("runtime scope get failed"));
      });
    }

    // Read states: one read-modify-write per coalesced agent scope.
    for (const entry of readStatePatches.values()) {
      const states = tx.objectStore(READ_STATES_STORE);
      const get = issue<LedgerReadStateRecord | undefined>(() =>
        states.get(agentRecordKey(entry.scope)),
      );
      if (!get) continue;
      get.onsuccess = () => {
        const merged: LedgerReadStateRecord = {
          ...(get.result ?? {}),
          ...entry.scope,
          ...entry.patch,
          updatedAt: now,
        };
        const put = issue(() => states.put(merged));
        put?.addEventListener("error", () => {
          failure = failure ?? (put.error ?? new Error("read state put failed"));
        });
      };
      get.addEventListener("error", () => {
        failure = failure ?? (get.error ?? new Error("read state get failed"));
      });
    }

    await settled;
  }

  private assertNotCommitted(): void {
    if (this.committed) {
      throw new Error("EventLedgerWriteBatch already committed");
    }
  }
}

function tryAbort(tx: IDBTransaction): void {
  try {
    tx.abort();
  } catch {
    // Transaction already aborting or closed; onabort still fires.
  }
}

/** Open handle to the event ledger database. */
export class EventLedger {
  private db: IDBDatabase | null = null;
  private durabilityState: LedgerDurability = "exact";
  private durabilityReasonValue: string | undefined;

  constructor(db: IDBDatabase) {
    this.db = db;
  }

  /** Open the ledger. Never silent: failures return `memory_only` explicitly. */
  static async open(): Promise<EventLedgerOpenResult> {
    if (typeof indexedDB === "undefined") {
      return { kind: "memory_only", reason: "no_indexeddb" };
    }
    let handle: EventLedger | null = null;
    try {
      const db = await openLedgerDatabase({
        onSuperseded: () => {
          handle?.noteSuperseded();
        },
      });
      handle = new EventLedger(db);
      return { kind: "available", ledger: handle };
    } catch (error) {
      return {
        kind: "memory_only",
        reason: "open_error",
        error: error instanceof LedgerUnavailableError ? error : undefined,
      };
    }
  }

  get durability(): LedgerDurability {
    return this.durabilityState;
  }

  get durabilityReason(): string | undefined {
    return this.durabilityReasonValue;
  }

  beginWrite(): EventLedgerWriteBatch {
    return new EventLedgerWriteBatch(this);
  }

  close(): void {
    this.noteClosed("closed");
  }

  async getRuntimeScope(
    scope: LedgerRemoteScopeKey,
  ): Promise<LedgerRuntimeScopeRecord | undefined> {
    const db = this.requireOpenDb();
    const key: IDBValidKey = [
      scope.remoteKey,
      scope.runtimeId,
      scope.visibilityScopeId,
      scope.eventLogEpoch,
    ];
    return runGet(db, RUNTIME_SCOPES_STORE, (store) => store.get(key));
  }

  async listRuntimeScopes(
    remoteKey: string,
    runtimeId: string,
    visibilityScopeId: string,
  ): Promise<LedgerRuntimeScopeRecord[]> {
    const db = this.requireOpenDb();
    return collectIndex<LedgerRuntimeScopeRecord>(
      db,
      RUNTIME_SCOPES_STORE,
      BY_REMOTE_RUNTIME_INDEX,
      IDBKeyRange.only([remoteKey, runtimeId, visibilityScopeId]),
    );
  }

  /**
   * List every runtime scope of one remote key. Used by the restart scan to
   * enumerate scopes with durable state without knowing their identities in
   * advance. Index keys are 3-part arrays; `[remoteKey, []]` sorts after
   * every string runtime id, so the bound covers the whole remote.
   */
  async listRuntimeScopesByRemoteKey(
    remoteKey: string,
  ): Promise<LedgerRuntimeScopeRecord[]> {
    const db = this.requireOpenDb();
    return collectIndex<LedgerRuntimeScopeRecord>(
      db,
      RUNTIME_SCOPES_STORE,
      BY_REMOTE_RUNTIME_INDEX,
      IDBKeyRange.bound([remoteKey, ""], [remoteKey, []]),
    );
  }

  async listAgentSessions(scope: LedgerRemoteScopeKey): Promise<LedgerAgentSessionRecord[]> {
    const db = this.requireOpenDb();
    return collectIndex<LedgerAgentSessionRecord>(
      db,
      AGENT_SESSIONS_STORE,
      BY_SCOPE_INDEX,
      agentScopeRange(scope),
    );
  }

  async getAgentSession(scope: LedgerScopeKey): Promise<LedgerAgentSessionRecord | undefined> {
    const db = this.requireOpenDb();
    return runGet(db, AGENT_SESSIONS_STORE, (store) => store.get(agentRecordKey(scope)));
  }

  async getRawEvent(
    scope: LedgerScopeKey,
    eventSeq: number,
  ): Promise<LedgerRawEventRecord | undefined> {
    const db = this.requireOpenDb();
    return runGet(db, RAW_EVENTS_STORE, (store) => store.get(rawEventKey(scope, eventSeq)));
  }

  async getRawEvents(scope: LedgerScopeKey): Promise<LedgerRawEventRecord[]> {
    const db = this.requireOpenDb();
    return collectRange<LedgerRawEventRecord>(db, RAW_EVENTS_STORE, rawEventRange(scope));
  }

  async getRawEventsBetween(
    scope: LedgerScopeKey,
    fromSeq: number,
    toSeq: number,
  ): Promise<LedgerRawEventRecord[]> {
    const db = this.requireOpenDb();
    return collectRange<LedgerRawEventRecord>(
      db,
      RAW_EVENTS_STORE,
      rawEventRangeBetween(scope, fromSeq, toSeq),
    );
  }

  async getPendingHydrationJobs(scope: LedgerScopeKey): Promise<LedgerHydrationJobRecord[]> {
    const db = this.requireOpenDb();
    return collectIndex<LedgerHydrationJobRecord>(
      db,
      PENDING_HYDRATION_STORE,
      BY_SCOPE_INDEX,
      IDBKeyRange.only(scopeKeyParts(scope)),
    );
  }


  async getCanonicalRecord(
    scope: LedgerScopeKey,
    recordKind: LedgerRecordKind,
    recordId: string,
  ): Promise<LedgerCanonicalRecord | undefined> {
    const db = this.requireOpenDb();
    return runGet(db, CANONICAL_RECORDS_STORE, (store) =>
      store.get(canonicalRecordKey(scope, recordKind, recordId)),
    );
  }

  async listCanonicalRecords(scope: LedgerScopeKey): Promise<LedgerCanonicalRecord[]> {
    const db = this.requireOpenDb();
    return collectIndex<LedgerCanonicalRecord>(
      db,
      CANONICAL_RECORDS_STORE,
      BY_SCOPE_INDEX,
      IDBKeyRange.only(scopeKeyParts(scope)),
    );
  }

  /**
   * Every stored session for one agent id under one remote, across runtime
   * scopes, epochs, and visibility scopes. Recovery uses this to find the
   * current scope and to detect identity changes that require a reset.
   */
  async findAgentSessionsByAgent(
    remoteKey: string,
    agentId: string,
  ): Promise<LedgerAgentSessionRecord[]> {
    const db = this.requireOpenDb();
    return collectIndex<LedgerAgentSessionRecord>(
      db,
      AGENT_SESSIONS_STORE,
      BY_AGENT_INDEX,
      IDBKeyRange.only([remoteKey, agentId]),
    );
  }

  async getReadState(scope: LedgerScopeKey): Promise<LedgerReadStateRecord | undefined> {
    const db = this.requireOpenDb();
    return runGet(db, READ_STATES_STORE, (store) => store.get(agentRecordKey(scope)));
  }

  /**
   * Advance `readThroughEventSeq` to `candidateSeq` as a monotonic maximum
   * inside one transaction. Tabs race this concurrently; the stored value
   * can only move forward. Returns whether a write happened.
   */
  async advanceReadMarker(
    scope: LedgerScopeKey,
    candidateSeq: number,
  ): Promise<ReadMarkerAdvanceResult> {
    if (!Number.isInteger(candidateSeq) || candidateSeq < 0) {
      throw new Error("read marker candidate must be a non-negative integer");
    }
    const db = this.requireOpenDb();
    return new Promise<ReadMarkerAdvanceResult>((resolve, reject) => {
      let settled = false;
      let result: ReadMarkerAdvanceResult | null = null;
      const tx = db.transaction(READ_STATES_STORE, "readwrite");
      const store = tx.objectStore(READ_STATES_STORE);
      const get = store.get(agentRecordKey(scope));
      get.onsuccess = () => {
        const current = get.result as LedgerReadStateRecord | undefined;
        const merged = mergeReadMarkerCandidate(
          current,
          scope,
          candidateSeq,
          Date.now(),
        );
        result = merged;
        if (!merged.advanced) return;
        const put = store.put(merged.record);
        put.onerror = () => {
          if (settled) return;
          settled = true;
          reject(put.error ?? new Error("read state put failed"));
        };
      };
      get.onerror = () => {
        if (settled) return;
        settled = true;
        reject(get.error ?? new Error("read state get failed"));
      };
      tx.oncomplete = () => {
        if (settled) return;
        settled = true;
        if (!result) {
          reject(new Error("read state transaction completed without a result"));
          return;
        }
        resolve(result);
      };
      tx.onerror = () => {
        if (!settled) {
          settled = true;
          reject(tx.error ?? new Error("read state transaction failed"));
        }
      };
      tx.onabort = () => {
        if (!settled) {
          settled = true;
          reject(tx.error ?? new Error("read state transaction aborted"));
        }
      };
    });
  }

  /**
   * Record an explicit truncation acknowledgement at `headSeq`: open a new
   * exact generation while keeping the truncation facts. Returns null when
   * no read state exists yet (nothing to acknowledge).
   */
  async acknowledgeReadTruncation(
    scope: LedgerScopeKey,
    headSeq: number,
  ): Promise<LedgerReadStateRecord | null> {
    if (!Number.isInteger(headSeq) || headSeq < 0) {
      throw new Error("truncation acknowledgement head must be a non-negative integer");
    }
    const db = this.requireOpenDb();
    return new Promise<LedgerReadStateRecord | null>((resolve, reject) => {
      let settled = false;
      let result: LedgerReadStateRecord | null = null;
      const tx = db.transaction(READ_STATES_STORE, "readwrite");
      const store = tx.objectStore(READ_STATES_STORE);
      const get = store.get(agentRecordKey(scope));
      get.onsuccess = () => {
        const current = get.result as LedgerReadStateRecord | undefined;
        if (!current) return;
        result = mergeTruncationAcknowledgement(current, headSeq, Date.now());
        const put = store.put(result);
        put.onerror = () => {
          if (settled) return;
          settled = true;
          reject(put.error ?? new Error("read state put failed"));
        };
      };
      get.onerror = () => {
        if (settled) return;
        settled = true;
        reject(get.error ?? new Error("read state get failed"));
      };
      tx.oncomplete = () => {
        if (settled) return;
        settled = true;
        resolve(result);
      };
      tx.onerror = () => {
        if (!settled) {
          settled = true;
          reject(tx.error ?? new Error("read state transaction failed"));
        }
      };
      tx.onabort = () => {
        if (!settled) {
          settled = true;
          reject(tx.error ?? new Error("read state transaction aborted"));
        }
      };
    });
  }

  /**
   * Count qualifying unread envelopes in (boundaryExclusive, throughSeq].
   * Uses a cursor so an unbounded backlog above the boundary is not
   * materialized. Hydration is guaranteed by the caller bounding
   * `throughSeq` at the projection readiness cursor.
   */
  async countQualifyingUnreadEvents(
    scope: LedgerScopeKey,
    boundaryExclusive: number,
    throughSeq: number,
  ): Promise<number> {
    if (throughSeq <= boundaryExclusive) return 0;
    const db = this.requireOpenDb();
    return new Promise<number>((resolve, reject) => {
      let settled = false;
      let count = 0;
      const tx = db.transaction(RAW_EVENTS_STORE, "readonly");
      const range = rawEventRangeBetween(scope, boundaryExclusive + 1, throughSeq);
      const request = tx.objectStore(RAW_EVENTS_STORE).openCursor(range);
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) return;
        if (isQualifyingUnreadEnvelope((cursor.value as LedgerRawEventRecord).envelope)) {
          count += 1;
        }
        cursor.continue();
      };
      request.onerror = () => {
        if (settled) return;
        settled = true;
        reject(request.error ?? new Error("unread count cursor failed"));
      };
      tx.oncomplete = () => {
        if (settled) return;
        settled = true;
        resolve(count);
      };
      tx.onerror = () => {
        if (!settled) {
          settled = true;
          reject(tx.error ?? new Error("unread count transaction failed"));
        }
      };
      tx.onabort = () => {
        if (!settled) {
          settled = true;
          reject(tx.error ?? new Error("unread count transaction aborted"));
        }
      };
    });
  }

  async getMigrationMeta<T extends { metaKey: string }>(
    metaKey: string,
  ): Promise<T | undefined> {
    const db = this.requireOpenDb();
    return runGet<T>(db, MIGRATION_META_STORE, (store) => store.get(metaKey));
  }

  async putMigrationMeta<T extends object>(metaKey: string, value: T): Promise<void> {
    const db = this.requireOpenDb();
    await runMutation(db, MIGRATION_META_STORE, (store) => store.put({ ...value, metaKey }));
  }

  async deleteHydrationJob(scope: LedgerScopeKey, jobId: string): Promise<void> {
    const db = this.requireOpenDb();
    await runMutation(db, PENDING_HYDRATION_STORE, (store) =>
      store.delete(hydrationJobKey(scope, jobId)),
    );
  }

  /** Test helper: delete the whole database (close handles first). */
  static async deleteDatabase(): Promise<void> {
    await deleteLedgerDatabase();
  }

  /** Internal: record a durability transition after commit failures. */
  noteDurability(state: LedgerDurability, reason?: string): void {
    this.durabilityState = state;
    this.durabilityReasonValue = reason;
  }

  /** Internal: another connection upgraded the schema; this handle closed. */
  noteSuperseded(): void {
    this.noteClosed("superseded");
  }

  private noteClosed(reason: "closed" | "superseded"): void {
    try {
      this.db?.close();
    } catch {
      // Already closed.
    }
    this.db = null;
    this.noteDurability("memory_only", reason);
  }

  requireOpenDb(): IDBDatabase {
    if (!this.db) {
      throw new LedgerUnavailableError("closed", this.durabilityState);
    }
    return this.db;
  }
}

async function runGet<T>(
  db: IDBDatabase,
  storeName: LedgerStoreName,
  make: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T | undefined> {
  return new Promise<T | undefined>((resolve, reject) => {
    let settled = false;
    const tx = db.transaction(storeName, "readonly");
    const req = make(tx.objectStore(storeName));
    req.onsuccess = () => {
      settled = true;
      resolve(req.result);
    };
    req.onerror = () => {
      settled = true;
      reject(req.error ?? new Error(`${storeName} request failed`));
    };
    tx.onerror = () => {
      if (!settled) reject(tx.error ?? new Error(`${storeName} transaction failed`));
    };
    tx.onabort = () => {
      if (!settled) reject(tx.error ?? new Error(`${storeName} transaction aborted`));
    };
  });
}

/** Run one mutating request in its own readwrite transaction. */
async function runMutation(
  db: IDBDatabase,
  storeName: LedgerStoreName,
  make: (store: IDBObjectStore) => IDBRequest,
): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    let settled = false;
    const tx = db.transaction(storeName, "readwrite");
    const req = make(tx.objectStore(storeName));
    req.onsuccess = () => {
      settled = true;
    };
    req.onerror = () => {
      settled = true;
      reject(req.error ?? new Error(`${storeName} request failed`));
    };
    tx.oncomplete = () => resolve();
    tx.onerror = () => {
      if (!settled) reject(tx.error ?? new Error(`${storeName} transaction failed`));
    };
    tx.onabort = () => {
      if (!settled) reject(tx.error ?? new Error(`${storeName} transaction aborted`));
    };
  });
}

async function collectRange<T>(
  db: IDBDatabase,
  storeName: LedgerStoreName,
  range: IDBKeyRange,
): Promise<T[]> {
  return new Promise<T[]>((resolve, reject) => {
    let settled = false;
    const tx = db.transaction(storeName, "readonly");
    const results: T[] = [];
    const req = tx.objectStore(storeName).openCursor(range);
    req.onsuccess = () => {
      const cursor = req.result;
      if (cursor) {
        results.push(cursor.value as T);
        cursor.continue();
      } else {
        settled = true;
        resolve(results);
      }
    };
    req.onerror = () => reject(req.error ?? new Error(`${storeName} cursor failed`));
    tx.onerror = () => reject(tx.error ?? new Error(`${storeName} transaction failed`));
    tx.onabort = () => {
      if (!settled) reject(tx.error ?? new Error(`${storeName} transaction aborted`));
    };
  });
}

async function collectIndex<T>(
  db: IDBDatabase,
  storeName: LedgerStoreName,
  indexName: string,
  range: IDBKeyRange,
): Promise<T[]> {
  return new Promise<T[]>((resolve, reject) => {
    let settled = false;
    const tx = db.transaction(storeName, "readonly");
    const results: T[] = [];
    const req = tx.objectStore(storeName).index(indexName).openCursor(range);
    req.onsuccess = () => {
      const cursor = req.result;
      if (cursor) {
        results.push(cursor.value as T);
        cursor.continue();
      } else {
        settled = true;
        resolve(results);
      }
    };
    req.onerror = () => reject(req.error ?? new Error(`${storeName} index cursor failed`));
    tx.onerror = () => reject(tx.error ?? new Error(`${storeName} transaction failed`));
    tx.onabort = () => {
      if (!settled) reject(tx.error ?? new Error(`${storeName} transaction aborted`));
    };
  });
}
