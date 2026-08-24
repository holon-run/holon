/**
 * Per-Agent recovery state machine (W3).
 *
 * Owns the observer-sync recovery decisions above the ingestion pipeline:
 * - a newly visible Agent bootstraps from its authoritative projection
 *   snapshot: install, boundary cursors, fresh unread baseline, replay
 *   beyond the boundary, then live;
 * - an Agent with a contiguous cache catches up by replaying pages after
 *   its contiguous cursor until the observed head;
 * - reset reasons are independent and explicit: retained-prefix gap,
 *   replay-budget exhaustion, cursor error, immutable content conflict,
 *   hydration divergence, epoch change, and visibility scope change;
 * - same-scope resets rebuild the Agent cache and keep the read-state
 *   record, marking truncation; epoch and visibility resets clear the old
 *   runtime scope entirely and never migrate read markers;
 * - live stream envelopes that arrive while recovery is in flight are
 *   buffered hints: they replay through the same ingest path when
 *   recovery settles and never bypass the state machine.
 */

import type { LedgerIngestionPipeline, LedgerIngestionStatus } from "./ingestion-pipeline";
import type { LedgerReadStateRecord } from "./ledger";
import { LedgerIdentityConflictError } from "./errors";
import { remoteScopeKeyParts } from "./keys";
import type {
  LedgerRecordKind,
  LedgerRemoteScopeKey,
  LedgerScopeKey,
} from "./keys";

export type AgentRecoveryPhase =
  | "idle"
  | "fetching_snapshot"
  | "installing"
  | "replaying"
  | "live"
  | "error";

export type LedgerResetReason =
  | "retained_prefix_gap"
  | "replay_budget_exceeded"
  | "cursor_error"
  | "immutable_conflict"
  | "hydration_divergence"
  | "epoch_change"
  | "visibility_scope_change";

/** Reset reasons that rebuild the same scope key and keep the marker. */
const SAME_SCOPE_RESETS: ReadonlySet<LedgerResetReason> = new Set([
  "retained_prefix_gap",
  "replay_budget_exceeded",
  "cursor_error",
  "immutable_conflict",
  "hydration_divergence",
]);

export interface RecoveryProjectionSnapshot {
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  snapshotThroughSeq: number;
  eventHeadSeq: number;
  oldestRetainedSeq: number;
  canonicalRecords: Array<{
    recordKind: LedgerRecordKind;
    recordId: string;
    record: unknown;
    revision?: string | number;
  }>;
  hydrationReferences: Array<{
    recordKind: LedgerRecordKind;
    recordId: string;
    revision?: string | number;
  }>;
  hydrationTombstones: Array<{
    recordKind: LedgerRecordKind;
    recordId: string;
  }>;
}

/** Rich `cursor_not_found` payload from one event page request. */
export interface RecoveryCursorError {
  afterSeq: number;
  eventLogEpoch: string;
  oldestRetainedSeq: number;
  eventHeadSeq: number;
}

export interface RecoveryEventPage {
  events: Array<Record<string, unknown>>;
  eventLogEpoch?: string;
  eventHeadSeq?: number;
  oldestRetainedSeq?: number | null;
  hasNewer?: boolean;
  cursorNotFound?: RecoveryCursorError;
  /** Serialized response size when the transport can measure it exactly. */
  responseBytes?: number;
}

export interface AgentRecoveryUpdate {
  agentId: string;
  scope: LedgerScopeKey | null;
  phase: AgentRecoveryPhase;
  ingestedThroughSeq?: number;
  projectionReadyThroughSeq?: number;
  /** Set when this sync performed (or reacted to) a reset. */
  resetReason?: LedgerResetReason;
  /** Snapshot capability absent or agent unknown: nothing was installed. */
  skipped?: "capability_absent";
  error?: string;
}

export interface AgentRecoveryHint {
  eventHeadSeq?: number;
  oldestRetainedSeq?: number | null;
}

/** Client-owned limits for one incremental replay attempt. */
export interface AgentReplayBudget {
  /** Maximum sequence distance accepted before the first page request. */
  maxEstimatedGap: number;
  maxPages: number;
  maxEvents: number;
  maxBytes: number;
  maxElapsedMs: number;
}

export interface AgentRecoveryDependencies {
  remoteKey: string;
  pipeline: LedgerIngestionPipeline;
  /** Null when the remote does not advertise the snapshot capability. */
  fetchProjectionSnapshot(agentId: string): Promise<RecoveryProjectionSnapshot | null>;
  fetchEventPage(
    agentId: string,
    afterSeq: number,
    limit: number,
  ): Promise<RecoveryEventPage>;
  onPhase?: (update: AgentRecoveryUpdate) => void;
  replayPageSize?: number;
  replayBudget?: Partial<AgentReplayBudget>;
  /** Injectable monotonic clock for deterministic budget tests. */
  now?: () => number;
}

interface AgentRecoveryState {
  agentId: string;
  scope: LedgerScopeKey | null;
  phase: AgentRecoveryPhase;
  syncPromise: Promise<AgentRecoveryUpdate> | null;
  /** Live envelopes received while recovery was in flight. */
  pendingHints: Array<Record<string, unknown>>;
  lastResetReason?: LedgerResetReason;
  capabilitySkipped: boolean;
  error?: string;
}

const DEFAULT_REPLAY_PAGE_SIZE = 200;
const DEFAULT_REPLAY_BUDGET: Readonly<AgentReplayBudget> = {
  maxEstimatedGap: 10_000,
  maxPages: 50,
  maxEvents: 10_000,
  maxBytes: 16 * 1024 * 1024,
  maxElapsedMs: 30_000,
};
const MAX_STALE_REPLAY_ROUNDS = 3;

function serializedPageBytes(page: RecoveryEventPage): number {
  if (page.responseBytes != null) return Math.max(0, page.responseBytes);
  // Non-HTTP adapters may not know the raw body size; bound their mapped
  // payload with a best-effort UTF-8 estimate instead of skipping the limit.
  try {
    return new TextEncoder().encode(JSON.stringify(page)).byteLength;
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

/** Effective local read boundary above which unread is counted. */
function effectiveReadBoundary(record: LedgerReadStateRecord | undefined): number {
  if (!record) return 0;
  return Math.max(record.unreadBaselineSeq ?? 0, record.readThroughEventSeq ?? 0);
}

function sameRemoteScope(
  scope: LedgerScopeKey,
  remoteScope: LedgerRemoteScopeKey,
): boolean {
  return (
    scope.remoteKey === remoteScope.remoteKey &&
    scope.runtimeId === remoteScope.runtimeId &&
    scope.visibilityScopeId === remoteScope.visibilityScopeId &&
    scope.eventLogEpoch === remoteScope.eventLogEpoch
  );
}

/**
 * Recovery coordinator over one remote's ledger pipeline. One instance per
 * remote connection; dispose by dropping the reference together with the
 * pipeline it shares.
 */
export class AgentRecoveryCoordinator {
  private readonly states = new Map<string, AgentRecoveryState>();
  private readonly pageSize: number;
  private readonly replayBudget: Readonly<AgentReplayBudget>;
  private readonly now: () => number;

  constructor(private readonly dependencies: AgentRecoveryDependencies) {
    this.pageSize = dependencies.replayPageSize ?? DEFAULT_REPLAY_PAGE_SIZE;
    this.replayBudget = { ...DEFAULT_REPLAY_BUDGET, ...dependencies.replayBudget };
    this.now = dependencies.now ?? (() => performance.now());
  }

  /**
   * Bring one agent to live state: bootstrap when it has no durable cache,
   * otherwise catch up from the contiguous cursor. Concurrent calls join
   * the in-flight sync.
   */
  sync(
    agentId: string,
    hint: AgentRecoveryHint = {},
    options: { forceReset?: LedgerResetReason } = {},
  ): Promise<AgentRecoveryUpdate> {
    const state = this.stateFor(agentId);
    if (state.syncPromise) return state.syncPromise;
    const promise = this.syncInner(agentId, hint, options)
      .catch((error): AgentRecoveryUpdate => {
        state.phase = "error";
        state.error = error instanceof Error ? error.message : String(error);
        return this.emit(state, {
          agentId,
          scope: state.scope,
          phase: "error",
          error: state.error,
        });
      })
      .finally(() => {
        if (state.syncPromise === promise) state.syncPromise = null;
      });
    state.syncPromise = promise;
    return promise;
  }

  /**
   * Offer live stream envelopes. While recovery is in flight they are
   * buffered and replayed when recovery settles, so concurrent live events
   * are never lost and never bypass the state machine; duplicates remain
   * idempotent by immutable identity.
   */
  async offer(
    agentId: string,
    envelopes: Array<Record<string, unknown>>,
  ): Promise<LedgerIngestionStatus | null> {
    if (envelopes.length === 0) return null;
    const state = this.states.get(agentId);
    if (!state) return null;
    // Buffer whenever recovery owns the agent — including the snapshot
    // fetch, before the scope is even known, so nothing offered during a
    // bootstrap or reset is applied around the state machine.
    const recoveryOwnsAgent =
      state.syncPromise != null ||
      (state.phase !== "idle" && state.phase !== "live" && state.phase !== "error");
    if (recoveryOwnsAgent) {
      state.pendingHints.push(...envelopes);
      return null;
    }
    if (!state.scope) return null;
    return this.dependencies.pipeline.ingest(state.scope, envelopes);
  }

  phaseOf(agentId: string): AgentRecoveryPhase {
    return this.states.get(agentId)?.phase ?? "idle";
  }

  scopeOf(agentId: string): LedgerScopeKey | null {
    return this.states.get(agentId)?.scope ?? null;
  }

  /** True when the remote answered without the snapshot capability. */
  capabilitySkipped(agentId: string): boolean {
    return this.states.get(agentId)?.capabilitySkipped ?? false;
  }

  /**
   * Request a divergence reset: re-bootstrap from the authoritative
   * snapshot after the pipeline's own bounded repair escalated to
   * sync_error. Bounded once per live stretch so persistent divergence
   * surfaces as an error instead of looping.
   */
  requestDivergenceReset(agentId: string): boolean {
    const state = this.states.get(agentId);
    if (!state) return false;
    if (state.lastResetReason === "hydration_divergence") return false;
    if (state.syncPromise) return false;
    void this.sync(agentId, {}, { forceReset: "hydration_divergence" }).catch(
      () => undefined,
    );
    return true;
  }

  private async syncInner(
    agentId: string,
    hint: AgentRecoveryHint,
    options: { forceReset?: LedgerResetReason },
  ): Promise<AgentRecoveryUpdate> {
    const state = this.stateFor(agentId);
    state.error = undefined;
    // Cached-scope shortcut: a live agent answers its contiguous cursor
    // from the pipeline tracker without the durable session scan. The scan
    // still runs when the scope is unknown, may be replaced by a reset, or
    // the tracker cannot answer; identity drift surfaces through the same
    // page-metadata escalation paths during replay.
    if (state.scope && state.phase === "live" && options.forceReset == null) {
      const status = this.dependencies.pipeline.status(state.scope);
      if (status && status.ingestedThroughSeq != null) {
        const contiguous = status.ingestedThroughSeq;
        const floor = hint.oldestRetainedSeq;
        if (floor != null && floor > 1 && contiguous < floor - 1) {
          return this.bootstrap(agentId, hint, "retained_prefix_gap");
        }
        const targetHeadSeq = Math.max(hint.eventHeadSeq ?? 0, contiguous);
        if (targetHeadSeq - contiguous > this.replayBudget.maxEstimatedGap) {
          return this.bootstrap(agentId, hint, "replay_budget_exceeded");
        }
        return this.replay(agentId, state.scope, contiguous, {
          targetHeadSeq,
          oldestRetainedSeq: hint.oldestRetainedSeq,
        });
      }
    }
    const sessions = await this.dependencies.pipeline.findAgentSessions(
      this.dependencies.remoteKey,
      agentId,
    );
    if (sessions.length === 0 || options.forceReset) {
      return this.bootstrap(agentId, hint, options.forceReset);
    }

    // The newest local session is the current scope candidate; identity
    // drift (epoch/visibility) is detected from page metadata or snapshot.
    const current = sessions.reduce((a, b) =>
      (b.ingestedThroughSeq ?? 0) > (a.ingestedThroughSeq ?? 0) ? b : a,
    );
    const scope = current.scope;
    state.scope = scope;
    const contiguous = current.ingestedThroughSeq ?? 0;
    const floor = hint.oldestRetainedSeq;
    if (floor != null && floor > 1 && contiguous < floor - 1) {
      return this.bootstrap(agentId, hint, "retained_prefix_gap");
    }
    const targetHeadSeq = hint.eventHeadSeq ?? contiguous;
    if (targetHeadSeq - contiguous > this.replayBudget.maxEstimatedGap) {
      return this.bootstrap(agentId, hint, "replay_budget_exceeded");
    }
    return this.replay(agentId, scope, contiguous, {
      targetHeadSeq,
      oldestRetainedSeq: hint.oldestRetainedSeq,
    });
  }

  /** Bootstrap from the authoritative snapshot; `reset` names the cause. */
  private async bootstrap(
    agentId: string,
    hint: AgentRecoveryHint,
    reset: LedgerResetReason | undefined,
  ): Promise<AgentRecoveryUpdate> {
    const state = this.stateFor(agentId);
    state.phase = "fetching_snapshot";
    this.emit(state, { agentId, scope: state.scope, phase: "fetching_snapshot", resetReason: reset });

    const snapshot = await this.dependencies.fetchProjectionSnapshot(agentId);
    if (!snapshot) {
      state.capabilitySkipped = true;
      // Nothing was installed, so hints buffered during the declined fetch
      // belong to no durable scope and are dropped, not half-applied.
      state.pendingHints.length = 0;
      state.phase = "idle";
      return this.emit(state, {
        agentId,
        scope: state.scope,
        phase: "idle",
        skipped: "capability_absent",
      });
    }

    const scope: LedgerScopeKey = {
      remoteKey: this.dependencies.remoteKey,
      runtimeId: snapshot.runtimeId,
      visibilityScopeId: snapshot.visibilityScopeId,
      eventLogEpoch: snapshot.eventLogEpoch,
      agentId,
    };

    // Identity resets: clear whole runtime scopes that no longer match the
    // server's identity before any new data becomes visible. Read markers
    // never migrate across scopes, and buffered hints from the old scope
    // are dropped instead of joined with the new one.
    const stale = await this.dependencies.pipeline.findAgentSessions(
      this.dependencies.remoteKey,
      agentId,
    );
    let identityReset: LedgerResetReason | undefined;
    // One agent may hold sessions in several old scopes; each distinct
    // runtime scope is cleared exactly once.
    const clearedScopes = new Set<string>();
    for (const session of stale) {
      if (sameRemoteScope(session.scope, scope)) continue;
      const remoteScope: LedgerRemoteScopeKey = {
        remoteKey: session.scope.remoteKey,
        runtimeId: session.scope.runtimeId,
        visibilityScopeId: session.scope.visibilityScopeId,
        eventLogEpoch: session.scope.eventLogEpoch,
      };
      const scopeKey = remoteScopeKeyParts(remoteScope).join("\u0000");
      if (clearedScopes.has(scopeKey)) continue;
      clearedScopes.add(scopeKey);
      await this.dependencies.pipeline.clearRuntimeScope(remoteScope);
      identityReset =
        session.scope.runtimeId === scope.runtimeId &&
        session.scope.visibilityScopeId === scope.visibilityScopeId
          ? "epoch_change"
          : "visibility_scope_change";
      state.pendingHints.length = 0;
      this.emit(state, {
        agentId,
        scope,
        phase: "fetching_snapshot",
        resetReason: identityReset,
      });
    }

    const sameScopeReset =
      identityReset == null && reset != null && SAME_SCOPE_RESETS.has(reset);
    const reportedFloor =
      snapshot.oldestRetainedSeq ??
      (sameScopeReset ? hint.oldestRetainedSeq ?? null : null);
    const floor = reportedFloor != null && reportedFloor > 0 ? reportedFloor : undefined;
    const budgetReset = reset === "replay_budget_exceeded";
    const truncationBoundary = budgetReset ? snapshot.snapshotThroughSeq + 1 : floor;
    let readState: Partial<LedgerReadStateRecord> | undefined;
    if (sameScopeReset) {
      const preserved = await this.dependencies.pipeline.readStateOf(scope);
      if (!preserved) {
        // A forced reset that landed on a scope with no preserved marker
        // (for example a divergence reset that also rotated identity)
        // establishes a fresh baseline instead of a half-empty record.
        readState = {
          unreadBaselineSeq: snapshot.snapshotThroughSeq,
          certainty: budgetReset ? "truncated" : "exact",
          historyTruncatedBeforeSeq: truncationBoundary,
        };
      } else {
        const boundary = effectiveReadBoundary(preserved);
        const recordedTruncation = Math.max(
          preserved.historyTruncatedBeforeSeq ?? 0,
          truncationBoundary ?? 0,
        );
        readState = {
          historyTruncatedBeforeSeq: recordedTruncation || undefined,
          certainty:
            budgetReset || (floor != null && boundary < floor - 1)
              ? "truncated"
              : preserved.certainty ?? "exact",
        };
      }
    } else {
      // Fresh visible agent or identity reset: new scope, fresh baseline.
      readState = { unreadBaselineSeq: snapshot.snapshotThroughSeq, certainty: "exact" };
    }

    state.phase = "installing";
    state.scope = scope;
    this.emit(state, { agentId, scope, phase: "installing", resetReason: reset ?? identityReset });
    let installed: LedgerIngestionStatus;
    try {
      installed = await this.dependencies.pipeline.installProjectionSnapshot(
        scope,
        {
          snapshotThroughSeq: snapshot.snapshotThroughSeq,
          eventHeadSeq: snapshot.eventHeadSeq,
          canonicalRecords: snapshot.canonicalRecords,
          hydrationReferences: snapshot.hydrationReferences,
          hydrationTombstones: snapshot.hydrationTombstones,
        },
        {
          clearFirst:
            reset != null || identityReset != null
              ? { preserveReadState: sameScopeReset }
              : undefined,
          readState,
        },
      );
    } catch (error) {
      state.phase = "error";
      state.error = error instanceof Error ? error.message : String(error);
      return this.emit(state, {
        agentId,
        scope,
        phase: "error",
        error: state.error,
        resetReason: reset ?? identityReset,
      });
    }
    // An identity change is the structural cause and outranks whatever
    // same-scope reason triggered the bootstrap.
    const resetReason = identityReset ?? reset;
    if (resetReason != null) state.lastResetReason = resetReason;

    return this.replay(agentId, scope, snapshot.snapshotThroughSeq, {
      targetHeadSeq: Math.max(snapshot.eventHeadSeq, hint.eventHeadSeq ?? 0),
      oldestRetainedSeq: snapshot.oldestRetainedSeq,
      reset: resetReason,
      installed,
      bootstrapAttempted: true,
    });
  }

  /** Replay pages after the cursor until the target head, then go live. */
  private async replay(
    agentId: string,
    scope: LedgerScopeKey,
    afterSeq: number,
    context: {
      targetHeadSeq: number;
      oldestRetainedSeq?: number | null;
      reset?: LedgerResetReason;
      installed?: LedgerIngestionStatus;
      bootstrapAttempted?: boolean;
    },
  ): Promise<AgentRecoveryUpdate> {
    const state = this.stateFor(agentId);
    state.phase = "replaying";
    this.emit(state, { agentId, scope, phase: "replaying", resetReason: context.reset });

    let after = afterSeq;
    let pages = 0;
    let fetchedEvents = 0;
    let responseBytes = 0;
    let staleRounds = 0;
    let status: LedgerIngestionStatus | null = context.installed ?? null;
    const startedAt = this.now();
    while (after < context.targetHeadSeq) {
      if (
        pages >= this.replayBudget.maxPages ||
        this.now() - startedAt >= this.replayBudget.maxElapsedMs
      ) {
        return this.handleReplayBudgetExhaustion(state, agentId, scope, context);
      }
      const page = await this.dependencies.fetchEventPage(agentId, after, this.pageSize);
      pages += 1;
      fetchedEvents += page.events.length;
      responseBytes += serializedPageBytes(page);
      if (page.cursorNotFound) {
        if (context.reset == null) {
          return this.bootstrap(
            agentId,
            {
              eventHeadSeq: page.cursorNotFound.eventHeadSeq,
              oldestRetainedSeq: page.cursorNotFound.oldestRetainedSeq,
            },
            "cursor_error",
          );
        }
        return this.failReplay(state, agentId, scope, "cursor_not_found_after_reset", context.reset);
      }
      if (
        page.eventLogEpoch != null &&
        page.eventLogEpoch !== scope.eventLogEpoch &&
        context.reset !== "epoch_change"
      ) {
        return this.bootstrap(
          agentId,
          { eventHeadSeq: page.eventHeadSeq, oldestRetainedSeq: page.oldestRetainedSeq },
          "epoch_change",
        );
      }
      if (
        fetchedEvents > this.replayBudget.maxEvents ||
        responseBytes > this.replayBudget.maxBytes ||
        this.now() - startedAt >= this.replayBudget.maxElapsedMs
      ) {
        return this.handleReplayBudgetExhaustion(state, agentId, scope, {
          ...context,
          targetHeadSeq: Math.max(context.targetHeadSeq, page.eventHeadSeq ?? 0),
          oldestRetainedSeq: page.oldestRetainedSeq ?? context.oldestRetainedSeq,
        });
      }
      const events = page.events;
      try {
        status =
          events.length > 0
            ? await this.dependencies.pipeline.ingest(scope, events)
            : status;
      } catch (error) {
        // Immutable content for a stored (epoch, agent, seq) is a protocol
        // error: one bounded re-bootstrap from the authoritative snapshot;
        // a persistent conflict surfaces as an explicit error.
        if (error instanceof LedgerIdentityConflictError && context.reset == null) {
          return this.bootstrap(
            agentId,
            { eventHeadSeq: page.eventHeadSeq, oldestRetainedSeq: page.oldestRetainedSeq },
            "immutable_conflict",
          );
        }
        throw error;
      }
      const contiguous = status?.ingestedThroughSeq ?? after;
      if (events.length === 0) {
        // The committed target head cannot disappear. A page that claims
        // no newer events before that boundary is a protocol contradiction,
        // never permission to enter live with an incomplete cursor.
        if (page.hasNewer !== true) {
          return this.failReplay(
            state,
            agentId,
            scope,
            "replay_ended_before_target",
            context.reset,
          );
        }
        // Repeated empty pages that still claim newer events are spin: fail
        // early instead of consuming the full page budget.
        staleRounds += 1;
        if (staleRounds >= MAX_STALE_REPLAY_ROUNDS) {
          return this.failReplay(state, agentId, scope, "empty_replay_page_no_progress", context.reset);
        }
        continue;
      }
      if (events.length > 0 && contiguous <= after) {
        staleRounds += 1;
        if (staleRounds >= MAX_STALE_REPLAY_ROUNDS) {
          return this.failReplay(state, agentId, scope, "replay_page_did_not_advance", context.reset);
        }
      } else {
        staleRounds = 0;
      }
      after = Math.max(after, contiguous);
      if (
        after < context.targetHeadSeq &&
        this.now() - startedAt >= this.replayBudget.maxElapsedMs
      ) {
        return this.handleReplayBudgetExhaustion(state, agentId, scope, context);
      }
    }

    // Buffered live hints replay through the same ingest path before the
    // live transition, so concurrent activity is not lost and cannot
    // duplicate (immutable identity is idempotent).
    status = (await this.drainPendingHints(state, scope)) ?? status;

    state.phase = "live";
    state.lastResetReason = context.reset;
    const update = this.emit(state, {
      agentId,
      scope,
      phase: "live",
      ingestedThroughSeq: status?.ingestedThroughSeq,
      projectionReadyThroughSeq: status?.projectionReadyThroughSeq,
      resetReason: context.reset,
    });
    // Hints offered while the pre-live drain was awaiting keep buffering
    // until this sync promise settles. Drain once more after the live
    // transition so nothing strands between the transition and settle.
    const lateStatus = await this.drainPendingHints(state, scope);
    if (lateStatus) {
      update.ingestedThroughSeq = lateStatus.ingestedThroughSeq;
      update.projectionReadyThroughSeq = lateStatus.projectionReadyThroughSeq;
    }
    return update;
  }

  /** Bootstrap at most once after one replay attempt exhausts its budget. */
  private handleReplayBudgetExhaustion(
    state: AgentRecoveryState,
    agentId: string,
    scope: LedgerScopeKey,
    context: {
      targetHeadSeq: number;
      oldestRetainedSeq?: number | null;
      reset?: LedgerResetReason;
      bootstrapAttempted?: boolean;
    },
  ): Promise<AgentRecoveryUpdate> | AgentRecoveryUpdate {
    if (context.bootstrapAttempted) {
      return this.failReplay(
        state,
        agentId,
        scope,
        "replay_budget_exceeded_after_snapshot",
        context.reset ?? "replay_budget_exceeded",
      );
    }
    return this.bootstrap(
      agentId,
      {
        eventHeadSeq: context.targetHeadSeq,
        oldestRetainedSeq: context.oldestRetainedSeq,
      },
      "replay_budget_exceeded",
    );
  }

  /** Ingest buffered live hints until the buffer stays empty. */
  private async drainPendingHints(
    state: AgentRecoveryState,
    scope: LedgerScopeKey,
  ): Promise<LedgerIngestionStatus | null> {
    let status: LedgerIngestionStatus | null = null;
    let hints = state.pendingHints.splice(0);
    while (hints.length > 0) {
      status = await this.dependencies.pipeline.ingest(scope, hints);
      hints = state.pendingHints.splice(0);
    }
    return status;
  }

  private failReplay(
    state: AgentRecoveryState,
    agentId: string,
    scope: LedgerScopeKey,
    error: string,
    reset: LedgerResetReason | undefined,
  ): AgentRecoveryUpdate {
    state.phase = "error";
    state.error = error;
    return this.emit(state, {
      agentId,
      scope,
      phase: "error",
      error,
      resetReason: reset,
    });
  }

  private stateFor(agentId: string): AgentRecoveryState {
    const existing = this.states.get(agentId);
    if (existing) return existing;
    const state: AgentRecoveryState = {
      agentId,
      scope: null,
      phase: "idle",
      syncPromise: null,
      pendingHints: [],
      capabilitySkipped: false,
    };
    this.states.set(agentId, state);
    return state;
  }

  private emit(state: AgentRecoveryState, update: AgentRecoveryUpdate): AgentRecoveryUpdate {
    state.phase = update.phase;
    if (update.scope) state.scope = update.scope;
    this.dependencies.onPhase?.(update);
    return update;
  }
}
