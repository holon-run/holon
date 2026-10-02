import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ROSTER_STALE_EXTENDED_RETRY_ATTEMPTS,
  retryDelayWithServerHintMs,
} from "./global-sync-coordinator";
import { createRuntimeClient } from "./client";
import { useRuntimeStore, type AgentSessionState } from "./runtime-store";
import { createSessionProjectionState } from "./session-projection";

const OBSERVER_SYNC_CAPABILITIES = [
  "agents.roster-snapshot.v1",
  "agents.projection-snapshot.v1",
  "events.projection-effect.v1",
  "briefs.atomic-created-event.v1",
];

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
      event_window: { event_head_seq: 5, oldest_retained_seq: 0 },
      latest_brief: null,
    })),
    ...overrides,
  };
}

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
    liveStatus: "idle",
    contentStatus: "unknown",
    syncStatus: "idle",
    sendingPrompt: false,
    abortingRun: false,
    pendingOperatorPrompts: [],
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
      if (url.pathname.endsWith("/agents/snapshot")) return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a"])));
      if (url.pathname.endsWith("/projection-snapshot")) return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(new Response(new ReadableStream<Uint8Array>({
          start(controller) {
            init?.signal?.addEventListener("abort", () => controller.close());
          },
        }), {
          status: 200,
          headers: {
            "content-type": "text/event-stream",
            "x-holon-event-contract-version": "3",
          },
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
      contract_version: 3,
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
      if (url.pathname.endsWith("/agents/snapshot")) return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a"])));
      if (url.pathname.endsWith("/projection-snapshot")) return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(new Response(new ReadableStream<Uint8Array>({
          start(controller) {
            init?.signal?.addEventListener("abort", () => controller.close());
          },
        }), {
          status: 200,
          headers: {
            "content-type": "text/event-stream",
            "x-holon-event-contract-version": "3",
          },
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
    expect(retryCallbacks.length).toBeGreaterThanOrEqual(1);

    for (const retry of retryCallbacks.splice(0)) retry();

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

describe("global event stream auth rejection", () => {
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

  it("stops reconnecting after a 401 stream rejection and surfaces unauthorized (#3299)", async () => {
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
    let streamRequests = 0;
    let snapshotRequests = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([listEntry("agent-a")]));
      }
      if (url.pathname.endsWith("/agents/snapshot")) {
        snapshotRequests += 1;
        return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a"])));
      }
      if (url.pathname.endsWith("/events/stream")) {
        streamRequests += 1;
        return Promise.resolve(errorJsonResponse(401, { error: "control token required", code: "auth_required" }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("unauthorized");
    });
    const discovery = useRuntimeStore.getState().discovery;
    expect(discovery.freshness).toBe("unauthorized");
    expect(discovery.unauthorizedReason).toContain("control token required");
    expect(discovery.retryAt).toBeUndefined();
    // The stream was attempted exactly once and discovery never started.
    expect(streamRequests).toBe(1);
    expect(snapshotRequests).toBe(0);

    // Firing every captured timer must not schedule another stream attempt:
    // an auth rejection is terminal for this session, not transient (#3299).
    for (const timer of timers.splice(0)) timer();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(streamRequests).toBe(1);
    expect(snapshotRequests).toBe(0);
    expect(useRuntimeStore.getState().globalStreamStatus).toBe("unauthorized");
  });

  it("keeps bounded reconnect backoff after a non-auth stream failure", async () => {
    const reconnectCallbacks: Array<() => void> = [];
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout: (callback: () => void, delay?: number) => {
        if (delay == null || delay >= 900) reconnectCallbacks.push(callback);
        return reconnectCallbacks.length;
      },
      clearTimeout: () => undefined,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let streamRequests = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([listEntry("agent-a")]));
      }
      if (url.pathname.endsWith("/events/stream")) {
        streamRequests += 1;
        return Promise.resolve(errorJsonResponse(503, { error: "daemon restarting", code: "internal" }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("reconnecting");
    });
    expect(streamRequests).toBe(1);

    // A transport-style failure still schedules exactly one reconnect.
    expect(reconnectCallbacks.length).toBeGreaterThanOrEqual(1);
    reconnectCallbacks.shift()?.();
    await vi.waitFor(() => {
      expect(streamRequests).toBe(2);
    });
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("reconnecting");
    });
  });

  it("keeps terminal unauthorized discovery when a roster failure settles after the stream 401 (#3299)", async () => {
    const timers = stubWindowWithRecordingTimers();
    let streamRequests = 0;
    let snapshotRequests = 0;
    let failSnapshot!: (error: unknown) => void;
    const snapshotResponse = new Promise<Response>((_, reject) => {
      failSnapshot = reject;
    });
    let streamController: ReadableStreamDefaultController<Uint8Array> | undefined;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([]));
      }
      if (url.pathname.endsWith("/agents/snapshot")) {
        snapshotRequests += 1;
        return snapshotResponse;
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      if (url.pathname.endsWith("/events/stream")) {
        streamRequests += 1;
        if (streamRequests === 1) {
          return Promise.resolve(new Response(new ReadableStream<Uint8Array>({
            start(controller) {
              streamController = controller;
            },
          }), {
            status: 200,
            headers: {
              "content-type": "text/event-stream",
              "x-holon-event-contract-version": "3",
            },
          }));
        }
        return Promise.resolve(errorJsonResponse(401, { error: "control token required", code: "auth_required" }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    // The stream opens and discovery starts an in-flight roster snapshot.
    await vi.waitFor(() => {
      expect(streamRequests).toBe(1);
      expect(snapshotRequests).toBe(1);
      expect(streamController).toBeTruthy();
    });

    // The healthy stream drops, and the reconnect attempt hits the
    // terminal 401 while the roster snapshot is still in flight.
    streamController?.close();
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("reconnecting");
    });
    const reconnectTimer = timers.find(
      (timer) => !timer.cancelled && (timer.delay ?? 0) >= 900 && (timer.delay ?? 0) < 45_000,
    );
    expect(reconnectTimer).toBeDefined();
    reconnectTimer?.callback();
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("unauthorized");
    });
    // Consume the manually fired timer so the final sweep cannot re-run it.
    if (reconnectTimer) reconnectTimer.cancelled = true;
    expect(useRuntimeStore.getState().discovery.freshness).toBe("unauthorized");

    // The in-flight snapshot now fails with a non-auth error (the same
    // auth outage can break it through a proxy or network reset). The
    // terminal unauthorized state must survive the late failure settle.
    failSnapshot(new Error("network reset while unauthorized"));
    await new Promise((resolve) => setTimeout(resolve, 50));

    const discovery = useRuntimeStore.getState().discovery;
    expect(discovery.freshness).toBe("unauthorized");
    expect(discovery.unauthorizedReason).toContain("control token required");
    expect(discovery.staleReason).toBeUndefined();
    expect(discovery.retryAt).toBeUndefined();

    // Firing every still-armed timer must not restart stream or roster.
    fireRecordedTimers(timers);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(streamRequests).toBe(2);
    expect(snapshotRequests).toBe(1);
    expect(useRuntimeStore.getState().globalStreamStatus).toBe("unauthorized");
  });

  it("cancels an armed reconnect timer when a later stream attempt hits 401 (#3299)", async () => {
    const timers = stubWindowWithRecordingTimers();
    let streamRequests = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([]));
      }
      if (url.pathname.endsWith("/events/stream")) {
        streamRequests += 1;
        if (streamRequests === 1) {
          return Promise.resolve(errorJsonResponse(503, { error: "daemon restarting", code: "internal" }));
        }
        return Promise.resolve(errorJsonResponse(401, { error: "control token required", code: "auth_required" }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("reconnecting");
    });
    expect(streamRequests).toBe(1);
    const reconnectTimer = timers.find(
      (timer) => !timer.cancelled && (timer.delay ?? 0) >= 900 && (timer.delay ?? 0) < 45_000,
    );
    expect(reconnectTimer).toBeDefined();

    // A second registration restarts the stream while the reconnect timer
    // is still armed; that attempt hits the terminal 401.
    useRuntimeStore.getState().registerAgentForEvents("agent-b");
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("unauthorized");
    });
    expect(streamRequests).toBe(2);
    // The armed reconnect timer was cancelled by the terminal rejection, so
    // it can no longer restart the loop afterwards.
    expect(reconnectTimer?.cancelled).toBe(true);

    fireRecordedTimers(timers);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(streamRequests).toBe(2);
    expect(useRuntimeStore.getState().globalStreamStatus).toBe("unauthorized");
    useRuntimeStore.getState().unregisterAgentForEvents("agent-b");
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
        event_window: { event_head_seq: 5, oldest_retained_seq: 0 },
        latest_brief: null,
      })),
      ...overrides,
    };
  }

  function emptyEventsPage(agentId: string): Record<string, unknown> {
    return {
      contract_version: 3,
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
      contract_version: 3,
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
      headers: {
        "content-type": "text/event-stream",
        "x-holon-event-contract-version": "3",
      },
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
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
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
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
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
      `event: agent_roster_hint\ndata: ${JSON.stringify({ agent_id: "agent-c" })}\n\n`,
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

  it("applies a stale-marked roster but keeps discovery stale and retries to fresh", async () => {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let snapshotCalls = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([listEntry("agent-a")]));
      }
      if (url.pathname.endsWith("/agents/snapshot")) {
        snapshotCalls += 1;
        if (snapshotCalls === 1) {
          // The runtime degrades to its last good projection and marks it.
          return Promise.resolve(new Response(JSON.stringify(rosterSnapshot(["agent-a"])), {
            status: 200,
            headers: {
              "content-type": "application/json",
              "x-holon-projection-stale": "true",
            },
          }));
        }
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

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    // The stale-marked snapshot is still applied (it is the best known
    // roster), but discovery stays stale with a bounded retry scheduled.
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "stale",
        retryAttempt: 1,
      });
    });
    expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-a"]);

    // The bounded retry converges once the runtime serves a fresh snapshot.
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "fresh",
      });
    }, { timeout: 5_000 });
    expect(snapshotCalls).toBeGreaterThanOrEqual(2);
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
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
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
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
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
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
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

  it("rejects a daemon that does not advertise the complete observer-sync contract", async () => {
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
    expect(useRuntimeStore.getState().bootstrapError).toContain(
      "missing capabilities: agents.roster-snapshot.v1",
    );
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
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
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
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

  function headWindowRoster(): Record<string, unknown> {
    return rosterSnapshot(["agent-a"], {
      agents: [{
        agent: listEntry("agent-a"),
        event_window: { event_head_seq: 5000, oldest_retained_seq: 0 },
        latest_brief: null,
      }],
    });
  }

  function backfillRequestsAfterSeqs(fetchMock: ReturnType<typeof vi.fn>): Array<string> {
    return fetchMock.mock.calls
      .map(([input]) => new URL(String(input), "http://localhost"))
      .filter((url) => url.pathname.endsWith("/agents/agent-a/events"))
      .map((url) => url.searchParams.get("after_seq"))
      .filter((value): value is string => value != null);
  }

  function headWindowFetchMock(): ReturnType<typeof vi.fn> {
    return vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      }
      if (url.pathname.endsWith("/agents/list")) return Promise.resolve(jsonResponse([listEntry("agent-a")]));
      if (url.pathname.endsWith("/agents/snapshot")) {
        return Promise.resolve(jsonResponse(headWindowRoster()));
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(sseResponse(init, () => undefined));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
        const afterSeq = Number(url.searchParams.get("after_seq") ?? "0");
        const start = afterSeq + 1;
        const end = Math.min(start + 99, 5000);
        if (start > 5000) return Promise.resolve(jsonResponse(emptyEventsPage("agent-a")));
        return Promise.resolve(jsonResponse({
          events: Array.from({ length: end - start + 1 }, (_, index) => ({
            id: `event-${start + index}`,
            event_seq: start + index,
            event_log_epoch: "epoch-1",
            ts: "2026-08-10T00:00:00Z",
            agent_id: "agent-a",
            type: "legacy_event",
            payload: {},
          })),
          event_log_epoch: "epoch-1",
          has_older: false,
          has_newer: end < 5000,
          order: "asc",
          limit: 100,
          agent_id: "agent-a",
        }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
  }

  it("seeds catch-up from the roster head window instead of a stale gap baseline", async () => {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    const fetchMock = headWindowFetchMock();
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();
    // A gap record left over from an earlier daemon restart must not drag a
    // fresh session into a full-history replay (#2986).
    useRuntimeStore.setState({
      sessionsByAgentId: {
        "agent-a": sessionState({
          eventLogEpoch: "epoch-1",
          eventSeqs: [1],
          newestSeq: 1,
          gaps: [{ afterSeq: 1, beforeSeq: 5000 }],
        }),
      },
    });

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
    expect(backfillRequestsAfterSeqs(fetchMock)[0]).toBe("4000");
  });

  it("resumes catch-up from a persisted cursor for the same runtime connection", async () => {
    const sessionStorage = new MemoryStorage();
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage,
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    sessionStorage.setItem("holon.globalSync.catchUpCursor.v1", JSON.stringify({
      runtimeId: "rt-1",
      visibilityScopeId: "vis-1",
      eventLogEpoch: "epoch-1",
      agents: {
        "agent-a": { eventLogEpoch: "epoch-1", contiguousSeq: 4200, highestObservedSeq: 5000 },
      },
    }));
    const fetchMock = headWindowFetchMock();
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();
    useRuntimeStore.setState({
      sessionsByAgentId: {
        "agent-a": sessionState({ eventLogEpoch: "epoch-1" }),
      },
    });

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
    // The reload resumes from the persisted cursor instead of restarting.
    expect(backfillRequestsAfterSeqs(fetchMock)[0]).toBe("4200");
    const stored = JSON.parse(
      sessionStorage.getItem("holon.globalSync.catchUpCursor.v1") ?? "{}",
    ) as { agents?: Record<string, { contiguousSeq?: number }> };
    expect(stored.agents?.["agent-a"]?.contiguousSeq).toBeGreaterThanOrEqual(4200);
  });

  it("ignores persisted cursors from a different runtime connection", async () => {
    const sessionStorage = new MemoryStorage();
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage,
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    sessionStorage.setItem("holon.globalSync.catchUpCursor.v1", JSON.stringify({
      runtimeId: "rt-other",
      visibilityScopeId: "vis-1",
      eventLogEpoch: "epoch-1",
      agents: {
        "agent-a": { eventLogEpoch: "epoch-1", contiguousSeq: 4200, highestObservedSeq: 5000 },
      },
    }));
    const fetchMock = headWindowFetchMock();
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();
    useRuntimeStore.setState({
      sessionsByAgentId: {
        "agent-a": sessionState({ eventLogEpoch: "epoch-1" }),
      },
    });

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
    // Without a valid cursor the session falls back to the bounded
    // head-seeded baseline, never a full-history replay.
    expect(backfillRequestsAfterSeqs(fetchMock)[0]).toBe("4000");
    const stored = JSON.parse(
      sessionStorage.getItem("holon.globalSync.catchUpCursor.v1") ?? "{}",
    ) as { runtimeId?: string };
    expect(stored.runtimeId).toBe("rt-1");
  });
});

interface RecordedTimer {
  id: number;
  callback: () => void;
  delay?: number;
  cancelled: boolean;
}

function stubWindowWithRecordingTimers(): RecordedTimer[] {
  const timers: RecordedTimer[] = [];
  let nextTimerId = 1;
  vi.stubGlobal("window", {
    localStorage: new MemoryStorage(),
    sessionStorage: new MemoryStorage(),
    setTimeout: (callback: () => void, delay?: number) => {
      const timer: RecordedTimer = { id: nextTimerId, callback, delay, cancelled: false };
      nextTimerId += 1;
      timers.push(timer);
      return timer.id;
    },
    clearTimeout: (id?: number) => {
      for (const timer of timers) {
        if (timer.id === id) timer.cancelled = true;
      }
    },
    location: { hostname: "localhost", protocol: "http:" },
  });
  return timers;
}

function fireRecordedTimers(timers: RecordedTimer[]): void {
  for (const timer of timers.splice(0)) {
    if (!timer.cancelled) timer.callback();
  }
}

function jsonResponse(body: unknown): Response {
  const isEventPage = body && typeof body === "object"
    && "events" in body
    && Array.isArray(body.events);
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: {
      "content-type": "application/json",
      ...(isEventPage ? { "x-holon-event-contract-version": "3" } : {}),
    },
  });
}

function errorJsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("retryDelayWithServerHintMs", () => {
  it("keeps the computed backoff when no server hint is present", () => {
    expect(retryDelayWithServerHintMs(1)).toBe(1_000);
    expect(retryDelayWithServerHintMs(2, new Error("offline"))).toBe(2_000);
  });

  it("waits at least the server Retry-After hint from a projection_busy response", async () => {
    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: (async () =>
        Response.json(
          {
            ok: false,
            error: "projection capacity is busy; retry later",
            code: "projection_busy",
            retryable: true,
          },
          { status: 429, headers: { "retry-after": "5" } },
        )) as typeof fetch,
    });
    const error = await client.getAgentState("agent-one").then(
      () => undefined,
      (error: unknown) => error,
    );

    expect(retryDelayWithServerHintMs(1, error)).toBe(5_000);
    // Computed exponential backoff eventually overtakes a small hint.
    expect(retryDelayWithServerHintMs(5, error)).toBe(15_000);
  });
});
