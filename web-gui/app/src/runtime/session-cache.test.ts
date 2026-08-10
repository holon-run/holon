import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  currentRemoteKey,
  extractCacheableSession,
  hydrateAllSessions,
  hydrateSessionFromCache,
  SessionCacheWriter,
  enforceCacheLimits,
} from "./session-cache";
import type { AgentSessionState } from "./runtime-store-helpers";
import { CACHE_SCHEMA_VERSION, type CachedAgentSession } from "./idb-cache";
import {
  SESSION_PROJECTION_GENERATION,
  createSessionProjectionState,
  type ProjectionEvent,
} from "./session-projection";

function makeSession(overrides: Partial<AgentSessionState> = {}): AgentSessionState {
  return {
    ...createSessionProjectionState(),
    loading: false,
    semanticHistoryByDisplayLevel: {},
    targetEventLoading: false,
    liveStatus: "idle",
    cacheStatus: "unchecked",
    contentStatus: "unknown",
    syncStatus: "idle",
    sendingPrompt: false,
    detail: null,
    workItemDetailsById: {},
    taskDetailsById: {},
    toolExecutionDetailsById: {},
    ...overrides,
  };
}

function makeCachedSession(
  overrides: Partial<CachedAgentSession> = {},
): CachedAgentSession {
  const eventSeqs = overrides.eventSeqs ?? [];
  const eventLogEpoch = overrides.eventLogEpoch;
  const newestSeq = overrides.newestSeq ?? eventSeqs.at(-1);
  return {
    remoteKey: "local",
    agentId: "agent-1",
    schemaVersion: CACHE_SCHEMA_VERSION,
    projectionGeneration: SESSION_PROJECTION_GENERATION,
    eventLogEpoch,
    eventsBySeq: {},
    eventSeqs,
    messagesById: {},
    transcriptEntriesById: {},
    briefRecordsById: {},
    newestSeq,
    oldestSeq: overrides.oldestSeq ?? eventSeqs[0],
    syncCoverage: {
      eventLogEpoch,
      contiguousSeq: newestSeq ?? 0,
      observedSeq: newestSeq ?? 0,
      retainedOldestSeq: eventSeqs[0],
      retainedNewestSeq: eventSeqs.at(-1),
      gaps: [],
    },
    cachedAt: Date.now(),
    ...overrides,
  };
}

describe("currentRemoteKey", () => {
  it("returns 'local' for local mode", () => {
    expect(currentRemoteKey({ mode: "local" })).toBe("local");
  });

  it("returns normalized baseUrl for remote mode", () => {
    expect(currentRemoteKey({ mode: "remote", baseUrl: "https://example.com/" })).toBe("https://example.com");
    expect(currentRemoteKey({ mode: "remote", baseUrl: "https://example.com///" })).toBe("https://example.com");
  });

  it("returns 'remote' for empty baseUrl", () => {
    expect(currentRemoteKey({ mode: "remote", baseUrl: "" })).toBe("remote");
    expect(currentRemoteKey({ mode: "remote", baseUrl: undefined })).toBe("remote");
  });
});

describe("extractCacheableSession", () => {
  it("extracts core data fields with correct metadata", () => {
    const session = makeSession({
      eventLogEpoch: "epoch-1",
      eventsBySeq: { 1: { id: "e1" }, 2: { id: "e2" } },
      eventSeqs: [1, 2],
      messagesById: { m1: { id: "m1" } },
      newestSeq: 2,
      oldestSeq: 1,
    });

    const result = extractCacheableSession("local", "agent-1", session);

    expect(result.remoteKey).toBe("local");
    expect(result.agentId).toBe("agent-1");
    expect(result.schemaVersion).toBe(CACHE_SCHEMA_VERSION);
    expect(result.eventLogEpoch).toBe("epoch-1");
    expect(result.eventsBySeq).toEqual(session.eventsBySeq);
    expect(result.eventSeqs).toEqual(session.eventSeqs);
    expect(result.messagesById).toEqual(session.messagesById);
    expect(result.agentSummary).toBeUndefined();
    expect(result.newestSeq).toBe(2);
    expect(result.oldestSeq).toBe(1);
    expect(result.syncCoverage).toEqual({
      eventLogEpoch: "epoch-1",
      contiguousSeq: 2,
      observedSeq: 2,
      retainedOldestSeq: 1,
      retainedNewestSeq: 2,
      gaps: [],
    });
    expect(result.cachedAt).toBeGreaterThan(0);
  });

  it("persists sync gaps, semantic history coverage, and read state atomically", () => {
    const session = makeSession({
      eventLogEpoch: "epoch-1",
      eventsBySeq: { 1: { id: "e1" }, 5: { id: "e5" } },
      eventSeqs: [1, 5],
      gaps: [{ afterSeq: 1, beforeSeq: 5 }],
      newestSeq: 5,
      oldestSeq: 1,
      semanticHistoryByDisplayLevel: {
        info: { eventLogEpoch: "epoch-1", cursorSeq: 1, hasOlder: false, loading: false },
        verbose: { eventLogEpoch: "epoch-1", cursorSeq: 1, hasOlder: true, loading: true },
        debug: { eventLogEpoch: "epoch-old", cursorSeq: 1, hasOlder: true, loading: false },
      },
    });

    const result = extractCacheableSession("local", "agent-1", session, {
      unreadCount: 2,
      lastUnreadDeliverySeq: 5,
      lastReadDeliverySeq: 3,
    });

    expect(result.syncCoverage).toMatchObject({
      contiguousSeq: 1,
      observedSeq: 5,
      gaps: [{ afterSeq: 1, beforeSeq: 5 }],
    });
    expect(result.semanticHistoryByDisplayLevel).toEqual({
      info: { eventLogEpoch: "epoch-1", cursorSeq: 1, hasOlder: false },
    });
    expect(result.readState).toEqual({
      unreadCount: 2,
      lastUnreadDeliverySeq: 5,
      lastReadDeliverySeq: 3,
    });
  });

  it("persists the agent summary needed to render cached history", () => {
    const session = makeSession({
      detail: {
        agent: {
          id: "agent-1",
          badge: "A",
          profile: "default",
          lifecycle: "asleep",
          focusSummary: "",
          workspace: "",
          attention: "",
          model: "default",
          footer: "",
          subtitle: "",
          lastBrief: "",
          lastTurnTime: "",
          pending: 0,
          activeTaskCount: 0,
          waitingCount: 0,
          posture: "",
          postureReason: "",
        },
        timeline: [],
        source: "http",
        events: [],
      },
    });

    expect(extractCacheableSession("local", "agent-1", session).agentSummary).toMatchObject({
      id: "agent-1",
      lifecycle: "asleep",
    });
  });

  it("excludes UI state fields", () => {
    const session = makeSession({
      loading: true,
      liveStatus: "streaming",
      error: "some error",
      sendingPrompt: true,
    });

    const result = extractCacheableSession("local", "agent-1", session);

    expect(result).not.toHaveProperty("loading");
    expect(result).not.toHaveProperty("liveStatus");
    expect(result).not.toHaveProperty("error");
    expect(result).not.toHaveProperty("sendingPrompt");
  });

  it("trims events exceeding MAX_EVENTS_PER_AGENT", () => {
    const MAX = 5000;
    const eventSeqs = Array.from({ length: MAX + 100 }, (_, i) => i + 1);
    const eventsBySeq: Record<number, ProjectionEvent> = {};
    for (const seq of eventSeqs) eventsBySeq[seq] = { id: `e${seq}` };

    const session = makeSession({ eventsBySeq, eventSeqs });

    const result = extractCacheableSession("local", "agent-1", session);

    expect(result.eventSeqs.length).toBe(MAX);
    expect(result.eventSeqs[0]).toBe(101); // First 100 trimmed
    expect(result.eventSeqs[MAX - 1]).toBe(MAX + 100);
    expect(result.oldestSeq).toBe(101);
    expect(result.newestSeq).toBe(MAX + 100);
    expect(result.syncCoverage).toMatchObject({
      contiguousSeq: MAX + 100,
      observedSeq: MAX + 100,
      retainedOldestSeq: 101,
      retainedNewestSeq: MAX + 100,
    });
  });
});

describe("hydrateSessionFromCache", () => {
  it("returns partial session with cached data", () => {
    const cached = makeCachedSession({
      eventLogEpoch: "epoch-1",
      eventsBySeq: { 1: { id: "e1" } },
      eventSeqs: [1],
      messagesById: { m1: { id: "m1" } },
      newestSeq: 1,
      oldestSeq: 1,
      syncCoverage: {
        eventLogEpoch: "epoch-1",
        contiguousSeq: 1,
        observedSeq: 1,
        retainedOldestSeq: 1,
        retainedNewestSeq: 1,
        gaps: [],
      },
    });

    const result = hydrateSessionFromCache(cached);

    expect(result.eventsBySeq).toEqual({ 1: { id: "e1", event_seq: 1 } });
    expect(result.eventLogEpoch).toBe("epoch-1");
    expect(result.eventSeqs).toEqual([1]);
    expect(result.newestSeq).toBe(1);
    expect(result.oldestSeq).toBe(1);
  });

  it("restores a renderable detail when the cache includes an agent summary", () => {
    const cached = makeCachedSession({
      agentSummary: {
        id: "agent-1",
        badge: "A",
        profile: "default",
        lifecycle: "asleep",
        focusSummary: "",
        workspace: "",
        attention: "",
        model: "default",
        footer: "",
        subtitle: "",
        lastBrief: "",
        lastTurnTime: "",
        pending: 0,
        activeTaskCount: 0,
        waitingCount: 0,
        posture: "",
        postureReason: "",
      },
      eventsBySeq: { 1: { id: "e1", event_seq: 1 } },
      eventSeqs: [1],
      syncCoverage: {
        contiguousSeq: 1,
        observedSeq: 1,
        retainedOldestSeq: 1,
        retainedNewestSeq: 1,
        gaps: [],
      },
    });

    const result = hydrateSessionFromCache(cached);

    expect(result.detail?.agent.id).toBe("agent-1");
    expect(result.cacheStatus).toBe("hit");
    expect(result.contentStatus).toBe("available");
    expect(result.syncStatus).toBe("stale");
  });

  it("ignores a malformed or mismatched cached agent summary", () => {
    const cached = makeCachedSession({
      agentSummary: { id: "agent-2" },
      eventsBySeq: { 1: { id: "e1", event_seq: 1 } },
      eventSeqs: [1],
      syncCoverage: {
        contiguousSeq: 1,
        observedSeq: 1,
        retainedOldestSeq: 1,
        retainedNewestSeq: 1,
        gaps: [],
      },
    });

    const result = hydrateSessionFromCache(cached);

    expect(result.detail).toBeUndefined();
    expect(result.eventSeqs).toEqual([1]);
    expect(result.contentStatus).toBe("available");
  });

  it("does not include UI state fields", () => {
    const cached = makeCachedSession();

    const result = hydrateSessionFromCache(cached);

    expect(result).not.toHaveProperty("loading");
    expect(result).not.toHaveProperty("liveStatus");
    expect(result).not.toHaveProperty("detail");
  });

  it("restores persisted gaps and independent display-level history coverage", () => {
    const cached = makeCachedSession({
      eventLogEpoch: "epoch-1",
      eventsBySeq: {
        1: { id: "e1", event_seq: 1 },
        5: { id: "e5", event_seq: 5 },
      },
      eventSeqs: [1, 5],
      newestSeq: 5,
      oldestSeq: 1,
      syncCoverage: {
        eventLogEpoch: "epoch-1",
        contiguousSeq: 1,
        observedSeq: 5,
        retainedOldestSeq: 1,
        retainedNewestSeq: 5,
        gaps: [{ afterSeq: 1, beforeSeq: 5 }],
      },
      semanticHistoryByDisplayLevel: {
        info: { eventLogEpoch: "epoch-1", cursorSeq: 1, hasOlder: false },
        verbose: { eventLogEpoch: "epoch-1", cursorSeq: 3, hasOlder: true },
        debug: { eventLogEpoch: "epoch-old", cursorSeq: 1, hasOlder: true },
      },
    });

    const result = hydrateSessionFromCache(cached);

    expect(result.gaps).toEqual([{ afterSeq: 1, beforeSeq: 5 }]);
    expect(result.semanticHistoryByDisplayLevel).toEqual({
      info: { eventLogEpoch: "epoch-1", cursorSeq: 1, hasOlder: false, loading: false },
      verbose: { eventLogEpoch: "epoch-1", cursorSeq: 3, hasOlder: true, loading: false },
    });
  });

  it("invalidates incomplete or internally inconsistent cache metadata", () => {
    const missingCoverage = makeCachedSession({
      eventsBySeq: { 1: { id: "e1", event_seq: 1 } },
      eventSeqs: [1],
      syncCoverage: undefined,
    });
    const mismatchedRetainedWindow = makeCachedSession({
      eventsBySeq: { 1: { id: "e1", event_seq: 1 } },
      eventSeqs: [1],
      syncCoverage: {
        contiguousSeq: 1,
        observedSeq: 1,
        retainedOldestSeq: 2,
        retainedNewestSeq: 1,
        gaps: [],
      },
    });

    expect(hydrateSessionFromCache(missingCoverage)).toMatchObject({
      eventSeqs: [],
      invalidatedReason: "cache_integrity_mismatch",
      syncStatus: "stale",
    });
    expect(hydrateSessionFromCache(mismatchedRetainedWindow)).toMatchObject({
      eventSeqs: [],
      invalidatedReason: "cache_integrity_mismatch",
      syncStatus: "stale",
    });
  });
});

describe("hydrateAllSessions", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("restores session and read state from the same cached record", async () => {
    const cached = makeCachedSession({
      eventsBySeq: { 1: { id: "e1", event_seq: 1 } },
      eventSeqs: [1],
      syncCoverage: {
        contiguousSeq: 1,
        observedSeq: 1,
        retainedOldestSeq: 1,
        retainedNewestSeq: 1,
        gaps: [],
      },
      readState: {
        unreadCount: 2,
        lastUnreadDeliverySeq: 8,
        lastReadDeliverySeq: 5,
      },
    });
    const idbModule = await import("./idb-cache");
    vi.spyOn(idbModule, "cacheGetAllSessions").mockResolvedValue([cached]);

    const result = await hydrateAllSessions("local");

    expect(result.sessionsByAgentId["agent-1"]?.eventSeqs).toEqual([1]);
    expect(result.readStateByAgentId["agent-1"]).toEqual(cached.readState);
  });
});

describe("SessionCacheWriter", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("schedules debounced writes", async () => {
    const writer = new SessionCacheWriter("local");
    const session = makeSession({ eventsBySeq: { 1: { id: "e1" } }, eventSeqs: [1] });

    // Mock the module's cachePutSession
    const putSpy = vi.spyOn(await import("./idb-cache"), "cachePutSession").mockResolvedValue(undefined);

    writer.scheduleWrite("agent-1", session);

    // Not flushed yet
    expect(putSpy).not.toHaveBeenCalled();

    // Advance timers to trigger write
    vi.advanceTimersByTime(2001);

    // Wait for async flush
    await vi.waitFor(() => expect(putSpy).toHaveBeenCalledTimes(1));

    putSpy.mockRestore();
    writer.cancel();
  });

  it("flush writes all pending immediately", async () => {
    const writer = new SessionCacheWriter("local");
    const session1 = makeSession({ eventSeqs: [1] });
    const session2 = makeSession({ eventSeqs: [2] });

    const putSpy = vi.spyOn(await import("./idb-cache"), "cachePutSession").mockResolvedValue(undefined);

    writer.scheduleWrite("agent-1", session1);
    writer.scheduleWrite("agent-2", session2);

    await writer.flush();

    expect(putSpy).toHaveBeenCalledTimes(2);

    putSpy.mockRestore();
  });

  it("cancel stops pending writes", async () => {
    const writer = new SessionCacheWriter("local");
    const session = makeSession();

    const putSpy = vi.spyOn(await import("./idb-cache"), "cachePutSession").mockResolvedValue(undefined);

    writer.scheduleWrite("agent-1", session);
    writer.cancel();

    vi.advanceTimersByTime(5000);

    expect(putSpy).not.toHaveBeenCalled();

    putSpy.mockRestore();
  });

  it("discard cancels a deleted agent without affecting other pending writes", async () => {
    const writer = new SessionCacheWriter("local");
    const putSpy = vi.spyOn(await import("./idb-cache"), "cachePutSession").mockResolvedValue(undefined);

    writer.scheduleWrite("agent-1", makeSession({ eventSeqs: [1] }));
    writer.scheduleWrite("agent-2", makeSession({ eventSeqs: [2] }));
    writer.discard("agent-1");
    await writer.flush();

    expect(putSpy).toHaveBeenCalledTimes(1);
    expect(putSpy.mock.calls[0]?.[0].agentId).toBe("agent-2");
    putSpy.mockRestore();
  });
});

describe("enforceCacheLimits", () => {
  const remoteKey = "local";

  function makeCachedSession(agentId: string, cachedAt: number) {
    return {
      remoteKey,
      agentId,
      schemaVersion: CACHE_SCHEMA_VERSION,
      eventsBySeq: {},
      eventSeqs: [] as number[],
      messagesById: {},
      transcriptEntriesById: {},
      briefRecordsById: {},
      cachedAt,
    };
  }

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("does nothing when agent count is within limit (<= 50)", async () => {
    const sessions = Array.from({ length: 50 }, (_, i) =>
      makeCachedSession(`agent-${i}`, Date.now() + i),
    );
    const idbModule = await import("./idb-cache");
    const getAllSpy = vi.spyOn(idbModule, "cacheGetAllSessions").mockResolvedValue(sessions);
    const deleteSpy = vi.spyOn(idbModule, "cacheDeleteSession").mockResolvedValue(undefined);

    await enforceCacheLimits(remoteKey);

    expect(getAllSpy).toHaveBeenCalledTimes(1);
    expect(deleteSpy).not.toHaveBeenCalled();
  });

  it("deletes oldest sessions when exceeding 50 agents", async () => {
    // 55 sessions: oldest 5 should be deleted to bring it to 50
    const sessions = Array.from({ length: 55 }, (_, i) =>
      makeCachedSession(`agent-${i}`, 1000 + i * 100),
    );
    const idbModule = await import("./idb-cache");
    vi.spyOn(idbModule, "cacheGetAllSessions").mockResolvedValue(sessions);
    const deleteSpy = vi.spyOn(idbModule, "cacheDeleteSession").mockResolvedValue(undefined);

    await enforceCacheLimits(remoteKey);

    expect(deleteSpy).toHaveBeenCalledTimes(5);
    // Oldest 5 are agent-0 through agent-4 (cachedAt 1000..1400)
    const deletedAgentIds = deleteSpy.mock.calls.map((c) => c[1]);
    expect(deletedAgentIds).toEqual(
      expect.arrayContaining(["agent-0", "agent-1", "agent-2", "agent-3", "agent-4"]),
    );
    // Newest sessions are NOT deleted
    expect(deletedAgentIds).not.toContain("agent-54");
  });

  it("preserves newest 50 sessions when count is well above limit", async () => {
 const sessions = Array.from({ length: 100 }, (_, i) =>
      makeCachedSession(`agent-${i}`, 5000 + i * 10),
    );
    const idbModule = await import("./idb-cache");
    vi.spyOn(idbModule, "cacheGetAllSessions").mockResolvedValue(sessions);
    const deleteSpy = vi.spyOn(idbModule, "cacheDeleteSession").mockResolvedValue(undefined);

    await enforceCacheLimits(remoteKey);

    expect(deleteSpy).toHaveBeenCalledTimes(50);
    // agent-50..agent-99 should survive
    const deletedAgentIds = new Set(deleteSpy.mock.calls.map((c) => c[1]));
    for (let i = 50; i < 100; i++) {
      expect(deletedAgentIds.has(`agent-${i}`)).toBe(false);
    }
    for (let i = 0; i < 50; i++) {
      expect(deletedAgentIds.has(`agent-${i}`)).toBe(true);
    }
  });
});
