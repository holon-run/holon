import type { StreamEventEnvelopeDto } from "./client";
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
  }>;
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
  missingMessageIds: (session: AgentSessionState | undefined) => string[];
  missingTranscriptIds: (session: AgentSessionState | undefined) => string[];
  missingBriefIds: (session: AgentSessionState | undefined) => string[];
  cachedReadState: (state: State, agentId: string) => Parameters<SessionCacheWriter["scheduleWrite"]>[2];
  rebaseRecovery: (agentId: string, session: AgentSessionState | undefined) => void;
  isWorkItemInvalidationEvent: (event: StreamEventEnvelopeDto) => boolean;
  isAgentStateInvalidationEvent: (event: StreamEventEnvelopeDto) => boolean;
  catchUpErrorKind: (error: unknown) => string;
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

  constructor(private readonly dependencies: AgentSessionRepositoryDependencies<State>) {}

  initializeCache(): void {
    if (this.cacheInitPromise) return;
    const context = this.currentCacheContext();
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
    } catch {
      if (this.cacheContextIsCurrent(context)) this.cacheWriter = null;
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
    this.cancelClientGenerationWork();
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

    this.dependencies.set((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...state.sessionsByAgentId[agentId],
          targetEventLoading: true,
          targetEventError: undefined,
        },
      },
    } as Partial<State>));

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
      this.dependencies.set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...state.sessionsByAgentId[agentId],
            targetEventLoading: false,
            targetEventError: undefined,
          },
        },
      } as Partial<State>));
    } catch (error) {
      if (!this.dependencies.isCurrentGeneration(generation)) return;
      this.dependencies.set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...state.sessionsByAgentId[agentId],
            targetEventLoading: false,
            targetEventError: error instanceof Error ? error.message : String(error),
          },
        },
      } as Partial<State>));
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
    if (this.dependencies.get().selectedAgentId === agentId) {
      this.hydrateSession(agentId, "debug");
    }

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
