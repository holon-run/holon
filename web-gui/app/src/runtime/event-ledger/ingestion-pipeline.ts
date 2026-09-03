/**
 * Durable raw ingestion pipeline (W2).
 *
 * Owns the ledger side of observer sync for one browser profile:
 * - `ingest()` durably stores raw envelopes with their classification and
 *   advances the contiguous ingestion cursor from stored content only —
 *   never from page or stream metadata — so out-of-order delivery, live
 *   duplicates, and filtered/semantic fetches can never fabricate coverage;
 * - reference events create durable hydration jobs; deletes complete via
 *   canonical tombstones; self-contained events are satisfied by ingestion;
 * - `projectionReadyThroughSeq` advances only when every display-affecting
 *   event below it is satisfied, so read markers (W5) can gate on it;
 * - `resume()` is the restart scan: it rebuilds cursors from the ledger and
 *   drains pending hydration before readiness may advance again;
 * - bounded hydration retries escalate to projection snapshot repair, then
 *   re-verification, then an explicit `sync_error` state;
 * - a degraded ledger handle is never reused: writes require an explicit
 *   rebuild-and-verify cycle before the pipeline may claim exactness again.
 */

import {
  EventLedger,
  type EventLedgerWriteBatch,
  type LedgerHydrationJobRecord,
  type LedgerReadStateRecord,
} from "./ledger";
import type { EventLedgerOpenResult } from "./ledger";
import {
  readMarkerBoundary,
  unreadSnapshotFromRecord,
  type LedgerUnreadSnapshot,
  type ReadMarkerAdvanceResult,
} from "./read-markers";
import type { LedgerDurability } from "./errors";
import {
  classifyEnvelope,
  type ClassifiedEnvelope,
} from "./classification";
import { remoteScopeKeyParts, type LedgerRemoteScopeKey } from "./keys";
import type { LedgerRecordKind, LedgerScopeKey } from "./keys";

/** How far above the contiguous cursor resume() scans for stored stragglers. */
const RESUME_LOOKAHEAD_SEQ = 1_000;
const DEFAULT_MAX_HYDRATION_ATTEMPTS = 5;
const DEFAULT_HYDRATION_BATCH_SIZE = 64;

export interface LedgerHydrationFetchers {
  /**
   * Fetch canonical records for one record kind. `missingIds` covers ids the
   * server could not serve; a returned record without a revision cannot
   * prove satisfaction of jobs that expect a revision.
   */
  fetchCanonicalRecords(
    agentId: string,
    recordKind: LedgerRecordKind,
    recordIds: string[],
  ): Promise<{
    recordsById: Record<string, { record: unknown; revision?: string | number }>;
    missingIds: string[];
  }>;
}

export interface ProjectionSnapshotRepairSource {
  /**
   * Fetch an authoritative projection snapshot for repair. Returns null
   * when repair is unavailable (capability absent or request failed).
   */
  fetchProjectionSnapshot(scope: LedgerScopeKey): Promise<{
    snapshotThroughSeq: number;
    canonicalRecords: Array<{
      recordKind: LedgerRecordKind;
      recordId: string;
      record: unknown;
      revision?: string | number;
    }>;
  } | null>;
}

/** Projection snapshot content for a bootstrap install (W3 recovery). */
export interface ProjectionSnapshotInstall {
  /** Authoritative boundary: every display event <= it is in the snapshot. */
  snapshotThroughSeq: number;
  /** Committed event head named by the snapshot; may exceed the boundary. */
  eventHeadSeq: number;
  /** Canonical records carried by the snapshot (e.g. the latest Brief). */
  canonicalRecords: Array<{
    recordKind: LedgerRecordKind;
    recordId: string;
    record: unknown;
    revision?: string | number;
  }>;
  /** Records the projection references but the snapshot does not carry. */
  hydrationReferences: Array<{
    recordKind: LedgerRecordKind;
    recordId: string;
    revision?: string | number;
  }>;
  /** Records deleted at or before the boundary; they end hydration demand. */
  hydrationTombstones: Array<{
    recordKind: LedgerRecordKind;
    recordId: string;
  }>;
}

/** Read-state fields installable with a snapshot in the same transaction. */
export type ProjectionInstallReadState = Partial<{
  unreadBaselineSeq: number;
  readThroughEventSeq: number;
  certainty: "exact" | "truncated";
  historyTruncatedBeforeSeq: number;
  acknowledgedTruncationBeforeSeq: number;
}>;

export interface ProjectionInstallOptions {
  /** Discard the agent's raw/projection cache first (reset path). */
  clearFirst?: { preserveReadState?: boolean };
  /** Browser-local read-state patch committed atomically with the install. */
  readState?: ProjectionInstallReadState;
}

export type LedgerIngestionState =
  | "idle"
  | "draining"
  | "repairing"
  | "sync_error"
  | "memory_only";

export interface LedgerIngestionStatus {
  scope: LedgerScopeKey;
  durability: LedgerDurability;
  state: LedgerIngestionState;
  ingestedThroughSeq?: number;
  projectionReadyThroughSeq?: number;
  observedEventHeadSeq?: number;
  pendingHydrationJobs: number;
  failedHydrationJobs: number;
  blockedByEventSeq?: number;
  blockedReason?: "pending_hydration" | "unknown_envelope_version";
  lastError?: string;
}

export interface IngestionPipelineDependencies {
  fetchers: LedgerHydrationFetchers;
  snapshotRepair?: ProjectionSnapshotRepairSource;
  onStatus?: (status: LedgerIngestionStatus) => void;
  maxHydrationAttempts?: number;
  hydrationBatchSize?: number;
  /**
   * Ledger handle factory. Production uses the default `EventLedger.open`;
   * tests inject a controllable opener to observe explicit rebuilds after a
   * degraded handle is discarded.
   */
  openLedger?: () => Promise<EventLedgerOpenResult>;
}

interface HydrationJobView {
  jobId: string;
  recordKind: LedgerRecordKind;
  recordId: string;
  createdByEventSeq: number;
  expectedRevision?: string | number;
  attemptCount: number;
  lastErrorKind?: string;
  lastAttemptAt?: number;
  state: "pending" | "failed";
  createdAt: number;
}

type Blocker =
  | { kind: "pending_hydration"; jobId: string }
  | { kind: "unknown_envelope_version" };

interface ScopeTracker {
  contiguousThrough: number;
  readyThrough: number;
  observedHead: number;
  outOfOrder: Set<number>;
  blockers: Map<number, Blocker>;
  jobs: Map<string, HydrationJobView>;
  state: LedgerIngestionState;
  lastError?: string;
  draining: Promise<void> | null;
  loaded: boolean;
}

export interface CanonicalTombstone {
  tombstone: true;
  deletedAt: number;
  deletedByEventSeq?: number;
}

export function isCanonicalTombstone(record: unknown): record is CanonicalTombstone {
  return (
    !!record &&
    typeof record === "object" &&
    (record as { tombstone?: unknown }).tombstone === true
  );
}

function jobIdFor(recordKind: LedgerRecordKind, recordId: string): string {
  return `${recordKind}:${recordId}`;
}

function remoteScopeOf(scope: LedgerScopeKey) {
  return {
    remoteKey: scope.remoteKey,
    runtimeId: scope.runtimeId,
    visibilityScopeId: scope.visibilityScopeId,
    eventLogEpoch: scope.eventLogEpoch,
  };
}

function revisionSatisfies(
  expected: string | number | undefined,
  actual: string | number | undefined,
): boolean {
  if (expected === undefined) return true;
  if (actual === undefined) return false;
  if (typeof expected === "number" && typeof actual === "number") {
    return actual >= expected;
  }
  return String(actual) >= String(expected);
}

/**
 * Merge two expected revisions by keeping whichever demands more: an
 * out-of-order invalidation carrying a lower revision must never weaken a
 * demand already merged from a later event (W2 review note).
 */
function strictestRevision(
  a: string | number | undefined,
  b: string | number | undefined,
): string | number | undefined {
  if (a === undefined) return b;
  if (b === undefined) return a;
  // a is the strictest when it already covers b's demand.
  return revisionSatisfies(b, a) ? a : b;
}

/**
 * Durable ingestion pipeline over one event ledger. All cursor advances are
 * computed from stored raw events, committed atomically with the events and
 * jobs they describe, and never regressed.
 */
export class LedgerIngestionPipeline {
  private ledger: EventLedger | null = null;
  private readonly trackers = new Map<string, ScopeTracker>();
  private readonly maxAttempts: number;
  private readonly batchSize: number;

  constructor(private readonly dependencies: IngestionPipelineDependencies) {
    this.maxAttempts = dependencies.maxHydrationAttempts ?? DEFAULT_MAX_HYDRATION_ATTEMPTS;
    this.batchSize = dependencies.hydrationBatchSize ?? DEFAULT_HYDRATION_BATCH_SIZE;
  }

  /** Open the ledger handle. Returns false when only memory remains. */
  async open(): Promise<boolean> {
    if (this.ledger?.durability === "exact") return true;
    const result = await (this.dependencies.openLedger?.() ?? EventLedger.open());
    if (result.kind !== "available") return false;
    this.ledger = result.ledger;
    return true;
  }

  /** Close the handle and forget in-memory trackers. */
  dispose(): void {
    this.ledger?.close();
    this.ledger = null;
    this.trackers.clear();
  }

  /**
   * Restart scan for one scope: rebuild cursors, blockers, and pending jobs
   * from durable state, then drain pending hydration. Readiness never
   * advances past unsatisfied durable demand, so a crash between event
   * persistence and hydration simply resumes here.
   */
  async resume(scope: LedgerScopeKey): Promise<LedgerIngestionStatus> {
    const tracker = await this.ensureTracker(scope);
    if (tracker.jobs.size > 0) {
      void this.drainHydration(scope).catch(() => undefined);
    }
    return this.statusFor(scope, tracker);
  }

  /**
   * Restart scan for every known scope of one remote key. Used on cache
   * (re)initialization to resume pending hydration work after a reload.
   */
  async resumeRemote(remoteKey: string): Promise<void> {
    if (!(await this.ensureExactHandle())) return;
    const scopes = await this.ledger!.listRuntimeScopesByRemoteKey(remoteKey);
    for (const runtimeScope of scopes) {
      const sessions = await this.ledger!.listAgentSessions({
        remoteKey: runtimeScope.remoteKey,
        runtimeId: runtimeScope.runtimeId,
        visibilityScopeId: runtimeScope.visibilityScopeId,
        eventLogEpoch: runtimeScope.eventLogEpoch,
      });
      for (const session of sessions) {
        await this.resume({
          remoteKey: session.remoteKey,
          runtimeId: session.runtimeId,
          visibilityScopeId: session.visibilityScopeId,
          eventLogEpoch: session.eventLogEpoch,
          agentId: session.agentId,
        });
      }
    }
  }

  /**
   * Ingest raw envelopes atomically: raw events, hydration jobs, tombstones,
   * the contiguous ingestion cursor, observed head, and the readiness
   * cursor land in one transaction or not at all.
   */
  async ingest(
    scope: LedgerScopeKey,
    envelopes: Array<Record<string, unknown>>,
  ): Promise<LedgerIngestionStatus> {
    const tracker = await this.ensureTracker(scope);
    if (!(await this.ensureExactHandle())) {
      return this.statusFor(scope, tracker);
    }
    const ledger = this.ledger!;
    const classified = envelopes
      .map((envelope) => ({ envelope, classified: classifyEnvelope(envelope) }))
      .sort((a, b) => a.classified.eventSeq - b.classified.eventSeq);
    if (classified.length === 0) return this.statusFor(scope, tracker);

    const batch = ledger.beginWrite();
    const jobUpdates = new Map<string, HydrationJobView>();
    const newSeqs: number[] = [];
    const now = Date.now();

    for (const item of classified) {
      batch.putRawEvent(scope, item.classified.eventSeq, item.envelope, item.classified.classification);
      newSeqs.push(item.classified.eventSeq);
      if (item.classified.blocksReadiness) {
        tracker.blockers.set(item.classified.eventSeq, { kind: "unknown_envelope_version" });
        continue;
      }
      if (item.classified.classification.projectionEffect !== "display_invalidation") continue;
      if (item.classified.tombstone) {
        const jobId = jobIdFor(item.classified.tombstone.recordKind, item.classified.tombstone.recordId);
        batch.putCanonicalRecord(
          scope,
          item.classified.tombstone.recordKind,
          item.classified.tombstone.recordId,
          {
            tombstone: true,
            deletedAt: now,
            deletedByEventSeq: item.classified.eventSeq,
          } satisfies CanonicalTombstone,
        );
        // A delete completes any outstanding hydration demand for the record.
        this.removeJobAndBlockers(tracker, jobId, batch, scope);
        continue;
      }
      if (item.classified.reference) {
        const jobId = jobIdFor(item.classified.reference.recordKind, item.classified.reference.recordId);
        const existing = tracker.jobs.get(jobId) ?? jobUpdates.get(jobId);
        const merged: HydrationJobView = existing
          ? {
              ...existing,
              createdByEventSeq: Math.min(existing.createdByEventSeq, item.classified.eventSeq),
              // Strictest (max) revision across every merged event, so a
              // late out-of-order invalidation cannot weaken the job.
              expectedRevision: strictestRevision(
                item.classified.reference.expectedRevision,
                existing.expectedRevision,
              ),
              state: "pending",
            }
          : {
              jobId,
              recordKind: item.classified.reference.recordKind,
              recordId: item.classified.reference.recordId,
              createdByEventSeq: item.classified.eventSeq,
              expectedRevision: item.classified.reference.expectedRevision,
              attemptCount: 0,
              state: "pending",
              createdAt: now,
            };
        jobUpdates.set(jobId, merged);
        tracker.jobs.set(jobId, merged);
        tracker.blockers.set(item.classified.eventSeq, { kind: "pending_hydration", jobId });
        continue;
      }
      // Self-contained display events carry their projection payload in the
      // envelope; ingestion itself satisfies them.
    }

    for (const job of jobUpdates.values()) {
      batch.putHydrationJob(scope, this.jobRecordFromView(scope, job));
    }

    const contiguous = this.advanceContiguity(tracker, newSeqs);
    if (contiguous > tracker.contiguousThrough) {
      batch.advanceIngestionCursor(scope, contiguous);
      tracker.contiguousThrough = contiguous;
    }
    const observedHead = Math.max(tracker.observedHead, ...newSeqs);
    if (observedHead > tracker.observedHead) {
      batch.applyProjectionChange(scope, { observedHeadSeq: observedHead });
      batch.putRuntimeScope(remoteScopeOf(scope), { eventHeadSeq: observedHead });
      tracker.observedHead = observedHead;
    }

    const ready = this.computeReady(tracker);
    if (ready > tracker.readyThrough) {
      batch.applyProjectionChange(scope, { projectionReadyThroughSeq: ready });
      tracker.readyThrough = ready;
    }

    try {
      await batch.commit();
    } catch (error) {
      // Durable-first: on failure the tracker's durable view is reloaded so
      // in-memory cursors never run ahead of committed state.
      this.trackers.delete(this.trackerKey(scope));
      if (this.isDurabilityFailure(error)) {
        // A storage failure makes the current handle unfit for further
        // exactness claims. Discard it before publishing status so recovery
        // must reopen and verify a fresh handle.
        this.ledger?.close();
        this.ledger = null;
        const status = this.statusFor(scope, this.freshTracker());
        this.dependencies.onStatus?.(status);
        return status;
      }
      throw error;
    }

    const status = this.statusFor(scope, tracker);
    this.dependencies.onStatus?.(status);
    if (jobUpdates.size > 0) {
      void this.drainHydration(scope).catch(() => undefined);
    }
    return status;
  }

  /**
   * Complete one canonical record via tombstone: the tombstone and the
   * removal of any outstanding hydration job for the record commit
   * atomically, and readiness may advance past the satisfied demand.
   */
  async applyTombstone(
    scope: LedgerScopeKey,
    recordKind: LedgerRecordKind,
    recordId: string,
    deletedByEventSeq?: number,
  ): Promise<void> {
    const tracker = await this.ensureTracker(scope);
    if (!(await this.ensureExactHandle())) return;
    const jobId = jobIdFor(recordKind, recordId);
    const batch = this.ledger!.beginWrite();
    batch.putCanonicalRecord(scope, recordKind, recordId, {
      tombstone: true,
      deletedAt: Date.now(),
      deletedByEventSeq,
    } satisfies CanonicalTombstone);
    this.removeJobAndBlockers(tracker, jobId, batch, scope);
    const ready = this.computeReady(tracker);
    if (ready > tracker.readyThrough) {
      batch.applyProjectionChange(scope, { projectionReadyThroughSeq: ready });
    }
    try {
      await batch.commit();
      tracker.readyThrough = ready;
    } catch (error) {
      this.trackers.delete(this.trackerKey(scope));
      if (this.isDurabilityFailure(error)) return;
      throw error;
    }
    this.emit(scope, tracker);
  }

  /**
   * Atomically install an authoritative projection snapshot (W3 bootstrap):
   * canonical records, tombstones, boundary hydration jobs, both cursors at
   * the snapshot boundary, the observed event head, and the browser-local
   * read baseline land in one transaction. An optional `clearFirst` discards
   * a previous cache for the same scope key inside the same transaction, so
   * a reset followed by the install is all-or-nothing.
   */
  async installProjectionSnapshot(
    scope: LedgerScopeKey,
    install: ProjectionSnapshotInstall,
    options: ProjectionInstallOptions = {},
  ): Promise<LedgerIngestionStatus> {
    if (!(await this.ensureExactHandle())) {
      return this.statusFor(scope, this.freshTracker());
    }
    const ledger = this.ledger!;
    const now = Date.now();
    const batch = ledger.beginWrite();
    if (options.clearFirst) {
      batch.clearAgentScope(scope, { preserveReadState: options.clearFirst.preserveReadState });
    }
    for (const record of install.canonicalRecords) {
      batch.putCanonicalRecord(
        scope,
        record.recordKind,
        record.recordId,
        record.record,
        record.revision,
      );
    }
    for (const tombstone of install.hydrationTombstones) {
      batch.putCanonicalRecord(scope, tombstone.recordKind, tombstone.recordId, {
        tombstone: true,
        deletedAt: now,
        deletedByEventSeq: install.snapshotThroughSeq,
      } satisfies CanonicalTombstone);
    }
    for (const reference of install.hydrationReferences) {
      batch.putHydrationJob(scope, {
        ...scope,
        jobId: jobIdFor(reference.recordKind, reference.recordId),
        recordKind: reference.recordKind,
        recordId: reference.recordId,
        createdByEventSeq: install.snapshotThroughSeq,
        expectedRevision: reference.revision,
        attemptCount: 0,
        state: "pending",
        createdAt: now,
      });
    }
    // The snapshot is authoritative through its boundary: both cursors sit
    // at the boundary regardless of any prior cache for this scope key.
    batch.advanceIngestionCursor(scope, install.snapshotThroughSeq);
    batch.applyProjectionChange(scope, {
      observedHeadSeq: install.eventHeadSeq,
      projectionReadyThroughSeq: install.snapshotThroughSeq,
    });
    batch.putRuntimeScope(remoteScopeOf(scope), { eventHeadSeq: install.eventHeadSeq });
    if (options.readState) {
      batch.putReadState(scope, options.readState);
    }
    try {
      await batch.commit();
    } catch (error) {
      this.trackers.delete(this.trackerKey(scope));
      if (this.isDurabilityFailure(error)) {
        return this.statusFor(scope, this.freshTracker());
      }
      throw error;
    }
    // Reload the tracker from the installed durable state; boundary
    // references become ordinary pending demand the drain can service.
    this.trackers.delete(this.trackerKey(scope));
    const tracker = await this.ensureTracker(scope);
    if (tracker.jobs.size > 0) {
      void this.drainHydration(scope).catch(() => undefined);
    }
    const status = this.statusFor(scope, tracker);
    this.dependencies.onStatus?.(status);
    return status;
  }

  /**
   * Clear an entire runtime scope durably (epoch or visibility reset) and
   * forget its in-memory trackers. Old-scope data must never join the new
   * scope's projection, so this removes sessions, raw events, jobs,
   * canonical records, and read states in one transaction.
   */
  async clearRuntimeScope(remoteScope: LedgerRemoteScopeKey): Promise<void> {
    if (!(await this.ensureExactHandle())) return;
    const batch = this.ledger!.beginWrite();
    batch.clearRuntimeScope(remoteScope);
    await batch.commit();
    this.forgetRuntimeScope(remoteScope);
  }

  /** Stored sessions for one agent under one remote, across scopes. */
  async findAgentSessions(
    remoteKey: string,
    agentId: string,
  ): Promise<Array<{ scope: LedgerScopeKey; ingestedThroughSeq?: number; projectionReadyThroughSeq?: number }>> {
    if (!(await this.ensureExactHandle())) return [];
    const sessions = await this.ledger!.findAgentSessionsByAgent(remoteKey, agentId);
    return sessions.map((session) => ({
      scope: {
        remoteKey: session.remoteKey,
        runtimeId: session.runtimeId,
        visibilityScopeId: session.visibilityScopeId,
        eventLogEpoch: session.eventLogEpoch,
        agentId: session.agentId,
      },
      ingestedThroughSeq: session.ingestedThroughSeq,
      projectionReadyThroughSeq: session.projectionReadyThroughSeq,
    }));
  }

  /** Forget the in-memory tracker of one agent scope (after a clear). */
  forgetAgentScope(scope: LedgerScopeKey): void {
    this.trackers.delete(this.trackerKey(scope));
  }

  /** Browser-local read state of one scope, if recorded. */
  async readStateOf(scope: LedgerScopeKey): Promise<LedgerReadStateRecord | undefined> {
    if (!(await this.ensureExactHandle())) return undefined;
    return this.ledger!.getReadState(scope);
  }

  /**
   * Advance the browser-local read marker as a monotonic maximum. Returns
   * null on memory-only durability (never claims a durable advance there).
   */
  async advanceReadMarker(
    scope: LedgerScopeKey,
    candidateSeq: number,
  ): Promise<ReadMarkerAdvanceResult | null> {
    if (!(await this.ensureExactHandle())) return null;
    return this.ledger!.advanceReadMarker(scope, candidateSeq);
  }

  /**
   * Record an explicit truncation acknowledgement at the current observed
   * event head. Null on memory-only durability or when no read state exists.
   * An explicit `headSeq` (the gated head a read marker caught up to)
   * overrides the observed head so the auto-restore path never claims a
   * boundary beyond what the marker actually reached.
   */
  async acknowledgeReadTruncation(
    scope: LedgerScopeKey,
    headSeq?: number,
  ): Promise<LedgerReadStateRecord | null> {
    if (!(await this.ensureExactHandle())) return null;
    const head = headSeq ?? this.status(scope)?.observedEventHeadSeq;
    if (head == null) return null;
    return this.ledger!.acknowledgeReadTruncation(scope, head);
  }

  /**
   * Unread snapshot for one scope: qualifying brief events between the read
   * boundary and the projection readiness cursor. The count is exact up to
   * `countedThroughSeq`; a `truncated` certainty makes it a lower bound
   * because older history was lost to retention. Null on memory-only.
   */
  async unreadSnapshot(scope: LedgerScopeKey): Promise<LedgerUnreadSnapshot | null> {
    if (!(await this.ensureExactHandle())) return null;
    const record = await this.ledger!.getReadState(scope);
    const gate = this.readinessGate(scope);
    const through = Math.max(0, gate.readyThroughSeq);
    const boundary = readMarkerBoundary(record);
    const count = await this.ledger!.countQualifyingUnreadEvents(scope, boundary, through);
    return unreadSnapshotFromRecord(
      record,
      count,
      through,
    );
  }

  /**
   * Every agent scope with durable state under one remote. Used to seed the
   * repository's scope registry from the restart scan without knowing the
   * server's roster in advance.
   */
  async listKnownScopes(remoteKey: string): Promise<LedgerScopeKey[]> {
    if (!(await this.ensureExactHandle())) return [];
    const runtimeScopes = await this.ledger!.listRuntimeScopesByRemoteKey(remoteKey);
    const result: LedgerScopeKey[] = [];
    for (const runtimeScope of runtimeScopes) {
      const sessions = await this.ledger!.listAgentSessions({
        remoteKey: runtimeScope.remoteKey,
        runtimeId: runtimeScope.runtimeId,
        visibilityScopeId: runtimeScope.visibilityScopeId,
        eventLogEpoch: runtimeScope.eventLogEpoch,
      });
      for (const session of sessions) {
        result.push({
          remoteKey: session.remoteKey,
          runtimeId: session.runtimeId,
          visibilityScopeId: session.visibilityScopeId,
          eventLogEpoch: session.eventLogEpoch,
          agentId: session.agentId,
        });
      }
    }
    return result;
  }

  /** Forget every in-memory tracker under one runtime scope. */
  forgetRuntimeScope(remoteScope: LedgerRemoteScopeKey): void {
    const prefix = `${remoteScopeKeyParts(remoteScope).join("\u0000")}\u0000`;
    for (const key of Array.from(this.trackers.keys())) {
      if (key.startsWith(prefix)) this.trackers.delete(key);
    }
  }

  /**
   * Drain pending hydration jobs: fetch canonical records in bounded
   * batches, complete satisfied jobs atomically with their records, bump
   * durable retry counters on failures, and escalate exhausted jobs to
   * snapshot repair followed by re-verification.
   */
  async drainHydration(scope: LedgerScopeKey): Promise<void> {
    const tracker = this.trackers.get(this.trackerKey(scope));
    if (!tracker) return;
    if (tracker.draining) return tracker.draining;
    const run = this.drainHydrationInner(scope, tracker)
      .finally(() => {
        if (tracker.draining === run) tracker.draining = null;
      });
    tracker.draining = run;
    return run;
  }

  private async drainHydrationInner(
    scope: LedgerScopeKey,
    tracker: ScopeTracker,
  ): Promise<void> {
    // Nothing to drain: leave the tracker's terminal state (idle,
    // sync_error, memory_only) untouched instead of clobbering it.
    const hasPendingWork = Array.from(tracker.jobs.values()).some(
      (job) => job.state === "pending",
    );
    if (!hasPendingWork) return;
    if (!(await this.ensureExactHandle())) return;
    const ledger = this.ledger!;
    tracker.state = "draining";
    this.emit(scope, tracker);

    const pending = () =>
      Array.from(tracker.jobs.values())
        .filter((job) => job.state === "pending")
        .sort((a, b) => a.createdByEventSeq - b.createdByEventSeq);

    let jobs = pending().slice(0, this.batchSize);
    while (jobs.length > 0) {
      const byKind = new Map<LedgerRecordKind, HydrationJobView[]>();
      for (const job of jobs) {
        const list = byKind.get(job.recordKind) ?? [];
        list.push(job);
        byKind.set(job.recordKind, list);
      }

      const fetched = new Map<
        string,
        { record: unknown; revision?: string | number } | undefined
      >();
      let fetchError: string | undefined;
      for (const [recordKind, kindJobs] of byKind) {
        try {
          const result = await this.dependencies.fetchers.fetchCanonicalRecords(
            scope.agentId,
            recordKind,
            kindJobs.map((job) => job.recordId),
          );
          for (const job of kindJobs) {
            fetched.set(job.jobId, result.recordsById[job.recordId]);
          }
        } catch (error) {
          fetchError = error instanceof Error ? error.message : String(error);
        }
      }

      const completion = ledger.beginWrite();
      let completed = 0;
      const retryJobs: HydrationJobView[] = [];
      for (const job of jobs) {
        const fetchedRecord = fetchError ? undefined : fetched.get(job.jobId);
        if (
          fetchedRecord &&
          (isCanonicalTombstone(fetchedRecord.record) ||
            revisionSatisfies(job.expectedRevision, fetchedRecord.revision))
        ) {
          completion.putCanonicalRecord(
            scope,
            job.recordKind,
            job.recordId,
            fetchedRecord.record,
            fetchedRecord.revision,
          );
          completion.deleteHydrationJob(scope, job.jobId);
          this.removeJobAndBlockers(tracker, job.jobId, null, scope);
          completed += 1;
        } else {
          retryJobs.push({
            ...job,
            attemptCount: job.attemptCount + 1,
            lastAttemptAt: Date.now(),
            lastErrorKind: fetchError ? "request_failed" : "missing_or_unproven",
          });
        }
      }
      const ready = this.computeReady(tracker);
      if (ready > tracker.readyThrough) {
        completion.applyProjectionChange(scope, { projectionReadyThroughSeq: ready });
      }
      if (completed > 0 || ready > tracker.readyThrough) {
        try {
          await completion.commit();
          tracker.readyThrough = ready;
        } catch (error) {
          if (this.isDurabilityFailure(error)) {
            tracker.state = "memory_only";
            tracker.lastError = String(error);
            this.emit(scope, tracker);
            return;
          }
          throw error;
        }
      }
      // Persist durable retry bookkeeping (including the failed marker) in
      // one batch so a restart never resurrects exhausted jobs as fresh.
      if (retryJobs.length > 0) {
        const retryBatch = ledger.beginWrite();
        for (const job of retryJobs) {
          const exhausted = job.attemptCount >= this.maxAttempts;
          const updated: HydrationJobView = {
            ...job,
            state: exhausted ? "failed" : "pending",
          };
          tracker.jobs.set(job.jobId, updated);
          retryBatch.putHydrationJob(scope, this.jobRecordFromView(scope, updated));
        }
        await retryBatch.commit().catch(() => undefined);
      }

      const hasFailedJobs = Array.from(tracker.jobs.values()).some(
        (job) => job.state === "failed",
      );
      let repairedThisRound = false;
      if (hasFailedJobs) {
        const repaired = await this.repairFromSnapshot(scope, tracker);
        if (!repaired) return;
        repairedThisRound = true;
      }
      jobs = pending().slice(0, this.batchSize);
      if (jobs.length === 0) break;
      // Keep looping only while rounds make durable progress (records or
      // repair); otherwise the next ingest or retry re-triggers the drain.
      if (completed === 0 && !repairedThisRound) break;
    }

    const hasFailedJobs = Array.from(tracker.jobs.values()).some(
      (job) => job.state === "failed",
    );
    if (pending().length === 0 && !hasFailedJobs) {
      tracker.state = "idle";
    }
    this.emit(scope, tracker);
  }

  /**
   * Bounded-retry escalation: install an authoritative projection snapshot,
   * clear the jobs it covers, re-verify remaining demand, and fall through
   * to an explicit sync error when divergence persists.
   */
  private async repairFromSnapshot(
    scope: LedgerScopeKey,
    tracker: ScopeTracker,
  ): Promise<boolean> {
    const repairSource = this.dependencies.snapshotRepair;
    if (!repairSource) {
      tracker.state = "sync_error";
      tracker.lastError = "hydration_exhausted_without_repair_source";
      this.emit(scope, tracker);
      return false;
    }
    tracker.state = "repairing";
    this.emit(scope, tracker);
    let snapshot: Awaited<ReturnType<ProjectionSnapshotRepairSource["fetchProjectionSnapshot"]>>;
    try {
      snapshot = await repairSource.fetchProjectionSnapshot(scope);
    } catch {
      snapshot = null;
    }
    if (!snapshot || !(await this.ensureExactHandle())) {
      tracker.state = "sync_error";
      tracker.lastError = snapshot ? "ledger_degraded_during_repair" : "snapshot_repair_unavailable";
      this.emit(scope, tracker);
      return false;
    }
    const ledger = this.ledger!;
    const batch = ledger.beginWrite();
    for (const record of snapshot.canonicalRecords) {
      batch.putCanonicalRecord(
        scope,
        record.recordKind,
        record.recordId,
        record.record,
        record.revision,
      );
    }
    for (const job of tracker.jobs.values()) {
      if (job.createdByEventSeq <= snapshot.snapshotThroughSeq) {
        batch.deleteHydrationJob(scope, job.jobId);
      }
    }
    const readyTarget = Math.min(
      Math.max(tracker.readyThrough, snapshot.snapshotThroughSeq),
      tracker.contiguousThrough,
    );
    if (readyTarget > tracker.readyThrough) {
      batch.applyProjectionChange(scope, { projectionReadyThroughSeq: readyTarget });
    }
    try {
      await batch.commit();
    } catch (error) {
      if (this.isDurabilityFailure(error)) {
        tracker.state = "memory_only";
        tracker.lastError = String(error);
      } else {
        tracker.state = "sync_error";
        tracker.lastError = String(error);
      }
      this.emit(scope, tracker);
      return false;
    }
    for (const job of Array.from(tracker.jobs.values())) {
      if (job.createdByEventSeq <= snapshot.snapshotThroughSeq) {
        this.removeJobAndBlockers(tracker, job.jobId, null, scope);
      }
    }
    tracker.readyThrough = readyTarget;
    const ready = this.computeReady(tracker);
    if (ready > tracker.readyThrough) {
      const advance = ledger.beginWrite();
      advance.applyProjectionChange(scope, { projectionReadyThroughSeq: ready });
      await advance.commit().catch(() => undefined);
      tracker.readyThrough = ready;
    }

    // Re-verify: leftover demand beyond the snapshot boundary is new work
    // the snapshot cannot speak to. Pending jobs keep draining; a failed
    // job beyond the boundary is divergence bounded retries and the
    // authoritative snapshot both failed to explain.
    const beyondBoundary = Array.from(tracker.jobs.values()).filter(
      (job) => job.createdByEventSeq > snapshot.snapshotThroughSeq,
    );
    if (beyondBoundary.some((job) => job.state === "failed")) {
      tracker.state = "sync_error";
      tracker.lastError = "hydration_diverged_after_snapshot_repair";
      this.emit(scope, tracker);
      return false;
    }
    tracker.state = beyondBoundary.length > 0 ? "draining" : "idle";
    this.emit(scope, tracker);
    return true;
  }

  /** Current status for one scope; `null` before the scope is known. */
  status(scope: LedgerScopeKey): LedgerIngestionStatus | null {
    const tracker = this.trackers.get(this.trackerKey(scope));
    return tracker ? this.statusFor(scope, tracker) : null;
  }

  /**
   * Read-marker gate (W5): the highest delivery seq a read state may claim,
   * plus the seq and reason readiness is currently blocked at.
   */
  readinessGate(
    scope: LedgerScopeKey,
  ): {
    readyThroughSeq: number;
    ingestedThroughSeq: number;
    observedHeadSeq?: number;
    blockedByEventSeq?: number;
    blockedReason?: "pending_hydration" | "unknown_envelope_version";
  } {
    const tracker = this.trackers.get(this.trackerKey(scope)) ?? this.freshTracker();
    const blockedSeq = Math.min(
      ...(tracker.blockers.size > 0 ? Array.from(tracker.blockers.keys()) : [Infinity]),
    );
    const blocker = Number.isFinite(blockedSeq) ? tracker.blockers.get(blockedSeq) : undefined;
    return {
      readyThroughSeq: tracker.readyThrough,
      ingestedThroughSeq: tracker.contiguousThrough,
      observedHeadSeq: Math.max(tracker.observedHead, tracker.contiguousThrough) || undefined,
      blockedByEventSeq: Number.isFinite(blockedSeq) ? blockedSeq : undefined,
      blockedReason: blocker?.kind,
    };
  }

  /** Explicitly rebuild a degraded handle and verify it before reuse. */
  async rebuildHandle(): Promise<boolean> {
    return this.ensureExactHandle();
  }

  private async ensureTracker(scope: LedgerScopeKey): Promise<ScopeTracker> {
    const key = this.trackerKey(scope);
    const existing = this.trackers.get(key);
    if (existing?.loaded) return existing;
    const tracker = this.freshTracker();
    this.trackers.set(key, tracker);
    if (!(await this.ensureExactHandle())) {
      tracker.state = "memory_only";
      return tracker;
    }
    const ledger = this.ledger!;
    const [session, jobs] = await Promise.all([
      ledger.getAgentSession(scope),
      ledger.getPendingHydrationJobs(scope),
    ]);
    tracker.contiguousThrough = session?.ingestedThroughSeq ?? 0;
    tracker.readyThrough = Math.min(
      session?.projectionReadyThroughSeq ?? 0,
      tracker.contiguousThrough,
    );
    // Older databases predate the agent-scoped head. A persisted contiguous
    // cursor is itself proof that this agent's observed head reached it.
    tracker.observedHead = Math.max(
      session?.observedHeadSeq ?? 0,
      tracker.contiguousThrough,
    );
    for (const job of jobs) {
      const view = this.jobViewFromRecord(job);
      tracker.jobs.set(view.jobId, view);
    }
    // Rebuild blockers for the open window only: everything at or below
    // readyThrough is satisfied by definition.
    if (tracker.contiguousThrough > tracker.readyThrough) {
      const window = await ledger.getRawEventsBetween(
        scope,
        tracker.readyThrough + 1,
        tracker.contiguousThrough,
      );
      for (const event of window) {
        this.registerBlocker(tracker, this.classifyStored(event));
      }
    }
    // Discover stored stragglers above the contiguous cursor so later
    // gap-filling ingests can count them without re-delivery.
    const stragglers = await ledger.getRawEventsBetween(
      scope,
      tracker.contiguousThrough + 1,
      tracker.contiguousThrough + RESUME_LOOKAHEAD_SEQ,
    );
    for (const event of stragglers) {
      if (event.eventSeq > tracker.contiguousThrough) {
        tracker.outOfOrder.add(event.eventSeq);
      }
    }
    // A prior tab may have satisfied hydration demand before this tracker
    // loaded. Reconcile readiness from the remaining durable blockers.
    const ready = this.computeReady(tracker);
    if (ready > tracker.readyThrough) {
      // Initialization has no later ingestion batch that could coalesce this repair.
      await ledger
        .beginWrite()
        .applyProjectionChange(scope, { projectionReadyThroughSeq: ready })
        .commit();
      tracker.readyThrough = ready;
    }
    tracker.loaded = true;
    return tracker;
  }

  private registerBlocker(tracker: ScopeTracker, item: ClassifiedEnvelope): void {
    if (item.blocksReadiness) {
      tracker.blockers.set(item.eventSeq, { kind: "unknown_envelope_version" });
      return;
    }
    if (item.classification.projectionEffect !== "display_invalidation") return;
    if (item.reference) {
      const jobId = jobIdFor(item.reference.recordKind, item.reference.recordId);
      // A job that is already gone was satisfied; only pending demand blocks.
      if (tracker.jobs.has(jobId)) {
        tracker.blockers.set(item.eventSeq, { kind: "pending_hydration", jobId });
      }
    }
    // Self-contained and tombstoned events never block.
  }

  private classifyStored(event: {
    eventSeq: number;
    envelope: unknown;
    classification: { projectionEffect: "none" | "display_invalidation"; envelopeContractVersion?: number };
  }): ReturnType<typeof classifyEnvelope> {
    return classifyEnvelope({
      ...(typeof event.envelope === "object" && event.envelope !== null
        ? (event.envelope as Record<string, unknown>)
        : {}),
      event_seq: event.eventSeq,
      projection_effect: event.classification.projectionEffect,
      contract_version: event.classification.envelopeContractVersion,
    });
  }

  private advanceContiguity(tracker: ScopeTracker, newSeqs: number[]): number {
    for (const seq of newSeqs) {
      if (seq > tracker.contiguousThrough) tracker.outOfOrder.add(seq);
    }
    let next = tracker.contiguousThrough + 1;
    while (tracker.outOfOrder.has(next)) {
      tracker.outOfOrder.delete(next);
      next += 1;
    }
    return next - 1;
  }

  private computeReady(tracker: ScopeTracker): number {
    let next = tracker.readyThrough + 1;
    while (next <= tracker.contiguousThrough && !tracker.blockers.has(next)) {
      next += 1;
    }
    return next - 1;
  }

  private removeJobAndBlockers(
    tracker: ScopeTracker,
    jobId: string,
    batch: EventLedgerWriteBatch | null,
    scope: LedgerScopeKey,
  ): void {
    tracker.jobs.delete(jobId);
    if (batch) batch.deleteHydrationJob(scope, jobId);
    for (const [seq, blocker] of tracker.blockers) {
      if (blocker.kind === "pending_hydration" && blocker.jobId === jobId) {
        tracker.blockers.delete(seq);
      }
    }
  }

  private jobRecordFromView(
    scope: LedgerScopeKey,
    job: HydrationJobView,
  ): LedgerHydrationJobRecord {
    return {
      ...scope,
      jobId: job.jobId,
      recordKind: job.recordKind,
      recordId: job.recordId,
      createdByEventSeq: job.createdByEventSeq,
      expectedRevision: job.expectedRevision,
      attemptCount: job.attemptCount,
      lastAttemptAt: job.lastAttemptAt,
      lastErrorKind: job.lastErrorKind,
      state: job.state,
      createdAt: job.createdAt,
    };
  }

  private jobViewFromRecord(record: LedgerHydrationJobRecord): HydrationJobView {
    return {
      jobId: record.jobId,
      recordKind: record.recordKind,
      recordId: record.recordId,
      createdByEventSeq: record.createdByEventSeq,
      expectedRevision: record.expectedRevision,
      attemptCount: record.attemptCount ?? 0,
      lastErrorKind: record.lastErrorKind,
      state: record.state === "failed" ? "failed" : "pending",
      createdAt: record.createdAt,
    };
  }

  private async ensureExactHandle(): Promise<boolean> {
    if (this.ledger?.durability === "exact") return true;
    // Never reuse a degraded handle (W1 review note): close it, open a new
    // one, and verify the new handle serves the same durable database.
    this.ledger?.close();
    const result = await (this.dependencies.openLedger?.() ?? EventLedger.open());
    if (result.kind !== "available") return false;
    this.ledger = result.ledger;
    try {
      await this.ledger.getMigrationMeta<{ metaKey: string }>("baseline");
      return true;
    } catch {
      this.ledger.close();
      this.ledger = null;
      return false;
    }
  }

  private isDurabilityFailure(error: unknown): boolean {
    const name = error instanceof Error ? error.name : "";
    return (
      name === "LedgerQuotaError" ||
      name === "LedgerTransactionAbortedError" ||
      name === "LedgerUnavailableError"
    );
  }

  private emit(scope: LedgerScopeKey, tracker: ScopeTracker): void {
    this.dependencies.onStatus?.(this.statusFor(scope, tracker));
  }

  private statusFor(scope: LedgerScopeKey, tracker: ScopeTracker): LedgerIngestionStatus {
    const blockedSeq = Math.min(
      ...(tracker.blockers.size > 0 ? Array.from(tracker.blockers.keys()) : [Infinity]),
    );
    const blocker = Number.isFinite(blockedSeq) ? tracker.blockers.get(blockedSeq) : undefined;
    let pendingJobs = 0;
    let failedJobs = 0;
    for (const job of tracker.jobs.values()) {
      if (job.state === "failed") failedJobs += 1;
      else pendingJobs += 1;
    }
    const durability: LedgerDurability = this.ledger?.durability ?? "memory_only";
    const state: LedgerIngestionState =
      durability !== "exact" ? "memory_only" : tracker.state;
    const observedHead = Math.max(
      tracker.observedHead,
      tracker.contiguousThrough,
    );
    return {
      scope,
      durability,
      state,
      ingestedThroughSeq: tracker.contiguousThrough || undefined,
      projectionReadyThroughSeq: tracker.readyThrough || undefined,
      observedEventHeadSeq: observedHead || undefined,
      pendingHydrationJobs: pendingJobs,
      failedHydrationJobs: failedJobs,
      blockedByEventSeq: Number.isFinite(blockedSeq) ? blockedSeq : undefined,
      blockedReason: blocker?.kind,
      lastError: tracker.lastError,
    };
  }

  private freshTracker(): ScopeTracker {
    return {
      contiguousThrough: 0,
      readyThrough: 0,
      observedHead: 0,
      outOfOrder: new Set(),
      blockers: new Map(),
      jobs: new Map(),
      state: "idle",
      draining: null,
      loaded: false,
    };
  }

  private trackerKey(scope: LedgerScopeKey): string {
    return [
      scope.remoteKey,
      scope.runtimeId,
      scope.visibilityScopeId,
      scope.eventLogEpoch,
      scope.agentId,
    ].join("\u0000");
  }
}
