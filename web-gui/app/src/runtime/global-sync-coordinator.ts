import {
  createRuntimeClient,
  httpRetryAfterMs,
  isAuthRequiredError,
  type AgentRosterSnapshotDto,
  type AgentRosterSnapshotResult,
} from "./client";
import {
  createRuntimeTrace,
  startRuntimeSpan,
} from "./runtime-trace";
import type {
  RosterDiscoveryIdentity,
  RosterDiscoveryState,
} from "./types";

type RuntimeClient = ReturnType<typeof createRuntimeClient>;

export type GlobalSyncStoreState = {
  bootstrap: {
    agents: Array<{ id: string }>;
    capabilities?: string[];
  };
  discovery: RosterDiscoveryState;
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
  captureClientRequest: () => ClientRequest;
  isCurrentClientRequest: (request: ClientRequest) => boolean;
  /** Fetch the authoritative roster snapshot from the embedded daemon. */
  fetchRosterSnapshot: (request: ClientRequest) => Promise<AgentRosterSnapshotResult>;
  /**
   * Atomically apply one complete roster snapshot: validate identity,
   * replace the roster, reset stale-scope sessions, and purge omitted
   * agents. Returns the ids of agents that were present before and are
   * omitted now.
   */
  applyRosterSnapshot: (
    set: GlobalSyncStoreSet<State>,
    snapshot: AgentRosterSnapshotDto,
    context: { previousIdentity?: RosterDiscoveryIdentity; request: ClientRequest },
  ) => string[] | Promise<string[]>;
}

const ROSTER_REFRESH_INTERVAL_MS = 1_000;

/**
 * Once transient roster retries pass this attempt count (backoff fully
 * saturated), the dashboard escalates the stale banner so an extended
 * outage stays visible to the operator.
 */
export const ROSTER_STALE_EXTENDED_RETRY_ATTEMPTS = 6;

/**
 * Owns the authoritative roster HTTP discovery cycle (#3808 slice 2): the
 * dormant global/agent SSE transports and their in-memory per-agent event
 * catch-up were removed; the conversation read model plus roster snapshots carry
 * all live data. No raw event replay is registered from roster discovery.
 */
export class GlobalSyncCoordinator<State extends GlobalSyncStoreState> {
  /** Authoritative discovery cycle: in-flight snapshot + coalescing. */
  private rosterRefreshPromise: Promise<void> | undefined;
  private rosterDirty = false;
  private rosterQueuedRefresh: { promise: Promise<void>; resolve: () => void } | undefined;
  private rosterIdentity: RosterDiscoveryIdentity | undefined;
  /** One timer covers both the completion-based cadence and server backoff. */
  private rosterRefreshTimer: number | undefined;
  private rosterRefreshNotBefore = 0;
  private rosterRetryAt = 0;
  private rosterRetryAttempt = 0;
  private rosterAuthRejected = false;
  private rosterGeneration = 0;

  constructor(private readonly dependencies: GlobalSyncCoordinatorDependencies<State>) {}

  start(get: () => State, set: GlobalSyncStoreSet<State>): void {
    void this.beginDiscovery(get, set, this.dependencies.captureClientRequest());
  }

  stop(): void {
    this.cancelRosterRefresh();
  }

  /**
   * Request one authoritative roster refresh outside the discovery cycle,
   * for example after a local create/delete/rename.
   */
  refreshRoster(get: () => State, set: GlobalSyncStoreSet<State>): void {
    void this.refreshRosterInner(
      get,
      set,
      this.dependencies.captureClientRequest(),
      "local_change",
    );
  }

  /**
   * Operator-driven sync retry forces one authoritative roster snapshot.
   */
  retryAgentSync(get: () => State, set: GlobalSyncStoreSet<State>, _agentId: string): void {
    this.refreshRosterInner(
      get,
      set,
      this.dependencies.captureClientRequest(),
      "agent_retry",
    ).catch(() => undefined);
  }

  /**
   * Discovery state machine (W4): request the authoritative roster, apply
   * it atomically.
   */
  private async beginDiscovery(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    request: ClientRequest,
  ): Promise<void> {
    await this.refreshRosterInner(get, set, request, "discovery_start");
  }

  private refreshRosterInner(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    request: ClientRequest,
    trigger: string,
  ): Promise<void> {
    if (
      !this.dependencies.isCurrentClientRequest(request)
      || this.rosterAuthRejected
    ) return Promise.resolve();
    this.rosterDirty = true;
    if (!this.rosterQueuedRefresh) {
      let resolve!: () => void;
      const promise = new Promise<void>((done) => { resolve = done; });
      this.rosterQueuedRefresh = { promise, resolve };
    }
    const pending = this.rosterQueuedRefresh.promise;
    this.scheduleRosterRefresh(get, set, request, trigger);
    return pending;
  }

  private scheduleRosterRefresh(
    get: () => State,
    set: GlobalSyncStoreSet<State>,
    request: ClientRequest,
    trigger: string,
  ): void {
    if (
      this.rosterRefreshPromise
      || this.rosterRefreshTimer != null
      || !this.rosterDirty
      || this.rosterAuthRejected
      || !this.dependencies.isCurrentClientRequest(request)
    ) return;
    const generation = this.rosterGeneration;
    const isCurrent = () => generation === this.rosterGeneration
      && this.dependencies.isCurrentClientRequest(request)
      && !this.rosterAuthRejected;
    const delay = Math.max(this.rosterRefreshNotBefore, this.rosterRetryAt) - Date.now();
    if (delay > 0) {
      // Local changes never move this deadline: continuous traffic cannot
      // debounce discovery forever, nor undercut a Retry-After deadline.
      this.rosterRefreshTimer = window.setTimeout(() => {
        this.rosterRefreshTimer = undefined;
        if (isCurrent()) this.scheduleRosterRefresh(get, set, request, trigger);
      }, delay);
      return;
    }
    this.rosterDirty = false;
    const queued = this.rosterQueuedRefresh;
    this.rosterQueuedRefresh = undefined;
    const span = startRuntimeSpan(
      createRuntimeTrace("roster.refresh", { trigger }),
      "roster.snapshot",
    );
    const cycle = (async () => {
      const result = await this.dependencies.fetchRosterSnapshot(request);
      if (!isCurrent()) return;
      const { snapshot, projectionStale } = result;
      const identity: RosterDiscoveryIdentity = {
        runtimeId: snapshot.runtime_id,
        visibilityScopeId: snapshot.visibility_scope_id,
        eventLogEpoch: snapshot.event_log_epoch,
      };
      const omittedAgentIds = await this.dependencies.applyRosterSnapshot(
        set,
        snapshot,
        { previousIdentity: this.rosterIdentity, request },
      );
      if (!isCurrent()) return;
      this.rosterIdentity = identity;
      span.end("ok", { agentCount: snapshot.agents.length, omitted: omittedAgentIds.length });
      if (projectionStale) {
        // The runtime served its last good projection marked stale (via
        // x-holon-projection-stale). The applied roster stays usable, but
        // discovery must not present it as fresh authoritative data; keep
        // the stale state visible and retry with bounded backoff until a
        // fresh snapshot settles.
        this.rosterRetryAttempt += 1;
        const delay = retryDelayWithServerHintMs(this.rosterRetryAttempt);
        this.rosterRetryAt = Date.now() + delay;
        this.rosterDirty = true;
        set((state) => ({
          discovery: {
            ...state.discovery,
            mode: "authoritative",
            freshness: "stale",
            staleReason: "runtime served a stale roster projection",
            unauthorizedReason: undefined,
            retryAttempt: this.rosterRetryAttempt,
            retryAt: Date.now() + delay,
          },
        } as Partial<State>));
        return;
      }
      this.rosterRetryAt = 0;
      this.rosterRetryAttempt = 0;
      set((state) => ({
        discovery: {
          ...state.discovery,
          mode: "authoritative",
          freshness: "fresh",
          identity: this.rosterIdentity,
          staleReason: undefined,
          unauthorizedReason: undefined,
          retryAttempt: 0,
          retryAt: undefined,
        },
      } as Partial<State>));
    })().catch((error: unknown) => {
      if (!isCurrent()) return;
      span.end("error");
      this.handleRosterSnapshotFailure(set, error);
    });
    const tracked = cycle.finally(() => {
      queued?.resolve();
      if (generation !== this.rosterGeneration || this.rosterRefreshPromise !== tracked) return;
      this.rosterRefreshPromise = undefined;
      this.rosterRefreshNotBefore = Date.now() + ROSTER_REFRESH_INTERVAL_MS;
      if (isCurrent()) this.scheduleRosterRefresh(get, set, request, "roster_trailing");
    });
    this.rosterRefreshPromise = tracked;
  }

  private handleRosterSnapshotFailure(
    set: GlobalSyncStoreSet<State>,
    error: unknown,
  ): void {
    const message = error instanceof Error ? error.message : String(error);
    if (isAuthRequiredError(error)) {
      // Authorization failed: the cached roster must stop being presented
      // as currently authorized data. No automatic retries; a successful
      // reauthentication followed by a new discovery cycle restarts it.
      this.rosterAuthRejected = true;
      this.clearRosterRefreshSchedule();
      this.rosterDirty = false;
      this.rosterQueuedRefresh?.resolve();
      this.rosterQueuedRefresh = undefined;
      this.rosterRetryAttempt = 0;
      set((state) => ({
        discovery: {
          ...state.discovery,
          mode: "authoritative",
          freshness: "unauthorized",
          unauthorizedReason: message,
          staleReason: undefined,
          retryAttempt: 0,
          retryAt: undefined,
        },
      } as Partial<State>));
      return;
    }
    // Transient failure: keep the last complete roster, mark discovery
    // stale, and retry with bounded backoff. Nothing is purged.
    this.rosterRetryAttempt += 1;
    const delay = retryDelayWithServerHintMs(this.rosterRetryAttempt, error);
    this.rosterRetryAt = Date.now() + delay;
    this.rosterDirty = true;
    set((state) => ({
      discovery: {
        ...state.discovery,
        mode: "authoritative",
        freshness: "stale",
        staleReason: message,
        retryAttempt: this.rosterRetryAttempt,
        retryAt: Date.now() + delay,
      },
    } as Partial<State>));
  }

  private clearRosterRefreshSchedule(): void {
    if (this.rosterRefreshTimer != null) {
      window.clearTimeout(this.rosterRefreshTimer);
      this.rosterRefreshTimer = undefined;
    }
  }

  private cancelRosterRefresh(): void {
    this.rosterGeneration += 1;
    this.clearRosterRefreshSchedule();
    this.rosterQueuedRefresh?.resolve();
    this.rosterQueuedRefresh = undefined;
    this.rosterRefreshPromise = undefined;
    this.rosterDirty = false;
    this.rosterRefreshNotBefore = 0;
    this.rosterRetryAt = 0;
    this.rosterRetryAttempt = 0;
    this.rosterAuthRejected = false;
  }

  cancelClientGenerationWork(): void {
    this.cancelRosterRefresh();
  }

  resetForClientChange(): void {
    this.cancelRosterRefresh();
    this.rosterIdentity = undefined;
  }

  closeForResume(): void {
    this.cancelRosterRefresh();
  }
}

export async function runWithConcurrencyLimit<T>(
  items: readonly T[],
  concurrency: number,
  run: (item: T) => Promise<void>,
  isCurrent: () => boolean = () => true,
): Promise<void> {
  let cursor = 0;
  const workers = Array.from({ length: Math.min(concurrency, items.length) }, async () => {
    while (isCurrent()) {
      const index = cursor++;
      if (index >= items.length) return;
      await run(items[index]);
    }
  });
  await Promise.all(workers);
}

/** Backoff ladder shared by roster retries and dashboard bootstrap retries. */
export function backfillRetryDelayMs(attempt: number): number {
  return retryDelayWithServerHintMs(attempt);
}

export function retryDelayWithServerHintMs(attempt: number, error?: unknown): number {
  const exponential = Math.min(15_000, 1_000 * 2 ** Math.max(0, attempt - 1));
  const hint = httpRetryAfterMs(error);
  return Math.max(exponential, hint ?? 0);
}
