import type { ConversationClient } from "./client.js";
import {
  ConversationCapabilityError,
  ConversationCompatibilityError,
  ConversationDecodeError,
  ConversationHttpError,
  ConversationProtocolError,
  ConversationResetError,
  ConversationStaleResponseError,
  ConversationStateLimitError,
} from "./errors.js";
import {
  ConversationProtocolState,
  type ConversationStateView,
} from "./state.js";
import {
  CONVERSATION_SCHEMA_VERSION,
  CONVERSATION_QUERY_VERSION,
  type BriefRecord,
  type ConversationActivityResponse,
  type ConversationCheckpoint,
  type ConversationDetailCursor,
  type ConversationHistoryCursor,
  type ConversationRequestIdentity,
  type ConversationSnapshotCacheEntry,
  type ConversationStateLimits,
  type ConversationStreamItem,
  type ConversationSummaryResult,
  type ConversationSummaryResponse,
} from "./types.js";

/**
 * Structural client surface the controller depends on. `ConversationClient`
 * satisfies it; tests may substitute fakes.
 */
export interface ConversationClientLike {
  readonly baseUrl: string;
  requireCapability(signal?: AbortSignal): Promise<unknown>;
  summary(
    agentId: string,
    options?: {
      readonly limit?: number;
      readonly before?: ConversationHistoryCursor;
      readonly ifNoneMatch?: string;
      readonly signal?: AbortSignal;
    },
  ): Promise<ConversationSummaryResult>;
  activities(
    agentId: string,
    turnId: string,
    options?: {
      readonly limit?: number;
      readonly before?: ConversationDetailCursor;
      readonly signal?: AbortSignal;
    },
  ): Promise<ConversationActivityResponse>;
  brief(
    agentId: string,
    briefId: string,
    signal?: AbortSignal,
  ): Promise<BriefRecord>;
  stream(
    agentId: string,
    options?: {
      readonly after?: ConversationCheckpoint;
      readonly signal?: AbortSignal;
    },
  ): AsyncGenerator<ConversationStreamItem>;
}

export type ConversationStatus =
  | { readonly kind: "idle" }
  | { readonly kind: "loading" }
  | { readonly kind: "ready" }
  | {
      readonly kind: "reconnecting";
      readonly attempt: number;
      readonly delayMs: number;
    }
  | { readonly kind: "unsupported"; readonly error: unknown }
  | { readonly kind: "recoverable_error"; readonly error: unknown }
  | { readonly kind: "terminal_error"; readonly error: unknown };

export type ConversationHistoryLoadState =
  | { readonly kind: "idle" }
  | { readonly kind: "loading" }
  | { readonly kind: "complete" }
  | {
      readonly kind: "error";
      readonly retryable: boolean;
      readonly error: unknown;
    };

export type ConversationDetailLoadState =
  | { readonly kind: "idle" }
  | { readonly kind: "loading" }
  | {
      readonly kind: "error";
      readonly retryable: boolean;
      readonly error: unknown;
    };

export type ConversationBriefLoadState =
  | { readonly kind: "loading" }
  | { readonly kind: "ready"; readonly brief: BriefRecord }
  | {
      readonly kind: "error";
      readonly retryable: boolean;
      readonly error: unknown;
    };

export type ConversationErrorClass =
  | "retryable"
  | "recoverable"
  | "unsupported"
  | "terminal";

export function classifyConversationError(
  error: unknown,
): ConversationErrorClass {
  if (error instanceof ConversationCapabilityError) {
    return "unsupported";
  }
  if (error instanceof ConversationCompatibilityError) {
    return "unsupported";
  }
  if (error instanceof ConversationResetError) {
    return error.reason === "agent_not_found" ? "recoverable" : "retryable";
  }
  if (error instanceof ConversationStaleResponseError) {
    return "retryable";
  }
  if (error instanceof ConversationHttpError) {
    if (error.status >= 500 || error.status === 429) {
      return "retryable";
    }
    if (error.status === 401 || error.status === 403 || error.status === 404) {
      return "recoverable";
    }
    return "terminal";
  }
  if (error instanceof ConversationDecodeError) {
    return "terminal";
  }
  if (
    error instanceof ConversationProtocolError ||
    error instanceof ConversationStateLimitError
  ) {
    return "terminal";
  }
  if (error instanceof TypeError) {
    return "retryable";
  }
  return "terminal";
}

export interface ConversationRetryOptions {
  readonly initialDelayMs?: number;
  readonly maxDelayMs?: number;
  readonly maxAttempts?: number;
  readonly jitterRatio?: number;
  readonly stableUptimeMs?: number;
}

/**
 * Best-effort persistent cache for brief records. Briefs are final,
 * immutable artifacts, so entries never need invalidation; implementations
 * must tolerate rejections (storage unavailable, quota exceeded) without
 * throwing into the controller lifecycle.
 */
export interface ConversationBriefCache {
  /** Return the cached brief, or null/undefined when absent. */
  get(briefId: string): Promise<BriefRecord | null | undefined>;
  /** Persist a successfully fetched brief. */
  put(briefId: string, brief: BriefRecord): Promise<void>;
}

/**
 * Best-effort persistent cache of the most recent bootstrap snapshot for one
 * agent scope. Implementations must tolerate rejections without throwing
 * into the controller lifecycle.
 */
export interface ConversationSnapshotCache {
  load(): Promise<ConversationSnapshotCacheEntry | null | undefined>;
  store(entry: ConversationSnapshotCacheEntry): Promise<void>;
}

export interface ConversationControllerOptions {
  readonly client: ConversationClientLike;
  readonly agentId: string;
  readonly remoteId?: string;
  readonly limits?: Partial<ConversationStateLimits>;
  readonly retry?: ConversationRetryOptions;
  readonly historyPageSize?: number;
  readonly activityPageSize?: number;
  readonly maxBriefCache?: number;
  readonly briefCache?: ConversationBriefCache;
  readonly snapshotCache?: ConversationSnapshotCache;
  readonly sleep?: (ms: number, signal: AbortSignal) => Promise<void>;
  readonly random?: () => number;
  readonly now?: () => number;
  readonly onEvent?: (event: ConversationControllerEvent) => void;
}

export type ConversationControllerEvent =
  | { readonly type: "status"; readonly status: ConversationStatus }
  | {
      readonly type: "view";
      readonly reason:
        | "bootstrap"
        | "batch"
        | "older_page"
        | "detail_page"
        | "reset";
    }
  | {
      readonly type: "stream_error";
      readonly attempt: number;
      readonly error: unknown;
    }
  | {
      readonly type: "request_error";
      readonly kind: "history" | "detail" | "brief";
      readonly error: unknown;
    };

const DEFAULT_INITIAL_DELAY_MS = 500;
const DEFAULT_MAX_DELAY_MS = 30_000;
const DEFAULT_MAX_ATTEMPTS = 8;
const DEFAULT_JITTER_RATIO = 0.2;
const DEFAULT_STABLE_UPTIME_MS = 30_000;
const DEFAULT_HISTORY_PAGE_SIZE = 30;
const DEFAULT_ACTIVITY_PAGE_SIZE = 50;
const DEFAULT_MAX_BRIEF_CACHE = 64;

function defaultSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => resolve(), ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        reject(signal.reason ?? new Error("aborted"));
      },
      { once: true },
    );
  });
}

function isAbortError(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as { name?: unknown }).name === "AbortError"
  );
}

function abortError(message: string): Error {
  return Object.assign(new Error(message), { name: "AbortError" });
}

/**
 * Framework-agnostic conversation lifecycle driver.
 *
 * Owns: capability check, snapshot bootstrap, stream supervision with bounded
 * exponential backoff, reset-triggered serial re-bootstrap, request
 * single-flight dedup for history/detail/brief loads, and dispose semantics.
 * All state is in-memory; nothing here persists across page reloads.
 */
export class ConversationController {
  readonly agentId: string;
  readonly remoteId: string;
  readonly limits: ConversationStateLimits;

  readonly #client: ConversationClientLike;
  readonly #identity: ConversationRequestIdentity;
  readonly #state: ConversationProtocolState;
  readonly #listeners = new Set<() => void>();
  readonly #onEvent:
    | ((event: ConversationControllerEvent) => void)
    | undefined;

  readonly #initialDelayMs: number;
  readonly #maxDelayMs: number;
  readonly #maxAttempts: number;
  readonly #jitterRatio: number;
  readonly #stableUptimeMs: number;
  readonly #historyPageSize: number;
  readonly #activityPageSize: number;
  readonly #maxBriefCache: number;
  readonly #briefCache: ConversationBriefCache | undefined;
  readonly #snapshotCache: ConversationSnapshotCache | undefined;
  readonly #sleep: (ms: number, signal: AbortSignal) => Promise<void>;
  readonly #random: () => number;
  readonly #now: () => number;

  #status: ConversationStatus = { kind: "idle" };
  #historyState: ConversationHistoryLoadState = { kind: "idle" };
  readonly #detailStates = new Map<string, ConversationDetailLoadState>();
  readonly #briefStates = new Map<string, ConversationBriefLoadState>();
  readonly #briefOrder: string[] = [];

  #runToken = 0;
  #runAbort: AbortController | null = null;
  // Auxiliary data requests (history/detail/brief) outlive individual stream
  // runs; only dispose cancels them.
  #requestAbort = new AbortController();
  #handshakePromise: Promise<unknown> | null = null;
  #disposed = false;
  #starting = false;
  // Set when the protocol state was hydrated from the snapshot cache and a
  // conditional revalidation must succeed before streaming; the stale cache
  // is never streamed from without a server-confirmed 304.
  #pendingRevalidate: { readonly etag: string | null } | undefined;
  #hydrateAttempted = false;

  constructor(options: ConversationControllerOptions) {
    if (options.agentId.length === 0) {
      throw new ConversationProtocolError("agentId must not be empty");
    }
    this.#client = options.client;
    this.agentId = options.agentId;
    this.remoteId = options.remoteId ?? "";
    this.#identity = {
      remote_id: this.remoteId,
      agent_id: this.agentId,
      generation: 1,
    };
    this.#state = new ConversationProtocolState(options.limits ?? {});
    this.#onEvent = options.onEvent;
    this.limits = this.#state.limits;
    const retry = options.retry ?? {};
    this.#initialDelayMs = retry.initialDelayMs ?? DEFAULT_INITIAL_DELAY_MS;
    this.#maxDelayMs = retry.maxDelayMs ?? DEFAULT_MAX_DELAY_MS;
    this.#maxAttempts = retry.maxAttempts ?? DEFAULT_MAX_ATTEMPTS;
    this.#jitterRatio = retry.jitterRatio ?? DEFAULT_JITTER_RATIO;
    this.#stableUptimeMs = retry.stableUptimeMs ?? DEFAULT_STABLE_UPTIME_MS;
    this.#historyPageSize =
      options.historyPageSize ?? DEFAULT_HISTORY_PAGE_SIZE;
    this.#activityPageSize =
      options.activityPageSize ?? DEFAULT_ACTIVITY_PAGE_SIZE;
    this.#maxBriefCache = options.maxBriefCache ?? DEFAULT_MAX_BRIEF_CACHE;
    this.#briefCache = options.briefCache;
    this.#snapshotCache = options.snapshotCache;
    this.#sleep = options.sleep ?? defaultSleep;
    this.#random = options.random ?? Math.random;
    this.#now = options.now ?? Date.now;
  }

  get status(): ConversationStatus {
    return this.#status;
  }

  get identity(): ConversationRequestIdentity {
    return this.#identity;
  }

  view(): ConversationStateView {
    return this.#state.view();
  }

  historyState(): ConversationHistoryLoadState {
    return this.#historyState;
  }

  detailState(turnId: string): ConversationDetailLoadState {
    return this.#detailStates.get(turnId) ?? { kind: "idle" };
  }

  briefState(briefId: string): ConversationBriefLoadState | null {
    return this.#briefStates.get(briefId) ?? null;
  }

  subscribe(listener: () => void): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  /**
   * Begin the lifecycle. Idempotent while a run is active; throws after
   * dispose.
   */
  start(): void {
    if (this.#disposed) {
      throw new ConversationProtocolError("controller is disposed");
    }
    if (this.#starting) {
      return;
    }
    this.#starting = true;
    this.#runToken += 1;
    const runToken = this.#runToken;
    this.#runAbort = new AbortController();
    void this.#supervise(runToken);
  }

  /**
   * Manually restart after a recoverable/terminal/unsupported outcome or
   * while reconnecting. Safe to call at any time before dispose.
   */
  retry(): void {
    if (this.#disposed) {
      throw new ConversationProtocolError("controller is disposed");
    }
    if (this.#starting) {
      this.#runToken += 1;
      this.#runAbort?.abort(abortError("superseded by manual retry"));
      this.#runAbort = new AbortController();
      this.#starting = false;
    }
    this.start();
  }

  /** Cancel every request, stream, timer, and subscription notification. */
  dispose(): void {
    if (this.#disposed) {
      return;
    }
    this.#disposed = true;
    this.#runToken += 1;
    this.#runAbort?.abort(abortError("controller disposed"));
    this.#requestAbort.abort(abortError("controller disposed"));
    this.#runAbort = null;
    this.#listeners.clear();
  }

  async loadOlderHistory(): Promise<ConversationHistoryLoadState> {
    if (this.#disposed) {
      return {
        kind: "error",
        retryable: false,
        error: new ConversationProtocolError("controller is disposed"),
      };
    }
    const view = this.view();
    if (view.scope === null || view.reset_reason !== null) {
      return {
        kind: "error",
        retryable: true,
        error: new ConversationProtocolError(
          "history requires a bootstrapped conversation",
        ),
      };
    }
    if (view.next_before_cursor === null || !view.has_more) {
      const complete: ConversationHistoryLoadState = { kind: "complete" };
      this.#historyState = complete;
      this.#emitChange();
      return complete;
    }
    if (this.#historyState.kind === "loading") {
      return this.#historyState;
    }
    const before = view.next_before_cursor;
    this.#historyState = { kind: "loading" };
    this.#emitChange();
    try {
      const result = await this.#client.summary(this.agentId, {
        limit: this.#historyPageSize,
        before,
        signal: this.#requestSignal(),
      });
      if (result.summary === null) {
        throw new ConversationProtocolError(
          "older-page summary returned 304 unexpectedly",
        );
      }
      const page = result.summary;
      if (this.#disposed) {
        return { kind: "idle" };
      }
      this.#state.applyOlderPage(this.#identity, before, page);
      const next: ConversationHistoryLoadState =
        page.has_more && page.next_before_cursor !== null
          ? { kind: "idle" }
          : { kind: "complete" };
      this.#historyState = next;
      this.#emitView("older_page");
      return next;
    } catch (error) {
      return this.#absorbHistoryError(error);
    }
  }

  async loadDetail(turnId: string): Promise<ConversationDetailLoadState> {
    return this.#requestDetail(turnId, undefined);
  }

  async loadOlderActivities(
    turnId: string,
  ): Promise<ConversationDetailLoadState> {
    const view = this.view();
    const detail = view.details.find((entry) => entry.turn_id === turnId);
    if (detail === undefined) {
      return this.#requestDetail(turnId, undefined);
    }
    if (!detail.has_more || detail.next_before_cursor === null) {
      return { kind: "idle" };
    }
    return this.#requestDetail(turnId, detail.next_before_cursor);
  }

  async loadBrief(briefId: string): Promise<ConversationBriefLoadState> {
    if (this.#disposed) {
      return {
        kind: "error",
        retryable: false,
        error: new ConversationProtocolError("controller is disposed"),
      };
    }
    const existing = this.#briefStates.get(briefId);
    if (existing !== undefined) {
      return existing;
    }
    this.#briefStates.set(briefId, { kind: "loading" });
    this.#briefOrder.push(briefId);
    this.#evictBriefCache();
    this.#emitChange();
    if (this.#briefCache !== undefined) {
      let cached: BriefRecord | null | undefined;
      try {
        cached = await this.#briefCache.get(briefId);
      } catch {
        // Persistent cache read failure falls through to the network.
      }
      if (this.#disposed) {
        return { kind: "loading" };
      }
      if (
        cached != null &&
        cached.id === briefId &&
        cached.agent_id === this.agentId
      ) {
        const cachedReady: ConversationBriefLoadState = {
          kind: "ready",
          brief: cached,
        };
        this.#briefStates.set(briefId, cachedReady);
        this.#emitChange();
        return cachedReady;
      }
    }
    try {
      const brief = await this.#client.brief(
        this.agentId,
        briefId,
        this.#requestSignal(),
      );
      if (this.#disposed) {
        return { kind: "loading" };
      }
      const ready: ConversationBriefLoadState = { kind: "ready", brief };
      this.#briefStates.set(briefId, ready);
      this.#emitChange();
      if (this.#briefCache !== undefined) {
        // Best-effort persistence; cache failures never fail the load.
        void Promise.resolve(this.#briefCache.put(briefId, brief)).catch(
          () => {},
        );
      }
      return ready;
    } catch (error) {
      if (this.#disposed) {
        return { kind: "loading" };
      }
      const klass = classifyConversationError(error);
      this.#onEvent?.({
        type: "request_error",
        kind: "brief",
        error,
      });
      const next: ConversationBriefLoadState = {
        kind: "error",
        retryable: klass === "retryable" || klass === "recoverable",
        error,
      };
      this.#briefStates.set(briefId, next);
      this.#emitChange();
      return next;
    }
  }

  #absorbHistoryError(error: unknown): ConversationHistoryLoadState {
    if (this.#disposed) {
      return { kind: "idle" };
    }
    if (
      error instanceof ConversationStaleResponseError ||
      isAbortError(error)
    ) {
      // The page raced a reset or newer cursor; retry is safe and idempotent.
      const idle: ConversationHistoryLoadState = { kind: "idle" };
      this.#historyState = idle;
      this.#emitChange();
      return idle;
    }
    const klass = classifyConversationError(error);
    this.#onEvent?.({ type: "request_error", kind: "history", error });
    if (klass === "retryable") {
      const idle: ConversationHistoryLoadState = { kind: "idle" };
      this.#historyState = idle;
      this.#emitChange();
      return idle;
    }
    const next: ConversationHistoryLoadState = {
      kind: "error",
      retryable: klass === "recoverable",
      error,
    };
    this.#historyState = next;
    this.#emitChange();
    return next;
  }

  async #requestDetail(
    turnId: string,
    before: ConversationDetailCursor | undefined,
  ): Promise<ConversationDetailLoadState> {
    if (this.#disposed) {
      return {
        kind: "error",
        retryable: false,
        error: new ConversationProtocolError("controller is disposed"),
      };
    }
    const view = this.view();
    if (view.scope === null || view.reset_reason !== null) {
      return {
        kind: "error",
        retryable: true,
        error: new ConversationProtocolError(
          "detail requires a bootstrapped conversation",
        ),
      };
    }
    const existing = this.#detailStates.get(turnId);
    if (existing?.kind === "loading") {
      return existing;
    }
    this.#detailStates.set(turnId, { kind: "loading" });
    this.#emitChange();
    try {
      const page = await this.#client.activities(this.agentId, turnId, {
        limit: this.#activityPageSize,
        ...(before === undefined ? {} : { before }),
        signal: this.#requestSignal(),
      });
      if (this.#disposed) {
        return { kind: "idle" };
      }
      this.#state.applyDetailPage(this.#identity, turnId, before, page);
      this.#detailStates.set(turnId, { kind: "idle" });
      this.#emitView("detail_page");
      return { kind: "idle" };
    } catch (error) {
      if (this.#disposed) {
        return { kind: "idle" };
      }
      if (
        error instanceof ConversationStaleResponseError ||
        isAbortError(error)
      ) {
        this.#detailStates.set(turnId, { kind: "idle" });
        this.#emitChange();
        return { kind: "idle" };
      }
      const klass = classifyConversationError(error);
      this.#onEvent?.({ type: "request_error", kind: "detail", error });
      const next: ConversationDetailLoadState = {
        kind: "error",
        retryable: klass === "retryable" || klass === "recoverable",
        error,
      };
      this.#detailStates.set(turnId, next);
      this.#emitChange();
      return next;
    }
  }

  async #supervise(runToken: number): Promise<void> {
    let attempt = 0;
    while (this.#alive(runToken)) {
      let openedAt = 0;
      try {
        await this.#ensureCapability();
        if (!this.#alive(runToken)) {
          return;
        }
        if (this.#state.reconnectCheckpoint() === null) {
          this.#setStatus({ kind: "loading" });
          if (!this.#hydrateAttempted) {
            this.#hydrateAttempted = true;
            await this.#hydrateCachedSnapshot(runToken);
            if (!this.#alive(runToken)) {
              return;
            }
          }
        }
        if (this.#pendingRevalidate !== undefined) {
          // Stale-while-revalidate: render the cached snapshot now, confirm
          // with the server before streaming from it.
          this.#setStatus({ kind: "loading" });
          await this.#revalidateSnapshot(runToken);
          if (!this.#alive(runToken)) {
            return;
          }
        } else if (this.#state.reconnectCheckpoint() === null) {
          this.#setStatus({ kind: "loading" });
          await this.#bootstrap(runToken);
          if (!this.#alive(runToken)) {
            return;
          }
        }
        openedAt = this.#now();
        this.#setStatus({ kind: "ready" });
        await this.#pumpStream(runToken);
        // Clean stream end (server closed) or applied reset: loop and
        // re-attach from the retained checkpoint or a fresh snapshot.
        const uptime = openedAt === 0 ? 0 : this.#now() - openedAt;
        attempt = uptime >= this.#stableUptimeMs ? 0 : attempt + 1;
        if (attempt > this.#maxAttempts) {
          this.#starting = false;
          this.#setStatus({
            kind: "recoverable_error",
            error: new ConversationProtocolError(
              "conversation stream kept closing before stabilizing",
            ),
          });
          return;
        }
        const delayMs = this.#backoffDelay(attempt);
        this.#setStatus({ kind: "reconnecting", attempt, delayMs });
        await this.#sleep(delayMs, this.#runSignal());
      } catch (error) {
        if (!this.#alive(runToken)) {
          return;
        }
        if (isAbortError(error)) {
          return;
        }
        const klass = classifyConversationError(error);
        if (klass !== "retryable") {
          this.#starting = false;
          this.#setStatus(
            klass === "unsupported"
              ? { kind: "unsupported", error }
              : klass === "recoverable"
                ? { kind: "recoverable_error", error }
                : { kind: "terminal_error", error },
          );
          return;
        }
        const uptime = openedAt === 0 ? 0 : this.#now() - openedAt;
        if (uptime >= this.#stableUptimeMs) {
          attempt = 0;
        }
        attempt += 1;
        this.#onEvent?.({ type: "stream_error", attempt, error });
        if (attempt > this.#maxAttempts) {
          this.#starting = false;
          this.#setStatus({ kind: "recoverable_error", error });
          return;
        }
        const delayMs = this.#backoffDelay(attempt);
        this.#setStatus({ kind: "reconnecting", attempt, delayMs });
        try {
          await this.#sleep(delayMs, this.#runSignal());
        } catch (sleepError) {
          if (isAbortError(sleepError) || !this.#alive(runToken)) {
            return;
          }
          throw sleepError;
        }
      }
    }
  }

  async #ensureCapability(): Promise<void> {
    if (this.#handshakePromise === null) {
      this.#handshakePromise = this.#client.requireCapability(
        this.#runSignal(),
      );
    }
    try {
      await this.#handshakePromise;
    } catch (error) {
      this.#handshakePromise = null;
      throw error;
    }
  }

  #historyStateFor(
    snapshot: ConversationSummaryResponse,
  ): ConversationHistoryLoadState {
    return snapshot.has_more && snapshot.next_before_cursor !== null
      ? { kind: "idle" }
      : { kind: "complete" };
  }

  async #hydrateCachedSnapshot(runToken: number): Promise<void> {
    if (this.#snapshotCache === undefined) {
      return;
    }
    let entry: ConversationSnapshotCacheEntry | null | undefined;
    try {
      entry = await this.#snapshotCache.load();
    } catch {
      return; // Cache read failure falls through to a network bootstrap.
    }
    if (!this.#alive(runToken) || entry == null) {
      return;
    }
    const snapshot = entry.summary;
    if (
      snapshot.schema_version !== CONVERSATION_SCHEMA_VERSION ||
      snapshot.query_version !== CONVERSATION_QUERY_VERSION ||
      typeof snapshot.snapshot_cursor !== "string" ||
      snapshot.snapshot_cursor.length === 0
    ) {
      return;
    }
    try {
      this.#state.bootstrap(this.#identity, snapshot);
    } catch {
      return; // Incompatible snapshot falls back to a network bootstrap.
    }
    this.#historyState = this.#historyStateFor(snapshot);
    this.#pendingRevalidate = { etag: entry.etag };
    this.#emitView("bootstrap");
  }

  async #revalidateSnapshot(runToken: number): Promise<void> {
    const etag = this.#pendingRevalidate?.etag ?? null;
    const result = await this.#client.summary(this.agentId, {
      limit: this.#historyPageSize,
      ...(etag === null ? {} : { ifNoneMatch: etag }),
      signal: this.#runSignal(),
    });
    if (!this.#alive(runToken)) {
      return;
    }
    if (result.summary === null) {
      // 304: the hydrated snapshot still matches the server; keep it and
      // clear the pending revalidation so the stream may attach.
      this.#pendingRevalidate = undefined;
      return;
    }
    this.#state.bootstrap(this.#identity, result.summary);
    this.#historyState = this.#historyStateFor(result.summary);
    this.#emitView("bootstrap");
    this.#persistSnapshot(result.summary, result.etag);
    this.#pendingRevalidate = undefined;
  }

  async #bootstrap(runToken: number): Promise<void> {
    const result = await this.#client.summary(this.agentId, {
      limit: this.#historyPageSize,
      signal: this.#runSignal(),
    });
    if (!this.#alive(runToken)) {
      return;
    }
    if (result.summary === null) {
      throw new ConversationProtocolError(
        "summary bootstrap returned 304 unexpectedly",
      );
    }
    const snapshot = result.summary;
    this.#state.bootstrap(this.#identity, snapshot);
    this.#historyState = this.#historyStateFor(snapshot);
    this.#emitView("bootstrap");
    this.#persistSnapshot(snapshot, result.etag);
  }

  #persistSnapshot(
    snapshot: ConversationSummaryResponse,
    etag: string | null,
  ): void {
    if (this.#snapshotCache === undefined) {
      return;
    }
    // Best-effort persistence; cache failures never fail the controller.
    void Promise.resolve(this.#snapshotCache.store({ etag, summary: snapshot }))
      .catch(() => {});
  }

  async #pumpStream(runToken: number): Promise<void> {
    const checkpoint = this.#state.reconnectCheckpoint();
    if (checkpoint === null) {
      throw new ConversationProtocolError(
        "stream attach requires a bootstrapped checkpoint",
      );
    }
    for await (const item of this.#client.stream(this.agentId, {
      after: checkpoint,
      signal: this.#runSignal(),
    })) {
      if (!this.#alive(runToken)) {
        return;
      }
      if (item.type === "reset_required") {
        this.#state.reset(item.reset.reason);
        this.#emitView("reset");
        return;
      }
      try {
        if (this.#state.applyBatch(this.#identity, item.batch)) {
          this.#emitView("batch");
        }
      } catch (error) {
        if (error instanceof ConversationStaleResponseError) {
          // Divergence between checkpoint and server framing: self-heal by
          // clearing state so the supervise loop re-snapshots serially.
          this.#state.reset("stream_recovery_failed");
          this.#emitView("reset");
          return;
        }
        throw error;
      }
    }
  }

  #backoffDelay(attempt: number): number {
    const exponential = Math.min(
      this.#maxDelayMs,
      this.#initialDelayMs * 2 ** Math.max(0, attempt - 1),
    );
    const jitterFactor = 1 + this.#jitterRatio * (2 * this.#random() - 1);
    // maxDelayMs stays a hard upper bound even after jitter is applied.
    return Math.min(
      this.#maxDelayMs,
      Math.max(0, Math.round(exponential * jitterFactor)),
    );
  }

  #alive(runToken: number): boolean {
    return !this.#disposed && runToken === this.#runToken;
  }

  #runSignal(): AbortSignal {
    if (this.#runAbort === null) {
      throw new ConversationProtocolError("controller run is not active");
    }
    return this.#runAbort.signal;
  }

  #requestSignal(): AbortSignal {
    return this.#requestAbort.signal;
  }

  #evictBriefCache(): void {
    while (this.#briefOrder.length > this.#maxBriefCache) {
      const index = this.#briefOrder.findIndex(
        (id) => this.#briefStates.get(id)?.kind !== "loading",
      );
      if (index === -1) {
        break;
      }
      const evicted = this.#briefOrder.splice(index, 1)[0];
      if (evicted === undefined) {
        break;
      }
      this.#briefStates.delete(evicted);
    }
  }

  #setStatus(status: ConversationStatus): void {
    this.#status = status;
    this.#onEvent?.({ type: "status", status });
    this.#emitChange();
  }

  #emitView(
    reason:
      | "bootstrap"
      | "batch"
      | "older_page"
      | "detail_page"
      | "reset",
  ): void {
    this.#onEvent?.({ type: "view", reason });
    this.#emitChange();
  }

  #emitChange(): void {
    if (this.#disposed) {
      return;
    }
    for (const listener of this.#listeners) {
      listener();
    }
  }
}
