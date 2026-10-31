import { httpRetryAfterMs, RuntimeHttpError } from "./client";

export interface SelectedAgentRefreshResource {
  refresh: () => Promise<void>;
  intervalMs: () => number;
}

export interface SelectedAgentRefreshOptions {
  resources: Record<string, SelectedAgentRefreshResource>;
  onError?: (key: string, error: unknown) => void;
  now?: () => number;
  random?: () => number;
}

const HINT_WINDOW_MS = 300;
const MIN_AUTOMATIC_INTERVAL_MS = 5_000;
const MAX_RETRY_DELAY_MS = 30_000;

interface ResourceState {
  key: string;
  resource: SelectedAgentRefreshResource;
  timer?: ReturnType<typeof setTimeout>;
  inFlight: boolean;
  dirtyGeneration: number;
  hintDeadline?: number;
  explicitPending: boolean;
  lastStartedAt: number;
  lastSettledAt?: number;
  retryAttempt: number;
  retryNotBefore: number;
  serverRetryNotBefore: number;
  stopped: boolean;
}

/**
 * Owns scheduling only. The resource/store owns response admission and stale
 * metadata, including rejecting results belonging to a replaced agent scope.
 */
export function createSelectedAgentRefreshScheduler(options: SelectedAgentRefreshOptions) {
  const now = options.now ?? Date.now;
  const random = options.random ?? Math.random;
  let active = false;
  let disposed = false;
  const states: ResourceState[] = Object.entries(options.resources).map(([key, resource]) => ({
    key,
    resource,
    inFlight: false,
    dirtyGeneration: 0,
    explicitPending: false,
    lastStartedAt: -Infinity,
    retryAttempt: 0,
    retryNotBefore: 0,
    serverRetryNotBefore: 0,
    stopped: false,
  }));

  function clearTimer(state: ResourceState) {
    if (state.timer !== undefined) {
      clearTimeout(state.timer);
      state.timer = undefined;
    }
  }

  function nextDeadline(state: ResourceState): number {
    if (state.explicitPending) return Math.max(now(), state.retryNotBefore);
    const pollDeadline = state.lastSettledAt === undefined
      ? Infinity
      : state.lastSettledAt + Math.max(MIN_AUTOMATIC_INTERVAL_MS, state.resource.intervalMs());
    return Math.max(
      Math.min(
        state.hintDeadline ?? Infinity,
        state.retryAttempt > 0 ? state.retryNotBefore : Infinity,
        pollDeadline,
      ),
      state.lastStartedAt + MIN_AUTOMATIC_INTERVAL_MS,
      state.retryNotBefore,
    );
  }

  function arm(state: ResourceState) {
    clearTimer(state);
    if (disposed || !active || state.inFlight || state.stopped) return;
    const deadline = nextDeadline(state);
    if (!Number.isFinite(deadline)) return;
    // A zero-delay timer also coalesces focus/online/open in the same turn.
    state.timer = setTimeout(() => {
      state.timer = undefined;
      if (disposed || !active || state.inFlight || state.stopped) return;
      // The interval can change without replacing the scheduler.
      if (nextDeadline(state) > now()) {
        arm(state);
      } else {
        start(state);
      }
    }, Math.max(0, Math.ceil(deadline - now())));
  }

  async function start(state: ResourceState) {
    state.inFlight = true;
    state.explicitPending = false;
    state.hintDeadline = undefined;
    state.lastStartedAt = now();
    const startedGeneration = state.dirtyGeneration;
    let failure: { error: unknown } | undefined;
    try {
      await state.resource.refresh();
    } catch (error) {
      failure = { error };
    }
    // In particular, do not call now(), intervalMs(), or onError after dispose.
    if (disposed) return;
    state.inFlight = false;
    state.lastSettledAt = now();
    if (failure) {
      state.retryAttempt += 1;
      const backoff = Math.round(Math.min(
        MAX_RETRY_DELAY_MS,
        MIN_AUTOMATIC_INTERVAL_MS * 2 ** Math.min(state.retryAttempt - 1, 3)
          * (0.8 + random() * 0.4),
      ));
      // A server-declared Retry-After stays binding even for operator retries;
      // the local backoff is only an estimate an operator may override.
      const serverRetryAfterMs = httpRetryAfterMs(failure.error) ?? 0;
      state.serverRetryNotBefore = serverRetryAfterMs > 0
        ? state.lastSettledAt + serverRetryAfterMs
        : 0;
      state.retryNotBefore = Math.max(
        state.lastSettledAt + backoff,
        state.serverRetryNotBefore,
      );
      state.stopped = failure.error instanceof RuntimeHttpError
        && [401, 403, 404, 410].includes(failure.error.status);
    } else {
      state.retryAttempt = 0;
      state.retryNotBefore = 0;
      state.serverRetryNotBefore = 0;
      if (state.dirtyGeneration === startedGeneration) state.hintDeadline = undefined;
    }
    arm(state);
    if (failure) options.onError?.(state.key, failure.error);
  }

  function requestExplicit(manual: boolean) {
    if (disposed) return;
    for (const state of states) {
      // Access/deletion failures stop automatic work until a scope replacement
      // or an explicit operator retry, not merely a reconnect/checkpoint.
      if (manual) {
        state.stopped = false;
        // An operator retry overrides the local failure backoff but keeps a
        // server-declared Retry-After deadline binding.
        state.retryAttempt = 0;
        state.retryNotBefore = state.serverRetryNotBefore;
      }
      if (state.stopped) continue;
      state.explicitPending = true;
      arm(state);
    }
  }

  return {
    hint() {
      if (disposed) return;
      for (const state of states) {
        if (state.stopped) continue;
        state.dirtyGeneration += 1;
        state.hintDeadline ??= now() + HINT_WINDOW_MS;
        arm(state);
      }
    },
    resume() {
      requestExplicit(false);
    },
    setActive(value: boolean) {
      if (disposed || active === value) return;
      active = value;
      if (active) {
        requestExplicit(false);
      } else {
        for (const state of states) clearTimer(state);
      }
    },
    refreshNow() {
      requestExplicit(true);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      active = false;
      for (const state of states) clearTimer(state);
    },
  };
}
