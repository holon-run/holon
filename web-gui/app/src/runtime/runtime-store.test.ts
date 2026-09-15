import { afterEach, describe, expect, it, vi } from "vitest";

import {
  appendOptimisticOperatorPrompt,
  agentBriefPatchFromEvents,
  agentDetailErrorKind,
  applyStreamEvents,
  backfillRetryDelayMs,
  canUseRemoteRuntimeConnections,
  isSessionCacheContextCurrent,
  isLoopbackWebHostname,
  materializeProjectionDetail,
  mergeBootstrapAgentState,
  mergeTimelineEventPage,
  modelCatalogCacheKey,
  observerSyncDiagnostics,
  clearStoredRuntimeConnectionToken,
  readStoredRemoteConnectionProfiles,
  retryPendingReadMarker,
  resetSessionsForResume,
  resetTransientRuntimeStateForResume,
  readStoredRuntimeConnectionConfig,
  runWithConcurrencyLimit,
  skillDetailCacheKey,
  streamEventFromBackfill,
  useRuntimeStore,
  writeStoredRuntimeConnectionConfig,
} from "./runtime-store";
import type { StreamEventEnvelopeDto } from "./client";
import { AgentSessionRepository } from "./agent-session-repository";
import {
  getRuntimeTraceRecords,
  setRuntimeTraceEnabled,
} from "./runtime-trace";
import type { AgentSessionState } from "./runtime-store";
import { createSessionProjectionState, reduceSessionProjection } from "./session-projection";
import type { AgentSummary } from "./types";

const OBSERVER_SYNC_CAPABILITIES = [
  "agents.roster-snapshot.v1",
  "agents.projection-snapshot.v1",
  "events.projection-effect.v1",
  "briefs.atomic-created-event.v1",
];

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
    detail: null,
    workItemDetailsById: {},
    taskDetailsById: {},
    toolExecutionDetailsById: {},
    ...overrides,
  };
}

describe("sendOperatorPrompt", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("reports UUID generation failures instead of silently ignoring the send", async () => {
    const previous = useRuntimeStore.getState();
    useRuntimeStore.setState({
      ...previous,
      sessionsByAgentId: {
        "agent-a": sessionState(),
      },
    }, true);
    vi.stubGlobal("crypto", {});

    try {
      await expect(
        useRuntimeStore.getState().sendOperatorPrompt("agent-a", "hello", "info"),
      ).rejects.toThrow("Secure random number generation is unavailable");

      expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toMatchObject({
        sendingPrompt: false,
        promptError: "Secure random number generation is unavailable",
      });
    } finally {
      useRuntimeStore.setState(previous, true);
    }
  });
});

describe("abortCurrentRun", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function abortFetchMock(handlers: {
    abort?: (init?: RequestInit) => Response;
  } = {}) {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith("/control/agents/agent-a/current-run/abort")) {
        return handlers.abort
          ? handlers.abort(init)
          : jsonResponse({ ok: true, aborted: true });
      }
      if (url.endsWith("/handshake")) {
        return jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES });
      }
      if (url.endsWith("/agents/list")) return jsonResponse([]);
      if (url.endsWith("/agents/snapshot")) {
        return jsonResponse({
          contract_version: 1,
          runtime_id: "runtime-1",
          event_log_epoch: "epoch-1",
          visibility_scope_id: "scope-1",
          agents: [],
        });
      }
      throw new Error(`Unexpected request: ${url}`);
    });
  }

  it("posts a turn-scoped abort with the current run id", async () => {
    const bodies: unknown[] = [];
    const fetchMock = abortFetchMock({
      abort: (init) => {
        bodies.push(init?.body ? JSON.parse(String(init.body)) : undefined);
        return jsonResponse({ ok: true, aborted: true });
      },
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

    useRuntimeStore.setState({
      sessionsByAgentId: { "agent-a": sessionState() },
    });

    await useRuntimeStore.getState().abortCurrentRun("agent-a", "run-1");

    expect(bodies).toEqual([
      { run_id: "run-1", mode: "idle_after_abort", authority_class: "operator_instruction" },
    ]);
    expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toMatchObject({
      abortingRun: false,
      abortError: undefined,
    });
  });

  it("treats stale run conflicts as benign convergence instead of an error", async () => {
    const fetchMock = abortFetchMock({
      abort: () =>
        new Response(
          JSON.stringify({
            error: "stale run_id run-old; current run is run-new",
            code: "stale_run_id",
          }),
          { status: 409, headers: { "content-type": "application/json" } },
        ),
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

    useRuntimeStore.setState({
      sessionsByAgentId: { "agent-a": sessionState() },
    });

    await useRuntimeStore.getState().abortCurrentRun("agent-a", "run-old");

    expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toMatchObject({
      abortingRun: false,
      abortError: undefined,
    });
  });

  it("surfaces abort failures as abortError and rethrows", async () => {
    const fetchMock = abortFetchMock({
      abort: () =>
        new Response(JSON.stringify({ error: "gateway exploded" }), {
          status: 502,
          headers: { "content-type": "application/json" },
        }),
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

    useRuntimeStore.setState({
      sessionsByAgentId: { "agent-a": sessionState() },
    });

    await expect(
      useRuntimeStore.getState().abortCurrentRun("agent-a", "run-1"),
    ).rejects.toThrow("current-run/abort failed with 502");

    expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toMatchObject({
      abortingRun: false,
    });
    expect(
      useRuntimeStore.getState().sessionsByAgentId["agent-a"]?.abortError,
    ).toContain("gateway exploded");
  });

  it("ignores abort requests without an agent or run id", async () => {
    const fetchMock = abortFetchMock();
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

    await useRuntimeStore.getState().abortCurrentRun(undefined, "run-1");
    await useRuntimeStore.getState().abortCurrentRun("agent-a", null);
    await useRuntimeStore.getState().abortCurrentRun("agent-a", undefined);

    expect(fetchMock).not.toHaveBeenCalledWith(
      expect.stringContaining("current-run/abort"),
      expect.anything(),
    );
  });
});

describe("refreshAgentDetail last-known-good", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function stubWindow() {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
  }

  function lastKnownGoodSummary(): AgentSummary {
    return {
      id: "agent-a",
      badge: "W",
      profile: "developer",
      lifecycle: "awake-idle",
      focusSummary: "Idle",
      workspace: "workspace-a",
      attention: "none",
      model: "test-model-live",
      footer: "",
      subtitle: "",
      lastBrief: "",
      lastTurnTime: "",
      pending: 0,
      activeTaskCount: 0,
      waitingCount: 0,
      posture: "idle",
      postureReason: "",
      tasks: [],
      workItems: [],
    };
  }

  function failingDetailFetchMock() {
    return vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/handshake")) {
        return jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES });
      }
      if (url.endsWith("/agents/list")) return jsonResponse([]);
      if (url.endsWith("/agents/snapshot")) {
        return jsonResponse({
          contract_version: 1,
          runtime_id: "runtime-1",
          event_log_epoch: "epoch-1",
          visibility_scope_id: "scope-1",
          agents: [],
        });
      }
      if (url.endsWith("/agents/agent-a/state")) {
        return new Response("server error", { status: 500 });
      }
      if (url.includes("/agents/agent-a/events")) {
        return jsonResponse({ events: [], has_older: false });
      }
      if (url.includes("/agents/agent-a/work-items")) return jsonResponse([]);
      throw new Error(`Unexpected request: ${url}`);
    });
  }

  it("keeps the last-known-good detail and marks the session reconnecting when the detail fetch fails", async () => {
    stubWindow();
    vi.stubGlobal("fetch", failingDetailFetchMock());
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

    const previous = useRuntimeStore.getState();
    try {
      useRuntimeStore.setState({
        ...previous,
        sessionsByAgentId: {
          "agent-a": sessionState({
            contentStatus: "available",
            detail: {
              agent: lastKnownGoodSummary(),
              timeline: [],
              source: "http",
              events: [],
            },
          }),
        },
      }, true);

      await useRuntimeStore.getState().refreshAgentDetail("agent-a", "info");

      const session = useRuntimeStore.getState().sessionsByAgentId["agent-a"];
      expect(session?.detail?.agent.model).toBe("test-model-live");
      expect(session?.detail?.agent.badge).toBe("W");
      expect(session?.syncStatus).toBe("reconnecting");
      expect(session?.error).toBeTruthy();
      expect(session?.detail?.error).toBeUndefined();
    } finally {
      useRuntimeStore.setState(previous, true);
    }
  });

  it("still renders the disconnected placeholder on a cold start without cached detail", async () => {
    stubWindow();
    vi.stubGlobal("fetch", failingDetailFetchMock());
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

    const previous = useRuntimeStore.getState();
    try {
      useRuntimeStore.setState({
        ...previous,
        sessionsByAgentId: { "agent-a": sessionState() },
      }, true);

      await useRuntimeStore.getState().refreshAgentDetail("agent-a", "info");

      const session = useRuntimeStore.getState().sessionsByAgentId["agent-a"];
      expect(session?.detail?.agent.model).toBe("unavailable");
      expect(session?.detail?.agent.badge).toBe("!");
      expect(session?.syncStatus).toBe("error");
    } finally {
      useRuntimeStore.setState(previous, true);
    }
  });
});

describe("credential mutations", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns durable credential success when the later model refresh fails", async () => {
    const previous = useRuntimeStore.getState();
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith("/handshake")) {
        return jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES });
      }
      if (url.endsWith("/agents/list")) return jsonResponse([]);
      if (url.endsWith("/agents/snapshot")) {
        return jsonResponse({
          contract_version: 1,
          runtime_id: "runtime-1",
          event_log_epoch: "epoch-1",
          visibility_scope_id: "scope-1",
          agents: [],
        });
      }
      if (url.endsWith("/control/runtime/credentials/openai%3Adefault") && init?.method === "PUT") {
        return jsonResponse({
          ok: true,
          profile: { profile: "openai:default", kind: "api_key", configured: true },
          reload_generation: 1,
        });
      }
      if (url.endsWith("/control/runtime/credentials")) {
        return jsonResponse({
          ok: true,
          profiles: [{ profile: "openai:default", kind: "api_key", configured: true }],
        });
      }
      if (url.endsWith("/control/runtime/config")) {
        return jsonResponse({
          ok: true,
          runtime_surface: {},
          reload: {
            requested_generation: 1,
            completed_generation: 1,
            state: "completed",
          },
        });
      }
      if (url.endsWith("/models/refresh")) {
        return new Response(JSON.stringify({ error: "catalog refresh failed" }), {
          status: 500,
          headers: { "content-type": "application/json" },
        });
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });

    try {
      const result = await useRuntimeStore
        .getState()
        .setCredential("openai:default", "api_key", "secret");

      expect(result).toMatchObject({
        reloadGeneration: 1,
        profile: { profile: "openai:default", configured: true },
      });
      await vi.waitFor(() => {
        expect(useRuntimeStore.getState().modelCatalogError).toContain("catalog refresh failed");
      });
    } finally {
      useRuntimeStore.setState(previous, true);
    }
  });
});

describe("appendOptimisticOperatorPrompt", () => {
  it("attributes the pending prompt to the current user display name", () => {
    const agent = { id: "agent-a" } as AgentSummary;
    const attributed = appendOptimisticOperatorPrompt(null, agent, "hello", "client-1", "Alice");
    expect(attributed?.timeline.at(-1)).toMatchObject({
      kind: "operator",
      body: "hello",
      senderName: "Alice",
    });
  });

  it("keeps local control prompts unattributed", () => {
    const agent = { id: "agent-a" } as AgentSummary;
    const local = appendOptimisticOperatorPrompt(null, agent, "hello", "client-2", undefined);
    expect(local?.timeline.at(-1)).toMatchObject({ kind: "operator", body: "hello" });
    expect(local?.timeline.at(-1)?.senderName).toBeUndefined();
  });
});

describe("skillDetailCacheKey", () => {
  it("keeps global and agent-scoped versions separate", () => {
    expect(skillDetailCacheKey("workspace:root:demo")).toBe("workspace:root:demo");
    expect(skillDetailCacheKey("workspace:root:demo", "agent-a")).toBe(
      "agent-a\u0000workspace:root:demo",
    );
  });
});

describe("modelCatalogCacheKey", () => {
  it("isolates model catalogs by runtime and credential without storing the token", () => {
    const first = modelCatalogCacheKey({
      mode: "remote",
      baseUrl: "https://runtime.example/",
      token: "secret-a",
    });
    const second = modelCatalogCacheKey({
      mode: "remote",
      baseUrl: "https://runtime.example",
      token: "secret-b",
    });

    expect(first).not.toBe(second);
    expect(first).toContain("https://runtime.example#auth-");
    expect(first).not.toContain("secret-a");
    expect(modelCatalogCacheKey({ mode: "local" })).toBe("local#anonymous");
  });
});

describe("backfillRetryDelayMs", () => {
  it("uses deterministic capped exponential backoff", () => {
    expect([1, 2, 3, 4, 5, 6].map(backfillRetryDelayMs)).toEqual([
      1_000,
      2_000,
      4_000,
      8_000,
      15_000,
      15_000,
    ]);
  });
});

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

describe("observerSyncDiagnostics", () => {
  it("reports only synchronization metadata for Agents in the authorized roster", () => {
    const previous = useRuntimeStore.getState();
    useRuntimeStore.setState({
      bootstrap: {
        ...previous.bootstrap,
        agents: [agentSummary({ id: "visible-agent", lastBrief: "private brief text" })],
      },
      discovery: {
        mode: "authoritative",
        freshness: "fresh",
        identity: {
          runtimeId: "runtime-1",
          visibilityScopeId: "scope-1",
          eventLogEpoch: "epoch-1",
        },
        retryAttempt: 0,
      },
      sessionsByAgentId: {
        "hidden-agent": sessionState(),
      },
      ledgerUnreadByAgentId: {},
    });

    const diagnostics = observerSyncDiagnostics();
    expect(diagnostics.agents.map((agent) => agent.agentId)).toEqual(["visible-agent"]);
    expect(diagnostics.discovery).toEqual({
      mode: "authoritative",
      freshness: "fresh",
      runtimeId: "runtime-1",
      visibilityScopeId: "scope-1",
      eventLogEpoch: "epoch-1",
    });
    expect(JSON.stringify(diagnostics)).not.toContain("private brief text");
    expect(JSON.stringify(diagnostics)).not.toContain("hidden-agent");

    useRuntimeStore.setState(previous, true);
  });
});

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
        sendingPrompt: true,
        liveStatus: "recovering",
        reconnectAttempt: 4,
        briefHydrationById: {
          loading: { briefId: "loading", status: "loading", attempt: 2 },
          failed: { briefId: "failed", status: "failed", attempt: 5, errorKind: "timeout" },
        },
        workItemDetailsById: { "work-1": { loading: true } },
        taskDetailsById: { "task-1": { loading: true } },
        toolExecutionDetailsById: { "tool-1": { loading: true } },
      }),
    });

    expect(reset["agent-a"]).toMatchObject({
      loading: false,
      sendingPrompt: false,
      liveStatus: "stale",
      reconnectAttempt: 0,
      briefHydrationById: {
        loading: { briefId: "loading", status: "pending", attempt: 2 },
        failed: { briefId: "failed", status: "failed", attempt: 5, errorKind: "timeout" },
      },
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

describe("timeline events state", () => {
  afterEach(() => {
    useRuntimeStore.setState({
      rightPanelOpen: true,
      rightPanelView: undefined,
      rightPanelViewStack: [],
      timelineEventsByAgentId: {},
    });
  });

  it("opens as a first-class right panel view and preserves back navigation", () => {
    useRuntimeStore.setState({
      selectedAgentId: "agent-a",
      rightPanelView: { kind: "agent_overview", agentId: "agent-a" },
      rightPanelViewStack: [],
      timelineEventsByAgentId: {
        "agent-a": {
          eventsBySeq: { 1: { id: "event-1", event_seq: 1, type: "message_enqueued" } },
          eventSeqs: [1],
          oldestSeq: 1,
          newestSeq: 1,
          hasOlder: false,
          loading: false,
          loadingOlder: false,
        },
      },
    });

    useRuntimeStore.getState().showTimelineEvents("agent-a");
    expect(useRuntimeStore.getState()).toMatchObject({
      rightPanelOpen: true,
      rightPanelView: { kind: "timeline_events", agentId: "agent-a" },
      rightPanelViewStack: [{ kind: "agent_overview", agentId: "agent-a" }],
    });

    useRuntimeStore.getState().navigateBack();
    expect(useRuntimeStore.getState()).toMatchObject({
      rightPanelView: { kind: "agent_overview", agentId: "agent-a" },
      rightPanelViewStack: [],
    });
  });

  it("returns developer-only UI to a non-debug closed state when diagnostics are disabled", () => {
    useRuntimeStore.setState({
      selectedAgentId: "agent-a",
      displayLevel: "debug",
      displayLevelsByAgentId: {
        "agent-a": "debug",
        "agent-b": "verbose",
      },
      rightPanelOpen: true,
      rightPanelView: { kind: "timeline_events", agentId: "agent-a" },
      rightPanelViewStack: [
        { kind: "agent_overview", agentId: "agent-a" },
        { kind: "timeline_events", agentId: "agent-b" },
      ],
    });

    useRuntimeStore.getState().disableDeveloperDiagnosticsUi("agent-a");

    expect(useRuntimeStore.getState()).toMatchObject({
      displayLevel: "info",
      displayLevelsByAgentId: {
        "agent-a": "info",
        "agent-b": "verbose",
      },
      rightPanelOpen: false,
      rightPanelView: { kind: "agent_overview", agentId: "agent-a" },
      rightPanelViewStack: [{ kind: "agent_overview", agentId: "agent-a" }],
    });
  });

  it("appends older pages in sequence order and resets on epoch changes", () => {
    const initial = mergeTimelineEventPage(
      {
        eventsBySeq: {},
        eventSeqs: [],
        hasOlder: false,
        loading: false,
        loadingOlder: false,
      },
      [
        { id: "event-3", event_seq: 3, event_log_epoch: "epoch-a", type: "task_created" },
        { id: "event-2", event_seq: 2, event_log_epoch: "epoch-a", type: "message_enqueued" },
      ],
      "epoch-a",
      true,
      false,
    );
    const appended = mergeTimelineEventPage(
      initial,
      [{ id: "event-1", event_seq: 1, event_log_epoch: "epoch-a", type: "agent_state_changed" }],
      "epoch-a",
      false,
      true,
    );
    expect(appended).toMatchObject({
      eventLogEpoch: "epoch-a",
      eventSeqs: [1, 2, 3],
      oldestSeq: 1,
      newestSeq: 3,
      hasOlder: false,
    });

    expect(mergeTimelineEventPage(
      appended,
      [{ id: "event-1-new", event_seq: 1, event_log_epoch: "epoch-b", type: "message_enqueued" }],
      "epoch-b",
      false,
      true,
    )).toMatchObject({
      eventLogEpoch: "epoch-b",
      eventSeqs: [1],
      eventsBySeq: { 1: { id: "event-1-new" } },
    });
  });
});

describe("runtime event epoch", () => {
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

  it("clears stale stored tokens once an oidc runtime is confirmed", () => {
    const sharedLocalStorage = new MemoryStorage();
    const windowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, windowSession);
    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "https://holon.example",
      token: "stale-static-token",
    });
    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "remote",
      baseUrl: "https://holon.example",
      token: "stale-static-token",
    });

    clearStoredRuntimeConnectionToken();

    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "remote",
      baseUrl: "https://holon.example",
    });

    // A fresh window session must not rehydrate the stale profile token.
    installWindow(sharedLocalStorage, new MemoryStorage());
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    expect(readStoredRemoteConnectionProfiles()).toEqual([
      { baseUrl: "https://holon.example", hasToken: false },
    ]);
  });
});

describe("agent deletion cache cleanup", () => {
  afterEach(() => {
    useRuntimeStore.setState({
      selectedAgentId: "",
      route: "dashboard",
      sessionsByAgentId: {},
      rosterActivityByAgentId: {},
    });
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("removes persisted session and read state after server deletion succeeds", async () => {
    const localStorage = new MemoryStorage();
    const sessionStorage = new MemoryStorage();
    vi.stubGlobal("window", {
      localStorage,
      sessionStorage,
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith("/handshake")) return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      if (url.endsWith("/agents/list")) return Promise.resolve(jsonResponse([]));
      if (
        url.endsWith("/control/agents/agent-a") &&
        init?.method === "DELETE"
      ) {
        return Promise.resolve(jsonResponse({
          created: true,
          ok: true,
          identity: { agent_id: "agent-a", status: "deleting" },
          job: {
            deletion_id: "delete-1",
            status: "completed",
            phase: "completed",
            attempts: 1,
            created_at: "2026-08-10T00:00:00Z",
            updated_at: "2026-08-10T00:00:01Z",
            completed_at: "2026-08-10T00:00:01Z",
          },
        }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const idbModule = await import("./idb-cache");
    const deleteSpy = vi.spyOn(idbModule, "cacheDeleteSession").mockResolvedValue(undefined);

    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    useRuntimeStore.setState({
      selectedAgentId: "agent-a",
      route: "agent",
      sessionsByAgentId: {
        "agent-a": sessionState({
          eventSeqs: [1],
          eventsBySeq: { 1: { id: "event-1", event_seq: 1 } },
        }),
      },
      rosterActivityByAgentId: {
        "agent-a": {
          unreadCount: 2,
          lastUnreadDeliverySeq: 3,
          lastReadDeliverySeq: 1,
        },
      },
    });

    await useRuntimeStore.getState().deleteAgent("agent-a");

    expect(deleteSpy).toHaveBeenCalledWith("local", "agent-a");
    expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toBeUndefined();
    expect(useRuntimeStore.getState().rosterActivityByAgentId["agent-a"]).toBeUndefined();
    expect(useRuntimeStore.getState()).toMatchObject({
      selectedAgentId: "",
      route: "dashboard",
    });
  });
});

describe("roster activity unread state", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    setRuntimeTraceEnabled(false, { clear: true });
    useRuntimeStore.setState({
      route: "dashboard",
      selectedAgentId: "",
      sessionsByAgentId: {},
      ledgerUnreadByAgentId: {},
    });
  });

  it("retries a pending read marker after ledger readiness becomes available", async () => {
    vi.stubGlobal("document", { visibilityState: "visible" });
    let ready = false;
    vi.spyOn(AgentSessionRepository.prototype, "sessionLedgerReadiness")
      .mockImplementation(() => ready
        ? { readyThroughSeq: 12, ingestedThroughSeq: 12, observedHeadSeq: 12 }
        : null);
    const advanceReadMarker = vi
      .spyOn(AgentSessionRepository.prototype, "advanceReadMarker")
      .mockResolvedValue({
        advanced: true,
        record: {
          remoteKey: "local",
          runtimeId: "runtime-1",
          visibilityScopeId: "scope-1",
          eventLogEpoch: "epoch-1",
          agentId: "agent-retry",
          readThroughEventSeq: 12,
          updatedAt: 1,
        },
      });
    const acknowledgeReadTruncation = vi
      .spyOn(AgentSessionRepository.prototype, "acknowledgeReadTruncation")
      .mockResolvedValue(null);
    vi.spyOn(AgentSessionRepository.prototype, "unreadSnapshot").mockResolvedValue({
      scopeAgentId: "agent-retry",
      boundarySeq: 12,
      countedThroughSeq: 12,
      certainty: "exact",
      count: 0,
      historyTruncatedBeforeSeq: null,
      acknowledgedTruncationBeforeSeq: null,
    });
    useRuntimeStore.setState({
      route: "agent",
      selectedAgentId: "agent-retry",
      discovery: {
        mode: "authoritative",
        freshness: "fresh",
        retryAttempt: 0,
      },
      sessionsByAgentId: {
        "agent-retry": sessionState(),
      },
      ledgerUnreadByAgentId: {
        "agent-retry": { mode: "exact", count: 2 },
      },
    });

    useRuntimeStore.getState().markAgentConversationRead("agent-retry");
    await Promise.resolve();
    expect(advanceReadMarker).not.toHaveBeenCalled();

    ready = true;
    await retryPendingReadMarker("agent-retry");

    expect(advanceReadMarker).toHaveBeenCalledWith("agent-retry", 12);
    // An exact record never triggers the auto-restore acknowledgement.
    expect(acknowledgeReadTruncation).not.toHaveBeenCalled();
    expect(useRuntimeStore.getState().ledgerUnreadByAgentId["agent-retry"]).toEqual({
      mode: "exact",
      count: 0,
    });
  });

  it("auto-restores exact certainty when a sticky truncated marker already covers the head", async () => {
    vi.stubGlobal("document", { visibilityState: "visible" });
    let ready = false;
    vi.spyOn(AgentSessionRepository.prototype, "sessionLedgerReadiness")
      .mockImplementation(() => ready
        ? { readyThroughSeq: 12, ingestedThroughSeq: 12, observedHeadSeq: 12 }
        : null);
    // Pre-fix durable state: the marker already reached the head, so the
    // monotonic advance is a no-op while certainty stays truncated.
    const advanceReadMarker = vi
      .spyOn(AgentSessionRepository.prototype, "advanceReadMarker")
      .mockResolvedValue({
      advanced: false,
      record: {
        remoteKey: "local",
        runtimeId: "runtime-1",
        visibilityScopeId: "scope-1",
        eventLogEpoch: "epoch-1",
        agentId: "agent-auto",
        readThroughEventSeq: 12,
        certainty: "truncated",
        updatedAt: 1,
      },
    });
    const acknowledgeReadTruncation = vi
      .spyOn(AgentSessionRepository.prototype, "acknowledgeReadTruncation")
      .mockResolvedValue({
        remoteKey: "local",
        runtimeId: "runtime-1",
        visibilityScopeId: "scope-1",
        eventLogEpoch: "epoch-1",
        agentId: "agent-auto",
        readThroughEventSeq: 12,
        unreadBaselineSeq: 12,
        acknowledgedTruncationBeforeSeq: 12,
        certainty: "exact",
        updatedAt: 2,
      });
    vi.spyOn(AgentSessionRepository.prototype, "unreadSnapshot").mockResolvedValue({
      scopeAgentId: "agent-auto",
      boundarySeq: 12,
      countedThroughSeq: 12,
      certainty: "exact",
      count: 0,
      historyTruncatedBeforeSeq: 5,
      acknowledgedTruncationBeforeSeq: 12,
    });
    useRuntimeStore.setState({
      route: "agent",
      selectedAgentId: "agent-auto",
      discovery: {
        mode: "authoritative",
        freshness: "fresh",
        retryAttempt: 0,
      },
      sessionsByAgentId: {
        "agent-auto": sessionState(),
      },
      ledgerUnreadByAgentId: {
        "agent-auto": { mode: "truncated", count: 2 },
      },
    });

    useRuntimeStore.getState().markAgentConversationRead("agent-auto");
    await Promise.resolve();
    expect(advanceReadMarker).not.toHaveBeenCalled();

    ready = true;
    await retryPendingReadMarker("agent-auto");

    // Auto-restore retires the truncated generation at the gated head the
    // marker already covers, flipping the badge back to exact.
    expect(acknowledgeReadTruncation).toHaveBeenCalledWith("agent-auto", 12);
    expect(useRuntimeStore.getState().ledgerUnreadByAgentId["agent-auto"]).toEqual({
      mode: "exact",
      count: 0,
    });
  });

  it("refreshes a stale unread view after a monotonic read-marker no-op", async () => {
    vi.stubGlobal("document", { visibilityState: "visible" });
    setRuntimeTraceEnabled(true, { clear: true });
    vi.spyOn(AgentSessionRepository.prototype, "sessionLedgerReadiness").mockReturnValue({
      readyThroughSeq: 12,
      ingestedThroughSeq: 12,
      observedHeadSeq: 12,
    });
    vi.spyOn(AgentSessionRepository.prototype, "advanceReadMarker").mockResolvedValue({
      advanced: false,
      record: {
        remoteKey: "local",
        runtimeId: "runtime-1",
        visibilityScopeId: "scope-1",
        eventLogEpoch: "epoch-1",
        agentId: "agent-noop",
        readThroughEventSeq: 12,
        updatedAt: 1,
      },
    });
    vi.spyOn(AgentSessionRepository.prototype, "unreadSnapshot").mockResolvedValue({
      scopeAgentId: "agent-noop",
      boundarySeq: 12,
      countedThroughSeq: 12,
      certainty: "exact",
      count: 0,
      historyTruncatedBeforeSeq: null,
      acknowledgedTruncationBeforeSeq: null,
    });
    useRuntimeStore.setState({
      route: "agent",
      selectedAgentId: "agent-noop",
      discovery: {
        mode: "authoritative",
        freshness: "fresh",
        retryAttempt: 0,
      },
      sessionsByAgentId: {
        "agent-noop": sessionState(),
      },
      ledgerUnreadByAgentId: {
        "agent-noop": { mode: "exact", count: 2 },
      },
    });

    useRuntimeStore.getState().markAgentConversationRead("agent-noop");
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().ledgerUnreadByAgentId["agent-noop"]?.count).toBe(0);
    });
    expect(getRuntimeTraceRecords({ agentId: "agent-noop" }).at(-1)).toMatchObject({
      name: "read_marker.advance",
      outcome: "ok",
      attributes: { advanced: false, candidateSeq: 12 },
    });
  });

  it("does not mutate legacy roster activity when marking a conversation read", async () => {
    const { touchRosterActivityFromEvent } = await import("./runtime-store");
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let activity: Record<string, any> = {
      "agent-a": { unreadCount: 0, lastUnreadDeliverySeq: 7, lastReadDeliverySeq: 7 },
    };
    for (const seq of [8, 9, 10]) {
      activity = touchRosterActivityFromEvent(
        activity,
        "agent-a",
        { agent_id: "agent-a", event_seq: seq, ts: `2026-01-01T00:00:0${seq}.000Z`, type: "brief_created", payload: {} },
        "agent-a",
      );
    }
    expect(activity["agent-a"]?.unreadCount).toBe(3);

    vi.stubGlobal("document", { visibilityState: "visible" });
    useRuntimeStore.setState({
      route: "agent",
      selectedAgentId: "agent-a",
      rosterActivityByAgentId: activity,
      sessionsByAgentId: {
        "agent-a": sessionState({
          contentStatus: "available",
          syncStatus: "streaming",
          liveStatus: "recovering",
          eventsBySeq: {
            8: { agent_id: "agent-a", event_seq: 8, type: "brief_created", payload: {} },
            9: { agent_id: "agent-a", event_seq: 9, type: "brief_created", payload: {} },
            10: { agent_id: "agent-a", event_seq: 10, type: "brief_created", payload: {} },
          },
          eventSeqs: [8, 9, 10],
        }),
      },
    });
    useRuntimeStore.getState().markAgentConversationRead("agent-a");
    expect(useRuntimeStore.getState().rosterActivityByAgentId["agent-a"]?.unreadCount).toBe(3);
    useRuntimeStore.setState((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        "agent-a": {
          ...state.sessionsByAgentId["agent-a"],
          liveStatus: "streaming",
        },
      },
    }));
    useRuntimeStore.getState().markAgentConversationRead("agent-a");
    activity = useRuntimeStore.getState().rosterActivityByAgentId;
    expect(activity["agent-a"]?.unreadCount).toBe(3);
    expect(activity["agent-a"]?.lastReadDeliverySeq).toBe(7);

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let replayed: Record<string, any> = activity;
    for (const seq of [8, 9, 10]) {
      replayed = touchRosterActivityFromEvent(
        replayed,
        "agent-a",
        { agent_id: "agent-a", event_seq: seq, ts: `2026-01-01T00:00:0${seq}.000Z`, type: "brief_created", payload: {} },
        "agent-b",
      );
    }
    expect(replayed["agent-a"]?.unreadCount).toBe(3);

    // A genuinely new event (seq 11) should still be counted.
    replayed = touchRosterActivityFromEvent(
      replayed,
      "agent-a",
      { agent_id: "agent-a", event_seq: 11, ts: "2026-01-01T00:00:11.000Z", type: "brief_created", payload: {} },
      "agent-b",
    );
    expect(replayed["agent-a"]?.unreadCount).toBe(4);
    expect(replayed["agent-a"]?.lastUnreadDeliverySeq).toBe(11);
  });
});

describe("brief projection and hydration", () => {
  afterEach(() => {
    useRuntimeStore.setState({
      sessionsByAgentId: {},
      globalStreamStatus: "idle",
      selectedAgentId: "",
    });
    vi.unstubAllGlobals();
  });


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


  it("tracks loading, transient failure, manual retry, and not found brief states", () => {
    let projection = reduceSessionProjection(createSessionProjectionState(), {
      type: "briefs_hydration_started",
      briefIds: ["brief-123"],
    });
    expect(projection.briefHydrationById["brief-123"]).toEqual({
      briefId: "brief-123",
      status: "loading",
      attempt: 1,
    });

    projection = reduceSessionProjection(projection, {
      type: "briefs_hydration_failed",
      briefIds: ["brief-123"],
      errorKind: "request_failed",
    });
    expect(projection.briefHydrationById["brief-123"]).toEqual({
      briefId: "brief-123",
      status: "failed",
      attempt: 1,
      errorKind: "request_failed",
    });

    projection = reduceSessionProjection(projection, {
      type: "briefs_hydration_started",
      briefIds: ["brief-123"],
    });
    expect(projection.briefHydrationById["brief-123"]?.attempt).toBe(2);

    projection = reduceSessionProjection(projection, {
      type: "briefs_hydrated",
      recordsById: {},
      missingIds: ["brief-123"],
    });
    expect(projection.briefHydrationById["brief-123"]).toEqual({
      briefId: "brief-123",
      status: "not_found",
      attempt: 2,
    });
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
      if (url.endsWith("/handshake")) return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
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

describe("projection saturation refresh handling", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("keeps the current roster when a best-effort bootstrap refresh is rejected", async () => {
    const localStorage = new MemoryStorage();
    const sessionStorage = new MemoryStorage();
    vi.stubGlobal("window", {
      localStorage,
      sessionStorage,
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });

    let saturated = false;
    vi.stubGlobal("fetch", vi.fn((input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith("/handshake")) return Promise.resolve(jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES }));
      if (url.endsWith("/agents/list")) {
        return Promise.resolve(
          saturated
            ? new Response(
                JSON.stringify({
                  ok: false,
                  error: "projection capacity is busy; retry later",
                  code: "projection_busy",
                  retryable: true,
                }),
                {
                  status: 429,
                  headers: {
                    "content-type": "application/json",
                    "retry-after": "1",
                  },
                },
              )
            : jsonResponse([{ id: "agent-a", lifecycle: "asleep" }]),
        );
      }
      throw new Error(`Unexpected request: ${url}`);
    }));

    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    await Promise.resolve();
    const currentRoster = useRuntimeStore.getState().bootstrap.agents;

    saturated = true;
    await useRuntimeStore.getState().refreshBootstrap({ syncEvents: false });

    expect(useRuntimeStore.getState()).toMatchObject({
      bootstrap: { agents: currentRoster },
      bootstrapLoading: false,
      bootstrapError: undefined,
    });
  });
});

describe("bounded resume refresh scheduling", () => {
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

describe("agentDetailErrorKind", () => {
  it("classifies AbortError as timeout", () => {
    expect(agentDetailErrorKind(new DOMException("aborted", "AbortError"))).toBe("timeout");
  });

  it("classifies error with timeout message as timeout", () => {
    const err = new Error("Request timed out after 8000ms");
    expect(agentDetailErrorKind(err)).toBe("timeout");
  });

  it("classifies RuntimeHttpError name as http_error", () => {
    const err = new Error("GET /agents/x failed with 500");
    err.name = "RuntimeHttpError";
    expect(agentDetailErrorKind(err)).toBe("http_error");
  });

  it("classifies TypeError as network_error", () => {
    expect(agentDetailErrorKind(new TypeError("fetch failed"))).toBe("network_error");
  });

  it("classifies SyntaxError as parse_error", () => {
    expect(agentDetailErrorKind(new SyntaxError("Unexpected token in JSON"))).toBe("parse_error");
  });

  it("returns unknown for unclassified errors", () => {
    expect(agentDetailErrorKind(new Error("something broke"))).toBe("unknown");
    expect(agentDetailErrorKind("string error")).toBe("unknown");
  });
});

describe("right panel expanded mode", () => {
  afterEach(() => {
    useRuntimeStore.setState({
      rightPanelOpen: true,
      rightPanelMode: "normal",
      rightPanelExpandedNavWasCollapsed: undefined,
      navCollapsed: false,
    });
  });

  it("collapses the nav rail while expanded and restores the pre-expansion nav state", () => {
    useRuntimeStore.setState({ navCollapsed: true, rightPanelMode: "normal" });

    useRuntimeStore.getState().toggleRightPanelExpanded();
    expect(useRuntimeStore.getState()).toMatchObject({
      rightPanelMode: "expanded",
      rightPanelOpen: true,
      navCollapsed: true,
      rightPanelExpandedNavWasCollapsed: true,
    });

    // Nav toggles issued while expanded must not leak into the restore value.
    useRuntimeStore.getState().toggleNavCollapsed();
    expect(useRuntimeStore.getState().navCollapsed).toBe(false);

    useRuntimeStore.getState().toggleRightPanelExpanded();
    expect(useRuntimeStore.getState()).toMatchObject({
      rightPanelMode: "normal",
      navCollapsed: true,
      rightPanelExpandedNavWasCollapsed: undefined,
    });
  });

  it("resets expansion when the panel is closed", () => {
    useRuntimeStore.setState({ navCollapsed: false, rightPanelMode: "normal" });

    useRuntimeStore.getState().toggleRightPanelExpanded();
    useRuntimeStore.getState().setRightPanelOpen(false);
    expect(useRuntimeStore.getState()).toMatchObject({
      rightPanelOpen: false,
      rightPanelMode: "normal",
      navCollapsed: false,
      rightPanelExpandedNavWasCollapsed: undefined,
    });
  });
});

describe("refreshAgentSkillCatalog bounded retry", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  function stubSkillWindow() {
    vi.stubGlobal("window", {
      localStorage: new MemoryStorage(),
      sessionStorage: new MemoryStorage(),
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
  }

  function skillsFetchMock(skills: () => Response) {
    let calls = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/agents/agent-a/skills")) {
        calls += 1;
        return skills();
      }
      if (url.endsWith("/handshake")) {
        return jsonResponse({ capabilities: OBSERVER_SYNC_CAPABILITIES });
      }
      if (url.endsWith("/agents/list")) return jsonResponse([]);
      if (url.endsWith("/agents/snapshot")) {
        return jsonResponse({
          contract_version: 1,
          runtime_id: "runtime-1",
          event_log_epoch: "epoch-1",
          visibility_scope_id: "scope-1",
          agents: [],
        });
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    return { fetchMock, calls: () => calls };
  }

  async function connectStreaming() {
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    useRuntimeStore.setState({ globalStreamStatus: "streaming" });
  }

  it("stores an error placeholder so the fetch effect terminates, then retries with bounded backoff", async () => {
    vi.useFakeTimers();
    stubSkillWindow();
    const { fetchMock, calls } = skillsFetchMock(() => new Response("server error", { status: 500 }));
    vi.stubGlobal("fetch", fetchMock);
    await connectStreaming();

    await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");

    // The catalog slot must be filled with an error-carrying entry so the
    // App-level fetch effect does not immediately re-fire (the ~209 rps storm).
    expect(useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-a"]).toMatchObject({
      source: "http",
      error: "GET /agents/agent-a/skills failed with 500",
    });
    expect(useRuntimeStore.getState().agentSkillCatalogErrorByAgentId["agent-a"]).toBe(
      "GET /agents/agent-a/skills failed with 500",
    );
    expect(useRuntimeStore.getState().agentSkillCatalogLoadingByAgentId["agent-a"]).toBe(false);

    await vi.advanceTimersByTimeAsync(90_000);
    // Initial call plus exactly three bounded retries.
    expect(calls()).toBe(4);
    await vi.advanceTimersByTimeAsync(90_000);
    expect(calls()).toBe(4);
  });

  it("pauses retries while the runtime connection is down and resumes after reconnect", async () => {
    vi.useFakeTimers();
    stubSkillWindow();
    const { fetchMock, calls } = skillsFetchMock(() => new Response("server error", { status: 500 }));
    vi.stubGlobal("fetch", fetchMock);
    await connectStreaming();
    useRuntimeStore.setState({ globalStreamStatus: "reconnecting" });

    await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");
    expect(calls()).toBe(1);

    await vi.advanceTimersByTimeAsync(120_000);
    // Paused while disconnected: no additional fetches at all.
    expect(calls()).toBe(1);

    useRuntimeStore.setState({ globalStreamStatus: "streaming" });
    await vi.advanceTimersByTimeAsync(120_000);
    // Resumed, and still bounded to three retries.
    expect(calls()).toBe(4);
  });

  it("clears the retry schedule once a retry succeeds", async () => {
    vi.useFakeTimers();
    stubSkillWindow();
    let failing = true;
    const { fetchMock, calls } = skillsFetchMock(() =>
      failing ? new Response("server error", { status: 500 }) : jsonResponse({ skills: [] }),
    );
    vi.stubGlobal("fetch", fetchMock);
    await connectStreaming();

    await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");
    failing = false;
    await vi.advanceTimersByTimeAsync(10_000);

    const state = useRuntimeStore.getState();
    expect(calls()).toBe(2);
    expect(state.agentSkillCatalogByAgentId["agent-a"]).toMatchObject({
      source: "http",
      agentId: "agent-a",
      catalog: [],
    });
    expect(state.agentSkillCatalogByAgentId["agent-a"]?.error).toBeUndefined();
    expect(state.agentSkillCatalogErrorByAgentId["agent-a"]).toBeUndefined();

    await vi.advanceTimersByTimeAsync(120_000);
    expect(calls()).toBe(2);
  });

  it("drops error placeholders on resume reset while keeping clean catalogs", () => {
    const previous = useRuntimeStore.getState();
    try {
      useRuntimeStore.setState({
        agentSkillCatalogByAgentId: {
          "agent-clean": { source: "http", catalog: [] },
          "agent-failed": { source: "http", catalog: [], error: "GET failed with 500" },
        },
      });
      const patch = resetTransientRuntimeStateForResume(useRuntimeStore.getState());
      expect(patch.agentSkillCatalogByAgentId).toEqual({
        "agent-clean": { source: "http", catalog: [] },
      });
    } finally {
      useRuntimeStore.setState(previous, true);
    }
  });
});


function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}
