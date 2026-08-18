/**
 * Correctness keys for the observer-sync event ledger.
 *
 * Every durable record is partitioned by the full scope identity
 * (remoteKey, runtimeId, visibilityScopeId, eventLogEpoch, agentId). Records
 * from different remotes, runtimes, visibility scopes, epochs, or agents can
 * never share a key, so they can never be stitched together by accident.
 */

export interface LedgerScopeKey {
  remoteKey: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  agentId: string;
}

export interface LedgerRemoteScopeKey {
  remoteKey: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
}

export type LedgerRecordKind = "message" | "brief" | "transcript_entry";

/** Primary-key fields shared by every agent-scoped store. */
export function scopeKeyParts(key: LedgerScopeKey): string[] {
  return [
    key.remoteKey,
    key.runtimeId,
    key.visibilityScopeId,
    key.eventLogEpoch,
    key.agentId,
  ];
}

export function remoteScopeKeyParts(key: LedgerRemoteScopeKey): string[] {
  return [key.remoteKey, key.runtimeId, key.visibilityScopeId, key.eventLogEpoch];
}

/** Primary key of one runtime scope record. */
export function remoteScopeRecordKey(key: LedgerRemoteScopeKey): IDBValidKey {
  return remoteScopeKeyParts(key);
}

/** Compound in-line key for agent-scoped records. */
export function agentRecordKey(key: LedgerScopeKey): IDBValidKey {
  return scopeKeyParts(key);
}

/** Compound in-line key for one raw event. */
export function rawEventKey(key: LedgerScopeKey, eventSeq: number): IDBValidKey {
  return [...scopeKeyParts(key), eventSeq];
}

/** Compound in-line key for one canonical record. */
export function canonicalRecordKey(
  key: LedgerScopeKey,
  recordKind: LedgerRecordKind,
  recordId: string,
): IDBValidKey {
  return [...scopeKeyParts(key), recordKind, recordId];
}

/** Compound in-line key for one hydration job. */
export function hydrationJobKey(key: LedgerScopeKey, jobId: string): IDBValidKey {
  return [...scopeKeyParts(key), jobId];
}

/**
 * Open key range covering every raw event of one agent scope, in seq order.
 * Primary keys are 6-part arrays; the bare 5-part scope array sorts before
 * every event of that scope, and no finite event_seq exceeds
 * Number.MAX_VALUE. Infinity is not a valid IndexedDB key.
 */
export function rawEventRange(key: LedgerScopeKey): IDBKeyRange {
  const scope = scopeKeyParts(key);
  return IDBKeyRange.bound(scope, [...scope, Number.MAX_VALUE]);
}

/** Closed key range for raw events in [fromSeq, toSeq]. */
export function rawEventRangeBetween(
  key: LedgerScopeKey,
  fromSeq: number,
  toSeq: number,
): IDBKeyRange {
  const scope = scopeKeyParts(key);
  return IDBKeyRange.bound([...scope, fromSeq], [...scope, toSeq]);
}

/**
 * Key range covering every byScope index entry of one remote scope. Index
 * keys are 5-part arrays; the empty-array upper bound sorts after every
 * string agent id without matching any real key.
 */
export function agentScopeRange(key: LedgerRemoteScopeKey): IDBKeyRange {
  const parts = remoteScopeKeyParts(key);
  return IDBKeyRange.bound([...parts, ""], [...parts, []]);
}

/** Stable, order-independent JSON serialization for identity fingerprints. */
export function stableStringify(value: unknown): string {
  return serialize(value);
}

function serialize(value: unknown): string {
  if (value === null || value === undefined) return "null";
  switch (typeof value) {
    case "number":
      return Number.isFinite(value) ? JSON.stringify(value) : "null";
    case "string":
    case "boolean":
      return JSON.stringify(value);
    case "object": {
      if (Array.isArray(value)) {
        return `[${value.map((item) => serialize(item)).join(",")}]`;
      }
      const record = value as Record<string, unknown>;
      const keys = Object.keys(record).sort();
      const body = keys
        .filter((k) => record[k] !== undefined)
        .map((k) => `${JSON.stringify(k)}:${serialize(record[k])}`)
        .join(",");
      return `{${body}}`;
    }
    default:
      return "null";
  }
}

/**
 * Fingerprint of the immutable identity content of a raw event envelope.
 * Two envelopes with the same correctness key but different fingerprints are
 * an identity conflict.
 */
export function computeEnvelopeFingerprint(envelope: unknown): string {
  return stableStringify(envelope);
}
