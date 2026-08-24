import {
  cursorNotFoundPayload,
  isSnapshotAgentMissingError,
  isSnapshotCapabilityUnavailableError,
  type AgentProjectionSnapshotDto,
  type StreamEventEnvelopeDto,
} from "./client";
import { cacheDeleteSession } from "./idb-cache";
import {
  currentRemoteKey,
  hydrateAgentSession,
  hydrateAllSessions,
  initSessionCache,
  SessionCacheWriter,
  type HydratedAgentCache,
  type HydratedRemoteCache,
} from "./session-cache";
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
  displayLevel: DisplayLevel;
  globalStreamStatus: string;
  sessionsByAgentId: Record<string, AgentSessionState>;
  refreshAgentDetail: (
    agentId: string | undefined,
    displayLevel: DisplayLevel,
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
  getAgentMessagesBatch: (
    agentId: string,
    ids: string[],
  ) => Promise<{ messages?: RuntimeMessageEnvelope[]; missing_message_ids?: string[] }>;
  getAgentTranscriptEntriesBatch: (
    agentId: string,
    ids: string[],
  ) => Promise<{ entries?: RuntimeTranscriptEntry[]; missing_entry_ids?: string[] }>;
  getAgentBriefsById: (
    agentId: string,
    ids: string[],
  ) => Promise<{ recordsById: Record<string, RuntimeBriefRecord>; notFoundIds: string[] }>;
}

export interface AgentSessionRepositoryDependencies<State extends AgentSessionRepositoryState> {
  get: () => State;
  set: StoreSet<State>;
  getClient: () => RuntimeClientLike;
  getConnectionConfig: () => RuntimeConnectionConfig;
  getGeneration: () => number;
  isCurrentGeneration: (generation: number) => boolean;
  mergeRemoteCache: (
    state: State,
    cached: HydratedRemoteCache,
    remoteKey: string,
  ) => { partial: Partial<State>; restoredAgentIds: string[] };
  mergeAgentCache: (
    state: State,
    agentId: string,
    cached: HydratedAgentCache | undefined,
    remoteKey: string,
  ) => Partial<State>;
  markCacheUnavailable: (state: State) => Partial<State>;
  mergeEventPage: (
    state: State,
    agentId: string,
    events: StreamEventEnvelopeDto[],
    oldestSeq: number | undefined,
    hasOlder: boolean | undefined,
    displayLevel: DisplayLevel,
    options?: {
      newestSeq?: number;
      append?: boolean;
      eventLogEpoch?: string;
      historyDisplayLevel?: DisplayLevel;
      historyLoading?: boolean;
    },
  ) => Partial<State>;
  mergeMessages: (
    state: State,
    agentId: string,
    messages: RuntimeMessageEnvelope[],
    missingIds: string[],
    displayLevel: DisplayLevel,
  ) => Partial<State>;
  mergeTranscripts: (
    state: State,
    agentId: string,
    entries: RuntimeTranscriptEntry[],
    missingIds: string[],
    displayLevel: DisplayLevel,
  ) => Partial<State>;
  mergeBriefs: (
    state: State,
    agentId: string,
    recordsById: Record<string, RuntimeBriefRecord>,
    missingIds: string[],
    displayLevel: DisplayLevel,
  ) => Partial<State>;
  markBriefHydrationStarted: (
    state: State,
    agentId: string,
    briefIds: string[],
    displayLevel: DisplayLevel,
  ) => Partial<State>;
  markBriefHydrationFailed: (
    state: State,
    agentId: string,
    briefIds: string[],
    errorKind: string,
    displayLevel: DisplayLevel,
  ) => Partial<State>;
  markHydrationError: (state: State, agentId: string, error: string) => Partial<State>;
  updateTargetEventState: (
    state: State,
    agentId: string,
    update: { loading: boolean; error?: string },
  ) => Partial<State>;
  missingMessageIds: (session: AgentSessionState | undefined) => string[];
  missingTranscriptIds: (session: AgentSessionState | undefined) => string[];
  missingBriefIds: (session: AgentSessionState | undefined) => string[];
  cachedReadState: (state: State, agentId: string) => Parameters<SessionCacheWriter["scheduleWrite"]>[2];
  rebaseRecovery: (agentId: string, session: AgentSessionState | undefined) => void;
  isWorkItemInvalidationEvent: (event: StreamEventEnvelopeDto) => boolean;
  isAgentStateInvalidationEvent: (event: StreamEventEnvelopeDto) => boolean;
  catchUpErrorKind: (error: unknown) => string;
  ledgerIngestion?: LedgerIngestionIntegration;
}

export class AgentSessionRepository<State extends AgentSessionRepositoryState> {
  private cacheWriter: SessionCacheWriter | null = null;
  private cacheInitPromise: Promise<void> | null = null;
  private readonly ensureInFlight = new Map<string, Promise<void>>();
  private readonly catchUpInFlight = new Map<string, Promise<void>>();
  private readonly messageHydrationInFlight = new Map<string, Set<string>>();
  private readonly transcriptHydrationInFlight = new Map<string, Set<string>>();
  private readonly briefHydrationInFlight = new Map<string, Set<string>>();
  private readonly briefHydrationRetryTimers = new Map<string, number>();

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
      if (!ok) {
        this.cacheWriter = null;
        this.dependencies.set((state) => this.dependencies.markCacheUnavailable(state));
        return;
      }

      this.cacheWriter?.cancel();
      const writer = new SessionCacheWriter(context.remoteKey);
      this.cacheWriter = writer;
      const cached = await hydrateAllSessions(context.remoteKey);
      if (!this.cacheContextIsCurrent(context) || this.cacheWriter !== writer) return;
      if (Object.keys(cached.sessionsByAgentId).length === 0) return;

      let restoredAgentIds: string[] = [];
      this.dependencies.set((state) => {
        const merged = this.dependencies.mergeRemoteCache(state, cached, context.remoteKey);
        restoredAgentIds = merged.restoredAgentIds;
        return merged.partial;
      });
      if (!this.cacheContextIsCurrent(context)) return;
      for (const agentId of restoredAgentIds) {
        this.dependencies.rebaseRecovery(
          agentId,
          this.dependencies.get().sessionsByAgentId[agentId],
        );
      }
      const state = this.dependencies.get();
      if (state.selectedAgentId && restoredAgentIds.includes(state.selectedAgentId)) {
        this.hydrateSelected(state.selectedAgentId, state.displayLevel);
      }
    } catch (error) {
      if (this.cacheContextIsCurrent(context)) this.cacheWriter = null;
      console.warn("Failed to initialize the agent session cache.", error);
    }
  }

  async restoreAgentCache(agentId: string): Promise<boolean> {
    if (this.cacheInitPromise) await this.cacheInitPromise;
    const context = this.currentCacheContext();
    const cached = await hydrateAgentSession(context.remoteKey, agentId);
    if (!this.cacheContextIsCurrent(context)) return false;
    this.dependencies.set((state) =>
      this.dependencies.mergeAgentCache(state, agentId, cached, context.remoteKey),
    );
    this.dependencies.rebaseRecovery(
      agentId,
      this.dependencies.get().sessionsByAgentId[agentId],
    );
    return Boolean(cached);
  }

  runEnsureOnce(agentId: string, operation: () => Promise<void>): Promise<void> {
    const existing = this.ensureInFlight.get(agentId);
    if (existing) return existing;
    let promise!: Promise<void>;
    promise = operation().finally(() => {
      if (this.ensureInFlight.get(agentId) === promise) this.ensureInFlight.delete(agentId);
    });
    this.ensureInFlight.set(agentId, promise);
    return promise;
  }

  scheduleCacheWrite(agentId: string): void {
    if (!this.cacheWriter) return;
    const state = this.dependencies.get();
    const session = state.sessionsByAgentId[agentId];
    if (!session) return;
    this.cacheWriter.scheduleWrite(
      agentId,
      session,
      this.dependencies.cachedReadState(state, agentId),
    );
  }

  async deleteCachedSession(agentId: string): Promise<void> {
    this.cacheWriter?.discard(agentId);
    await cacheDeleteSession(
      currentRemoteKey(this.dependencies.getConnectionConfig()),
      agentId,
    );
  }

  switchRemote(): void {
    void this.cacheWriter?.flush();
    this.cacheWriter = null;
    this.cacheInitPromise = null;
    this.ledgerPipeline?.dispose();
    this.ledgerPipeline = null;
    this.ledgerInitPromise = null;
    this.recoveryCoordinator = null;
    this.recoveryScopeRegistry.clear();
    this.recoveryTriggered.clear();
    this.cancelClientGenerationWork();
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
          if (
            isSnapshotCapabilityUnavailableError(error) ||
            isSnapshotAgentMissingError(error)
          ) {
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
    if (coordinator.capabilitySkipped(agentId)) return null;
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
      this.recoveryScopeRegistry.delete(scope.agentId);
      this.recoveryTriggered.delete(scope.agentId);
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

  /**
   * Read-marker gate: the highest delivery seq a read state may claim for
   * this agent without crossing unsatisfied display demand.
   */
  sessionLedgerReadiness(agentId: string): {
    readyThroughSeq: number;
    ingestedThroughSeq: number;
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
   * facts. Null when unavailable; false-y records when nothing changed.
   */
  async acknowledgeReadTruncation(
    agentId: string,
  ): Promise<LedgerReadStateRecord | null> {
    await this.initializeLedgerIngestion();
    const pipeline = this.ledgerPipeline;
    const scope =
      this.dependencies.ledgerIngestion?.resolveScope(agentId) ??
      this.knownLedgerScope(agentId);
    if (!pipeline || !scope) return null;
    return pipeline.acknowledgeReadTruncation(scope);
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

  cancelClientGenerationWork(): void {
    this.ensureInFlight.clear();
    this.catchUpInFlight.clear();
    this.messageHydrationInFlight.clear();
    this.transcriptHydrationInFlight.clear();
    this.briefHydrationInFlight.clear();
    for (const timer of this.briefHydrationRetryTimers.values()) {
      globalThis.clearTimeout(timer);
    }
    this.briefHydrationRetryTimers.clear();
  }

  hydrateSelected(agentId: string, displayLevel: DisplayLevel): void {
    const state = this.dependencies.get();
    if (state.route !== "agent" || state.selectedAgentId !== agentId) return;
    this.scheduleMessageHydration(agentId, displayLevel);
    this.scheduleTranscriptHydration(agentId, displayLevel);
    this.scheduleBriefHydration(agentId, displayLevel);
  }

  hydrateSession(agentId: string, displayLevel: DisplayLevel): void {
    this.scheduleMessageHydration(agentId, displayLevel);
    this.scheduleTranscriptHydration(agentId, displayLevel);
    this.scheduleBriefHydration(agentId, displayLevel);
  }

  hydrateSelectedContent(agentId: string, displayLevel: DisplayLevel): void {
    if (this.dependencies.get().selectedAgentId !== agentId) return;
    this.scheduleMessageHydration(agentId, displayLevel);
    this.scheduleTranscriptHydration(agentId, displayLevel);
  }

  hydrateBriefs(agentId: string, displayLevel: DisplayLevel): void {
    this.scheduleBriefHydration(agentId, displayLevel);
  }

  retryBriefHydration(
    agentId: string,
    briefId: string,
    displayLevel: DisplayLevel,
    trace: RuntimeTraceContext,
  ): void {
    this.scheduleBriefHydration(agentId, displayLevel, {
      forceIds: [briefId],
      trace,
      trigger: "manual",
    });
  }

  async loadTargetEventWindow(agentId: string, displayLevel: DisplayLevel): Promise<void> {
    const generation = this.dependencies.getGeneration();
    const session = this.dependencies.get().sessionsByAgentId[agentId];
    const targetEventSeq = session?.targetEventSeq;
    if (targetEventSeq == null || session?.eventsBySeq[targetEventSeq]) return;

    this.dependencies.set((state) =>
      this.dependencies.updateTargetEventState(state, agentId, { loading: true }),
    );

    try {
      const page = await this.dependencies.getClient().getAgentEvents(agentId, {
        afterSeq: targetEventSeq - 1,
        limit: 80,
        order: "asc",
        displayLevel,
      });
      if (!this.dependencies.isCurrentGeneration(generation)) return;
      this.dependencies.set((state) =>
        this.dependencies.mergeEventPage(
          state,
          agentId,
          page.events ?? [],
          page.oldest_seq ?? undefined,
          page.has_older,
          displayLevel,
          {
            newestSeq: page.cursor_seq ?? page.newest_seq ?? undefined,
            append: true,
            eventLogEpoch: page.event_log_epoch,
          },
        ),
      );
      this.dependencies.set((state) =>
        this.dependencies.updateTargetEventState(state, agentId, { loading: false }),
      );
    } catch (error) {
      if (!this.dependencies.isCurrentGeneration(generation)) return;
      this.dependencies.set((state) =>
        this.dependencies.updateTargetEventState(state, agentId, {
          loading: false,
          error: error instanceof Error ? error.message : String(error),
        }),
      );
    }
  }

  catchUpEvents(
    agentId: string,
    displayLevel: DisplayLevel,
    trace = createRuntimeTrace("events.catch_up", {
      agentId,
      trigger: "events.catch_up",
    }),
  ): Promise<void> {
    const existing = this.catchUpInFlight.get(agentId);
    if (existing) {
      startRuntimeSpan(trace, "events.catch_up").end("deduped");
      return existing;
    }
    const span = startRuntimeSpan(trace, "events.catch_up");
    const request = this.catchUpEventsInner(agentId, displayLevel, span)
      .catch((error) => {
        span.end("error", { errorKind: this.dependencies.catchUpErrorKind(error) });
        throw error;
      })
      .finally(() => {
        if (this.catchUpInFlight.get(agentId) === request) {
          this.catchUpInFlight.delete(agentId);
        }
      });
    this.catchUpInFlight.set(agentId, request);
    return request;
  }

  private async catchUpEventsInner(
    agentId: string,
    displayLevel: DisplayLevel,
    span: ReturnType<typeof startRuntimeSpan>,
  ): Promise<void> {
    const generation = this.dependencies.getGeneration();
    const session = this.dependencies.get().sessionsByAgentId[agentId];
    // When cached or streamed events leave a gap, newestSeq is ahead of the
    // contiguous range. Resume from the gap cursor so catch-up fills it.
    const gaps = session?.gaps ?? [];
    const initialAfterSeq = gaps.length > 0 ? gaps[0].afterSeq : session?.newestSeq;
    let eventCount = 0;
    let pageCount = 0;
    let refreshWorkItems = false;
    let refreshAgentState = false;
    const client = this.dependencies.getClient();

    const mergePage = (
      events: StreamEventEnvelopeDto[],
      oldestSeq: number | undefined,
      hasOlder: boolean | undefined,
      options: Parameters<AgentSessionRepositoryDependencies<State>["mergeEventPage"]>[6],
    ) => {
      this.dependencies.set((state) =>
        this.dependencies.mergeEventPage(
          state,
          agentId,
          events,
          oldestSeq,
          hasOlder,
          "debug",
          options,
        ),
      );
      eventCount += events.length;
      pageCount += 1;
      refreshWorkItems ||= events.some(this.dependencies.isWorkItemInvalidationEvent);
      refreshAgentState ||= events.some(this.dependencies.isAgentStateInvalidationEvent);
    };

    // Fetch the newest tail first so current activity appears immediately
    // while any older gap is filled in the background.
    const tailPage = await client.getAgentEvents(agentId, { limit: 100, order: "desc" });
    if (!this.dependencies.isCurrentGeneration(generation)) {
      span.end("cancelled");
      return;
    }
    const tailEvents = tailPage.events ?? [];
    const tailConsumedSeq =
      Math.max(...tailEvents.map((event) => event.event_seq ?? 0)) || undefined;
    const tailOldestSeq = tailPage.oldest_seq ?? undefined;
    const tailHasOlder = tailPage.has_older ?? false;
    mergePage(tailEvents, tailOldestSeq, tailHasOlder, {
      newestSeq: tailConsumedSeq,
      append: true,
      eventLogEpoch: tailPage.event_log_epoch,
    });
    // Opportunistic durable-ledger recovery: the catch-up page already
    // observed the head and floor, so bootstrap/catch-up can reuse them.
    this.scheduleLedgerRecovery(agentId, {
      eventHeadSeq: tailConsumedSeq,
      oldestRetainedSeq: tailOldestSeq,
    });
    if (this.dependencies.get().selectedAgentId === agentId) {
      this.hydrateSession(agentId, "debug");
    }

    // The unfiltered tail can be entirely debug-level for active agents.
    // Fetch a filtered tail so the visible timeline has meaningful content.
    if (displayLevel) {
      const displayTailPage = await client.getAgentEvents(agentId, {
        limit: 80,
        order: "desc",
        displayLevel,
      });
      if (!this.dependencies.isCurrentGeneration(generation)) {
        span.end("cancelled");
        return;
      }
      const displayTailEvents = displayTailPage.events ?? [];
      const consumedSeq =
        Math.max(...displayTailEvents.map((event) => event.event_seq ?? 0)) || undefined;
      mergePage(
        displayTailEvents,
        displayTailPage.oldest_seq ?? undefined,
        displayTailPage.has_older,
        {
          newestSeq: consumedSeq,
          append: true,
          eventLogEpoch: displayTailPage.event_log_epoch,
          historyDisplayLevel: displayLevel,
        },
      );
    }

    // Backfill ascending until the cursor overlaps the tail's oldest event.
    const hasGap =
      tailHasOlder &&
      tailOldestSeq != null &&
      (initialAfterSeq == null || tailOldestSeq > initialAfterSeq + 1);
    if (hasGap) {
      let afterSeq = initialAfterSeq;
      while (true) {
        const page = await client.getAgentEvents(agentId, {
          afterSeq,
          limit: 100,
          order: "asc",
        });
        if (!this.dependencies.isCurrentGeneration(generation)) {
          span.end("cancelled");
          return;
        }
        const events = page.events ?? [];
        const consumedSeq =
          Math.max(...events.map((event) => event.event_seq ?? 0)) || undefined;
        mergePage(events, page.oldest_seq ?? undefined, page.has_older, {
          newestSeq: consumedSeq,
          append: true,
          eventLogEpoch: page.event_log_epoch,
        });
        if (!page.has_newer) break;
        if (consumedSeq != null && tailOldestSeq != null && consumedSeq >= tailOldestSeq) break;
        if (consumedSeq == null || (afterSeq != null && consumedSeq <= afterSeq)) {
          throw new Error("Agent event catch-up page did not advance its consumed cursor.");
        }
        afterSeq = consumedSeq;
      }
    }

    if (refreshWorkItems) void this.dependencies.get().refreshAgentWorkItems(agentId);
    if (refreshAgentState) void this.dependencies.get().refreshAgentState(agentId);
    if (this.dependencies.get().selectedAgentId === agentId) {
      this.hydrateSession(agentId, "debug");
    }
    this.scheduleCacheWrite(agentId);
    span.end("ok", {
      afterSeq: initialAfterSeq,
      gapRecovery: gaps.length > 0,
      gapCount: gaps.length,
      eventCount,
      pageCount,
    });
  }

  /**
   * Trigger durable ledger recovery for one agent at most once per
   * generation, and only while the remote serves the snapshot contract.
   */
  private scheduleLedgerRecovery(
    agentId: string,
    hint: AgentRecoveryHint,
  ): void {
    const coordinator = this.recoveryCoordinator;
    if (!coordinator) return;
    if (this.recoveryTriggered.has(agentId)) return;
    if (coordinator.capabilitySkipped(agentId)) return;
    this.recoveryTriggered.add(agentId);
    void coordinator.sync(agentId, hint).catch(() => undefined);
  }

  private scheduleMessageHydration(agentId: string, displayLevel: DisplayLevel): void {
    const ids = this.dependencies.missingMessageIds(
      this.dependencies.get().sessionsByAgentId[agentId],
    );
    const requestIds = this.takeHydrationIds(this.messageHydrationInFlight, agentId, ids);
    if (!requestIds.length) return;
    const span = startRuntimeSpan(
      createRuntimeTrace("object.hydration", {
        agentId,
        trigger: "message.hydration",
      }),
      "object.hydration",
      { resource: "message", idCount: requestIds.length },
    );
    const generation = this.dependencies.getGeneration();
    void this.dependencies.getClient().getAgentMessagesBatch(agentId, requestIds)
      .then((response) => {
        if (!this.dependencies.isCurrentGeneration(generation)) return;
        this.dependencies.set((state) =>
          this.dependencies.mergeMessages(
            state,
            agentId,
            response.messages ?? [],
            response.missing_message_ids ?? [],
            displayLevel,
          ),
        );
        span.end("ok", { returnedCount: response.messages?.length ?? 0 });
      })
      .catch((error) => this.markHydrationError(generation, agentId, error))
      .finally(() => {
        this.releaseHydrationIds(
          this.messageHydrationInFlight,
          generation,
          agentId,
          requestIds,
        );
      });
  }

  private scheduleTranscriptHydration(agentId: string, displayLevel: DisplayLevel): void {
    const ids = this.dependencies.missingTranscriptIds(
      this.dependencies.get().sessionsByAgentId[agentId],
    );
    const requestIds = this.takeHydrationIds(
      this.transcriptHydrationInFlight,
      agentId,
      ids,
    );
    if (!requestIds.length) return;
    const span = startRuntimeSpan(
      createRuntimeTrace("object.hydration", {
        agentId,
        trigger: "transcript.hydration",
      }),
      "object.hydration",
      { resource: "transcript", idCount: requestIds.length },
    );
    const generation = this.dependencies.getGeneration();
    void this.dependencies.getClient().getAgentTranscriptEntriesBatch(agentId, requestIds)
      .then((response) => {
        if (!this.dependencies.isCurrentGeneration(generation)) return;
        this.dependencies.set((state) =>
          this.dependencies.mergeTranscripts(
            state,
            agentId,
            response.entries ?? [],
            response.missing_entry_ids ?? [],
            displayLevel,
          ),
        );
        span.end("ok", { returnedCount: response.entries?.length ?? 0 });
      })
      .catch((error) => this.markHydrationError(generation, agentId, error))
      .finally(() => {
        this.releaseHydrationIds(
          this.transcriptHydrationInFlight,
          generation,
          agentId,
          requestIds,
        );
      });
  }

  private scheduleBriefHydration(
    agentId: string,
    displayLevel: DisplayLevel,
    options: {
      forceIds?: string[];
      trace?: RuntimeTraceContext;
      trigger?: "automatic" | "manual" | "scheduled";
    } = {},
  ): void {
    const session = this.dependencies.get().sessionsByAgentId[agentId];
    const ids = options.forceIds ?? this.dependencies.missingBriefIds(session);
    const isForced = Boolean(options.forceIds);
    const eligible = ids.filter((briefId) =>
      isForced ||
      (session?.briefHydrationById[briefId]?.attempt ?? 0) < BRIEF_HYDRATION_MAX_ATTEMPTS
    );
    const requestIds = this.takeHydrationIds(
      this.briefHydrationInFlight,
      agentId,
      eligible,
    );
    if (!requestIds.length) return;
    this.dependencies.set((state) =>
      this.dependencies.markBriefHydrationStarted(
        state,
        agentId,
        requestIds,
        displayLevel,
      ),
    );
    const span = startRuntimeSpan(
      options.trace ?? createRuntimeTrace("object.hydration", {
        agentId,
        trigger: `brief.hydration.${options.trigger ?? "automatic"}`,
      }),
      "object.hydration",
      {
        resource: "brief",
        idCount: requestIds.length,
        retry: options.trigger ?? "automatic",
      },
    );
    const generation = this.dependencies.getGeneration();
    void this.dependencies.getClient().getAgentBriefsById(agentId, requestIds)
      .then(({ recordsById, notFoundIds }) => {
        if (!this.dependencies.isCurrentGeneration(generation)) return;
        this.dependencies.set((state) =>
          this.dependencies.mergeBriefs(
            state,
            agentId,
            recordsById,
            notFoundIds,
            displayLevel,
          ),
        );
        span.end("ok", {
          returnedCount: Object.keys(recordsById).length,
          notFoundCount: notFoundIds.length,
        });
      })
      .catch((error) => {
        if (!this.dependencies.isCurrentGeneration(generation)) return;
        const errorKind = briefHydrationErrorKind(error);
        this.dependencies.set((state) =>
          this.dependencies.markBriefHydrationFailed(
            state,
            agentId,
            requestIds,
            errorKind,
            displayLevel,
          ),
        );
        span.end("error", { errorKind });
        this.scheduleAutomaticBriefRetry(agentId, requestIds, displayLevel);
      })
      .finally(() => {
        this.releaseHydrationIds(
          this.briefHydrationInFlight,
          generation,
          agentId,
          requestIds,
        );
      });
  }

  private scheduleAutomaticBriefRetry(
    agentId: string,
    briefIds: string[],
    displayLevel: DisplayLevel,
  ): void {
    const retryIds = briefIds.filter((briefId) => {
      const attempt =
        this.dependencies.get().sessionsByAgentId[agentId]?.briefHydrationById[briefId]
          ?.attempt ?? 1;
      return attempt <= BRIEF_HYDRATION_RETRY_DELAYS_MS.length;
    });
    if (!retryIds.length) return;
    const key = `${agentId}:${retryIds.join(",")}`;
    if (this.briefHydrationRetryTimers.has(key)) return;
    const attempt = Math.max(...retryIds.map((briefId) =>
      this.dependencies.get().sessionsByAgentId[agentId]?.briefHydrationById[briefId]
        ?.attempt ?? 1
    ));
    const delay = BRIEF_HYDRATION_RETRY_DELAYS_MS[Math.max(0, attempt - 1)];
    const timer = globalThis.setTimeout(() => {
      this.briefHydrationRetryTimers.delete(key);
      this.scheduleBriefHydration(agentId, displayLevel, {
        forceIds: retryIds,
        trigger: "scheduled",
      });
    }, delay);
    this.briefHydrationRetryTimers.set(key, timer);
  }

  private takeHydrationIds(
    inFlightByAgent: Map<string, Set<string>>,
    agentId: string,
    ids: string[],
  ): string[] {
    let inFlight = inFlightByAgent.get(agentId);
    if (!inFlight) {
      inFlight = new Set<string>();
      inFlightByAgent.set(agentId, inFlight);
    }
    const requestIds = ids.filter((id) => !inFlight.has(id));
    requestIds.forEach((id) => inFlight.add(id));
    return requestIds;
  }

  private releaseHydrationIds(
    inFlightByAgent: Map<string, Set<string>>,
    generation: number,
    agentId: string,
    ids: string[],
  ): void {
    if (!this.dependencies.isCurrentGeneration(generation)) return;
    const current = inFlightByAgent.get(agentId);
    if (!current) return;
    ids.forEach((id) => current.delete(id));
    if (!current.size) inFlightByAgent.delete(agentId);
  }

  private markHydrationError(generation: number, agentId: string, error: unknown): void {
    if (!this.dependencies.isCurrentGeneration(generation)) return;
    this.dependencies.set((state) =>
      this.dependencies.markHydrationError(
        state,
        agentId,
        error instanceof Error ? error.message : String(error),
      ),
    );
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
  if (error instanceof DOMException && error.name === "AbortError") return "timeout";
  if (error instanceof Error && /timeout|aborted/i.test(error.message)) return "timeout";
  return "request_failed";
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
        if (
          isSnapshotCapabilityUnavailableError(error) ||
          isSnapshotAgentMissingError(error)
        ) {
          return null;
        }
        throw error;
      }
    },
  };
}
