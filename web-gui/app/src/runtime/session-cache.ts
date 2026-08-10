/**
 * Business logic layer for session caching on top of idb-cache.
 *
 * Provides remote-key computation, extract/hydrate between AgentSessionState
 * and CachedAgentSession, a debounced writer, and eviction enforcement.
 */

import {
  CACHE_SCHEMA_VERSION,
  cacheDeleteSession,
  cacheGetAllSessions,
  cacheGetSession,
  cachePutSession,
  ensureCacheSchemaVersion,
  type CachedAgentReadState,
  type CachedAgentSession,
  type CachedSemanticHistoryState,
  type CachedSyncCoverage,
} from "./idb-cache";
import type { AgentSessionState } from "./runtime-store-helpers";
import {
  SESSION_PROJECTION_GENERATION,
  createSessionProjectionState,
  deriveSessionTimeline,
  reduceSessionProjection,
  type ProjectionEvent,
} from "./session-projection";
import type {
  AgentSummary,
  DisplayLevel,
  RuntimeBriefRecord,
  RuntimeMessageEnvelope,
  RuntimeTranscriptEntry,
} from "./types";
import type { RuntimeConnectionConfig } from "./types";

export type { CachedAgentSession };

const MAX_CACHED_AGENTS_PER_REMOTE = 50;
const MAX_EVENTS_PER_AGENT = 5000;
const WRITE_DEBOUNCE_MS = 2000;
const DISPLAY_LEVELS: DisplayLevel[] = ["info", "verbose", "debug"];

export interface HydratedAgentCache {
  session: Partial<AgentSessionState>;
  readState?: CachedAgentReadState;
}

export interface HydratedRemoteCache {
  sessionsByAgentId: Record<string, Partial<AgentSessionState>>;
  readStateByAgentId: Record<string, CachedAgentReadState>;
}

function cachedAgentSummary(
  value: unknown,
  expectedAgentId: string,
): AgentSummary | undefined {
  if (!value || typeof value !== "object") return undefined;
  const agent = value as Partial<AgentSummary>;
  return agent.id === expectedAgentId ? (agent as AgentSummary) : undefined;
}

/**
 * Compute the isolation key for a remote connection.
 * Local mode uses "local"; remote mode uses the normalized baseUrl.
 */
export function currentRemoteKey(config: RuntimeConnectionConfig): string {
  if (config.mode === "local") return "local";
  return config.baseUrl?.trim().replace(/\/+$/, "") || "remote";
}

/**
 * Extract cacheable fields from an AgentSessionState into a CachedAgentSession.
 * Only the "heavy" data is cached: events, messages, transcripts, briefs.
 * UI state (loading, liveStatus, etc.) is excluded.
 */
export function extractCacheableSession(
  remoteKey: string,
  agentId: string,
  session: AgentSessionState,
  readState?: CachedAgentReadState,
): CachedAgentSession {
  const syncCoverage = extractSyncCoverage(session);

  // Trim events if exceeding the per-agent limit (keep the newest).
  let eventsBySeq = session.eventsBySeq;
  let eventSeqs = session.eventSeqs;
  if (eventSeqs.length > MAX_EVENTS_PER_AGENT) {
    const keepSeqs = eventSeqs.slice(eventSeqs.length - MAX_EVENTS_PER_AGENT);
    const keepSet = new Set(keepSeqs);
    eventsBySeq = {};
    for (const seq of keepSeqs) {
      if (keepSet.has(seq)) {
        eventsBySeq[seq] = session.eventsBySeq[seq];
      }
    }
    eventSeqs = keepSeqs;
  }

  return {
    remoteKey,
    agentId,
    schemaVersion: CACHE_SCHEMA_VERSION,
    projectionGeneration: session.generation,
    agentSummary: session.detail?.agent,
    eventLogEpoch: session.eventLogEpoch,
    eventsBySeq,
    eventSeqs,
    messagesById: session.messagesById as Record<string, unknown>,
    transcriptEntriesById: session.transcriptEntriesById as Record<string, unknown>,
    briefRecordsById: session.briefRecordsById as Record<string, unknown>,
    newestSeq: eventSeqs.at(-1) ?? session.newestSeq,
    oldestSeq: eventSeqs[0] ?? session.oldestSeq,
    syncCoverage: {
      ...syncCoverage,
      retainedOldestSeq: eventSeqs[0],
      retainedNewestSeq: eventSeqs.at(-1),
    },
    semanticHistoryByDisplayLevel: extractSemanticHistory(
      session.semanticHistoryByDisplayLevel,
      session.eventLogEpoch,
      eventSeqs[0],
    ),
    readState: coerceReadState(readState),
    cachedAt: Date.now(),
  };
}

function extractSyncCoverage(session: AgentSessionState): CachedSyncCoverage {
  const observedSeq = Math.max(session.newestSeq ?? 0, session.eventSeqs.at(-1) ?? 0);
  return {
    eventLogEpoch: session.eventLogEpoch,
    contiguousSeq: session.gaps[0]?.afterSeq ?? observedSeq,
    observedSeq,
    retainedOldestSeq: session.eventSeqs[0],
    retainedNewestSeq: session.eventSeqs.at(-1),
    gaps: session.gaps.map((gap) => ({ ...gap })),
  };
}

function extractSemanticHistory(
  histories: AgentSessionState["semanticHistoryByDisplayLevel"],
  eventLogEpoch: string | undefined,
  retainedOldestSeq: number | undefined,
): Record<string, CachedSemanticHistoryState> {
  const cached: Record<string, CachedSemanticHistoryState> = {};
  for (const displayLevel of DISPLAY_LEVELS) {
    const history = histories[displayLevel];
    if (!history || history.loading || history.error) continue;
    if (history.eventLogEpoch && eventLogEpoch && history.eventLogEpoch !== eventLogEpoch) continue;
    let cursorSeq = finiteSequence(history.cursorSeq);
    let hasOlder = history.hasOlder;
    if (cursorSeq != null && retainedOldestSeq != null && cursorSeq < retainedOldestSeq) {
      cursorSeq = retainedOldestSeq;
      hasOlder = true;
    }
    cached[displayLevel] = {
      eventLogEpoch: history.eventLogEpoch ?? eventLogEpoch,
      cursorSeq,
      hasOlder,
    };
  }
  return cached;
}

/**
 * Hydrate a partial AgentSessionState from cached data.
 * The caller merges this into an emptyAgentSession() base.
 */
export function hydrateSessionFromCache(cached: CachedAgentSession): Partial<AgentSessionState> {
  const coverage = coerceSyncCoverage(cached);
  if (!coverage) {
    return {
      ...createSessionProjectionState(cached.eventLogEpoch),
      invalidatedReason: "cache_integrity_mismatch",
      cacheStatus: "hit",
      contentStatus: "unknown",
      syncStatus: "stale",
      semanticHistoryByDisplayLevel: {},
    };
  }
  const projection = reduceSessionProjection(createSessionProjectionState(), {
    type: "cache_restored",
    generation: cached.projectionGeneration ?? SESSION_PROJECTION_GENERATION - 1,
    eventLogEpoch: cached.eventLogEpoch,
    eventsBySeq: cached.eventsBySeq as Record<number, ProjectionEvent>,
    eventSeqs: cached.eventSeqs,
    messagesById: cached.messagesById as Record<string, RuntimeMessageEnvelope>,
    transcriptEntriesById: cached.transcriptEntriesById as Record<string, RuntimeTranscriptEntry>,
    briefRecordsById: cached.briefRecordsById as Record<string, RuntimeBriefRecord>,
    newestSeq: cached.newestSeq,
    oldestSeq: cached.oldestSeq,
  });
  if (projection.invalidatedReason) {
    return {
      ...projection,
      cacheStatus: "hit",
      contentStatus: "unknown",
      syncStatus: "stale",
      semanticHistoryByDisplayLevel: {},
    };
  }
  const restoredProjection = {
    ...projection,
    eventLogEpoch: coverage.eventLogEpoch,
    gaps: coverage.gaps,
    newestSeq: coverage.observedSeq || undefined,
    oldestSeq: coverage.retainedOldestSeq,
  };
  const semanticHistoryByDisplayLevel = hydrateSemanticHistory(
    cached.semanticHistoryByDisplayLevel,
    coverage,
  );
  const agent = cachedAgentSummary(cached.agentSummary, cached.agentId);
  const events = restoredProjection.eventSeqs
    .map((seq) => restoredProjection.eventsBySeq[seq])
    .filter((event): event is ProjectionEvent => Boolean(event));
  return {
    ...restoredProjection,
    semanticHistoryByDisplayLevel,
    cacheStatus: "hit",
    contentStatus: restoredProjection.eventSeqs.length ? "available" : "unknown",
    syncStatus: "stale",
    ...(agent
      ? { detail: {
          agent,
          timeline: deriveSessionTimeline(restoredProjection, "debug"),
          source: "http",
          events,
          eventLogEpoch: restoredProjection.eventLogEpoch,
          newestEventSeq: restoredProjection.newestSeq,
          oldestEventSeq: restoredProjection.oldestSeq,
        } }
      : {}),
  };
}

export async function hydrateAgentSession(
  remoteKey: string,
  agentId: string,
): Promise<HydratedAgentCache | undefined> {
  const cached = await cacheGetSession(remoteKey, agentId);
  return cached
    ? {
        session: hydrateSessionFromCache(cached),
        readState: coerceReadState(cached.readState),
      }
    : undefined;
}

function coerceSyncCoverage(cached: CachedAgentSession): CachedSyncCoverage | undefined {
  const coverage = cached.syncCoverage;
  if (!coverage || typeof coverage !== "object") return undefined;
  const observedSeq = finiteSequence(coverage.observedSeq);
  const contiguousSeq = finiteSequence(coverage.contiguousSeq);
  const retainedOldestSeq = finiteSequence(coverage.retainedOldestSeq);
  const retainedNewestSeq = finiteSequence(coverage.retainedNewestSeq);
  const actualOldestSeq = finiteSequence(cached.eventSeqs[0]);
  const actualNewestSeq = finiteSequence(cached.eventSeqs.at(-1));
  if (
    observedSeq == null ||
    contiguousSeq == null ||
    contiguousSeq > observedSeq ||
    retainedOldestSeq !== actualOldestSeq ||
    retainedNewestSeq !== actualNewestSeq ||
    (actualNewestSeq != null && actualNewestSeq > observedSeq) ||
    coverage.eventLogEpoch !== cached.eventLogEpoch
  ) {
    return undefined;
  }
  if (!Array.isArray(coverage.gaps)) return undefined;
  const gaps = coverage.gaps.flatMap((gap) => {
    const afterSeq = finiteSequence(gap?.afterSeq);
    const beforeSeq = finiteSequence(gap?.beforeSeq);
    return afterSeq != null && beforeSeq != null && afterSeq < beforeSeq && beforeSeq <= observedSeq
      ? [{ afterSeq, beforeSeq }]
      : [];
  });
  if (gaps.length !== coverage.gaps.length) return undefined;
  for (let index = 1; index < gaps.length; index += 1) {
    if (gaps[index - 1].beforeSeq > gaps[index].afterSeq) return undefined;
  }
  if ((gaps[0]?.afterSeq ?? observedSeq) !== contiguousSeq) return undefined;
  return {
    eventLogEpoch: coverage.eventLogEpoch,
    contiguousSeq,
    observedSeq,
    retainedOldestSeq,
    retainedNewestSeq,
    gaps,
  };
}

function hydrateSemanticHistory(
  cached: CachedAgentSession["semanticHistoryByDisplayLevel"],
  coverage: CachedSyncCoverage,
): AgentSessionState["semanticHistoryByDisplayLevel"] {
  if (!cached || typeof cached !== "object") return {};
  const histories: AgentSessionState["semanticHistoryByDisplayLevel"] = {};
  for (const displayLevel of DISPLAY_LEVELS) {
    const value = cached[displayLevel];
    if (!value || typeof value !== "object" || typeof value.hasOlder !== "boolean") continue;
    if (value.eventLogEpoch !== coverage.eventLogEpoch) continue;
    let cursorSeq = finiteSequence(value.cursorSeq);
    let hasOlder = value.hasOlder;
    if (cursorSeq != null && cursorSeq > coverage.observedSeq) continue;
    if (
      cursorSeq != null &&
      coverage.retainedOldestSeq != null &&
      cursorSeq < coverage.retainedOldestSeq
    ) {
      cursorSeq = coverage.retainedOldestSeq;
      hasOlder = true;
    }
    histories[displayLevel] = {
      eventLogEpoch: coverage.eventLogEpoch,
      cursorSeq,
      hasOlder,
      loading: false,
    };
  }
  return histories;
}

function coerceReadState(value: CachedAgentReadState | undefined): CachedAgentReadState | undefined {
  if (!value || typeof value !== "object") return undefined;
  const readState: CachedAgentReadState = {};
  const unreadCount = finiteNonNegative(value.unreadCount);
  const lastUnreadDeliverySeq = finiteSequence(value.lastUnreadDeliverySeq);
  const lastReadDeliverySeq = finiteSequence(value.lastReadDeliverySeq);
  if (unreadCount != null) readState.unreadCount = unreadCount;
  if (lastUnreadDeliverySeq != null) readState.lastUnreadDeliverySeq = lastUnreadDeliverySeq;
  if (lastReadDeliverySeq != null) readState.lastReadDeliverySeq = lastReadDeliverySeq;
  return Object.keys(readState).length ? readState : undefined;
}

function finiteSequence(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? Math.floor(value)
    : undefined;
}

function finiteNonNegative(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? Math.floor(value)
    : undefined;
}

/**
 * Enforce the max-agents-per-remote limit by deleting oldest entries.
 */
export async function enforceCacheLimits(remoteKey: string): Promise<void> {
  const sessions = await cacheGetAllSessions(remoteKey);
  if (sessions.length <= MAX_CACHED_AGENTS_PER_REMOTE) return;

  sessions.sort((a, b) => a.cachedAt - b.cachedAt);
  const toDelete = sessions.slice(0, sessions.length - MAX_CACHED_AGENTS_PER_REMOTE);
  await Promise.all(toDelete.map((s) => cacheDeleteSession(s.remoteKey, s.agentId)));
}

/**
 * Debounced cache writer for session updates.
 * Schedules writes at most once per WRITE_DEBOUNCE_MS per agent.
 */
export class SessionCacheWriter {
  private timers = new Map<string, number>();
  private pending = new Map<string, {
    remoteKey: string;
    session: AgentSessionState;
    readState?: CachedAgentReadState;
  }>();
  private readonly remoteKey: string;

  constructor(remoteKey: string) {
    this.remoteKey = remoteKey;
  }

  scheduleWrite(
    agentId: string,
    session: AgentSessionState,
    readState?: CachedAgentReadState,
  ): void {
    this.pending.set(agentId, { remoteKey: this.remoteKey, session, readState });

    const existing = this.timers.get(agentId);
    if (existing != null) {
      globalThis.clearTimeout(existing);
    }

    const timer = globalThis.setTimeout(() => {
      this.timers.delete(agentId);
      void this.flushAgent(agentId);
    }, WRITE_DEBOUNCE_MS);
    this.timers.set(agentId, timer);
  }

  private async flushAgent(agentId: string): Promise<void> {
    const entry = this.pending.get(agentId);
    if (!entry) return;
    this.pending.delete(agentId);
    try {
      const cached = extractCacheableSession(
        entry.remoteKey,
        agentId,
        entry.session,
        entry.readState,
      );
      await cachePutSession(cached);
    } catch {
      // Silent fallback.
    }
  }

  /** Flush all pending writes immediately. */
  async flush(): Promise<void> {
    for (const timer of this.timers.values()) {
      globalThis.clearTimeout(timer);
    }
    this.timers.clear();

    const agentIds = Array.from(this.pending.keys());
    await Promise.all(agentIds.map((id) => this.flushAgent(id)));
  }

  /** Stop all pending writes without flushing. */
  cancel(): void {
    for (const timer of this.timers.values()) {
      globalThis.clearTimeout(timer);
    }
    this.timers.clear();
    this.pending.clear();
  }

  discard(agentId: string): void {
    const timer = this.timers.get(agentId);
    if (timer != null) globalThis.clearTimeout(timer);
    this.timers.delete(agentId);
    this.pending.delete(agentId);
  }
}

/**
 * One-time initialization: check schema version and prepare for hydration.
 * Returns true if the cache is usable.
 */
export async function initSessionCache(): Promise<boolean> {
  const compatible = await ensureCacheSchemaVersion();
  return compatible || ensureCacheSchemaVersion();
}

/**
 * Load all cached sessions for a remote key and return them as a map
 * of agentId -> partial AgentSessionState.
 */
export async function hydrateAllSessions(
  remoteKey: string,
): Promise<HydratedRemoteCache> {
  const cached = await cacheGetAllSessions(remoteKey);
  const sessionsByAgentId: Record<string, Partial<AgentSessionState>> = {};
  const readStateByAgentId: Record<string, CachedAgentReadState> = {};
  for (const entry of cached) {
    sessionsByAgentId[entry.agentId] = hydrateSessionFromCache(entry);
    const readState = coerceReadState(entry.readState);
    if (readState) readStateByAgentId[entry.agentId] = readState;
  }
  return { sessionsByAgentId, readStateByAgentId };
}
