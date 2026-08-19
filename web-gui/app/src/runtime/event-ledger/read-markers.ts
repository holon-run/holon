/**
 * Browser-local read-state views (RFC LocalReadState).
 *
 * The durable record lives in READ_STATES_STORE; this module holds the pure
 * boundary/certainty rules shared by the ledger transactions, the ingestion
 * pipeline, and the runtime unread UI. No marker is ever sent to the server.
 */

import type { LedgerReadStateRecord } from "./ledger";

export type ReadCertainty = "exact" | "truncated";

/** Effective boundary above which unread events are counted. */
export function readMarkerBoundary(record: LedgerReadStateRecord | undefined): number {
  if (!record) return 0;
  return Math.max(record.unreadBaselineSeq ?? 0, record.readThroughEventSeq ?? 0);
}

/** True when one raw envelope qualifies as a user-facing unread brief. */
export function isQualifyingUnreadEnvelope(envelope: unknown): boolean {
  return (
    typeof envelope === "object" &&
    envelope !== null &&
    (envelope as { type?: unknown }).type === "brief_created"
  );
}

/**
 * Unread snapshot for one agent scope as consumed by the UI. `count` counts
 * only qualifying brief events that are already hydrated (bounded by the
 * projection readiness cursor), never a raw sequence distance. When
 * `certainty` is `truncated`, `count` is a lower bound.
 */
export interface LedgerUnreadSnapshot {
  scopeAgentId: string;
  boundarySeq: number;
  /** Highest seq the count covers (projection readiness cursor). */
  countedThroughSeq: number;
  certainty: ReadCertainty;
  count: number;
  historyTruncatedBeforeSeq: number | null;
  acknowledgedTruncationBeforeSeq: number | null;
}

export function unreadSnapshotFromRecord(
  record: LedgerReadStateRecord | undefined,
  count: number,
  countedThroughSeq: number,
): LedgerUnreadSnapshot {
  return {
    scopeAgentId: record?.agentId ?? "",
    boundarySeq: readMarkerBoundary(record),
    countedThroughSeq,
    certainty: record?.certainty ?? "exact",
    count,
    historyTruncatedBeforeSeq: record?.historyTruncatedBeforeSeq ?? null,
    acknowledgedTruncationBeforeSeq: record?.acknowledgedTruncationBeforeSeq ?? null,
  };
}

/**
 * Result of one marker-advance transaction. `advanced` is false when the
 * stored marker already covers the candidate (monotonic maximum, no write).
 */
export interface ReadMarkerAdvanceResult {
  advanced: boolean;
  record: LedgerReadStateRecord;
}

/**
 * Merge a candidate read-through seq into a read-state record as the
 * monotonic maximum. Pure counterpart of the ledger transaction so callers
 * can preview or tests can verify the transition without IndexedDB.
 */
export function mergeReadMarkerCandidate(
  current: LedgerReadStateRecord | undefined,
  scope: Omit<LedgerReadStateRecord, "updatedAt">,
  candidateSeq: number,
  now: number,
): { advanced: boolean; record: LedgerReadStateRecord } {
  const existing = current?.readThroughEventSeq ?? null;
  if (existing != null && existing >= candidateSeq) {
    return {
      advanced: false,
      record: current ?? { ...scope, updatedAt: now },
    };
  }
  return {
    advanced: true,
    record: {
      ...(current ?? {}),
      ...scope,
      readThroughEventSeq: candidateSeq,
      updatedAt: now,
    },
  };
}

/**
 * Apply an explicit truncation acknowledgement: open a new exact generation
 * at `headSeq` while preserving the recorded truncation facts. History is
 * never rewritten to claim the lost interval was reconstructed.
 */
export function mergeTruncationAcknowledgement(
  current: LedgerReadStateRecord,
  headSeq: number,
  now: number,
): LedgerReadStateRecord {
  return {
    ...current,
    unreadBaselineSeq: Math.max(current.unreadBaselineSeq ?? 0, headSeq),
    acknowledgedTruncationBeforeSeq: Math.max(
      current.acknowledgedTruncationBeforeSeq ?? 0,
      headSeq,
    ),
    certainty: "exact",
    updatedAt: now,
  };
}
