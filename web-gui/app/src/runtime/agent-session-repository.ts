import {
  cursorNotFoundPayload,
  isTimeoutAbortError,
  isSnapshotAgentMissingError,
  type AgentProjectionSnapshotDto,
  type StreamEventEnvelopeDto,
} from "./client";
import { cacheClearRemoteSessions, cacheDeleteSession } from "./idb-cache";
import { currentRemoteKey, initSessionCache } from "./session-cache";
import {
  createRuntimeTrace,
  startRuntimeSpan,
  type RuntimeTraceContext,
} from "./runtime-trace";
import {
  AgentRecoveryCoordinator,
  type AgentRecoveryHint,
  type AgentRecoveryUpdate,
  type RecoveryProjectionSnapshot,
} from "./event-ledger/agent-recovery";
import {
  LedgerIngestionPipeline,
  type LedgerHydrationFetchers,
  type LedgerIngestionStatus,
  type LedgerReadStateRecord,
  type LedgerScopeKey,
  type LedgerUnreadSnapshot,
  type ProjectionSnapshotRepairSource,
  type ReadMarkerAdvanceResult,
} from "./event-ledger";
import type { AgentSessionState } from "./runtime-store-helpers";
import type {
  DisplayLevel,
  RuntimeBriefRecord,
  RuntimeConnectionConfig,
  RuntimeMessageEnvelope,
  RuntimeTranscriptEntry,
} from "./types";

const BRIEF_HYDRATION_RETRY_DELAYS_MS = [1_000, 2_000] as const;
const BRIEF_HYDRATION_MAX_ATTEMPTS = 5;

// Session catch-up closes at most this many ascending pages per run; the
// rest of the retained history stays behind the explicit load-older flow.
export const SESSION_CATCHUP_MAX_PAGES = 10;

export interface SessionCacheContext {
  remoteKey: string;
  generation: number;
}

export function isSessionCacheContextCurrent(
  context: SessionCacheContext,
  remoteKey: string,
  generation: number,
): boolean {
  return context.remoteKey === remoteKey && context.generation === generation;
}

export interface AgentSessionRepositoryState {
  route: string;
  selectedAgentId: string;
  globalStreamStatus: string;
  sessionsByAgentId: Record<string, AgentSessionState>;
  refreshAgentDetail: (
    agentId: string | undefined,
    options?: { force?: boolean; trace?: RuntimeTraceContext; trigger?: string },
  ) => Promise<void>;
  refreshAgentWorkItems: (agentId: string | undefined) => Promise<void>;
  refreshAgentState: (agentId: string | undefined) => Promise<void>;
}

/**
 * Durable ledger ingestion integration (W2). The repository owns the
 * pipeline lifecycle; scope resolution decides whether a remote already
 * exposes the stable runtime identity (runtime id + visibility scope) the
 * correctness key requires. Remotes without it return null and ingest
 * nothing — the in-memory path is unchanged until the W3/W4 cutover.
 */
export interface LedgerIngestionIntegration {
  /** Full ledger scope for one agent, or null while identity is unknown. */
  resolveScope: (agentId: string) => LedgerScopeKey | null;
  fetchers: LedgerHydrationFetchers;
  snapshotRepair?: ProjectionSnapshotRepairSource;
  onStatus?: (status: LedgerIngestionStatus) => void;
}

type StoreSet<State> = (
  partial: Partial<State> | State | ((state: State) => Partial<State> | State),
  replace?: false,
) => void;

interface RuntimeClientLike {
  getAgentEvents: (
    agentId: string,
    options: {
      afterSeq?: number;
      beforeSeq?: number;
      limit?: number;
      order?: "asc" | "desc";
      displayLevel?: DisplayLevel;
    },
  ) => Promise<{
    events?: StreamEventEnvelopeDto[];
    event_log_epoch?: string;
    cursor_seq?: number | null;
    newest_seq?: number | null;
    oldest_seq?: number | null;
    has_older?: boolean;
    has_newer?: boolean;
    responseBytes?: number;
  }>;
  getAgentProjectionSnapshot: (
    agentId: string,
  ) => Promise<AgentProjectionSnapshotDto | null>;
}

export interface AgentSessionRepositoryDependencies<State extends AgentSessionRepositoryState> {
  get: () => State;
  set: StoreSet<State>;
  getClient: () => RuntimeClientLike;
  getConnectionConfig: () => RuntimeConnectionConfig;
  getGeneration: () => number;
  ledgerIngestion?: LedgerIngestionIntegration;
}


export class AgentSessionRepository<State extends AgentSessionRepositoryState> {
  private cacheInitPromise: Promise<void> | null = null;

  private ledgerPipeline: LedgerIngestionPipeline | null = null;
  private ledgerInitPromise: Promise<void> | null = null;
  private recoveryCoordinator: AgentRecoveryCoordinator | null = null;
  /** agentId -> durable scope discovered by recovery or the restart scan. */
  private readonly recoveryScopeRegistry = new Map<string, LedgerScopeKey>();
  /** Agents whose ledger recovery was triggered this generation. */
  private readonly recoveryTriggered = new Set<string>();

  constructor(private readonly dependencies: AgentSessionRepositoryDependencies<State>) {}

  initializeCache(): void {
    if (this.cacheInitPromise) return;
    const context = this.currentCacheContext();
    // Durable ledger ingestion is independent of the legacy cache: start the
    // restart scan even when the legacy cache is unavailable.
    void this.initializeLedgerIngestion();
    const initialization = this.initializeCacheForContext(context);
    this.cacheInitPromise = initialization;
    void initialization.finally(() => {
      if (this.cacheInitPromise === initialization) this.cacheInitPromise = null;
    });
  }

  private async initializeCacheForContext(context: SessionCacheContext): Promise<void> {
    try {
      const ok = await initSessionCache();
      if (!this.cacheContextIsCurrent(context)) return;
      if (!ok) return;
      // Conversation content now flows from the conversation read model; the
      // legacy per-agent session cache is no longer read or written. Clear the
      // replaced content namespace for this remote while preserving the model
      // catalog cache and the durable event ledger.
      await cacheClearRemoteSessions(context.remoteKey);
    } catch (error) {
      console.warn("Failed to initialize the agent session cache.", error);
    }
  }

  async deleteCachedSession(agentId: string): Promise<void> {
    await cacheDeleteSession(
      currentRemoteKey(this.dependencies.getConnectionConfig()),
      agentId,
    );
  }

  switchRemote(): void {
    this.cacheInitPromise = null;
    this.ledgerPipeline?.dispose();
    this.ledgerPipeline = null;
    this.ledgerInitPromise = null;
    this.recoveryCoordinator = null;
    this.recoveryScopeRegistry.clear();
    this.recoveryTriggered.clear();
  }

  /**
   * Open the durable ingestion pipeline and run the restart scan for every
   * scope of the current remote: pending hydration resumes before any new
   * readiness claim. Idempotent; safe to call on every cache init.
   */
  initializeLedgerIngestion(): Promise<void> {
    if (!this.dependencies.ledgerIngestion) return Promise.resolve();
    if (this.ledgerInitPromise) return this.ledgerInitPromise;
    const integration = this.dependencies.ledgerIngestion;
    const pipeline = new LedgerIngestionPipeline({
      fetchers: integration.fetchers,
      snapshotRepair: integration.snapshotRepair,
      onStatus: (status) => {
        integration.onStatus?.(status);
        // Bounded escalation: a durable sync_error that survived the
        // pipeline's own snapshot repair re-bootstraps once via recovery.
        if (status.state === "sync_error") {
          this.recoveryCoordinator?.requestDivergenceReset(status.scope.agentId);
        }
      },
    });
    this.ledgerPipeline = pipeline;
    this.recoveryCoordinator = this.createRecoveryCoordinator(pipeline);
    this.ledgerInitPromise = (async () => {
      if (!(await pipeline.open())) return;
      await pipeline.resumeRemote(
        currentRemoteKey(this.dependencies.getConnectionConfig()),
      );
      await this.seedRecoveryScopes(pipeline);
    })().catch((error) => {
      console.warn("Failed to initialize the event ledger pipeline.", error);
    });
    return this.ledgerInitPromise;
  }

  private createRecoveryCoordinator(
    pipeline: LedgerIngestionPipeline,
  ): AgentRecoveryCoordinator {
    const remoteKey = currentRemoteKey(this.dependencies.getConnectionConfig());
    const client = this.dependencies.getClient();
    return new AgentRecoveryCoordinator({
      remoteKey,
      pipeline,
      fetchProjectionSnapshot: async (agentId) => {
        try {
          const dto = await client.getAgentProjectionSnapshot(agentId);
          return dto ? recoverySnapshotFromDto(dto) : null;
        } catch (error) {
          if (isSnapshotAgentMissingError(error)) {
            return null;
          }
          throw error;
        }
      },
      fetchEventPage: async (agentId, afterSeq, limit) => {
        try {
          const page = await client.getAgentEvents(agentId, { afterSeq, limit, order: "asc" });
          return {
            events: (page.events ?? []) as Array<Record<string, unknown>>,
            eventLogEpoch: page.event_log_epoch || undefined,
            eventHeadSeq: page.newest_seq ?? page.cursor_seq ?? undefined,
            oldestRetainedSeq: page.oldest_seq ?? null,
            hasNewer: page.has_newer,
            responseBytes: page.responseBytes,
          };
        } catch (error) {
          const cursorNotFound = cursorNotFoundPayload(error);
          if (cursorNotFound) return { events: [], cursorNotFound };
          throw error;
        }
      },
      onPhase: (update) => {
        if (update.scope && update.phase !== "idle") {
          this.recoveryScopeRegistry.set(update.agentId, update.scope);
        }
      },
    });
  }

  private async seedRecoveryScopes(pipeline: LedgerIngestionPipeline): Promise<void> {
    const remoteKey = currentRemoteKey(this.dependencies.getConnectionConfig());
    for (const scope of await pipeline.listKnownScopes(remoteKey)) {
      if (!this.recoveryScopeRegistry.has(scope.agentId)) {
        this.recoveryScopeRegistry.set(scope.agentId, scope);
      }
    }
  }

  /** Durable ledger scope for one agent, once recovery discovered it. */
  knownLedgerScope(agentId: string): LedgerScopeKey | null {
    return this.recoveryScopeRegistry.get(agentId) ?? null;
  }

  /** Bring one agent's durable ledger to live state (W3 recovery). */
  async syncAgentRecovery(
    agentId: string,
    hint: AgentRecoveryHint = {},
  ): Promise<AgentRecoveryUpdate | null> {
    await this.initializeLedgerIngestion();
    const coordinator = this.recoveryCoordinator;
    if (!coordinator) return null;
    this.recoveryTriggered.add(agentId);
    return coordinator.sync(agentId, hint);
  }

  /**
   * Clear every durable ledger scope of the current remote whose identity
   * (runtime, visibility scope, or epoch) no longer matches the
   * authoritative roster. Called when the roster reports an identity
   * change so old-scope data is never joined with the new scope.
   */
  async clearLedgerScopesNotMatching(identity: {
    runtimeId: string;
    visibilityScopeId: string;
    eventLogEpoch: string;
  }): Promise<void> {
    await this.initializeLedgerIngestion();
    const pipeline = this.ledgerPipeline;
    if (!pipeline) return;
    const remoteKey = currentRemoteKey(this.dependencies.getConnectionConfig());
    for (const scope of await pipeline.listKnownScopes(remoteKey)) {
      if (
        scope.runtimeId === identity.runtimeId &&
        scope.visibilityScopeId === identity.visibilityScopeId &&
        scope.eventLogEpoch === identity.eventLogEpoch
      ) {
        continue;
      }
      await pipeline.clearRuntimeScope({
        remoteKey: scope.remoteKey,
        runtimeId: scope.runtimeId,
        visibilityScopeId: scope.visibilityScopeId,
        eventLogEpoch: scope.eventLogEpoch,
      });
      const registeredScope = this.recoveryScopeRegistry.get(scope.agentId);
      if (
        registeredScope?.remoteKey === scope.remoteKey &&
        registeredScope.runtimeId === scope.runtimeId &&
        registeredScope.visibilityScopeId === scope.visibilityScopeId &&
        registeredScope.eventLogEpoch === scope.eventLogEpoch
      ) {
        this.recoveryScopeRegistry.delete(scope.agentId);
        this.recoveryTriggered.delete(scope.agentId);
      }
    }
  }

  /**
   * Purge one agent's durable ledger state and cache after the
   * authoritative roster omitted it (deletion or lost visibility).
   */
  async purgeAgentLedger(agentId: string): Promise<void> {
    await this.initializeLedgerIngestion();
    const pipeline = this.ledgerPipeline;
    if (!pipeline) return;
    const remoteKey = currentRemoteKey(this.dependencies.getConnectionConfig());
    const scopes = (await pipeline.listKnownScopes(remoteKey))
      .filter((scope) => scope.agentId === agentId);
    const cleared = new Set<string>();
    for (const scope of scopes) {
      const remoteScope = {
        remoteKey: scope.remoteKey,
        runtimeId: scope.runtimeId,
        visibilityScopeId: scope.visibilityScopeId,
        eventLogEpoch: scope.eventLogEpoch,
      };
      const key = [remoteScope.remoteKey, remoteScope.runtimeId, remoteScope.visibilityScopeId, remoteScope.eventLogEpoch].join("\u0000");
      if (cleared.has(key)) continue;
      cleared.add(key);
      await pipeline.clearRuntimeScope(remoteScope);
    }
    this.recoveryScopeRegistry.delete(agentId);
    this.recoveryTriggered.delete(agentId);
    await this.deleteCachedSession(agentId);
  }

  /**
   * Ingest raw envelopes for one agent into the durable ledger. Returns
   * null when ledger ingestion is unavailable or the agent's runtime
   * identity is not resolvable yet.
   */
  async ingestSessionEvents(
    agentId: string,
    events: StreamEventEnvelopeDto[],
  ): Promise<LedgerIngestionStatus | null> {
    const integration = this.dependencies.ledgerIngestion;
    if (!integration || !this.ledgerPipeline || events.length === 0) return null;
    const coordinator = this.recoveryCoordinator;
    if (coordinator?.scopeOf(agentId)) {
      // Live hints route through the recovery coordinator so envelopes that
      // arrive during a bootstrap or reset buffer instead of bypassing it.
      return coordinator.offer(agentId, events as Array<Record<string, unknown>>);
    }
    const scope = integration.resolveScope(agentId);
    if (!scope) return null;
    await this.initializeLedgerIngestion();
    return this.ledgerPipeline.ingest(
      scope,
      events as Array<Record<string, unknown>>,
    );
  }

  /** Current durable ingestion status for one agent, if tracked. */
  sessionLedgerStatus(agentId: string): LedgerIngestionStatus | null {
    const integration = this.dependencies.ledgerIngestion;
    if (!integration || !this.ledgerPipeline) return null;
    const scope = integration.resolveScope(agentId);
    return scope ? this.ledgerPipeline.status(scope) : null;
  }

  sessionLedgerResetReason(agentId: string): string | undefined {
    return this.recoveryCoordinator?.lastResetReasonOf(agentId);
  }

  /**
   * Read-marker gate: the highest delivery seq a read state may claim for
   * this agent without crossing unsatisfied display demand.
   */
  sessionLedgerReadiness(agentId: string): {
    readyThroughSeq: number;
    ingestedThroughSeq: number;
    observedHeadSeq?: number;
    blockedByEventSeq?: number;
    blockedReason?: "pending_hydration" | "unknown_envelope_version";
  } | null {
    const integration = this.dependencies.ledgerIngestion;
    if (!integration || !this.ledgerPipeline) return null;
    const scope = integration.resolveScope(agentId);
    return scope ? this.ledgerPipeline.readinessGate(scope) : null;
  }

  /**
   * Advance the browser-local read marker for one agent as a monotonic
   * maximum. Null when the ledger path is unavailable (no scope, no
   * pipeline, or memory-only durability).
   */
  async advanceReadMarker(
    agentId: string,
    candidateSeq: number,
  ): Promise<ReadMarkerAdvanceResult | null> {
    await this.initializeLedgerIngestion();
    const pipeline = this.ledgerPipeline;
    const scope =
      this.dependencies.ledgerIngestion?.resolveScope(agentId) ??
      this.knownLedgerScope(agentId);
    if (!pipeline || !scope) return null;
    return pipeline.advanceReadMarker(scope, candidateSeq);
  }

  /**
   * Record an explicit acknowledgement that truncated history is unknown.
   * Opens a new exact generation while preserving the recorded truncation
   * facts. Null when unavailable; false-y records when nothing changed. An
   * explicit `headSeq` acknowledges at the gated head a read marker caught
   * up to instead of the current observed head.
   */
  async acknowledgeReadTruncation(
    agentId: string,
    headSeq?: number,
  ): Promise<LedgerReadStateRecord | null> {
    await this.initializeLedgerIngestion();
    const pipeline = this.ledgerPipeline;
    const scope =
      this.dependencies.ledgerIngestion?.resolveScope(agentId) ??
      this.knownLedgerScope(agentId);
    if (!pipeline || !scope) return null;
    return pipeline.acknowledgeReadTruncation(scope, headSeq);
  }

  /**
   * Unread snapshot for one agent from the durable ledger: qualifying
   * user-facing brief events above the read boundary and below the
   * projection readiness cursor. Null when the ledger path is unavailable;
   * callers fall back to the legacy in-memory display.
   */
  async unreadSnapshot(agentId: string): Promise<LedgerUnreadSnapshot | null> {
    await this.initializeLedgerIngestion();
    const pipeline = this.ledgerPipeline;
    const scope =
      this.dependencies.ledgerIngestion?.resolveScope(agentId) ??
      this.knownLedgerScope(agentId);
    if (!pipeline || !scope) return null;
    return pipeline.unreadSnapshot(scope);
  }


  /**
   * Trigger durable ledger recovery for one agent at most once per
   * generation.
   */
  private scheduleLedgerRecovery(
    agentId: string,
    hint: AgentRecoveryHint,
  ): void {
    const coordinator = this.recoveryCoordinator;
    if (!coordinator) return;
    if (this.recoveryTriggered.has(agentId)) return;
    this.recoveryTriggered.add(agentId);
    void coordinator.sync(agentId, hint).catch(() => undefined);
  }

  private currentCacheContext(): SessionCacheContext {
    return {
      remoteKey: currentRemoteKey(this.dependencies.getConnectionConfig()),
      generation: this.dependencies.getGeneration(),
    };
  }

  private cacheContextIsCurrent(context: SessionCacheContext): boolean {
    const current = this.currentCacheContext();
    return isSessionCacheContextCurrent(
      context,
      current.remoteKey,
      current.generation,
    );
  }
}

function briefHydrationErrorKind(error: unknown): string {
  return isTimeoutAbortError(error) ? "timeout" : "request_failed";
}

/** Map an S5 projection snapshot DTO into the recovery-layer shape. */
export function recoverySnapshotFromDto(
  snapshot: AgentProjectionSnapshotDto,
): RecoveryProjectionSnapshot {
  const projection = snapshot.projection ?? {
    hydration_references: [],
    hydration_tombstones: [],
    latest_brief: null,
  };
  const brief = projection.latest_brief ?? null;
  return {
    runtimeId: snapshot.runtime_id,
    visibilityScopeId: snapshot.visibility_scope_id,
    eventLogEpoch: snapshot.event_log_epoch,
    snapshotThroughSeq: snapshot.snapshot_through_seq,
    eventHeadSeq: snapshot.event_head_seq,
    oldestRetainedSeq: snapshot.oldest_retained_seq,
    canonicalRecords: brief
      ? [
          {
            recordKind: "brief",
            recordId: brief.brief_id,
            record: brief,
            revision: brief.created_event_seq ?? undefined,
          },
        ]
      : [],
    hydrationReferences: (projection.hydration_references ?? []).map((key) => ({
      recordKind: key.record_kind,
      recordId: key.record_id,
    })),
    hydrationTombstones: (projection.hydration_tombstones ?? []).map((key) => ({
      recordKind: key.record_kind,
      recordId: key.record_id,
    })),
  };
}

/**
 * Snapshot repair source over the S5 endpoint. Capability-unavailable and
 * unknown-agent responses map to null: repair is explicitly absent.
 */
export function snapshotRepairFromClient(
  fetchSnapshot: (agentId: string) => Promise<AgentProjectionSnapshotDto | null>,
): ProjectionSnapshotRepairSource {
  return {
    fetchProjectionSnapshot: async (scope) => {
      try {
        const dto = await fetchSnapshot(scope.agentId);
        if (!dto) return null;
        const snapshot = recoverySnapshotFromDto(dto);
        return {
          snapshotThroughSeq: snapshot.snapshotThroughSeq,
          canonicalRecords: snapshot.canonicalRecords,
        };
      } catch (error) {
        if (isSnapshotAgentMissingError(error)) {
          return null;
        }
        throw error;
      }
    },
  };
}
