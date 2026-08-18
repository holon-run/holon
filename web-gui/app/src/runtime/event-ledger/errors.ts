/**
 * Explicit error and durability states for the observer-sync event ledger.
 *
 * The legacy cache silently swallowed IndexedDB failures. This module does the
 * opposite: every failure mode is a typed error plus an explicit durability
 * state, so callers can never silently continue in "exact" mode.
 */

/**
 * What the in-memory layer may claim about persisted state:
 * - "exact": the last known commit is durably reflected in IndexedDB.
 * - "memory_only": IndexedDB is unusable for this handle (missing, open
 *   failure, quota exhausted, schema superseded); nothing may be claimed.
 * - "uncertain": a transaction failed in a way whose outcome the caller must
 *   treat as unproven until it re-verifies (re-read, repair, or reset).
 */
export type LedgerDurability = "exact" | "memory_only" | "uncertain";

export type LedgerUnavailableReason = "no_indexeddb" | "open_error" | "closed" | "superseded";

export class LedgerUnavailableError extends Error {
  readonly reason: LedgerUnavailableReason;
  readonly durability: LedgerDurability;

  constructor(reason: LedgerUnavailableReason, durability: LedgerDurability, cause?: unknown) {
    const detail = cause instanceof Error ? `: ${cause.message}` : "";
    super(`event ledger unavailable (${reason})${detail}`);
    this.name = "LedgerUnavailableError";
    this.reason = reason;
    this.durability = durability;
  }
}

export class LedgerQuotaError extends Error {
  readonly durability: LedgerDurability = "memory_only";

  constructor(cause?: unknown) {
    const detail = cause instanceof Error ? `: ${cause.message}` : "";
    super(`event ledger write rejected by storage quota${detail}`);
    this.name = "LedgerQuotaError";
  }
}

export class LedgerTransactionAbortedError extends Error {
  readonly durability: LedgerDurability = "uncertain";

  constructor(cause?: unknown) {
    const detail = cause instanceof Error ? `: ${cause.message}` : "";
    super(`event ledger transaction aborted${detail}`);
    this.name = "LedgerTransactionAbortedError";
  }
}

/**
 * Immutable identity content conflict. The same correctness key was written
 * twice with different immutable content. This is a protocol error: the
 * caller must surface it, never overwrite the stored value.
 */
export class LedgerIdentityConflictError extends Error {
  readonly store: string;
  readonly key: string;
  readonly existingFingerprint: string;
  readonly incomingFingerprint: string;

  constructor(params: {
    store: string;
    key: string;
    existingFingerprint: string;
    incomingFingerprint: string;
  }) {
    super(
      `event ledger identity conflict in ${params.store} at [${params.key}]: ` +
        `stored ${params.existingFingerprint} != incoming ${params.incomingFingerprint}`,
    );
    this.name = "LedgerIdentityConflictError";
    this.store = params.store;
    this.key = params.key;
    this.existingFingerprint = params.existingFingerprint;
    this.incomingFingerprint = params.incomingFingerprint;
  }
}

/**
 * The contiguous ingestion cursor would move backwards. The ingestion
 * protocol only allows monotonic advance; callers must never regress it.
 */
export class LedgerCursorRegressionError extends Error {
  readonly currentThroughSeq: number;
  readonly requestedThroughSeq: number;

  constructor(currentThroughSeq: number, requestedThroughSeq: number) {
    super(
      `ingestion cursor regression: stored through ${currentThroughSeq}, ` +
        `requested ${requestedThroughSeq}`,
    );
    this.name = "LedgerCursorRegressionError";
    this.currentThroughSeq = currentThroughSeq;
    this.requestedThroughSeq = requestedThroughSeq;
  }
}

export function isQuotaExceeded(error: unknown): boolean {
  return (
    error instanceof DOMException &&
    (error.name === "QuotaExceededError" || error.code === 22)
  );
}
