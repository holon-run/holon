import { afterEach, describe, expect, it, vi } from "vitest";

import { ROSTER_STALE_EXTENDED_RETRY_ATTEMPTS } from "./global-sync-coordinator";
import { useRuntimeStore, type AgentSessionState } from "./runtime-store";
import { createSessionProjectionState } from "./session-projection";

class MemoryStorage implements Storage {
  private readonly items = new Map<string, string>();

  get length() {
    return this.items.size;
  }

  clear(): void {
    this.items.clear();
  }

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  key(index: number): string | null {
    return Array.from(this.items.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }

  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }
}

function sessionState(overrides: Partial<AgentSessionState> = {}): AgentSessionState {
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

describe("global event stream recovery", () => {
  afterEach(() => {
    useRuntimeStore.getState().stopGlobalEventStream();
    useRuntimeStore.getState().unregisterAgentForEvents("agent-a");
    useRuntimeStore.setState({
      sessionsByAgentId: {},
      globalStreamStatus: "idle",
      selectedAgentId: "",
    });
    vi.unstubAllGlobals();
  });

  it("does not report streaming until the subscribed agent backfill completes", async () => {
    const localStorage = new MemoryStorage();
    const sessionStorage = new MemoryStorage();
    vi.stubGlobal("window", {
      localStorage,
      sessionStorage,
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let resolveBackfill!: (response: Response) => void;
    const backfill = new Promise<Response>((resolve) => {
      resolveBackfill = resolve;
    });
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) return Promise.resolve(jsonResponse({}));
      if (url.pathname.endsWith("/agents/list")) return Promise.resolve(jsonResponse([]));
      if (url.pathname.endsWith("/agents/snapshot")) return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      if (url.pathname.endsWith("/projection-snapshot")) return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(new Response(new ReadableStream<Uint8Array>({
          start(controller) {
            init?.signal?.addEventListener("abort", () => controller.close());
          },
        }), {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        }));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) return backfill;
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();
    useRuntimeStore.setState({
      sessionsByAgentId: {
        "agent-a": sessionState({
          eventLogEpoch: "epoch-1",
          eventsBySeq: {
            1: {
              agent_id: "agent-a",
              event_seq: 1,
              event_log_epoch: "epoch-1",
              type: "legacy_event",
              payload: {},
            },
            5: {
              agent_id: "agent-a",
              event_seq: 5,
              event_log_epoch: "epoch-1",
              type: "legacy_event",
              payload: {},
            },
          },
          eventSeqs: [1, 5],
          newestSeq: 5,
          gaps: [{ afterSeq: 1, beforeSeq: 5 }],
        }),
      },
    });

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("catching_up");
    });
    const eventRequest = fetchMock.mock.calls
      .map(([input]) => new URL(String(input), "http://localhost"))
      .find((url) => url.pathname.endsWith("/agents/agent-a/events"));
    expect(eventRequest?.searchParams.get("after_seq")).toBe("1");

    resolveBackfill(jsonResponse({
      events: [2, 3, 4, 5].map((eventSeq) => ({
        id: `event-${eventSeq}`,
        event_seq: eventSeq,
        event_log_epoch: "epoch-1",
        ts: "2026-08-09T00:00:00Z",
        agent_id: "agent-a",
        type: "legacy_event",
        payload: {},
      })),
      event_log_epoch: "epoch-1",
      has_older: false,
      has_newer: false,
      order: "asc",
      limit: 100,
    }));
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
  });

  it("retries baseline initialization before declaring recovery complete", async () => {
    const localStorage = new MemoryStorage();
    const sessionStorage = new MemoryStorage();
    const retryCallbacks: Array<() => void> = [];
    vi.stubGlobal("window", {
      localStorage,
      sessionStorage,
      setTimeout: (callback: () => void, delay?: number) => {
        if (delay === 1_000) retryCallbacks.push(callback);
        return retryCallbacks.length;
      },
      clearTimeout: () => undefined,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let baselineAttempts = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) return Promise.resolve(jsonResponse({}));
      if (url.pathname.endsWith("/agents/list")) return Promise.resolve(jsonResponse([]));
      if (url.pathname.endsWith("/agents/snapshot")) return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      if (url.pathname.endsWith("/projection-snapshot")) return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(new Response(new ReadableStream<Uint8Array>({
          start(controller) {
            init?.signal?.addEventListener("abort", () => controller.close());
          },
        }), {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        }));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") {
          baselineAttempts += 1;
          if (baselineAttempts === 1) return Promise.reject(new Error("baseline unavailable"));
          return Promise.resolve(jsonResponse({
            events: [{
              id: "event-1",
              event_seq: 1,
              event_log_epoch: "epoch-1",
              ts: "2026-08-10T00:00:00Z",
              agent_id: "agent-a",
              type: "legacy_event",
              payload: {},
            }],
            event_log_epoch: "epoch-1",
            has_older: false,
            has_newer: false,
            order: "desc",
            limit: 100,
          }));
        }
        return Promise.resolve(jsonResponse({
          events: [],
          event_log_epoch: "epoch-1",
          has_older: false,
          has_newer: false,
          order: "asc",
          limit: 100,
        }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toMatchObject({
        liveStatus: "recovering",
        syncError: "baseline unavailable",
        syncRetryAttempt: 1,
      });
    });
    expect(useRuntimeStore.getState().globalStreamStatus).toBe("catching_up");
    expect(retryCallbacks).toHaveLength(1);

    retryCallbacks.shift()?.();

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
    expect(baselineAttempts).toBe(2);
    expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toMatchObject({
      eventSeqs: [1],
      liveStatus: "streaming",
      syncError: undefined,
      syncRetryAttempt: undefined,
    });
  });
});


describe("authoritative discovery cutover", () => {
  afterEach(() => {
    useRuntimeStore.getState().stopGlobalEventStream();
    useRuntimeStore.getState().unregisterAgentForEvents("agent-a");
    useRuntimeStore.getState().unregisterAgentForEvents("agent-b");
    useRuntimeStore.getState().unregisterAgentForEvents("agent-c");
    useRuntimeStore.setState({
      sessionsByAgentId: {},
      globalStreamStatus: "idle",
      selectedAgentId: "",
      discovery: { mode: "pending", freshness: "fresh", retryAttempt: 0 },
      bootstrap: {
        attentionCount: 0,
        connection: {
          mode: "local",
          source: "fixture",
          baseUrl: undefined,
          hasToken: false,
          summary: "",
        },
        metrics: [],
        agents: [],
      },
    });
    vi.unstubAllGlobals();
  });

  function listEntry(agentId: string): Record<string, unknown> {
    return {
      identity: {
        agent_id: agentId,
        visibility: "public",
        ownership: "self_owned",
        profile_preset: "public_named",
      },
      status: "awake_idle",
      pending: 0,
    };
  }

  function rosterSnapshot(
    agentIds: string[],
    overrides: Record<string, unknown> = {},
  ): Record<string, unknown> {
    return {
      contract_version: 1,
      runtime_id: "rt-1",
      event_log_epoch: "epoch-1",
      visibility_scope_id: "vis-1",
      agents: agentIds.map((agentId) => ({
        agent: listEntry(agentId),
        event_window: { event_head_seq: 5, oldest_retained_seq: null },
        latest_brief: null,
      })),
      ...overrides,
    };
  }

  function emptyEventsPage(agentId: string): Record<string, unknown> {
    return {
      events: [],
      event_log_epoch: "epoch-1",
      has_older: false,
      has_newer: false,
      order: "asc",
      limit: 100,
      agent_id: agentId,
    };
  }

  function baselinePage(agentId: string): Record<string, unknown> {
    return {
      events: [{
        id: `event-${agentId}-1`,
        event_seq: 1,
        event_log_epoch: "epoch-1",
        ts: "2026-08-10T00:00:00Z",
        agent_id: agentId,
        type: "legacy_event",
        payload: {},
      }],
      event_log_epoch: "epoch-1",
      has_older: false,
      has_newer: false,
      order: "desc",
      limit: 100,
    };
  }

  function sseResponse(
    init: RequestInit | undefined,
    onController: (controller: ReadableStreamDefaultController<Uint8Array>) => void,
  ): Response {
    return new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        onController(controller);
        init?.signal?.addEventListener("abort", () => {
          try {
            controller.close();
          } catch {
            // Already closed by the test harness.
          }
        });
      },
    }), {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  }

  it("applies the authoritative roster, purges omitted agents, and settles fresh", async () => {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: ["agents.list", "agents.roster-snapshot.v1"] }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([listEntry("agent-a"), listEntry("agent-b")]));
      }
      if (url.pathname.endsWith("/agents/snapshot")) {
        return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a"])));
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(sseResponse(init, () => undefined));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-a")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();
    useRuntimeStore.setState({
      sessionsByAgentId: {
        "agent-a": sessionState({ eventLogEpoch: "epoch-1" }),
        "agent-b": sessionState({ eventLogEpoch: "epoch-1" }),
      },
    });

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "fresh",
        identity: {
          runtimeId: "rt-1",
          visibilityScopeId: "vis-1",
          eventLogEpoch: "epoch-1",
        },
      });
    });
    expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-a"]);
    expect(useRuntimeStore.getState().sessionsByAgentId["agent-b"]).toBeUndefined();
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
  });

  it("coalesces a roster hint that arrives while the snapshot is in flight", async () => {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let snapshotRequests = 0;
    let releaseFirst: ((response: Response) => void) | null = null;
    const firstSnapshot = new Promise<Response>((resolve) => {
      releaseFirst = resolve;
    });
    let streamController: ReadableStreamDefaultController<Uint8Array> | null = null;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: ["agents.list", "agents.roster-snapshot.v1"] }));
      }
      if (url.pathname.endsWith("/agents/list")) return Promise.resolve(jsonResponse([listEntry("agent-a")]));
      if (url.pathname.endsWith("/agents/snapshot")) {
        snapshotRequests += 1;
        if (snapshotRequests === 1) return firstSnapshot;
        return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a", "agent-c"])));
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(sseResponse(init, (controller) => {
          streamController = controller;
        }));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-a")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
      }
      if (url.pathname.endsWith("/agents/agent-c/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-c")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-c")));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => expect(snapshotRequests).toBe(1));
    // An event for an agent outside the roster lands while the snapshot
    // request is still in flight: it must coalesce into one extra refresh.
    streamController!.enqueue(new TextEncoder().encode(
      `data: ${JSON.stringify({
        id: "event-c-1",
        event_seq: 1,
        event_log_epoch: "epoch-1",
        ts: "2026-08-10T00:00:00Z",
        agent_id: "agent-c",
        type: "agent_state_changed",
        payload: {},
      })}\n\n`,
    ));
    releaseFirst!(jsonResponse(rosterSnapshot(["agent-a"])));

    await vi.waitFor(() => expect(snapshotRequests).toBe(2));
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "fresh",
      });
    });
    expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-a", "agent-c"]);
  });

  it("keeps the last roster and marks discovery stale on a transient snapshot failure", async () => {
    const retryCallbacks: Array<() => void> = [];
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout: (callback: () => void, delay?: number) => {
        if (delay === 1_000) retryCallbacks.push(callback);
        return retryCallbacks.length;
      },
      clearTimeout: () => undefined,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let snapshotRequests = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: ["agents.list", "agents.roster-snapshot.v1"] }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([listEntry("agent-a"), listEntry("agent-b")]));
      }
      if (url.pathname.endsWith("/agents/snapshot")) {
        snapshotRequests += 1;
        if (snapshotRequests === 1) {
          return Promise.resolve(errorJsonResponse(500, { error: "snapshot assembly failed" }));
        }
        return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a", "agent-b"])));
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(sseResponse(init, () => undefined));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-a")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "stale",
        retryAttempt: 1,
      });
    });
    // A failed or partial snapshot never purges the previous roster.
    expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-a", "agent-b"]);
    expect(retryCallbacks.length).toBeGreaterThanOrEqual(1);

    for (const retry of retryCallbacks.splice(0)) retry();
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "fresh",
      });
    });
  });

  it("escalates extended transient snapshot failure streaks without purging the roster", async () => {
    const retryCallbacks: Array<() => void> = [];
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout: (callback: () => void, _delay?: number) => {
        retryCallbacks.push(callback);
        return retryCallbacks.length;
      },
      clearTimeout: () => undefined,
      location: { hostname: "localhost", protocol: "http:" },
    });
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: ["agents.list", "agents.roster-snapshot.v1"] }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([listEntry("agent-a"), listEntry("agent-b")]));
      }
      if (url.pathname.endsWith("/agents/snapshot")) {
        return Promise.resolve(errorJsonResponse(500, { error: "snapshot assembly failed" }));
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(sseResponse(init, () => undefined));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-a")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "stale",
        retryAttempt: 1,
      });
    });
    // A long transient streak keeps the previous roster and lets the retry
    // counter climb past the dashboard's extended-stale threshold.
    while (
      (useRuntimeStore.getState().discovery?.retryAttempt ?? 0)
        < ROSTER_STALE_EXTENDED_RETRY_ATTEMPTS
    ) {
      const pending = retryCallbacks.splice(0);
      expect(pending.length).toBeGreaterThan(0);
      for (const retry of pending) retry();
      await vi.waitFor(() => {
        expect(
          (useRuntimeStore.getState().discovery?.retryAttempt ?? 0)
            >= ROSTER_STALE_EXTENDED_RETRY_ATTEMPTS
            || retryCallbacks.length > 0,
        ).toBe(true);
      });
    }
    expect(useRuntimeStore.getState().discovery).toMatchObject({
      mode: "authoritative",
      freshness: "stale",
    });
    expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-a", "agent-b"]);
  });

  it("separates authorization failure from transient failure and stops retrying", async () => {
    const timers: Array<() => void> = [];
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout: (callback: () => void) => {
        timers.push(callback);
        return timers.length;
      },
      clearTimeout: () => undefined,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let snapshotRequests = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: ["agents.list", "agents.roster-snapshot.v1"] }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([listEntry("agent-a"), listEntry("agent-b")]));
      }
      if (url.pathname.endsWith("/agents/snapshot")) {
        snapshotRequests += 1;
        return Promise.resolve(errorJsonResponse(401, { error: "control token required", code: "auth_required" }));
      }
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(sseResponse(init, () => undefined));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-a")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
      }
            if (url.pathname.endsWith("/agents/agent-b/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-b")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-b")));
      }
throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery.freshness).toBe("unauthorized");
    });
    const discovery = useRuntimeStore.getState().discovery;
    expect(discovery.unauthorizedReason).toContain("control token required");
    expect(discovery.retryAt).toBeUndefined();
    // The cached roster stays visible but is not purged and not retried.
    expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-a", "agent-b"]);
    expect(snapshotRequests).toBe(1);
    // Firing every captured timer must still not retry an unauthorized roster.
    for (const timer of timers.splice(0)) timer();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(snapshotRequests).toBe(1);
    expect(useRuntimeStore.getState().discovery.freshness).toBe("unauthorized");
  });

  it("keeps the legacy /agents/list path without a purge when the roster capability is absent", async () => {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        // An older server: capabilities are advertised without the roster
        // snapshot contract.
        return Promise.resolve(jsonResponse({ capabilities: ["agents.list", "agents.state"] }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([listEntry("agent-a"), listEntry("agent-b")]));
      }
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(sseResponse(init, () => undefined));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-a")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
      }
            if (url.pathname.endsWith("/agents/agent-b/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-b")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-b")));
      }
throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
    expect(useRuntimeStore.getState().discovery.mode).toBe("legacy");
    // The legacy path never performs an authoritative purge.
    expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-a", "agent-b"]);
    expect(fetchMock.mock.calls.filter((call) => String(call[0]).endsWith("/agents/snapshot"))).toHaveLength(0);
  });

  it("repeats the roster snapshot on every successful reconnect", async () => {
    const retryCallbacks: Array<() => void> = [];
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout: (callback: () => void, delay?: number) => {
        if (delay == null || delay >= 900) retryCallbacks.push(callback);
        return retryCallbacks.length;
      },
      clearTimeout: () => undefined,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let snapshotRequests = 0;
    const streams: Array<ReadableStreamDefaultController<Uint8Array>> = [];
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: ["agents.list", "agents.roster-snapshot.v1"] }));
      }
      if (url.pathname.endsWith("/agents/list")) return Promise.resolve(jsonResponse([listEntry("agent-a")]));
      if (url.pathname.endsWith("/agents/snapshot")) {
        snapshotRequests += 1;
        return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a"])));
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(sseResponse(init, (controller) => {
          streams.push(controller);
        }));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(baselinePage("agent-a")));
        return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");
    await vi.waitFor(() => expect(snapshotRequests).toBe(1));
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery.freshness).toBe("fresh");
    });
    retryCallbacks.length = 0;

    // The server closes the stream: the reconnect must repeat the snapshot.
    streams[0].close();
    await vi.waitFor(() => expect(retryCallbacks.length).toBeGreaterThanOrEqual(1));
    retryCallbacks.shift()?.();
    await vi.waitFor(() => expect(snapshotRequests).toBe(2));
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function errorJsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

