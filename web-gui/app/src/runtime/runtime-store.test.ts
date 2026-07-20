import { afterEach, describe, expect, it, vi } from "vitest";

import {
  agentBriefPatchFromEvents,
  buildResumeRefreshes,
  canUseRemoteRuntimeConnections,
  hasEventIdentityConflict,
  isSessionCacheContextCurrent,
  isLoopbackWebHostname,
  materializeProjectionDetail,
  mergeBootstrapAgentState,
  mergeCachedSessionIntoCurrent,
  missingBriefIdsForHydration,
  readStoredRemoteConnectionProfiles,
  resetSessionsForResume,
  resetTransientRuntimeStateForResume,
  readStoredRuntimeConnectionConfig,
  runWithConcurrencyLimit,
  sessionForEventLogEpoch,
  streamEventFromBackfill,
  useRuntimeStore,
  writeStoredRuntimeConnectionConfig,
} from "./runtime-store";
import type { StreamEventEnvelopeDto } from "./client";
import type { AgentSessionState } from "./runtime-store";
import { createSessionProjectionState, reduceSessionProjection } from "./session-projection";
import type { AgentSummary } from "./types";

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
    loadingOlder: false,
    liveStatus: "idle",
    sendingPrompt: false,
    detail: null,
    workItemDetailsById: {},
    taskDetailsById: {},
    toolExecutionDetailsById: {},
    ...overrides,
  };
}

function agentSummary(overrides: Partial<AgentSummary> = {}): AgentSummary {
  return {
    id: "agent-a",
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
    ...overrides,
  };
}

describe("agent snapshot merging", () => {
  it("lets a fresh bootstrap snapshot clear cached running state and counts", () => {
    const cached = agentSummary({
      lifecycle: "awake-running",
      currentRunId: "run-old",
      pending: 3,
      activeTaskCount: 2,
      waitingCount: 1,
      tasks: [{ id: "task-old", kind: "command", status: "running", summary: "old" }],
    });
    const fresh = agentSummary({
      lifecycle: "asleep",
      currentRunId: null,
      pending: 0,
      activeTaskCount: 0,
      waitingCount: 0,
      tasks: [],
    });

    expect(mergeBootstrapAgentState(fresh, cached)).toMatchObject({
      lifecycle: "asleep",
      currentRunId: null,
      pending: 0,
      activeTaskCount: 0,
      waitingCount: 0,
    });
  });

  it("preserves rich detail omitted by the bootstrap snapshot", () => {
    const cachedWorkItem = { id: "work-1", objective: "Preserve me", state: "open", current: true };
    const cached = agentSummary({
      currentWork: cachedWorkItem,
      workItems: [cachedWorkItem],
      tasks: [{ id: "task-old", kind: "command", status: "completed", summary: "cached" }],
      attachedWorkspaces: [{ workspaceId: "ws-1", name: "repo", anchor: "/repo" }],
    });

    expect(mergeBootstrapAgentState(agentSummary(), cached)).toMatchObject({
      currentWork: cachedWorkItem,
      workItems: [cachedWorkItem],
      tasks: cached.tasks,
      attachedWorkspaces: cached.attachedWorkspaces,
    });
  });
});

describe("resume session reset", () => {
  it("clears stale transport loading state before reconciliation restarts", () => {
    const reset = resetSessionsForResume({
      "agent-a": sessionState({
        loading: true,
        loadingOlder: true,
        sendingPrompt: true,
        liveStatus: "recovering",
        reconnectAttempt: 4,
        workItemDetailsById: { "work-1": { loading: true } },
        taskDetailsById: { "task-1": { loading: true } },
        toolExecutionDetailsById: { "tool-1": { loading: true } },
      }),
    });

    expect(reset["agent-a"]).toMatchObject({
      loading: false,
      loadingOlder: false,
      sendingPrompt: false,
      liveStatus: "stale",
      reconnectAttempt: 0,
      workItemDetailsById: { "work-1": { loading: false } },
      taskDetailsById: { "task-1": { loading: false } },
      toolExecutionDetailsById: { "tool-1": { loading: false } },
    });
  });

  it("clears global transient loading state invalidated by the new generation", () => {
    const patch = resetTransientRuntimeStateForResume({
      ...useRuntimeStore.getState(),
      modelCatalogLoading: true,
      runtimeConfigLoading: true,
      runtimeConfigSaving: true,
      skillCatalogLoading: true,
      skillDetailLoadingById: { skill: true },
      templateCatalogLoading: true,
      templateSyncInProgress: true,
      templateDetailLoadingById: { template: true },
      agentSkillCatalogLoadingByAgentId: { agent: true },
      credentialStoreLoading: true,
      codexDeviceLogin: { status: "waiting", jobId: "job-1" },
      searchLoading: true,
      searchResultContentLoadingBySourceRef: { source: true },
      rightPanelView: {
        kind: "task_detail",
        agentId: "agent-a",
        task: { id: "task-current", kind: "command", status: "running", summary: "Current" },
        detailState: { loading: true },
      },
      rightPanelViewStack: [{
        kind: "tool_execution_detail",
        agentId: "agent-a",
        toolExecutionId: "tool-stacked",
        detailState: { loading: true },
      }],
    });

    expect(patch).toMatchObject({
      modelCatalogLoading: false,
      runtimeConfigLoading: false,
      runtimeConfigSaving: false,
      skillCatalogLoading: false,
      skillDetailLoadingById: { skill: false },
      templateCatalogLoading: false,
      templateSyncInProgress: false,
      templateDetailLoadingById: { template: false },
      agentSkillCatalogLoadingByAgentId: { agent: false },
      credentialStoreLoading: false,
      codexDeviceLogin: { status: "idle" },
      searchLoading: false,
      searchResultContentLoadingBySourceRef: { source: false },
      rightPanelView: { detailState: { loading: false } },
      rightPanelViewStack: [{ detailState: { loading: false } }],
    });
  });
});

describe("runtime event epoch", () => {
  it("drops seq-indexed history and hydration caches when the epoch changes", () => {
    const current = sessionState({
      eventLogEpoch: "epoch-old",
      eventsBySeq: { 7: { id: "evt-old" } },
      eventSeqs: [7],
      messagesById: { msg: { id: "msg" } },
      newestSeq: 7,
      oldestSeq: 7,
      hasOlder: true,
      detail: {
        agent: { id: "agent-1" } as NonNullable<AgentSessionState["detail"]>["agent"],
        source: "http",
        timeline: [],
        events: [],
        eventCursorSeq: 7,
        hasOlderEvents: true,
      },
    });

    const reset = sessionForEventLogEpoch(current, "epoch-new");

    expect(reset.eventLogEpoch).toBe("epoch-new");
    expect(reset.eventsBySeq).toEqual({});
    expect(reset.eventSeqs).toEqual([]);
    expect(reset.messagesById).toEqual({});
    expect(reset.newestSeq).toBeUndefined();
    expect(reset.oldestSeq).toBeUndefined();
    expect(reset.hasOlder).toBeUndefined();
    expect(reset.detail?.eventCursorSeq).toBeUndefined();
    expect(reset.detail?.hasOlderEvents).toBeUndefined();
  });

  it("detects conflicting immutable content for the same epoch and sequence", () => {
    const existing: StreamEventEnvelopeDto = {
      id: "evt-1",
      event_seq: 7,
      event_log_epoch: "epoch-1",
      contract_version: 1,
      ts: "2026-07-16T00:00:00Z",
      agent_id: "agent-1",
      type: "legacy_event",
      payload_schema: "holon.runtime_event.legacy",
      payload_schema_version: 1,
      provenance: {},
      payload: { value: 1 },
    };
    const current = sessionState({
      eventLogEpoch: "epoch-1",
      eventsBySeq: { 7: existing },
      eventSeqs: [7],
    });

    expect(hasEventIdentityConflict(current, [{ ...existing }])).toBe(false);
    expect(
      hasEventIdentityConflict(current, [
        { ...existing, id: "evt-conflict", payload: { value: 2 } },
      ]),
    ).toBe(true);
  });

  it("preserves typed contract metadata when rebuilding gap backfill events", () => {
    const provenance = {
      source: "runtime",
      correlation_id: "correlation-1",
    };
    const event = streamEventFromBackfill(
      {
        id: "evt-1",
        event_seq: 7,
        event_log_epoch: "",
        contract_version: 2,
        ts: "2026-07-16T00:00:00Z",
        agent_id: "page-agent",
        type: "brief_created",
        payload_schema: "holon.runtime_event.brief_created",
        payload_schema_version: 1,
        provenance,
        payload: { brief_id: "brief-1" },
      },
      "subscribed-agent",
      "epoch-1",
    );

    expect(event).toMatchObject({
      event_log_epoch: "epoch-1",
      agent_id: "subscribed-agent",
      contract_version: 2,
      payload_schema: "holon.runtime_event.brief_created",
      payload_schema_version: 1,
      provenance,
    });
  });
});

describe("session cache restoration", () => {
  it("rejects cache hydration captured for an older remote or generation", () => {
    const captured = { remoteKey: "https://old.example", generation: 7 };

    expect(isSessionCacheContextCurrent(captured, "https://old.example", 7)).toBe(true);
    expect(isSessionCacheContextCurrent(captured, "https://new.example", 7)).toBe(false);
    expect(isSessionCacheContextCurrent(captured, "https://old.example", 8)).toBe(false);
  });

  it("does not overwrite HTTP or SSE state that arrived while cache was loading", () => {
    const current = sessionState({
      eventLogEpoch: "epoch-live",
      eventsBySeq: { 9: { id: "live-event", event_seq: 9 } },
      eventSeqs: [9],
      newestSeq: 9,
      oldestSeq: 9,
      liveStatus: "streaming",
    });
    const cached = {
      ...createSessionProjectionState("epoch-cache"),
      eventsBySeq: { 1: { id: "cached-event", event_seq: 1 } },
      eventSeqs: [1],
      newestSeq: 1,
      oldestSeq: 1,
    };

    expect(mergeCachedSessionIntoCurrent(current, cached)).toBe(current);
  });

  it("restores cached projection into an empty session without changing UI state", () => {
    const current = sessionState({ loading: true, liveStatus: "connecting" });
    const cached = {
      ...createSessionProjectionState("epoch-cache"),
      eventsBySeq: { 1: { id: "cached-event", event_seq: 1 } },
      eventSeqs: [1],
      newestSeq: 1,
      oldestSeq: 1,
    };

    const restored = mergeCachedSessionIntoCurrent(current, cached);

    expect(restored.eventSeqs).toEqual([1]);
    expect(restored.loading).toBe(true);
    expect(restored.liveStatus).toBe("connecting");
  });
});

function installWindow(localStorage: Storage, sessionStorage: Storage, hostname = "localhost") {
  vi.stubGlobal("window", {
    clearTimeout: () => undefined,
    location: { hostname },
    localStorage,
    sessionStorage,
  });
}

describe("runtime connection storage", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("keeps active runtime connections isolated per window session", () => {
    const sharedLocalStorage = new MemoryStorage();
    const remoteWindowSession = new MemoryStorage();
    const localWindowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, remoteWindowSession);
    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "http://remote.example:7878/",
      token: "remote-token",
    });

    installWindow(sharedLocalStorage, localWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    writeStoredRuntimeConnectionConfig({ mode: "local" });

    installWindow(sharedLocalStorage, remoteWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "remote-token",
    });

    installWindow(sharedLocalStorage, localWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
  });

  it("retains saved remote tokens without making new windows remote by default", () => {
    const sharedLocalStorage = new MemoryStorage();
    const firstWindowSession = new MemoryStorage();
    const secondWindowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, firstWindowSession);
    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "saved-token",
    });

    installWindow(sharedLocalStorage, secondWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    writeStoredRuntimeConnectionConfig({ mode: "remote", baseUrl: "http://remote.example:7878" });

    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "saved-token",
    });
  });

  it("keeps same-origin runtime tokens in the active window session", () => {
    const sharedLocalStorage = new MemoryStorage();
    const windowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, windowSession, "100.92.113.47");
    writeStoredRuntimeConnectionConfig({ mode: "local", token: "same-origin-token" });

    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "local",
      token: "same-origin-token",
    });
    expect(readStoredRemoteConnectionProfiles()).toEqual([]);
  });

  it("detects loopback page origins as eligible for remote runtime connections", () => {
    expect(isLoopbackWebHostname("localhost")).toBe(true);
    expect(isLoopbackWebHostname("127.0.0.1")).toBe(true);
    expect(isLoopbackWebHostname("127.42.0.9")).toBe(true);
    expect(isLoopbackWebHostname("::1")).toBe(true);
    expect(isLoopbackWebHostname("100.92.113.47")).toBe(false);
    expect(isLoopbackWebHostname("holon.example.test")).toBe(false);
  });

  it("forces same-origin local mode on non-loopback embedded pages", () => {
    const sharedLocalStorage = new MemoryStorage();
    const remoteWindowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, remoteWindowSession, "100.92.113.47");
    expect(canUseRemoteRuntimeConnections()).toBe(false);

    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "http://127.0.0.1:7878",
      token: "saved-token",
    });

    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    expect(readStoredRemoteConnectionProfiles()).toEqual([]);
  });
});

describe("roster activity unread state", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("hydrates persisted roster activity per remote key", async () => {
    const sharedLocalStorage = new MemoryStorage();
    installWindow(sharedLocalStorage, new MemoryStorage());
    sharedLocalStorage.setItem(
      "holon.webGui.rosterActivityByRemote.v1",
      JSON.stringify({
        local: {
          localAgent: { unreadCount: 2, lastUnreadSeq: 12, lastReadSeq: 7, briefAt: "2026-01-01T00:00:00.000Z" },
        },
        "http://remote.example:7878": {
          remoteAgent: { unreadCount: 4, lastUnreadSeq: 20 },
        },
      }),
    );

    const { readStoredRosterActivity } = await import("./runtime-store");

    expect(readStoredRosterActivity("local")).toEqual({
      localAgent: { unreadCount: 2, lastUnreadSeq: 12, lastReadSeq: 7, briefAt: "2026-01-01T00:00:00.000Z" },
    });
    expect(readStoredRosterActivity("http://remote.example:7878")).toEqual({
      remoteAgent: { unreadCount: 4, lastUnreadSeq: 20 },
    });
  });

  it("counts unread brief and non-operator message events once by seq", async () => {
    const { touchRosterActivityFromEvent } = await import("./runtime-store");
    const afterBrief = touchRosterActivityFromEvent(
      {},
      "agent-a",
      { agent_id: "agent-a", event_seq: 10, ts: "2026-01-01T00:00:00.000Z", type: "brief_created", payload: {} },
      "agent-b",
    );
    const afterDuplicate = touchRosterActivityFromEvent(
      afterBrief,
      "agent-a",
      { agent_id: "agent-a", event_seq: 10, ts: "2026-01-01T00:00:01.000Z", type: "brief_created", payload: {} },
      "agent-b",
    );
    const afterAgentMessage = touchRosterActivityFromEvent(
      afterDuplicate,
      "agent-a",
      {
        agent_id: "agent-a",
        event_seq: 11,
        ts: "2026-01-01T00:00:02.000Z",
        type: "message_enqueued",
        payload: { origin: { kind: "agent" } },
      },
      "agent-b",
    );

    expect(afterAgentMessage["agent-a"]).toMatchObject({ unreadCount: 2, lastUnreadSeq: 11 });
  });

  it("does not count unread for the currently open agent or operator messages", async () => {
    const { touchRosterActivityFromEvent } = await import("./runtime-store");
    const afterSelectedBrief = touchRosterActivityFromEvent(
      {},
      "agent-a",
      { agent_id: "agent-a", event_seq: 10, ts: "2026-01-01T00:00:00.000Z", type: "brief_created", payload: {} },
      "agent-a",
    );
    const afterOperatorMessage = touchRosterActivityFromEvent(
      afterSelectedBrief,
      "agent-a",
      {
        agent_id: "agent-a",
        event_seq: 11,
        ts: "2026-01-01T00:00:01.000Z",
        type: "message_enqueued",
        payload: { origin: { kind: "operator" }, created_at: "2026-01-01T00:00:01.000Z" },
      },
      "agent-b",
    );

    expect(afterOperatorMessage["agent-a"]?.unreadCount).toBeUndefined();
    expect(afterOperatorMessage["agent-a"]?.operatorAt).toBe("2026-01-01T00:00:01.000Z");
  });
});

describe("brief projection and hydration", () => {
  it("uses persisted brief text for roster patches", () => {
    const patch = agentBriefPatchFromEvents(
      [
        {
          agent_id: "agent-a",
          event_seq: 23,
          ts: "2026-07-10T00:00:00Z",
          type: "brief_created",
          payload: {
            brief_id: "brief-123",
            finalizes_assistant_round_id: "round-123",
          },
        },
      ],
      {
        "brief-123": {
          id: "brief-123",
          text: "Canonical persisted brief.",
        },
      },
    );

    expect(patch).toEqual(
      expect.objectContaining({
        lastBrief: "Canonical persisted brief.",
      }),
    );
  });

  it("hydrates a missing brief even when its associated transcript is loaded", () => {
    const session: AgentSessionState = {
      ...createSessionProjectionState(),
      loading: false,
      loadingOlder: false,
      liveStatus: "idle",
      sendingPrompt: false,
      detail: null,
      eventsBySeq: {
        23: {
          agent_id: "agent-a",
          event_seq: 23,
          ts: "2026-07-10T00:00:00Z",
          type: "brief_created",
          payload: {
            brief_id: "brief-123",
            finalizes_assistant_round_id: "round-123",
          },
        },
      },
      eventSeqs: [23],
      referencedBriefIds: { "brief-123": true },
      transcriptEntriesById: {
        "round-123": {
          id: "round-123",
          data: {
            blocks: [
              { type: "thinking", text: "Internal reasoning must not be visible." },
              { type: "text", text: "Transcript final text." },
            ],
          },
        },
      },
      workItemDetailsById: {},
      taskDetailsById: {},
      toolExecutionDetailsById: {},
    };

    expect(missingBriefIdsForHydration(session)).toEqual(["brief-123"]);
  });
});

describe("optimistic operator prompt reconciliation", () => {
  it("removes a confirmed optimistic item when its canonical message is projected", () => {
    const projection = reduceSessionProjection(createSessionProjectionState(), {
      type: "events_received",
      eventLogEpoch: "epoch-1",
      events: [{
        id: "message-event",
        event_seq: 1,
        event_log_epoch: "epoch-1",
        ts: "2026-07-17T00:00:01Z",
        type: "message_enqueued",
        payload: {
          message_id: "message-123",
          origin: { kind: "operator" },
          body: "Run the checks",
        },
      }],
    });
    const detail = materializeProjectionDetail({
      agent: { id: "agent-1" } as NonNullable<AgentSessionState["detail"]>["agent"],
      source: "http",
      timeline: [{
        id: "operator-prompt:pending:client-123",
        kind: "operator",
        label: "Operator input",
        body: "Run the checks",
        timestamp: "2026-07-17T00:00:00Z",
        meta: "Sent",
        minDisplayLevel: "info",
        sourceIds: [
          "pending-operator-prompt",
          "operator-prompt-client:client-123",
          "operator-prompt-message:message-123",
        ],
      }, {
        id: "operator-prompt:pending:client-456",
        kind: "operator",
        label: "Operator input",
        body: "Run different checks",
        timestamp: "2026-07-17T00:00:00Z",
        meta: "Sent",
        minDisplayLevel: "info",
        sourceIds: [
          "pending-operator-prompt",
          "operator-prompt-client:client-456",
          "operator-prompt-message:message-456",
        ],
      }],
    }, projection, "info");

    expect(detail?.timeline).toHaveLength(2);
    expect(detail?.timeline).toEqual(expect.arrayContaining([
      expect.objectContaining({
        id: "message:message-123",
      }),
      expect.objectContaining({
      id: "operator-prompt:pending:client-456",
      body: "Run different checks",
      }),
    ]));
    expect(detail?.timeline).not.toEqual(expect.arrayContaining([
      expect.objectContaining({
        id: "operator-prompt:pending:client-123",
      }),
    ]));
  });
});

describe("runtime client generation", () => {
  it("drops an old work-item response after switching clients with the same agent id", async () => {
    const localStorage = new MemoryStorage();
    const sessionStorage = new MemoryStorage();
    vi.stubGlobal("window", {
      localStorage,
      sessionStorage,
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });

    let resolveOldWorkItems!: (response: Response) => void;
    const oldWorkItems = new Promise<Response>((resolve) => {
      resolveOldWorkItems = resolve;
    });
    const fetchMock = vi.fn((input: string | URL | Request) => {
      const url = String(input);
      if (url.includes("/agents/agent-a/work-items")) return oldWorkItems;
      if (url.endsWith("/handshake")) return Promise.resolve(jsonResponse({}));
      if (url.endsWith("/agents/list")) {
        return Promise.resolve(jsonResponse([{ id: "agent-a", lifecycle: "asleep" }]));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    try {
      await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
      const staleRefresh = useRuntimeStore.getState().refreshAgentWorkItems("agent-a");
      await vi.waitFor(() => {
        expect(fetchMock).toHaveBeenCalledWith(
          expect.stringContaining("/agents/agent-a/work-items"),
          expect.anything(),
        );
      });

      await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
      resolveOldWorkItems(jsonResponse([{ id: "old-work", objective: "old remote", state: "open" }]));
      await staleRefresh;

      expect(useRuntimeStore.getState().bootstrap.agents[0]?.workItems).toEqual([]);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe("bounded resume refresh scheduling", () => {
  it("uses detail for the selected agent instead of scheduling duplicate state and detail refreshes", () => {
    expect(buildResumeRefreshes(["agent-a", "agent-b", "agent-c"], "agent-b")).toEqual([
      { agentId: "agent-a", detail: false },
      { agentId: "agent-b", detail: true },
      { agentId: "agent-c", detail: false },
    ]);
  });

  it("limits concurrent refreshes", async () => {
    let active = 0;
    let maxActive = 0;
    const releases: Array<() => void> = [];

    const scheduled = runWithConcurrencyLimit(
      Array.from({ length: 10 }, (_, index) => index),
      4,
      async () => {
        active += 1;
        maxActive = Math.max(maxActive, active);
        await new Promise<void>((resolve) => releases.push(resolve));
        active -= 1;
      },
    );

    await vi.waitFor(() => expect(active).toBe(4));
    while (releases.length) {
      releases.shift()?.();
      await Promise.resolve();
    }
    await scheduled;

    expect(maxActive).toBe(4);
  });

  it("stops starting queued refreshes after the generation becomes stale", async () => {
    let current = true;
    const started: number[] = [];

    await runWithConcurrencyLimit(
      [1, 2, 3, 4, 5],
      1,
      async (value) => {
        started.push(value);
        current = false;
      },
      () => current,
    );

    expect(started).toEqual([1]);
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}
