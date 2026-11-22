import { parseSseStream } from "@holon/conversation-sdk";

import { backfillRetryDelayMs } from "./global-sync-coordinator";

const ROSTER_HINT_EVENT = "agent_roster_hint";
const NOTIFICATIONS_HINT_EVENT = "notifications.changed";

export interface RosterHintStreamDependencies {
  /**
   * Open GET /events/stream with current client credentials. Resolves with
   * the raw Response like fetch; rejects on transport errors and aborts.
   */
  openStream: (signal: AbortSignal) => Promise<Response>;
  /** One agent_roster_hint frame arrived while the page was visible. */
  onHint: () => void;
  onNotificationHint?: () => void;
  onConnected?: () => void;
  onDisconnected?: (status?: number) => void;
  /** Visibility gate override for tests; defaults to document state. */
  isHidden?: () => boolean;
  /** Reconnect backoff override for tests; defaults to the roster curve. */
  retryDelayMs?: (attempt: number) => number;
}

export interface RosterHintStreamController {
  /** Start (or keep) the subscription for the current client. */
  start(): void;
  /** Abort the subscription; suppressed hints are dropped. */
  stop(): void;
}

/**
 * Subscription to the global roster hint stream (#4310). Every frame is a
 * payload-free nudge: a visible hint schedules one throttled roster snapshot,
 * a hidden tab only remembers that a refresh is due and flushes it on the
 * next visible transition, and a reconnect pulls one snapshot because hints
 * missed while disconnected are never replayed. Auth rejection or a runtime
 * without the stream stops the controller instead of hot-looping; the next
 * healthy bootstrap restarts it.
 */
export function createRosterHintStream(
  dependencies: RosterHintStreamDependencies,
): RosterHintStreamController {
  const isHidden = (): boolean =>
    dependencies.isHidden?.() ?? document.visibilityState !== "visible";
  const retryDelayMs = (attempt: number): number =>
    dependencies.retryDelayMs?.(attempt) ?? backfillRetryDelayMs(attempt);

  let generation = 0;
  let active = false;
  let abortController: AbortController | undefined;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let retryAttempt = 0;
  let suppressedHintWhileHidden = false;

  function clearRetryTimer(): void {
    if (retryTimer !== undefined) {
      clearTimeout(retryTimer);
      retryTimer = undefined;
    }
  }

  function emitHint(): void {
    if (isHidden()) {
      // Keep the cheap subscription, but never pull roster snapshots for a
      // tab nobody is watching; one flush covers the whole hidden window.
      suppressedHintWhileHidden = true;
      return;
    }
    dependencies.onHint();
  }

  function handleVisibilityChange(): void {
    if (suppressedHintWhileHidden && !isHidden()) {
      suppressedHintWhileHidden = false;
      dependencies.onHint();
    }
  }

  async function run(token: number): Promise<void> {
    while (active && token === generation) {
      const connection = abortController;
      if (connection == null) return;
      try {
        const response = await dependencies.openStream(connection.signal);
        if (!active || token !== generation) return;
        if (!response.ok || response.body == null) {
          if (response.status === 401 || response.status === 403 || response.status === 404) {
            // Rejected credentials or a runtime without the global stream:
            // stop instead of retrying; the next healthy bootstrap (for
            // example after a client switch or resume) starts a fresh one.
            dependencies.onDisconnected?.(response.status);
            stop();
            return;
          }
          throw new Error(`roster hint stream http_${response.status}`);
        }
        if (retryAttempt > 0) {
          // Hints emitted while disconnected are not replayed; one snapshot
          // covers the gap exactly like a visible hint would.
          emitHint();
        }
        retryAttempt = 0;
        dependencies.onConnected?.();
        for await (const frame of parseSseStream(response.body)) {
          if (!active || token !== generation) return;
          if (frame.event === ROSTER_HINT_EVENT) emitHint();
          if (frame.event === NOTIFICATIONS_HINT_EVENT) dependencies.onNotificationHint?.();
        }
        // The server closes the stream on receiver lag; reconnect below.
        throw new Error("roster hint stream closed by server");
      } catch {
        if (!active || token !== generation) return;
        dependencies.onDisconnected?.();
        retryAttempt += 1;
        clearRetryTimer();
        retryTimer = setTimeout(() => {
          retryTimer = undefined;
          void run(token);
        }, retryDelayMs(retryAttempt));
        return;
      }
    }
  }

  function stop(): void {
    if (!active) return;
    active = false;
    generation += 1;
    clearRetryTimer();
    suppressedHintWhileHidden = false;
    document.removeEventListener("visibilitychange", handleVisibilityChange);
    abortController?.abort();
    abortController = undefined;
  }

  return {
    start(): void {
      if (active) return;
      active = true;
      generation += 1;
      retryAttempt = 0;
      suppressedHintWhileHidden = false;
      clearRetryTimer();
      abortController = new AbortController();
      document.addEventListener("visibilitychange", handleVisibilityChange);
      void run(generation);
    },
    stop,
  };
}
