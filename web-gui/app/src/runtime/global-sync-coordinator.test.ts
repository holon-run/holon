import { afterEach, describe, expect, it, vi } from "vitest";

import {
  GlobalSyncCoordinator,
  ROSTER_STALE_EXTENDED_RETRY_ATTEMPTS,
  retryDelayWithServerHintMs,
  type GlobalSyncStoreSet,
  type GlobalSyncStoreState,
} from "./global-sync-coordinator";
import {
  createRuntimeClient,
  RuntimeHttpError,
  type AgentRosterSnapshotDto,
  type AgentRosterSnapshotResult,
} from "./client";
import { useRuntimeStore, type AgentSessionState } from "./runtime-store";
import { createSessionProjectionState } from "./session-projection";

const OBSERVER_SYNC_CAPABILITIES = [
  "agents.roster-snapshot.v1",
  "agents.projection-snapshot.v1",
  "events.projection-effect.v1",
  "briefs.atomic-created-event.v1",
];

const RUNTIME_DB_BUSY_ENVELOPE = {
  ok: false,
  error: "runtime database operation reader pool checkout is temporarily unavailable",
  code: "runtime_db_busy",
  domain: "storage",
  retryable: true,
  hint: "retry the operation",
  context: { operation: "reader pool checkout" },
};

function stubAuthenticatedFetch(fetchMock: typeof fetch): void {
  vi.stubGlobal("fetch", (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname.endsWith("/auth/session/me")) {
      return Promise.resolve(Response.json({ user_id: "test-user", auth_method: "token" }));
    }
    return fetchMock(input, init);
  });
}

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

describe("authoritative discovery cutover", () => {
  afterEach(() => {
    useRuntimeStore.setState({
      sessionsByAgentId: {},
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
    vi.useRealTimers();
    vi.restoreAllMocks();
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
      throw new Error(`Unexpected request: ${url}`);
    });
    stubAuthenticatedFetch(fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();
    useRuntimeStore.setState({
      sessionsByAgentId: {
        "agent-a": sessionState({ eventLogEpoch: "epoch-1" }),
        "agent-b": sessionState({ eventLogEpoch: "epoch-1" }),
      },
    });

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
  });

  it.each(["epoch-1", "epoch-2"])(
    "refreshes unread state from roster updates without reload in %s",
    async (nextEpoch) => {
      vi.useFakeTimers();
      vi.stubGlobal("window", {
        localStorage: new MemoryStorage(), sessionStorage: new MemoryStorage(),
        setTimeout, clearTimeout, location: { hostname: "localhost", protocol: "http:" },
      });
      let epoch = "epoch-1";
      let head = 5;
      let delayRead = false;
      let releaseRead!: (response: Response) => void;
      const readState = () => ({
        agent_id: "agent-a", event_log_epoch: epoch, visibility_scope_id: "vis-1",
        oldest_retained_seq: 0, read_through_event_seq: 3, event_head_seq: head,
        reset_required: false, retention_gap: false, revision: head, unread_count: head - 3,
      });
      const fetchMock = vi.fn((input: string | URL | Request) => {
        const url = new URL(String(input), "http://localhost");
        if (url.pathname.endsWith("/handshake")) {
          return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
        }
        if (url.pathname.endsWith("/agents/list")) {
          return Promise.resolve(jsonResponse([listEntry("agent-a")]));
        }
        if (url.pathname.endsWith("/agents/snapshot")) {
          return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a"], { event_log_epoch: epoch })));
        }
        if (url.pathname.endsWith("/agents/brief-read-states")) {
          if (delayRead) {
            delayRead = false;
            return new Promise<Response>((resolve) => { releaseRead = resolve; });
          }
          return Promise.resolve(jsonResponse([readState()]));
        }
        throw new Error(`Unexpected request: ${url}`);
      });
      stubAuthenticatedFetch(fetchMock);
      await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
      await vi.advanceTimersByTimeAsync(0);
      expect(useRuntimeStore.getState().briefReadStateByAgentId["agent-a"]).toMatchObject({
        event_log_epoch: "epoch-1", event_head_seq: 5, unread_count: 2,
      });

      // A read request predating the next roster must not swallow its refresh.
      const oldState = readState();
      delayRead = true;
      const pending = useRuntimeStore.getState().refreshBriefReadStates();
      epoch = nextEpoch;
      head = 9;
      useRuntimeStore.getState().retryAgentSync("agent-a");
      await vi.advanceTimersByTimeAsync(2_000);
      expect(useRuntimeStore.getState().discovery.identity?.eventLogEpoch).toBe(nextEpoch);
      if (nextEpoch === "epoch-2") {
        expect(useRuntimeStore.getState().briefReadStateByAgentId["agent-a"]).toBeUndefined();
      }
      const restoredEpochs: string[] = [];
      const unsubscribe = useRuntimeStore.subscribe((state) => {
        const read = state.briefReadStateByAgentId["agent-a"];
        if (read) restoredEpochs.push(read.event_log_epoch);
      });
      try {
        releaseRead(jsonResponse([oldState]));
        await pending;
        await vi.advanceTimersByTimeAsync(0);
        expect(useRuntimeStore.getState().briefReadStateByAgentId["agent-a"]).toMatchObject({
          event_log_epoch: nextEpoch, event_head_seq: 9, unread_count: 6,
        });
        if (nextEpoch === "epoch-2") expect(restoredEpochs).not.toContain("epoch-1");
        expect(fetchMock.mock.calls.some(([input]) =>
          /\/events(?:\/stream)?(?:\?|$)/.test(String(input)))).toBe(false);
      } finally {
        unsubscribe();
      }
    },
  );

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
      throw new Error(`Unexpected request: ${url}`);
    });
    stubAuthenticatedFetch(fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

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

  it.each([
    { status: 500, envelope: { error: "snapshot assembly failed" } },
    { status: 503, envelope: RUNTIME_DB_BUSY_ENVELOPE },
  ])("keeps the last roster and retries without logging out on an HTTP $status snapshot failure", async ({ status, envelope }) => {
    vi.useFakeTimers();
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
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
          return Promise.resolve(errorJsonResponse(status, envelope));
        }
        return Promise.resolve(jsonResponse(rosterSnapshot(["agent-a", "agent-b"])));
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    stubAuthenticatedFetch(fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "stale",
        retryAttempt: 1,
      });
    });
    // A failed or partial snapshot never purges the previous roster.
    expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-a", "agent-b"]);
    expect(useRuntimeStore.getState().discovery.retryAt).toBeGreaterThan(Date.now());

    expect(useRuntimeStore.getState().currentUser).toEqual({ userId: "test-user", authMethod: "token" });
    expect(useRuntimeStore.getState().bootstrap.connection.authRequired).not.toBe(true);
    expect(useRuntimeStore.getState().discovery.unauthorizedReason).toBeUndefined();

    await vi.advanceTimersByTimeAsync(1_000);
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery).toMatchObject({
        mode: "authoritative",
        freshness: "fresh",
      });
    });
    expect(snapshotRequests).toBe(2);
    expect(useRuntimeStore.getState().currentUser?.userId).toBe("test-user");
  });

  it("escalates extended transient snapshot failure streaks without purging the roster", async () => {
    vi.useFakeTimers();
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
        return Promise.resolve(errorJsonResponse(500, { error: "snapshot assembly failed" }));
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "capability unavailable", code: "capability_unavailable" }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    stubAuthenticatedFetch(fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

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
      const attempt = useRuntimeStore.getState().discovery.retryAttempt;
      const retryAt = useRuntimeStore.getState().discovery.retryAt!;
      expect(retryAt).toBeGreaterThan(Date.now());
      await vi.advanceTimersByTimeAsync(retryAt - Date.now());
      expect(useRuntimeStore.getState().discovery.retryAttempt).toBe(attempt + 1);
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
      throw new Error(`Unexpected request: ${url}`);
    });
    stubAuthenticatedFetch(fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().discovery.freshness).toBe("unauthorized");
    });
    const discovery = useRuntimeStore.getState().discovery;
    expect(discovery.unauthorizedReason).toContain("control token required");
    expect(discovery.retryAt).toBeUndefined();
    // Auth rejection revokes private presentation, unlike a transient failure.
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
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
      throw new Error(`Unexpected request: ${url}`);
    });
    stubAuthenticatedFetch(fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    expect(useRuntimeStore.getState().bootstrapError).toContain(
      "missing capabilities: agents.roster-snapshot.v1",
    );
    expect(useRuntimeStore.getState().bootstrap.agents).toEqual([]);
    expect(fetchMock.mock.calls.filter((call) => String(call[0]).endsWith("/agents/snapshot"))).toHaveLength(0);
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

  it("discards an old discovery continuation after the authenticated subject changes", async () => {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(), sessionStorage: new MemoryStorage(),
      setTimeout, clearTimeout, location: { hostname: "localhost", protocol: "http:" },
    });
    let releaseOldRoster!: (response: Response) => void;
    const oldRoster = new Promise<Response>((resolve) => { releaseOldRoster = resolve; });
    let releaseBobRoster!: (response: Response) => void;
    const bobRoster = new Promise<Response>((resolve) => { releaseBobRoster = resolve; });
    let user = "alice";
    let snapshots = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/auth/session/me")) {
        return Promise.resolve(jsonResponse({ user_id: user, auth_method: "token" }));
      }
      if (url.pathname.endsWith("/handshake")) {
        return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      }
      if (url.pathname.endsWith("/agents/list")) {
        return user === "bob" ? bobRoster
          : Promise.resolve(jsonResponse([listEntry("agent-a"), listEntry("agent-b")]));
      }
      if (url.pathname.endsWith("/agents/snapshot")) {
        snapshots++;
        return snapshots === 1
          ? Promise.resolve(jsonResponse(rosterSnapshot(["agent-a", "agent-b"])))
          : oldRoster;
      }
      if (url.pathname.endsWith("/projection-snapshot")) {
        return Promise.resolve(errorJsonResponse(503, { error: "unavailable", code: "capability_unavailable" }));
      }
      if (/\/agents\/agent-[ab]\/events$/.test(url.pathname)) {
        const agentId = url.pathname.split("/").at(-2)!;
        return Promise.resolve(jsonResponse(url.searchParams.get("order") === "desc"
          ? baselinePage(agentId) : emptyEventsPage(agentId)));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    let refreshBob: Promise<void> | undefined;
    try {
      await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
      await vi.waitFor(() => {
        expect(useRuntimeStore.getState().discovery).toMatchObject({
          freshness: "fresh", identity: { eventLogEpoch: "epoch-1" },
        });
      });
      // Hold the old subject response across the authenticated subject change.
      await useRuntimeStore.getState().refreshBootstrap();
      await vi.waitFor(() => expect(snapshots).toBe(2));
      user = "bob";
      refreshBob = useRuntimeStore.getState().refreshBootstrap({ syncEvents: false });
      await vi.waitFor(() => expect(useRuntimeStore.getState().currentUser?.userId).toBe("bob"));
      releaseOldRoster(jsonResponse(rosterSnapshot(["agent-a"], { event_log_epoch: "epoch-2" })));
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(useRuntimeStore.getState().discovery).toMatchObject({ mode: "pending" });
      expect(useRuntimeStore.getState().discovery.identity).toBeUndefined();
      releaseBobRoster(jsonResponse([listEntry("agent-b")]));
      await refreshBob;
      expect(useRuntimeStore.getState().bootstrap.agents.map((agent) => agent.id)).toEqual(["agent-b"]);
    } finally {
      releaseOldRoster(jsonResponse(rosterSnapshot(["agent-a"], { event_log_epoch: "epoch-2" })));
      releaseBobRoster(jsonResponse([listEntry("agent-b")]));
      await refreshBob;
    }
  });

  function headWindowFetchMock() {
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

describe("roster refresh cadence", () => {
  let cleanup: (() => void) | undefined;

  afterEach(() => {
    cleanup?.();
    cleanup = undefined;
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  function deferred<T>() {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>((done) => { resolve = done; });
    return { promise, resolve };
  }

  function harness() {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    type State = GlobalSyncStoreState & {
      bootstrap: { agents: Array<{ id: string; latestBrief?: string }> };
    };
    let state: State = {
      bootstrap: { agents: [] },
      discovery: { mode: "pending", freshness: "fresh", retryAttempt: 0 },
    };
    const get = () => state;
    const set: GlobalSyncStoreSet<State> = (update) => {
      state = { ...state, ...(typeof update === "function" ? update(state) : update) };
    };
    let generation = 0;
    let active = 0;
    let maxActive = 0;
    const starts: number[] = [];
    const completions: number[] = [];
    const client = createRuntimeClient({ mode: "local" });
    const result = (ids = ["agent-a", "agent-b"], preview = "latest"): AgentRosterSnapshotResult => ({
      snapshot: rosterSnapshot(ids, {
        agents: ids.map((id) => ({
          agent: listEntry(id),
          event_window: { event_head_seq: 5, oldest_retained_seq: 0 },
          latest_brief: { preview },
        })),
      }) as unknown as AgentRosterSnapshotDto,
      projectionStale: false,
    });
    const response = vi.fn<() => Promise<AgentRosterSnapshotResult>>(async () => result());
    const applySnapshot = vi.fn((_set, snapshot: AgentRosterSnapshotDto) => {
      const agents = snapshot.agents.map((entry) => ({
        id: entry.agent.identity.agent_id,
        latestBrief: entry.latest_brief?.preview,
      }));
      const omitted = state.bootstrap.agents.filter((old) => !agents.some((next) => next.id === old.id));
      set({ bootstrap: { agents } });
      return omitted.map(({ id }) => id);
    });
    const coordinator = new GlobalSyncCoordinator<State>({
      captureClientRequest: () => ({ client, generation }),
      isCurrentClientRequest: (request) => request.generation === generation,
      fetchRosterSnapshot: async () => {
        starts.push(Date.now());
        maxActive = Math.max(maxActive, ++active);
        try {
          return await response();
        } finally {
          active -= 1;
          completions.push(Date.now());
        }
      },
      applyRosterSnapshot: applySnapshot,
    });
    cleanup = () => coordinator.closeForResume();
    return {
      coordinator, get, set, response, result, starts, completions, applySnapshot,
      maxActive: () => maxActive,
      refresh: () => coordinator.refreshRoster(get, set),
      open: async () => {
        coordinator.start(get, set);
        await vi.advanceTimersByTimeAsync(0);
      },
      switchSubject: () => {
        generation += 1;
        coordinator.resetForClientChange();
        set({ bootstrap: { agents: [] }, discovery: {
          mode: "pending", freshness: "fresh", retryAttempt: 0,
        } });
      },
    };
  }

  it("bounds continuous local changes to one refresh per second without idle extras", async () => {
    const h = harness();
    await h.open();
    const start = Date.now();
    expect(h.starts).toEqual([start]);
    for (let i = 0; i < 400; i += 1) {
      h.refresh();
      await vi.advanceTimersByTimeAsync(25);
    }
    expect(h.starts).toEqual(Array.from({ length: 11 }, (_, i) => start + i * 1_000));
    expect(h.maxActive()).toBe(1);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(h.starts).toHaveLength(11);
  });

  it("waits a second after a slow flight and keeps exactly one trailing refresh", async () => {
    const h = harness();
    const first = deferred<AgentRosterSnapshotResult>();
    h.response.mockImplementationOnce(() => first.promise);
    await h.open();
    h.refresh();
    await vi.advanceTimersByTimeAsync(2_500);
    expect(h.starts).toHaveLength(1);
    first.resolve(h.result());
    await vi.advanceTimersByTimeAsync(0);
    h.response.mockImplementation(async () => h.result(["agent-a", "agent-b"], "updated brief"));
    await vi.advanceTimersByTimeAsync(999);
    expect(h.starts).toHaveLength(1);
    h.refresh();
    await vi.advanceTimersByTimeAsync(1);
    expect(h.starts).toHaveLength(2);
    expect(h.starts[1] - h.completions[0]).toBe(1_000);
    expect(h.maxActive()).toBe(1);
    expect(h.get().bootstrap.agents).toEqual(["agent-a", "agent-b"].map((id) => ({
      id, latestBrief: "updated brief",
    })));
    await vi.advanceTimersByTimeAsync(2_000);
    expect(h.starts).toHaveLength(2);
    // An explicit delete still refreshes authoritative membership.
    h.response.mockImplementation(async () => h.result(["agent-c"]));
    h.refresh();
    await vi.advanceTimersByTimeAsync(0);
    expect(h.starts).toHaveLength(3);
    expect(h.get().bootstrap.agents.map(({ id }) => id)).toEqual(["agent-c"]);
  });

  it("keeps stale snapshots stale and local changes cannot shorten their exponential retry wait", async () => {
    const h = harness();
    h.response.mockImplementationOnce(async () => ({ ...h.result(), projectionStale: true }));
    h.response.mockImplementationOnce(async () => ({ ...h.result(), projectionStale: true }));
    await h.open();
    expect(h.get().discovery).toMatchObject({ freshness: "stale", retryAttempt: 1 });
    for (let i = 0; i < 10; i += 1) {
      h.refresh();
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(h.starts).toHaveLength(2);
    expect(h.get().discovery).toMatchObject({ freshness: "stale", retryAttempt: 2 });
    h.refresh();
    await vi.advanceTimersByTimeAsync(1_999);
    expect(h.starts).toHaveLength(2);
    expect(h.get().discovery.freshness).toBe("stale");
    await vi.advanceTimersByTimeAsync(1);
    expect(h.starts).toHaveLength(3);
    expect(h.get().discovery.freshness).toBe("fresh");
  });

  it.each([429, 503])("honors %i Retry-After across repeated local changes", async (status) => {
    const h = harness();
    h.response.mockRejectedValueOnce(new RuntimeHttpError(
      "GET", "/agents/snapshot", status, "busy", "projection_busy", undefined, undefined, 5,
    ));
    await h.open();
    expect(h.get().discovery).toMatchObject({ freshness: "stale", retryAttempt: 1 });
    for (let i = 0; i < 49; i += 1) {
      h.refresh();
      await vi.advanceTimersByTimeAsync(100);
    }
    await vi.advanceTimersByTimeAsync(99);
    expect(h.starts).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(h.starts).toHaveLength(2);
    expect(h.starts[1] - h.starts[0]).toBe(5_000);
    expect(h.get().discovery.freshness).toBe("fresh");
    await vi.advanceTimersByTimeAsync(2_000);
    expect(h.starts).toHaveLength(2);
  });

  it("makes roster 401 terminal even when later local changes arrive", async () => {
    const h = harness();
    h.response.mockRejectedValueOnce(new RuntimeHttpError("GET", "/agents/snapshot", 401, "denied", "auth_required"));
    await h.open();
    for (let i = 0; i < 50; i += 1) {
      h.refresh();
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(h.starts).toHaveLength(1);
    expect(h.get().discovery).toMatchObject({ freshness: "unauthorized", retryAt: undefined });
  });

  it("applies roster metadata without registering event recovery", async () => {
    const h = harness();
    await h.open();
    expect(h.applySnapshot).toHaveBeenCalledTimes(1);
    expect(h.get().bootstrap.agents.map(({ id }) => id)).toEqual(["agent-a", "agent-b"]);
  });

  it("discards old in-flight responses after a subject reset", async () => {
    const h = harness();
    const first = deferred<AgentRosterSnapshotResult>();
    h.response.mockImplementationOnce(() => first.promise);
    await h.open();
    h.switchSubject();
    h.response.mockImplementation(async () => h.result(["agent-c"]));
    await h.open();
    first.resolve(h.result());
    await vi.advanceTimersByTimeAsync(2_000);
    expect(h.starts).toHaveLength(2);
    expect(h.applySnapshot).toHaveBeenCalledTimes(1);
    expect(h.get().bootstrap.agents.map(({ id }) => id)).toEqual(["agent-c"]);
  });

  it.each(["closeForResume", "resetForClientChange"] as const)(
    "clears a pending refresh timer on %s",
    async (method) => {
      const h = harness();
      await h.open();
      const requests = h.starts.length;
      h.coordinator[method]();
      await vi.advanceTimersByTimeAsync(2_000);
      expect(h.starts).toHaveLength(requests);
      expect(vi.getTimerCount()).toBe(0);
    },
  );
});

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
