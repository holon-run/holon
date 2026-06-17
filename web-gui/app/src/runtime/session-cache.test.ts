import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  currentRemoteKey,
  extractCacheableSession,
  hydrateSessionFromCache,
  SessionCacheWriter,
} from "./session-cache";
import type { AgentSessionState } from "./runtime-store-helpers";
import { CACHE_SCHEMA_VERSION } from "./idb-cache";

function makeSession(overrides: Partial<AgentSessionState> = {}): AgentSessionState {
  return {
    loading: false,
    loadingOlder: false,
    liveStatus: "idle",
    sendingPrompt: false,
    detail: null,
    eventsBySeq: {},
    eventSeqs: [],
    messagesById: {},
    missingMessageIds: {},
    transcriptEntriesById: {},
    missingTranscriptEntryIds: {},
    briefRecordsById: {},
    missingBriefIds: {},
    workItemDetailsById: {},
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
    expect(result.eventsBySeq).toEqual(session.eventsBySeq);
    expect(result.eventSeqs).toEqual(session.eventSeqs);
    expect(result.messagesById).toEqual(session.messagesById);
    expect(result.newestSeq).toBe(2);
    expect(result.oldestSeq).toBe(1);
    expect(result.cachedAt).toBeGreaterThan(0);
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
    const eventsBySeq: Record<number, unknown> = {};
    for (const seq of eventSeqs) eventsBySeq[seq] = { id: `e${seq}` };

    const session = makeSession({ eventsBySeq, eventSeqs });

    const result = extractCacheableSession("local", "agent-1", session);

    expect(result.eventSeqs.length).toBe(MAX);
    expect(result.eventSeqs[0]).toBe(101); // First 100 trimmed
    expect(result.eventSeqs[MAX - 1]).toBe(MAX + 100);
  });
});

describe("hydrateSessionFromCache", () => {
  it("returns partial session with cached data", () => {
    const cached = {
      remoteKey: "local",
      agentId: "agent-1",
      schemaVersion: CACHE_SCHEMA_VERSION,
      eventsBySeq: { 1: { id: "e1" } },
      eventSeqs: [1],
      messagesById: { m1: { id: "m1" } },
      transcriptEntriesById: {},
      briefRecordsById: {},
      newestSeq: 1,
      oldestSeq: 1,
      cachedAt: Date.now(),
    };

    const result = hydrateSessionFromCache(cached);

    expect(result.eventsBySeq).toEqual({ 1: { id: "e1" } });
    expect(result.eventSeqs).toEqual([1]);
    expect(result.newestSeq).toBe(1);
    expect(result.oldestSeq).toBe(1);
  });

  it("does not include UI state fields", () => {
    const cached = {
      remoteKey: "local",
      agentId: "agent-1",
      schemaVersion: CACHE_SCHEMA_VERSION,
      eventsBySeq: {},
      eventSeqs: [],
      messagesById: {},
      transcriptEntriesById: {},
      briefRecordsById: {},
      cachedAt: Date.now(),
    };

    const result = hydrateSessionFromCache(cached);

    expect(result).not.toHaveProperty("loading");
    expect(result).not.toHaveProperty("liveStatus");
    expect(result).not.toHaveProperty("detail");
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
});
