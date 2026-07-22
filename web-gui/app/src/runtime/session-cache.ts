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
  type CachedAgentSession,
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
  RuntimeBriefRecord,
  RuntimeMessageEnvelope,
  RuntimeTranscriptEntry,
} from "./types";
import type { RuntimeConnectionConfig } from "./types";

export type { CachedAgentSession };

const MAX_CACHED_AGENTS_PER_REMOTE = 50;
const MAX_EVENTS_PER_AGENT = 5000;
const WRITE_DEBOUNCE_MS = 2000;

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
): CachedAgentSession {
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
    cachedAt: Date.now(),
  };
}

/**
 * Hydrate a partial AgentSessionState from cached data.
 * The caller merges this into an emptyAgentSession() base.
 */
export function hydrateSessionFromCache(cached: CachedAgentSession): Partial<AgentSessionState> {
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
  const agent = cached.agentSummary as AgentSummary | undefined;
  const events = projection.eventSeqs
    .map((seq) => projection.eventsBySeq[seq])
    .filter((event): event is ProjectionEvent => Boolean(event));
  return {
    ...projection,
    cacheStatus: "hit",
    contentStatus: projection.eventSeqs.length ? "available" : "unknown",
    syncStatus: "stale",
    ...(agent
      ? { detail: {
          agent,
          timeline: deriveSessionTimeline(projection, "debug"),
          source: "http",
          events,
          eventLogEpoch: cached.eventLogEpoch,
          newestEventSeq: cached.newestSeq,
          oldestEventSeq: cached.oldestSeq,
        } }
      : {}),
  };
}

export async function hydrateAgentSession(
  remoteKey: string,
  agentId: string,
): Promise<Partial<AgentSessionState> | undefined> {
  const cached = await cacheGetSession(remoteKey, agentId);
  return cached ? hydrateSessionFromCache(cached) : undefined;
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
  private pending = new Map<string, { remoteKey: string; session: AgentSessionState }>();
  private readonly remoteKey: string;

  constructor(remoteKey: string) {
    this.remoteKey = remoteKey;
  }

  scheduleWrite(agentId: string, session: AgentSessionState): void {
    this.pending.set(agentId, { remoteKey: this.remoteKey, session });

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
      const cached = extractCacheableSession(entry.remoteKey, agentId, entry.session);
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
): Promise<Record<string, Partial<AgentSessionState>>> {
  const cached = await cacheGetAllSessions(remoteKey);
  const result: Record<string, Partial<AgentSessionState>> = {};
  for (const entry of cached) {
    result[entry.agentId] = hydrateSessionFromCache(entry);
  }
  return result;
}
