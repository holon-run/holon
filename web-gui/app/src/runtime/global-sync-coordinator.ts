import {
  createRuntimeClient,
  type AgentEventStreamSubscription,
  type StreamEventEnvelopeDto,
} from "./client";
import {
  eventLogEpochFromEvents,
  shouldResetForEventLogEpoch,
} from "./conversation-store";
import { EventGapRecoveryTracker, recoverEventGap } from "./event-gap-recovery";
import {
  createRuntimeTrace,
  startRuntimeSpan,
} from "./runtime-trace";
import type {
  AgentLiveStatus,
  AgentSessionState,
} from "./runtime-store-helpers";

type RuntimeClient = ReturnType<typeof createRuntimeClient>;

export type GlobalSyncStoreState = {
  bootstrap: {
    agents: Array<{ id: string }>;
  };
  globalStreamStatus: "idle" | "connecting" | "catching_up" | "streaming" | "reconnecting";
  sessionsByAgentId: Record<string, AgentSessionState>;
};

export type GlobalSyncStoreSet<State extends GlobalSyncStoreState> = (
  partial:
    | Partial<State>
    | State
    | ((state: State) => Partial<State> | State),
  replace?: false,
) => void;

interface ClientRequest {
  client: RuntimeClient;
  generation: number;
}

interface GlobalSyncCoordinatorDependencies<State extends GlobalSyncStoreState> {
  applyStreamEvents: (
    set: GlobalSyncStoreSet<State>,
    agentId: string,
    events: StreamEventEnvelopeDto[],
  ) => void;
  captureClientRequest: () => ClientRequest;
  isCurrentClientRequest: (request: ClientRequest) => boolean;
  setAgentLiveStatus: (
    set: GlobalSyncStoreSet<State>,
    agentId: string,
    liveStatus: AgentLiveStatus,
  ) => void;
  setStreamState: (
    set: GlobalSyncStoreSet<State>,
    agentId: string,
    liveStatus: AgentLiveStatus,
    updates?: Partial<AgentSessionState>,
  ) => void;
}

const STREAM_FLUSH_INTERVAL_MS = 100;
const STREAM_RECONNECT_BASE_MS = 1_000;
const STREAM_RECONNECT_MAX_MS = 15_000;
const GLOBAL_STREAM_STALE_TIMEOUT_MS = 45_000;
const GLOBAL_BACKFILL_LIMIT = 100;
const GLOBAL_BACKFILL_MAX_PAGES = 10;
const GLOBAL_BACKFILL_CONCURRENCY = 4;

export class GlobalSyncCoordinator<State extends GlobalSyncStoreState> {
  private readonly pendingStreamEvents = new Map<string, StreamEventEnvelopeDto[]>();
  private readonly streamFlushTimers = new Map<string, number>();
  private globalEventStream: AgentEventStreamSubscription | undefined;
  private globalStreamReconnectTimer: number | undefined;
  private globalStreamStaleTimer: number | undefined;
  private globalStreamReconnectAttempt = 0;
  private readonly subscribedAgents = new Set<string>();
  private readonly catchUpPendingAgents = new Set<string>();
  private readonly recovery = new EventGapRecoveryTracker();
  private readonly backfillRetryTimers = new Map<string, number>();
  private readonly backfillRetryAttempts = new Map<string, number>();
  private readonly recoveryBaselineInFlight = new Map<string, Promise<void>>();

  constructor(private readonly dependencies: GlobalSyncCoordinatorDependencies<State>) {}

  start(get: () => State, set: GlobalSyncStoreSet<State>): void {
    if (this.globalEventStream) return;

    set({ globalStreamStatus: "connecting" } as Partial<State>);
    const trace = createRuntimeTrace("stream.connect", { trigger: "stream.connect" });
    const connectSpan = startRuntimeSpan(trace, "sse.connect");
    const request = this.dependencies.captureClientRequest();
    const subscription = request.client.streamGlobalEvents({
      onOpen: () => {
        if (!this.dependencies.isCurrentClientRequest(request)) return;
        this.globalStreamReconnectAttempt = 0;
        connectSpan.end("ok");
        this.scheduleStaleWatchdog(get, set);
        void Promise.resolve().then(() => this.catchUp(get, set, request));
      },
      onActivity: () => {
        if (!this.dependencies.isCurrentClientRequest(request)) return;
        this.scheduleStaleWatchdog(get, set);
      },
      onEvent: (event) => {
        if (!this.dependencies.isCurrentClientRequest(request)) return;
        this.scheduleStaleWatchdog(get, set);
        this.dispatch(get, set, event);
      },
      onClose: () => {
        if (this.dependencies.isCurrentClientRequest(request)) {
          this.scheduleReconnect(get, set, "global event stream closed");
        }
      },
      onError: (error) => {
        if (this.dependencies.isCurrentClientRequest(request)) {
          this.scheduleReconnect(get, set, error.message);
        }
      },
    });
    if (!subscription) return;
    this.globalEventStream = subscription;
  }

  stop(set: GlobalSyncStoreSet<State>): void {
    this.globalEventStream?.close();
    this.globalEventStream = undefined;
    if (this.globalStreamReconnectTimer != null) {
      window.clearTimeout(this.globalStreamReconnectTimer);
      this.globalStreamReconnectTimer = undefined;
    }
    if (this.globalStreamStaleTimer != null) {
      window.clearTimeout(this.globalStreamStaleTimer);
      this.globalStreamStaleTimer = undefined;
    }
    this.globalStreamReconnectAttempt = 0;
    this.catchUpPendingAgents.clear();
    for (const agentId of Array.from(this.backfillRetryTimers.keys())) {
      this.clearBackfillRetry(agentId);
    }
    set({ globalStreamStatus: "idle" } as Partial<State>);
    for (const agentId of this.subscribedAgents) {
      this.flushStreamEvents(set, agentId);
    }
  }

  register(get: () => State, set: GlobalSyncStoreSet<State>, agentId: string): void {
    const wasSubscribed = this.subscribedAgents.has(agentId);
    this.subscribedAgents.add(agentId);
    if (!wasSubscribed && !this.recovery.snapshotFor(agentId)) {
      const session = get().sessionsByAgentId[agentId];
      this.recovery.register(
        agentId,
        contiguousEventSeq(session),
        session?.eventLogEpoch,
        observedEventSeq(session),
      );
    }
    this.start(get, set);
    const globalStreamStatus = get().globalStreamStatus;
    if (
      !wasSubscribed
      && (globalStreamStatus === "streaming" || globalStreamStatus === "catching_up")
    ) {
      void this.catchUpRegisteredAgent(get, set, agentId);
    }
  }

  unregister(agentId: string): void {
    this.subscribedAgents.delete(agentId);
    this.catchUpPendingAgents.delete(agentId);
    this.recoveryBaselineInFlight.delete(agentId);
    this.clearBackfillRetry(agentId);
    this.recovery.unregister(agentId);
  }

  syncRoster(get: () => State, set: GlobalSyncStoreSet<State>): void {
    const agentIds = new Set(get().bootstrap.agents.map((agent) => agent.id));
    for (const agentId of Array.from(this.subscribedAgents)) {
      if (!agentIds.has(agentId)) this.unregister(agentId);
    }
    for (const agentId of agentIds) {
      this.register(get, set, agentId);
    }
  }

  retryAgentSync(get: () => State, set: GlobalSyncStoreSet<State>, agentId: string): void {
    if (!this.subscribedAgents.has(agentId)) {
      this.register(get, set, agentId);
      return;
    }
    this.clearBackfillRetry(agentId);
    this.dependencies.setStreamState(set, agentId, "recovering", {
      syncError: undefined,
      syncRetryAttempt: undefined,
      syncRetryAt: undefined,
    });
    if (!this.globalEventStream) {
      this.start(get, set);
      return;
    }
    this.catchUpPendingAgents.add(agentId);
    set({ globalStreamStatus: "catching_up" } as Partial<State>);
    void this.recoverRegisteredAgent(
      get,
      set,
      agentId,
      this.dependencies.captureClientRequest(),
    );
  }

  enqueueStreamEvent(
    set: GlobalSyncStoreSet<State>,
    agentId: string,
    event: StreamEventEnvelopeDto,
  ): void {
    const pending = this.pendingStreamEvents.get(agentId);
    const incomingEpoch = event.event_log_epoch || undefined;
    const pendingEpoch = pending ? eventLogEpochFromEvents(pending) : undefined;
    if (pending && incomingEpoch && pendingEpoch && incomingEpoch !== pendingEpoch) {
      this.pendingStreamEvents.set(agentId, [event]);
      return;
    }
    if (pending) {
      pending.push(event);
    } else {
      this.pendingStreamEvents.set(agentId, [event]);
    }

    if (this.streamFlushTimers.has(agentId)) return;
    const timer = window.setTimeout(() => {
      this.streamFlushTimers.delete(agentId);
      this.flushStreamEvents(set, agentId);
    }, STREAM_FLUSH_INTERVAL_MS);
    this.streamFlushTimers.set(agentId, timer);
  }

  flushStreamEvents(set: GlobalSyncStoreSet<State>, agentId: string): void {
    const events = this.pendingStreamEvents.get(agentId);
    if (!events?.length) return;
    this.pendingStreamEvents.delete(agentId);
    this.dependencies.applyStreamEvents(set, agentId, events);
  }

  stopBatchingAgent(set: GlobalSyncStoreSet<State>, agentId: string): void {
    this.flushStreamEvents(set, agentId);
    const timer = this.streamFlushTimers.get(agentId);
    if (timer != null) window.clearTimeout(timer);
    this.streamFlushTimers.delete(agentId);
  }

  discardPendingEvents(agentId: string): void {
    this.pendingStreamEvents.delete(agentId);
  }

  unregisterRecovery(agentId: string): void {
    this.recovery.unregister(agentId);
  }

  isRecovering(agentId: string): boolean {
    return Boolean(this.recovery.snapshotFor(agentId)?.recovering);
  }

  rebaseRecoveryFromSession(agentId: string, session: AgentSessionState | undefined): void {
    if (!this.subscribedAgents.has(agentId)) return;
    this.recoveryBaselineInFlight.delete(agentId);
    this.recovery.rebase(
      agentId,
      contiguousEventSeq(session),
      session?.eventLogEpoch,
      observedEventSeq(session),
    );
  }

  cancelClientGenerationWork(): void {
    for (const timer of this.backfillRetryTimers.values()) window.clearTimeout(timer);
    this.backfillRetryTimers.clear();
    this.backfillRetryAttempts.clear();
    this.recoveryBaselineInFlight.clear();
    this.catchUpPendingAgents.clear();
  }

  resetForClientChange(): void {
    this.cancelClientGenerationWork();
    this.globalEventStream?.close();
    this.globalEventStream = undefined;
    if (this.globalStreamReconnectTimer != null) {
      window.clearTimeout(this.globalStreamReconnectTimer);
      this.globalStreamReconnectTimer = undefined;
    }
    if (this.globalStreamStaleTimer != null) {
      window.clearTimeout(this.globalStreamStaleTimer);
      this.globalStreamStaleTimer = undefined;
    }
    for (const timer of this.streamFlushTimers.values()) window.clearTimeout(timer);
    this.streamFlushTimers.clear();
    this.pendingStreamEvents.clear();
    this.subscribedAgents.clear();
    this.recovery.clear();
    this.globalStreamReconnectAttempt = 0;
  }

  closeForResume(set: GlobalSyncStoreSet<State>): void {
    this.stop(set);
    for (const timer of this.streamFlushTimers.values()) window.clearTimeout(timer);
    this.streamFlushTimers.clear();
    this.pendingStreamEvents.clear();
    this.subscribedAgents.clear();
    this.recovery.clear();
  }

  private async catchUp(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    request: ClientRequest,
  ): Promise<void> {
    const agentIds = Array.from(this.subscribedAgents);
    this.catchUpPendingAgents.clear();
    agentIds.forEach((agentId) => this.catchUpPendingAgents.add(agentId));
    set({ globalStreamStatus: "catching_up" } as Partial<State>);
    await runWithConcurrencyLimit(
      agentIds,
      GLOBAL_BACKFILL_CONCURRENCY,
      async (agentId) => {
        if (!this.dependencies.isCurrentClientRequest(request) || !this.globalEventStream) return;
        await this.recoverRegisteredAgent(get, set, agentId, request);
      },
      () => this.dependencies.isCurrentClientRequest(request) && Boolean(this.globalEventStream),
    );
    if (
      this.dependencies.isCurrentClientRequest(request)
      && this.globalEventStream
      && this.catchUpPendingAgents.size === 0
    ) {
      set({ globalStreamStatus: "streaming" } as Partial<State>);
    }
  }

  private catchUpRegisteredAgent(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    agentId: string,
  ): Promise<void> {
    if (this.catchUpPendingAgents.has(agentId)) return Promise.resolve();
    const request = this.dependencies.captureClientRequest();
    this.catchUpPendingAgents.add(agentId);
    set({ globalStreamStatus: "catching_up" } as Partial<State>);
    return this.recoverRegisteredAgent(get, set, agentId, request);
  }

  private async recoverRegisteredAgent(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    agentId: string,
    request: ClientRequest,
  ): Promise<void> {
    try {
      await this.ensureRecoveryBaseline(get, set, agentId, request);
      if (!this.dependencies.isCurrentClientRequest(request) || !this.globalEventStream) return;
      const recovered = await this.backfillAgentEvents(get, set, agentId, request, true);
      if (recovered) this.completeCatchUp(set, agentId);
    } catch (error) {
      this.scheduleBackfillRetry(get, set, agentId, error);
    }
  }

  private completeCatchUp(set: GlobalSyncStoreSet<State>, agentId: string): void {
    this.catchUpPendingAgents.delete(agentId);
    if (this.globalEventStream && this.catchUpPendingAgents.size === 0) {
      set({ globalStreamStatus: "streaming" } as Partial<State>);
    }
  }

  private async ensureRecoveryBaseline(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    agentId: string,
    request: ClientRequest,
  ): Promise<void> {
    const session = get().sessionsByAgentId[agentId];
    if (session?.eventSeqs.length) {
      this.rebaseRecoveryFromSession(agentId, session);
      return;
    }
    const existing = this.recoveryBaselineInFlight.get(agentId);
    if (existing) return existing;
    let initialization!: Promise<void>;
    initialization = (async () => {
      const page = await request.client.getAgentEvents(agentId, {
        order: "desc",
        limit: GLOBAL_BACKFILL_LIMIT,
      });
      if (
        !this.dependencies.isCurrentClientRequest(request)
        || this.recoveryBaselineInFlight.get(agentId) !== initialization
        || !this.subscribedAgents.has(agentId)
      ) return;
      const events = (page.events ?? [])
        .filter((event) => event.event_seq != null)
        .map((event) => streamEventFromBackfill(event, agentId, page.event_log_epoch));
      const seqs = eventSeqs(events);
      const baselineSeq = seqs.length ? Math.max(0, seqs[0] - 1) : 0;
      this.recovery.rebase(agentId, baselineSeq, page.event_log_epoch);
      for (const seq of seqs) {
        this.recovery.observe(agentId, seq, page.event_log_epoch);
      }
      if (events.length) this.dependencies.applyStreamEvents(set, agentId, events);
    })().finally(() => {
      if (this.recoveryBaselineInFlight.get(agentId) === initialization) {
        this.recoveryBaselineInFlight.delete(agentId);
      }
    });
    this.recoveryBaselineInFlight.set(agentId, initialization);
    return initialization;
  }

  private dispatch(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    event: StreamEventEnvelopeDto,
  ): void {
    const agentId = event.agent_id;
    if (!agentId || !this.subscribedAgents.has(agentId)) return;

    const incomingEpoch = event.event_log_epoch || undefined;
    const session = get().sessionsByAgentId[agentId];
    if (session && shouldResetForEventLogEpoch(session, incomingEpoch)) {
      this.pendingStreamEvents.delete(agentId);
    }
    const seq = event.event_seq;
    if (seq != null) {
      const recovery = this.recovery.observe(agentId, seq, incomingEpoch);
      if (recovery.recovering) {
        this.dependencies.setAgentLiveStatus(set, agentId, "recovering");
        void this.backfillAgentEvents(
          get,
          set,
          agentId,
          this.dependencies.captureClientRequest(),
        );
      }
    }

    this.enqueueStreamEvent(set, agentId, event);
  }

  private async backfillAgentEvents(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    agentId: string,
    request: ClientRequest,
    force = false,
  ): Promise<boolean> {
    const span = startRuntimeSpan(
      createRuntimeTrace("stream.reconnect", { agentId, trigger: "events.backfill" }),
      "events.backfill",
      { force },
    );
    let eventCount = 0;
    try {
      const result = await recoverEventGap(this.recovery, agentId, {
        force,
        limit: GLOBAL_BACKFILL_LIMIT,
        maxPages: GLOBAL_BACKFILL_MAX_PAGES,
        fetchPage: async (afterSeq) => {
          const page = await request.client.getAgentEvents(agentId, {
            afterSeq,
            order: "asc",
            limit: GLOBAL_BACKFILL_LIMIT,
          });
          if (!this.dependencies.isCurrentClientRequest(request)) {
            return { eventLogEpoch: page.event_log_epoch, events: [] };
          }
          return {
            eventLogEpoch: page.event_log_epoch,
            events: (page.events ?? [])
              .filter((event) => event.event_seq != null)
              .map((event) => streamEventFromBackfill(event, agentId, page.event_log_epoch)),
          };
        },
        applyEvents: (events) => {
          eventCount += events.length;
          if (this.dependencies.isCurrentClientRequest(request)) {
            this.dependencies.applyStreamEvents(set, agentId, events);
          }
        },
      });
      if (!result.complete) {
        this.scheduleBackfillRetry(get, set, agentId);
        span.end("ok", { eventCount, incomplete: true });
        return false;
      }
      this.clearBackfillRetry(agentId);
      this.dependencies.setStreamState(set, agentId, "streaming", {
        syncError: undefined,
        syncRetryAttempt: undefined,
        syncRetryAt: undefined,
      });
      span.end("ok", { eventCount });
      return true;
    } catch (error) {
      span.end("error");
      this.scheduleBackfillRetry(get, set, agentId, error);
      return false;
    }
  }

  private scheduleBackfillRetry(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    agentId: string,
    error?: unknown,
  ): void {
    if (!this.subscribedAgents.has(agentId) || this.backfillRetryTimers.has(agentId)) return;
    const attempt = (this.backfillRetryAttempts.get(agentId) ?? 0) + 1;
    this.backfillRetryAttempts.set(agentId, attempt);
    const delay = backfillRetryDelayMs(attempt);
    this.dependencies.setStreamState(set, agentId, "recovering", {
      syncError: error == null ? undefined : error instanceof Error ? error.message : String(error),
      syncRetryAttempt: attempt,
      syncRetryAt: Date.now() + delay,
    });
    const timer = window.setTimeout(() => {
      this.backfillRetryTimers.delete(agentId);
      void this.recoverRegisteredAgent(
        get,
        set,
        agentId,
        this.dependencies.captureClientRequest(),
      );
    }, delay);
    this.backfillRetryTimers.set(agentId, timer);
  }

  private clearBackfillRetry(agentId: string): void {
    const timer = this.backfillRetryTimers.get(agentId);
    if (timer != null) window.clearTimeout(timer);
    this.backfillRetryTimers.delete(agentId);
    this.backfillRetryAttempts.delete(agentId);
  }

  private scheduleStaleWatchdog(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
  ): void {
    if (this.globalStreamStaleTimer != null) window.clearTimeout(this.globalStreamStaleTimer);
    this.globalStreamStaleTimer = window.setTimeout(() => {
      if (!this.globalEventStream) return;
      for (const agentId of this.subscribedAgents) {
        this.flushStreamEvents(set, agentId);
      }
      this.scheduleReconnect(get, set, "global event stream idle timeout");
    }, GLOBAL_STREAM_STALE_TIMEOUT_MS);
  }

  private scheduleReconnect(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    reason: string,
  ): void {
    this.globalEventStream?.close();
    this.globalEventStream = undefined;
    this.catchUpPendingAgents.clear();
    for (const agentId of Array.from(this.backfillRetryTimers.keys())) {
      this.clearBackfillRetry(agentId);
    }
    if (this.globalStreamStaleTimer != null) {
      window.clearTimeout(this.globalStreamStaleTimer);
      this.globalStreamStaleTimer = undefined;
    }
    if (this.globalStreamReconnectTimer != null) return;

    this.globalStreamReconnectAttempt += 1;
    set({ globalStreamStatus: "reconnecting" } as Partial<State>);
    const delay = reconnectDelayMs(this.globalStreamReconnectAttempt);
    for (const agentId of this.subscribedAgents) {
      this.dependencies.setStreamState(set, agentId, "reconnecting", {
        reconnectAttempt: this.globalStreamReconnectAttempt,
        error: reason,
      });
    }
    this.globalStreamReconnectTimer = window.setTimeout(() => {
      this.globalStreamReconnectTimer = undefined;
      this.start(get, set);
    }, delay);
  }
}

export async function runWithConcurrencyLimit<T>(
  values: readonly T[],
  limit: number,
  run: (value: T) => Promise<void>,
  shouldContinue: () => boolean = () => true,
): Promise<void> {
  const workerCount = Math.min(values.length, Math.max(1, Math.floor(limit)));
  let nextIndex = 0;
  const worker = async () => {
    while (shouldContinue()) {
      const index = nextIndex;
      nextIndex += 1;
      if (index >= values.length) return;
      await run(values[index]);
    }
  };
  await Promise.all(Array.from({ length: workerCount }, worker));
}

export function backfillRetryDelayMs(attempt: number): number {
  return Math.min(
    STREAM_RECONNECT_MAX_MS,
    STREAM_RECONNECT_BASE_MS * 2 ** Math.max(0, attempt - 1),
  );
}

export function streamEventFromBackfill(
  event: StreamEventEnvelopeDto,
  agentId: string,
  pageEventLogEpoch: string,
): StreamEventEnvelopeDto {
  return {
    ...event,
    event_log_epoch: event.event_log_epoch || pageEventLogEpoch,
    agent_id: agentId,
  };
}

function contiguousEventSeq(session: AgentSessionState | undefined): number {
  if (!session) return 0;
  return session.gaps[0]?.afterSeq ?? highestSeq(session.eventSeqs) ?? session.newestSeq ?? 0;
}

function observedEventSeq(session: AgentSessionState | undefined): number {
  return session ? highestSeq(session.eventSeqs) ?? session.newestSeq ?? 0 : 0;
}

function eventSeqs(events: StreamEventEnvelopeDto[]): number[] {
  return events
    .map((event) => event.event_seq)
    .filter((seq): seq is number => seq != null)
    .sort((left, right) => left - right);
}

function highestSeq(eventSeqs: number[]): number | undefined {
  return eventSeqs.length ? eventSeqs[eventSeqs.length - 1] : undefined;
}

function reconnectDelayMs(attempt: number): number {
  const exponential = Math.min(
    STREAM_RECONNECT_MAX_MS,
    STREAM_RECONNECT_BASE_MS * 2 ** Math.max(0, attempt - 1),
  );
  const jitter = Math.floor(Math.random() * 500);
  return exponential + jitter;
}
