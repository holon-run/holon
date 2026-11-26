import { backfillRetryDelayMs } from "./global-sync-coordinator";

export type NotificationKind = "operator_first_brief" | "work_item_completed" | "operator_input_required";

export interface NotificationTarget {
  agent_id: string;
  work_item_id?: string;
}

export interface RuntimeNotification {
  id: string;
  kind: NotificationKind;
  created_at: string;
  target: NotificationTarget;
  title?: string;
  summary?: string;
}

export interface NotificationSnapshot {
  status: "loading" | "ready" | "stale" | "unavailable";
  pending: RuntimeNotification[];
  recent: RuntimeNotification[];
  toasts: RuntimeNotification[];
}

type Lists = Pick<NotificationSnapshot, "pending" | "recent">;
type RefreshMode = "catchup" | "live";

interface NotificationDependencies {
  load: (signal: AbortSignal) => Promise<Lists>;
  isHidden: () => boolean;
  isViewing: (target: NotificationTarget) => boolean;
  publish: (snapshot: NotificationSnapshot) => void;
  /** Delivers OS notifications for items found while the page is hidden. */
  notify?: (items: RuntimeNotification[]) => void;
  /** Optional browser-persisted set of manually ignored pending notification IDs. */
  readIgnoredPendingIds?: () => ReadonlySet<string>;
  writeIgnoredPendingIds?: (ids: ReadonlySet<string>) => void;
}

export const emptyNotifications: NotificationSnapshot = {
  status: "loading", pending: [], recent: [], toasts: [],
};

const START_INTERVAL_MS = 500;
const MAX_TOASTS = 3;
const MAX_IGNORED_PENDING_IDS = 500;

export class NotificationHttpError extends Error {
  constructor(readonly status: number) {
    super(`notifications http_${status}`);
  }
}

/** One app-scoped reader, with one coalesced trailing read per burst. */
export function createNotificationSync(deps: NotificationDependencies) {
  let snapshot = emptyNotifications;
  let rawPending: RuntimeNotification[] = [];
  let active = false;
  let connected = false;
  let hasBaseline = false;
  let continuity = 0;
  let dirty: RefreshMode | undefined;
  let flight: AbortController | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let nextReadAt = 0;
  let failureCount = 0;

  function readIgnored(): Set<string> {
    return new Set(deps.readIgnoredPendingIds?.() ?? []);
  }

  function writeIgnored(ids: Set<string>) {
    if (!deps.writeIgnoredPendingIds) return;
    if (ids.size <= MAX_IGNORED_PENDING_IDS) {
      deps.writeIgnoredPendingIds(ids);
      return;
    }
    const bounded = Array.from(ids).slice(-MAX_IGNORED_PENDING_IDS);
    deps.writeIgnoredPendingIds(new Set(bounded));
  }

  function publish(next: NotificationSnapshot) {
    snapshot = next;
    deps.publish(next);
  }

  function clearTimer() {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  }

  function reset(status: NotificationSnapshot["status"]) {
    clearTimer();
    flight?.abort();
    flight = undefined;
    dirty = undefined;
    hasBaseline = false;
    nextReadAt = 0;
    failureCount = 0;
    rawPending = [];
    publish({ ...emptyNotifications, status });
  }

  function schedule() {
    if (!active || flight || !dirty || timer !== undefined) return;
    const delay = nextReadAt - Date.now();
    if (delay > 0) {
      timer = setTimeout(() => {
        timer = undefined;
        schedule();
      }, delay);
    } else {
      void read(dirty);
    }
  }

  function request(mode: RefreshMode) {
    if (!active) return;
    if (mode === "catchup" || dirty === undefined) dirty = mode;
    schedule();
  }

  async function read(mode: RefreshMode) {
    dirty = undefined;
    const requestContinuity = continuity;
    const controller = new AbortController();
    flight = controller;
    nextReadAt = Date.now() + START_INTERVAL_MS;
    try {
      const lists = await deps.load(controller.signal);
      if (flight !== controller) return;
      rawPending = lists.pending;
      const ignored = readIgnored();
      const serverPendingIds = new Set(lists.pending.map(({ id }) => id));
      let prunedChanged = false;
      for (const ignoredId of ignored) {
        if (!serverPendingIds.has(ignoredId)) {
          ignored.delete(ignoredId);
          prunedChanged = true;
        }
      }
      if (prunedChanged) writeIgnored(ignored);
      const visiblePending = lists.pending.filter((item) => !ignored.has(item.id));
      const known = new Set(snapshot.recent.map(({ id }) => id));
      const live = mode === "live" && connected && hasBaseline && requestContinuity === continuity;
      const fresh = live
        ? lists.recent.filter((item) => !known.has(item.id) && !deps.isViewing(item.target) && !ignored.has(item.id))
        : [];
      const hidden = deps.isHidden();
      hasBaseline = true;
      failureCount = 0;
      if (hidden && fresh.length) deps.notify?.(fresh);
      publish({
        recent: lists.recent,
        pending: visiblePending,
        status: connected ? "ready" : "stale",
        // A hidden tab keeps no toasts; its reminders go through the OS instead.
        toasts: hidden ? [] : [...fresh, ...snapshot.toasts].slice(0, MAX_TOASTS),
      });
    } catch (error) {
      if (flight !== controller) return;
      if (error instanceof NotificationHttpError && [401, 403, 404].includes(error.status)) {
        reset("unavailable");
      } else {
        hasBaseline = false;
        publish({ ...snapshot, status: "stale" });
        nextReadAt = Date.now() + backfillRetryDelayMs(++failureCount);
        dirty = "catchup";
      }
    } finally {
      // A failed half of the snapshot must not leave the sibling request running.
      controller.abort();
      if (flight === controller) {
        flight = undefined;
        schedule();
      }
    }
  }

  return {
    start() { active = true; },
    stop() {
      active = false;
      connected = false;
      reset("loading");
    },
    connected() {
      connected = true;
      continuity += 1;
      request("catchup");
    },
    disconnected(status?: number) {
      if (!active) return;
      connected = false;
      continuity += 1;
      if (status === 401 || status === 403) {
        reset("unavailable");
      } else if (snapshot.status !== "unavailable") {
        if (snapshot.status === "ready") publish({ ...snapshot, status: "stale" });
        // Still discover support when the global stream isn't available.
        if (!hasBaseline || dirty) request("catchup");
      }
    },
    hint() {
      // A hidden tab still reads, otherwise nothing could reach the OS. Catchup
      // stays reserved for reconnects and for the read after visibility returns.
      if (snapshot.status !== "unavailable") request("live");
    },
    visibilityChanged() {
      continuity += 1;
      if (deps.isHidden()) {
        if (snapshot.toasts.length) publish({ ...snapshot, toasts: [] });
      } else {
        request("catchup");
      }
    },
    healthRefresh() { request("catchup"); },
    dismiss(id: string) {
      publish({ ...snapshot, toasts: snapshot.toasts.filter((item) => item.id !== id) });
    },
    ignorePending(id: string) {
      const ignored = readIgnored();
      ignored.add(id);
      writeIgnored(ignored);
      publish({
        ...snapshot,
        pending: snapshot.pending.filter((item) => item.id !== id),
        toasts: snapshot.toasts.filter((item) => item.id !== id),
      });
    },
    ignoreAllPending() {
      const ignored = readIgnored();
      const ids = new Set([
        ...rawPending.map((item) => item.id),
        ...snapshot.pending.map((item) => item.id),
      ]);
      for (const id of ids) ignored.add(id);
      writeIgnored(ignored);
      publish({
        ...snapshot,
        pending: [],
        toasts: snapshot.toasts.filter((item) => !ids.has(item.id)),
      });
    },
  };
}
