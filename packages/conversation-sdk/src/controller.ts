import type { ConversationClient } from "./client.js";
import { ConversationBrowseWindow, type BrowsePage } from "./browse.js";
import {
  ConversationCapabilityError,
  ConversationCompatibilityError,
  ConversationDecodeError,
  ConversationHttpError,
  ConversationProtocolError,
  ConversationTimeoutError,
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
  type ConversationResetReason,
  type ConversationSnapshotCacheEntry,
  type ConversationStateLimits,
  type ConversationStreamItem,
  type ConversationSummaryResult,
  type ConversationSummaryResponse,
  type ConversationLocateResponse,
  type ConversationLocateTarget,
  type ConversationStreamBatch,
} from "./types.js";

/**
 * Structural client surface the controller depends on. `ConversationClient`
 * satisfies it; tests may substitute fakes.
 */
export interface ConversationClientLike {
  readonly baseUrl: string;
  /** SDK-owned validator; only use after exact capability negotiation. */
  withIncarnationFence?(identity: import("./types.js").ConversationIdentity): ConversationClientLike;
  requireCapability(signal?: AbortSignal): Promise<unknown>;
  summary(
    agentId: string,
    options?: {
      readonly limit?: number;
      readonly before?: ConversationHistoryCursor;
      readonly after?: ConversationHistoryCursor;
      readonly ifNoneMatch?: string;
      readonly signal?: AbortSignal;
    },
  ): Promise<ConversationSummaryResult>;
  locateSource?(
    agentId: string,
    sourceRef: string,
    options?: { readonly contextLimit?: number; readonly signal?: AbortSignal },
  ): Promise<ConversationLocateResponse>;
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
  | { readonly kind: "paused" }
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

export interface ConversationBrowseState {
  readonly sourceRef: string;
  readonly target: ConversationLocateTarget | null;
  readonly status: "resolving" | "ready" | "source_only" | "error";
  readonly older: ConversationHistoryLoadState;
  readonly newer: ConversationHistoryLoadState;
  readonly hasOlder: boolean;
  readonly hasNewer: boolean;
  readonly hasLiveUpdates: boolean;
  readonly liveConnected: boolean;
  readonly error?: unknown;
  readonly reason?: string;
}

/** UI-facing location lifecycle; pagination remains available through browseState(). */
export type ConversationLocateState =
  | { readonly status: "idle" }
  | (Omit<ConversationBrowseState, "status"> & {
      readonly status: "loading" | "ready" | "source_only" | "error";
    });

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
  if (error instanceof ConversationTimeoutError) {
    return "retryable";
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
  clear?(): Promise<void>;
}

/**
 * Best-effort persistent cache of the most recent bootstrap snapshot for one
 * agent scope. Implementations must tolerate rejections without throwing
 * into the controller lifecycle.
 */
export interface ConversationSnapshotCache {
  load(): Promise<ConversationSnapshotCacheEntry | null | undefined>;
  store(entry: ConversationSnapshotCacheEntry): Promise<void>;
  clear?(): Promise<void>;
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
        | "locate"
        | "newer_page"
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

function browsePage(summary: ConversationSummaryResponse): BrowsePage {
  const window = summary.coverage;
  if (!window) throw new ConversationProtocolError("server omitted the bidirectional conversation window");
  if (summary.turns.length > 0 && window.membership_upper_bound == null) {
    throw new ConversationProtocolError("server omitted the conversation membership upper bound");
  }
  if (window.has_older !== (summary.next_before_cursor !== null) || window.has_newer !== (summary.next_after_cursor != null)
    || window.reaches_oldest === window.has_older || window.reaches_latest === window.has_newer) {
    throw new ConversationProtocolError("conversation window cursor coverage is inconsistent");
  }
  return {
    summary, before: summary.next_before_cursor, after: summary.next_after_cursor ?? null,
    hasOlder: window.has_older, hasNewer: window.has_newer,
  };
}

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
  #snapshotBase: ConversationSummaryResponse | undefined;
  #snapshotTimer: ReturnType<typeof setTimeout> | undefined;
  #snapshotWrites: Promise<void> = Promise.resolve();
  #cacheGeneration = 0;
  #capabilityReady = false;
  #browse: ConversationBrowseState | null = null;
  #browseWindow: ConversationBrowseWindow | null = null;
  #browseGeneration = 0;
  #browseAbort = new AbortController();
  readonly #browseBatches: ConversationStreamBatch[] = [];

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

  /** Selected list data; historical windows never carry read authority. */
  displayView(): ConversationStateView {
    return this.browsingView() ?? this.view();
  }

  /** Null until a location resolves to an independent history window. */
  browsingView(): ConversationStateView | null {
    if (this.#browseWindow !== null) {
      const view = this.#browseWindow.view();
      return {
        ...view,
        next_before_cursor: this.#browseWindow.before,
        has_more: this.#browseWindow.hasOlder,
        // Browsing never grants authority to advance the read/stream cursor.
        checkpoint: null,
        through_seq: null,
      };
    }
    return null;
  }

  historyState(): ConversationHistoryLoadState {
    return this.#browse?.older ?? this.#historyState;
  }

  browseState(): ConversationBrowseState | null {
    return this.#browse;
  }

  locateState(): ConversationLocateState {
    const browse = this.#browse;
    if (browse === null) return { status: "idle" };
    return { ...browse, status: browse.status === "resolving" ? "loading" : browse.status };
  }

  async locateSource(sourceRef: string): Promise<ConversationBrowseState | null> {
    if (this.#disposed) throw new ConversationProtocolError("controller is disposed");
    this.#clearBrowse();
    const generation = this.#browseGeneration;
    const scope = this.#state.view().scope;
    this.#browse = {
      sourceRef, target: null, status: "resolving",
      older: { kind: "idle" }, newer: { kind: "idle" },
      hasOlder: false, hasNewer: false, hasLiveUpdates: false, liveConnected: false,
    };
    this.#emitChange();
    try {
      if (sourceRef.length === 0) throw new ConversationProtocolError("sourceRef must not be empty");
      if (!this.#client.locateSource) throw new ConversationCapabilityError("agents.conversation-locate.v1");
      if (!scope) throw new ConversationProtocolError("locate requires a bootstrapped conversation");
      const response = await this.#client.locateSource(this.agentId, sourceRef, {
        contextLimit: Math.min(this.#historyPageSize, this.limits.max_turns),
        signal: this.#browseAbort.signal,
      });
      if (!this.#browseAlive(generation)) return null;
      if (response.status === "source_only") {
        const currentScope = this.#state.view().scope;
        if (!currentScope || response.runtime_id !== currentScope.runtime_id
          || response.event_log_epoch !== currentScope.event_log_epoch
          || response.visibility_scope_id !== currentScope.visibility_scope_id) {
          throw new ConversationStaleResponseError("source-only response belongs to a stale conversation scope");
        }
        this.#browse = { ...this.#browse!, status: "source_only", reason: response.reason };
        this.#emitChange();
        return this.#browse;
      }
      const snapshot = response.window;
      this.#assertBrowseScope(snapshot);
      const pendingTargetId = response.target.kind === "pending_input" ? response.target.message_id : null;
      const window = new ConversationBrowseWindow(this.#identity, this.limits, browsePage({
        ...snapshot,
        pending_inputs: pendingTargetId !== null
          ? snapshot.pending_inputs.filter((input) => input.message_id === pendingTargetId)
          : [],
      }), response.target);
      this.#browseWindow = window;
      this.#browse = { ...this.#browse!, target: response.target, status: "ready" };
      for (const batch of this.#browseBatches) this.#applyBrowseBatch(batch);
      this.#tryJoinBrowse();
      this.#refreshBrowse();
      this.#emitView("locate");
      if (response.target.kind === "brief") void this.loadBrief(response.target.brief_id);
      if (response.target.kind === "activity" || response.target.kind === "input") void this.loadDetail(response.target.turn_id);
      return this.#browse;
    } catch (error) {
      if (!this.#browseAlive(generation)) return null;
      this.#discardUnauthorizedCache(error);
      if (!this.#browseAlive(generation)) return null;
      this.#browse = { ...this.#browse!, status: "error", error };
      this.#emitChange();
      return this.#browse;
    }
  }

  returnToLatest(): void {
    this.#clearBrowse();
    this.#emitChange();
  }

  async loadNewerHistory(): Promise<ConversationHistoryLoadState> {
    return this.#loadBrowsePage("newer");
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
      if (!this.#capabilityReady) this.#handshakePromise = null;
      this.#runAbort = new AbortController();
      this.#starting = false;
    }
    this.start();
  }

  /** Retain data and checkpoint while releasing the stream. */
  pause(): void {
    if (this.#disposed) return;
    this.#flushSnapshot();
    this.#runToken += 1;
    this.#runAbort?.abort(abortError("conversation paused"));
    if (!this.#capabilityReady) this.#handshakePromise = null;
    this.#runAbort = null;
    this.#starting = false;
    this.#setStatus({ kind: "paused" });
  }

  /** Cancel every request, stream, timer, and subscription notification. */
  dispose(persist = true): void {
    if (this.#disposed) {
      return;
    }
    if (persist) this.#flushSnapshot();
    else { this.#cancelSnapshotTimer(); this.#cacheGeneration += 1; }
    this.#disposed = true;
    this.#runToken += 1;
    this.#runAbort?.abort(abortError("controller disposed"));
    this.#requestAbort.abort(abortError("controller disposed"));
    this.#clearBrowse();
    this.#runAbort = null;
    this.#listeners.clear();
  }

  async loadOlderHistory(): Promise<ConversationHistoryLoadState> {
    if (this.#browse !== null) return this.#loadBrowsePage("older");
    if (this.#disposed) {
      return {
        kind: "error",
        retryable: false,
        error: new ConversationProtocolError("controller is disposed"),
      };
    }
    const generation = this.#cacheGeneration;
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
      if (this.#disposed || generation !== this.#cacheGeneration) {
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
      if (this.#disposed || generation !== this.#cacheGeneration) return { kind: "idle" };
      return this.#absorbHistoryError(error);
    }
  }

  async loadDetail(turnId: string): Promise<ConversationDetailLoadState> {
    return this.#requestDetail(turnId, undefined);
  }

  async loadOlderActivities(
    turnId: string,
  ): Promise<ConversationDetailLoadState> {
    const view = this.displayView();
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
    const generation = this.#cacheGeneration;
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
        cached = await readCache(() => this.#briefCache!.get(briefId));
      } catch {
        // Persistent cache read failure falls through to the network.
      }
      if (this.#disposed || generation !== this.#cacheGeneration) {
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
      if (this.#disposed || generation !== this.#cacheGeneration) {
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
      if (this.#disposed || generation !== this.#cacheGeneration) {
        return { kind: "loading" };
      }
      this.#discardUnauthorizedCache(error);
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

  #browseAlive(generation: number): boolean {
    return !this.#disposed && generation === this.#browseGeneration && this.#browse !== null;
  }

  #clearBrowse(): void {
    this.#browseGeneration += 1;
    this.#browseAbort.abort(abortError("conversation navigation superseded"));
    this.#browseAbort = new AbortController();
    this.#browseWindow = null;
    this.#browse = null;
    this.#browseBatches.length = 0;
    this.#detailStates.clear();
  }

  #invalidateBrowse(error: unknown, clearWindow = false): void {
    if (!this.#browse) return;
    const previous = this.#browse;
    const window = this.#browseWindow;
    this.#clearBrowse();
    if (!clearWindow) this.#browseWindow = window;
    this.#browse = { ...previous, status: "error", error, liveConnected: false };
  }

  #applyBrowseBatch(batch: ConversationStreamBatch): void {
    const target = this.#browse?.target;
    this.#browseWindow?.applyBatch(target?.kind === "pending_input" ? {
      ...batch,
      // A pending message keeps its identity until canonical assignment is
      // observable; a transient remove must not make navigation target vanish.
      mutations: batch.mutations.filter((mutation) => mutation.type !== "operator_remove" || mutation.message_id !== target.message_id),
    } : batch);
  }

  #assertBrowseScope(snapshot: ConversationSummaryResponse): void {
    const scope = this.#state.view().scope;
    if (!scope || this.#state.view().reset_reason !== null) throw new ConversationStaleResponseError("conversation reset during locate");
    for (const field of ["runtime_id", "event_log_epoch", "visibility_scope_id", "schema_version", "query_version"] as const) {
      if (scope[field] !== snapshot[field]) throw new ConversationStaleResponseError(`locate ${field} changed`);
    }
  }

  #refreshBrowse(): void {
    if (!this.#browse || !this.#browseWindow) return;
    const window = this.#browseWindow;
    this.#browse = {
      ...this.#browse,
      hasOlder: window.hasOlder, hasNewer: window.hasNewer,
      liveConnected: window.joined && this.#browse.status === "ready",
      hasLiveUpdates: (this.#state.view().through_seq ?? 0) > window.snapshot.snapshot_through_seq && !window.joined,
      older: window.hasOlder || this.#browse.older.kind === "error" ? this.#browse.older : { kind: "complete" },
      newer: window.hasNewer || this.#browse.newer.kind === "error" ? this.#browse.newer : { kind: "complete" },
    };
  }

  #tryJoinBrowse(): void {
    if (!this.#browseWindow || this.#browseWindow.joined || this.#browseWindow.hasNewer) return;
    const tail = this.#state.view();
    if (tail.through_seq !== null) this.#browseWindow.join(this.#browseBatches, tail.through_seq, tail.pending_inputs);
  }

  async #loadBrowsePage(direction: "older" | "newer"): Promise<ConversationHistoryLoadState> {
    const browse = this.#browse;
    const window = this.#browseWindow;
    if (!browse || !window || browse.status !== "ready") return { kind: "idle" };
    if (browse[direction].kind === "loading") return browse[direction];
    const cursor = direction === "older" ? window.before : window.after;
    const hasMore = direction === "older" ? window.hasOlder : window.hasNewer;
    if (!cursor || !hasMore) return { kind: "complete" };
    const generation = this.#browseGeneration;
    this.#browse = { ...browse, [direction]: { kind: "loading" } };
    this.#emitChange();
    try {
      const result = await this.#client.summary(this.agentId, {
        limit: Math.min(this.#historyPageSize, this.limits.max_turns),
        ...(direction === "older" ? { before: cursor } : { after: cursor }),
        signal: this.#browseAbort.signal,
      });
      if (!this.#browseAlive(generation)) return { kind: "idle" };
      if (!result.summary) throw new ConversationProtocolError("browse page returned 304");
      this.#assertBrowseScope(result.summary);
      if (cursor !== (direction === "older" ? window.before : window.after)) {
        this.#browse = { ...this.#browse!, [direction]: { kind: "idle" } };
        this.#emitChange();
        return { kind: "idle" };
      }
      window.append(browsePage(result.summary), direction);
      for (const batch of this.#browseBatches) {
        if (batch.begin.through_seq > result.summary.snapshot_through_seq) window.state.refreshWindow(this.#identity, batch);
      }
      this.#tryJoinBrowse();
      this.#browse = { ...this.#browse!, [direction]: { kind: "idle" } };
      this.#refreshBrowse();
      this.#emitView(direction === "older" ? "older_page" : "newer_page");
      return this.#browse![direction];
    } catch (error) {
      if (!this.#browseAlive(generation)) return { kind: "idle" };
      this.#discardUnauthorizedCache(error);
      if (!this.#browseAlive(generation)) return { kind: "idle" };
      const kind = classifyConversationError(error);
      const next: ConversationHistoryLoadState = { kind: "error", retryable: kind === "retryable" || kind === "recoverable", error };
      if (error instanceof ConversationStaleResponseError || error instanceof ConversationResetError) {
        this.#invalidateBrowse(error);
      }
      this.#browse = { ...this.#browse!, [direction]: next };
      this.#refreshBrowse();
      this.#emitChange();
      return next;
    }
  }

  #absorbHistoryError(error: unknown): ConversationHistoryLoadState {
    this.#discardUnauthorizedCache(error);
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
    const generation = this.#cacheGeneration;
    const navigation = this.#browseGeneration;
    const state = this.#browseWindow?.state ?? this.#state;
    const view = state.view();
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
        signal: this.#browseWindow ? this.#browseAbort.signal : this.#requestSignal(),
      });
      if (this.#disposed || generation !== this.#cacheGeneration) {
        return { kind: "idle" };
      }
      if (navigation !== this.#browseGeneration) return { kind: "idle" };
      state.applyDetailPage(this.#identity, turnId, before, page);
      this.#detailStates.set(turnId, { kind: "idle" });
      this.#emitView("detail_page");
      return { kind: "idle" };
    } catch (error) {
      if (navigation !== this.#browseGeneration) return { kind: "idle" };
      if (this.#disposed || generation !== this.#cacheGeneration) {
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
      this.#discardUnauthorizedCache(error);
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
        if (this.#state.reconnectCheckpoint() === null && !this.#hydrateAttempted) {
          // Paint the persisted snapshot before any network I/O so a cached
          // conversation renders without waiting for the capability
          // handshake. Hydration validates schema/query versions locally,
          // and every network step below still runs after the handshake.
          this.#setStatus({ kind: "loading" });
          this.#hydrateAttempted = true;
          await this.#hydrateCachedSnapshot(runToken);
          if (!this.#alive(runToken)) {
            return;
          }
        }
        await this.#ensureCapability();
        if (!this.#alive(runToken)) {
          return;
        }
        if (this.#state.reconnectCheckpoint() === null) {
          this.#setStatus({ kind: "loading" });
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
        if (!this.#alive(runToken)) return;
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
        this.#discardUnauthorizedCache(error, true);
        // A stream attach rejected with a reset means the local checkpoint
        // can never be resumed (retention pruning, replay limit,
        // epoch/schema change): retrying the same checkpoint
        // deterministically fails again. Drop the stale state so the next
        // loop takes the fresh-snapshot path, the same self-heal an
        // in-stream reset_required message receives.
        if (
          error instanceof ConversationResetError &&
          error.reason !== "agent_not_found"
        ) {
          this.#applyStreamReset(error.reason);
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
    const handshake = this.#handshakePromise;
    try {
      await handshake;
      if (this.#handshakePromise === handshake) this.#capabilityReady = true;
    } catch (error) {
      // An aborted previous run must not erase its successor's handshake.
      if (this.#handshakePromise === handshake) this.#handshakePromise = null;
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
      entry = await readCache(() => this.#snapshotCache!.load());
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
    this.#snapshotBase = snapshot;
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
    this.#cancelSnapshotTimer();
    this.#snapshotBase = result.summary;
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
    this.#snapshotBase = snapshot;
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
    const cache = this.#snapshotCache;
    const generation = this.#cacheGeneration;
    this.#snapshotWrites = this.#snapshotWrites.then(async () => {
      if (generation === this.#cacheGeneration) await cache.store({ etag, summary: snapshot });
    }).catch(() => {});
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
        this.#applyStreamReset(item.reset.reason);
        return;
      }
      try {
        if (this.#state.applyBatch(this.#identity, item.batch)) {
          if (this.#browse !== null && this.#browse.status !== "error" && this.#browse.status !== "source_only") {
            this.#browseBatches.push(item.batch);
            if (this.#browseBatches.length > 256) this.#browseBatches.shift();
            try {
              this.#applyBrowseBatch(item.batch);
              this.#tryJoinBrowse();
              this.#refreshBrowse();
              const target = this.#browse.target;
              if (target?.kind === "pending_input" && item.batch.mutations.some((mutation) =>
                mutation.type === "turn_summary_upsert" && mutation.turn.inputs.some((input) => input.message_id === target.message_id))) {
                void this.locateSource(this.#browse.sourceRef);
              }
            } catch (error) {
              this.#invalidateBrowse(error);
            }
          }
          this.#emitView("batch");
          if (item.batch.mutations.some((mutation) => mutation.type === "turn_summary_upsert" && mutation.turn.execution.kind === "terminal")) {
            this.#flushSnapshot();
          }
        }
      } catch (error) {
        if (error instanceof ConversationStaleResponseError) {
          // Divergence between checkpoint and server framing: self-heal by
          // clearing state so the supervise loop re-snapshots serially.
          this.#applyStreamReset("stream_recovery_failed");
          return;
        }
        throw error;
      }
    }
  }

  #applyStreamReset(reason: ConversationResetReason): void {
    this.#invalidateBrowse(new ConversationResetError(reason), true);
    this.#cancelSnapshotTimer();
    this.#snapshotBase = undefined;
    this.#pendingRevalidate = undefined;
    this.#state.reset(reason);
    this.#emitView("reset");
  }

  #cancelSnapshotTimer(): void {
    if (this.#snapshotTimer !== undefined) clearTimeout(this.#snapshotTimer);
    this.#snapshotTimer = undefined;
  }

  #flushSnapshot(): void {
    if (this.#snapshotTimer === undefined) return;
    this.#cancelSnapshotTimer();
    const view = this.#state.view();
    if (!this.#snapshotBase || !view.scope || !view.checkpoint || view.through_seq === null || view.reset_reason) return;
    this.#persistSnapshot({
      ...this.#snapshotBase,
      snapshot_cursor: view.checkpoint,
      snapshot_through_seq: view.through_seq,
      event_head_seq: view.through_seq,
      turns: view.turns,
      active_turns: view.turns.filter((turn) => turn.execution.kind === "active"),
      pending_inputs: view.pending_inputs,
      next_before_cursor: view.next_before_cursor,
      has_more: view.has_more,
    }, null); // A locally updated view must never reuse the server body's ETag.
  }

  #discardUnauthorizedCache(error: unknown, includeNotFound = false): void {
    if (!(error instanceof ConversationHttpError) ||
        !([401, 403].includes(error.status) || (includeNotFound && error.status === 404) ||
          error.body.code === "conversation_identity_changed")) return;
    this.#cancelSnapshotTimer();
    this.#runToken += 1;
    this.#runAbort?.abort(abortError("conversation access revoked"));
    this.#starting = false;
    this.#handshakePromise = null;
    this.#capabilityReady = false;
    this.#cacheGeneration += 1;
    this.#clearBrowse();
    this.#snapshotBase = undefined;
    this.#pendingRevalidate = undefined;
    this.#state.reset("stream_recovery_failed");
    this.#briefStates.clear();
    this.#briefOrder.length = 0;
    this.#detailStates.clear();
    this.#requestAbort.abort(abortError("conversation access revoked"));
    this.#requestAbort = new AbortController();
    this.#snapshotWrites = this.#snapshotWrites.then(async () => {
      await this.#snapshotCache?.clear?.();
    }).catch(() => {});
    void this.#briefCache?.clear?.().catch(() => {});
    this.#emitView("reset");
    this.#setStatus({ kind: "recoverable_error", error });
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
      | "locate"
      | "newer_page"
      | "reset",
  ): void {
    if (["batch", "older_page", "detail_page"].includes(reason) && this.#pendingRevalidate === undefined
      && this.#snapshotCache && this.#snapshotTimer === undefined) {
      this.#snapshotTimer = setTimeout(() => this.#flushSnapshot(), 750);
    }
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

/** Cache availability must never gate network recovery indefinitely. */
async function readCache<T>(read: () => Promise<T>): Promise<T | undefined> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([read(), new Promise<undefined>((resolve) => {
      timer = setTimeout(() => resolve(undefined), 1500);
    })]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}
