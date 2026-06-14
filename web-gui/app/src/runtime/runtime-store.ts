import { create } from "zustand";

import { createRuntimeClient, type AgentEventStreamSubscription, type StreamEventEnvelopeDto } from "./client";
import { compactAgentTimelineItems, mergeAgentTimelineItems, reduceAgentSessionTimeline } from "./session-reducer";
import type { AgentDetail, AgentSummary, AgentTimelineItem, DisplayLevel, RouteKey, RuntimeBootstrap, RuntimeModelCatalog } from "./types";

export type AgentLiveStatus = "idle" | "connecting" | "streaming" | "reconnecting" | "recovering" | "stale" | "error";

export interface BootstrapRefreshOptions {
  background?: boolean;
}

function createLiveAgentDetail(agent: AgentSummary | undefined): AgentDetail | null {
  if (!agent) return null;
  return {
    agent,
    timeline: [],
    source: "http",
    events: [],
  };
}

function preserveLiveAgentRunState(httpAgent: AgentSummary, liveAgent: AgentSummary): AgentSummary {
  if (!isLiveRunningAgent(liveAgent)) return httpAgent;
  return {
    ...httpAgent,
    currentRunId: liveAgent.currentRunId,
    lifecycle: liveAgent.lifecycle,
  };
}

function isLiveRunningAgent(agent: AgentSummary): boolean {
  return Boolean(agent.currentRunId) || agent.lifecycle.toLowerCase() === "awake-running";
}

function isAgentEventStreamActive(agentId: string, liveStatus: AgentLiveStatus | undefined): boolean {
  return activeEventStreams.has(agentId) && (liveStatus === "streaming" || liveStatus === "recovering");
}

export interface AgentSessionState {
  loading: boolean;
  loadingOlder: boolean;
  liveStatus: AgentLiveStatus;
  sendingPrompt: boolean;
  detail: AgentDetail | null;
  eventsBySeq: Record<number, unknown>;
  eventSeqs: number[];
  newestSeq?: number;
  oldestSeq?: number;
  hasOlder?: boolean;
  lastStreamActivityAt?: string;
  reconnectAttempt?: number;
  error?: string;
  historyError?: string;
  promptError?: string;
  modelError?: string;
}

interface AgentRosterActivity {
  operatorAt?: string;
  briefAt?: string;
}

function appendOptimisticOperatorPrompt(detail: AgentDetail | null, agent: AgentSummary | undefined, prompt: string): AgentDetail | null {
  const baseDetail = detail ?? createLiveAgentDetail(agent);
  if (!baseDetail) return null;
  const timestamp = new Date().toISOString();
  return {
    ...baseDetail,
    timeline: [
      ...baseDetail.timeline,
      {
        id: `operator-prompt:pending:${timestamp}`,
        kind: "operator",
        label: "Operator input",
        body: prompt,
        timestamp,
        meta: "sending",
        minDisplayLevel: "info",
        sourceIds: ["pending-operator-prompt"],
      },
    ],
  };
}

export interface RuntimeStoreState {
  route: RouteKey;
  selectedAgentId: string;
  displayLevel: DisplayLevel;
  displayLevelsByAgentId: Record<string, DisplayLevel>;
  inspectorOpen: boolean;
  navCollapsed: boolean;

  bootstrap: RuntimeBootstrap;
  bootstrapLoading: boolean;
  bootstrapError?: string;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  modelCatalogError?: string;
  rosterActivityByAgentId: Record<string, AgentRosterActivity>;
  sessionsByAgentId: Record<string, AgentSessionState>;

  setRoute: (route: RouteKey) => void;
  openAgent: (agentId: string) => void;
  setDisplayLevel: (displayLevel: DisplayLevel, agentId?: string) => void;
  setInspectorOpen: (open: boolean) => void;
  toggleInspector: () => void;
  toggleNavCollapsed: () => void;
  refreshBootstrap: (options?: BootstrapRefreshOptions) => Promise<void>;
  refreshModelCatalog: () => Promise<void>;
  refreshAgentDetail: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  loadOlderAgentEvents: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  sendOperatorPrompt: (agentId: string | undefined, text: string, displayLevel: DisplayLevel) => Promise<void>;
  setAgentModel: (agentId: string | undefined, model: string, displayLevel: DisplayLevel, reasoningEffort?: string) => Promise<void>;
  clearAgentModel: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  startAgentEventStream: (agentId: string | undefined, displayLevel: DisplayLevel) => void;
  stopAgentEventStream: (agentId: string | undefined) => void;
}

const runtimeClient = createRuntimeClient();
const activeEventStreams = new Map<string, AgentEventStreamSubscription>();
const pendingStreamEvents = new Map<string, StreamEventEnvelopeDto[]>();
const streamFlushTimers = new Map<string, number>();
const reconnectTimers = new Map<string, number>();
const staleTimers = new Map<string, number>();
let bootstrapRefreshInFlight: Promise<void> | undefined;
let bootstrapRefreshTimer: number | undefined;
const STREAM_FLUSH_INTERVAL_MS = 100;
const STREAM_STALE_TIMEOUT_MS = 45_000;
const STREAM_RECONNECT_BASE_MS = 1_000;
const STREAM_RECONNECT_MAX_MS = 15_000;
const DISPLAY_LEVEL_STORAGE_KEY = "holon.webGui.displayLevelsByAgentId.v1";

function readStoredDisplayLevels(): Record<string, DisplayLevel> {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(DISPLAY_LEVEL_STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter((entry): entry is [string, DisplayLevel] => {
        const [agentId, level] = entry;
        return typeof agentId === "string" && isDisplayLevel(level);
      }),
    );
  } catch {
    return {};
  }
}

function writeStoredDisplayLevels(displayLevelsByAgentId: Record<string, DisplayLevel>): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(DISPLAY_LEVEL_STORAGE_KEY, JSON.stringify(displayLevelsByAgentId));
  } catch {
    // Ignore storage failures; the in-memory selection still applies.
  }
}

function isDisplayLevel(value: unknown): value is DisplayLevel {
  return value === "info" || value === "verbose" || value === "debug";
}

const emptyBootstrap: RuntimeBootstrap = {
  attentionCount: 0,
  connection: {
    mode: "local",
    source: "fixture",
    summary: "Connecting to local runtime…",
  },
  metrics: [],
  agents: [],
};

const emptyModelCatalog: RuntimeModelCatalog = {
  source: "fixture",
  options: [],
};

export const useRuntimeStore = create<RuntimeStoreState>((set, get) => ({
  route: "dashboard",
  selectedAgentId: "",
  displayLevel: "info",
  displayLevelsByAgentId: readStoredDisplayLevels(),
  inspectorOpen: false,
  navCollapsed: false,

  bootstrap: emptyBootstrap,
  bootstrapLoading: true,
  modelCatalog: emptyModelCatalog,
  modelCatalogLoading: false,
  rosterActivityByAgentId: {},
  sessionsByAgentId: {},

  setRoute: (route) => set({ route }),
  openAgent: (agentId) =>
    set((state) => ({
      selectedAgentId: agentId,
      route: "agent",
      displayLevel: state.displayLevelsByAgentId[agentId] ?? "info",
    })),
  setDisplayLevel: (displayLevel, agentId) =>
    set((state) => {
      const targetAgentId = agentId ?? state.selectedAgentId;
      if (!targetAgentId) return { displayLevel };
      const displayLevelsByAgentId = {
        ...state.displayLevelsByAgentId,
        [targetAgentId]: displayLevel,
      };
      writeStoredDisplayLevels(displayLevelsByAgentId);
      return { displayLevel, displayLevelsByAgentId };
    }),
  setInspectorOpen: (open) => set({ inspectorOpen: open }),
  toggleInspector: () => set((state) => ({ inspectorOpen: !state.inspectorOpen })),
  toggleNavCollapsed: () => set((state) => ({ navCollapsed: !state.navCollapsed })),

  refreshBootstrap: async (options = {}) => {
    if (bootstrapRefreshInFlight) return bootstrapRefreshInFlight;
    if (options.background) {
      set({ bootstrapError: undefined });
    } else {
      set({ bootstrapLoading: true, bootstrapError: undefined });
    }

    bootstrapRefreshInFlight = (async () => {
      try {
        const bootstrap = await runtimeClient.getBootstrap();
        set((state) => {
          if (bootstrap.connection.source === "fixture" && state.bootstrap.connection.source === "http") {
            return {
              bootstrap: state.bootstrap,
              bootstrapLoading: false,
              bootstrapError: bootstrap.connection.error,
            };
          }
          return {
            bootstrap: sortBootstrapAgents(bootstrap, state.rosterActivityByAgentId),
            bootstrapLoading: false,
            bootstrapError: bootstrap.connection.error,
          };
        });
      } catch (error) {
        set({
          bootstrapLoading: false,
          bootstrapError: error instanceof Error ? error.message : String(error),
        });
      } finally {
        bootstrapRefreshInFlight = undefined;
      }
    })();

    return bootstrapRefreshInFlight;
  },

  refreshModelCatalog: async () => {
    set({ modelCatalogLoading: true, modelCatalogError: undefined });
    try {
      const modelCatalog = await runtimeClient.getModels();
      set({ modelCatalog, modelCatalogLoading: false, modelCatalogError: modelCatalog.error });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        modelCatalog: { ...state.modelCatalog, error: message },
        modelCatalogLoading: false,
        modelCatalogError: message,
      }));
    }
  },

  refreshAgentDetail: async (agentId, displayLevel) => {
    if (!agentId) {
      return;
    }

    set((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...emptyAgentSession(),
          ...state.sessionsByAgentId[agentId],
          loading: true,
          error: undefined,
        },
      },
    }));

    try {
      const detail = await runtimeClient.getAgentDetail(agentId, displayLevel);
      set((state) => mergeAgentDetailIntoSession(state, agentId, detail));
    } catch (error) {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            loading: false,
            liveStatus: "error",
            error: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    }
  },

  loadOlderAgentEvents: async (agentId, displayLevel) => {
    if (!agentId) return;
    const session = get().sessionsByAgentId[agentId] ?? emptyAgentSession();
    if (session.loadingOlder || !session.hasOlder || session.oldestSeq == null) return;

    set((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...emptyAgentSession(),
          ...state.sessionsByAgentId[agentId],
          loadingOlder: true,
          historyError: undefined,
        },
      },
    }));

    try {
      const page = await runtimeClient.getAgentEvents(agentId, {
        beforeSeq: session.oldestSeq,
        limit: 80,
        order: "desc",
        displayLevel,
      });

      set((state) => mergeEventPageIntoSession(state, agentId, page.events ?? [], page.oldest_seq, page.has_older, displayLevel));
    } catch (error) {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            loadingOlder: false,
            historyError: error instanceof Error ? error.message : String(error),
          },
        },
      }));
      throw error;
    }
  },

  sendOperatorPrompt: async (agentId, text, displayLevel) => {
    const prompt = text.trim();
    if (!agentId || !prompt) {
      return;
    }

    set((state) => {
      const rosterActivityByAgentId = touchRosterActivity(state.rosterActivityByAgentId, agentId, "operator", new Date().toISOString());
      return {
        bootstrap: sortBootstrapAgents(state.bootstrap, rosterActivityByAgentId),
        rosterActivityByAgentId,
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            sendingPrompt: true,
            promptError: undefined,
            detail: appendOptimisticOperatorPrompt(
              state.sessionsByAgentId[agentId]?.detail ?? null,
              state.bootstrap.agents.find((agent) => agent.id === agentId),
              prompt,
            ),
          },
        },
      };
    });

    try {
      await runtimeClient.sendOperatorPrompt(agentId, prompt);
      scheduleBootstrapRefresh(get, 250);
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            sendingPrompt: false,
            promptError: undefined,
          },
        },
      }));
      if (!isAgentEventStreamActive(agentId, get().sessionsByAgentId[agentId]?.liveStatus)) {
        void catchUpAgentEvents(get, set, agentId, displayLevel);
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            sendingPrompt: false,
            promptError: message,
          },
        },
      }));
      throw error;
    }
  },

  setAgentModel: async (agentId, model, displayLevel, reasoningEffort) => {
    if (!agentId || !model) return;
    const previousAgent = get().sessionsByAgentId[agentId]?.detail?.agent;
    setSessionModelError(set, agentId, undefined);
    try {
      const modelState = await runtimeClient.setAgentModel(agentId, model, reasoningEffort);
      set((state) =>
        updateAgentModelInState(state, agentId, {
          model: modelState?.active_model ?? modelState?.effective_model ?? model,
          modelSource: modelState?.source ?? "agent_override",
        }),
      );
      await get().refreshAgentDetail(agentId, displayLevel);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setSessionModelError(set, agentId, message);
      if (previousAgent) {
        set((state) => updateAgentModelInState(state, agentId, previousAgent));
      }
      throw error;
    }
  },

  clearAgentModel: async (agentId, displayLevel) => {
    if (!agentId) return;
    const previousAgent = get().sessionsByAgentId[agentId]?.detail?.agent;
    setSessionModelError(set, agentId, undefined);
    try {
      const modelState = await runtimeClient.clearAgentModel(agentId);
      set((state) =>
        updateAgentModelInState(state, agentId, {
          model: modelState?.active_model ?? modelState?.effective_model ?? "runtime default",
          modelSource: modelState?.source ?? "runtime_default",
        }),
      );
      await get().refreshAgentDetail(agentId, displayLevel);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setSessionModelError(set, agentId, message);
      if (previousAgent) {
        set((state) => updateAgentModelInState(state, agentId, previousAgent));
      }
      throw error;
    }
  },

  startAgentEventStream: (agentId, displayLevel) => {
    if (!agentId) return;
    stopAgentEventStream(agentId, set);
    const session = get().sessionsByAgentId[agentId] ?? emptyAgentSession();
    if (session.detail?.error) return;

    const reconnectAttempt = session.reconnectAttempt ?? 0;
    setStreamState(set, agentId, reconnectAttempt > 0 ? "reconnecting" : "connecting", {
      reconnectAttempt,
      error: undefined,
    });
    const subscription = runtimeClient.streamAgentEvents(agentId, {
      afterSeq: highestSeq(session.eventSeqs) ?? session.newestSeq ?? 0,
      limit: 100,
      onOpen: () => {
        markStreamActivity(set, agentId);
        setStreamState(set, agentId, reconnectAttempt > 0 ? "recovering" : "streaming", {
          reconnectAttempt: 0,
          error: undefined,
        });
        scheduleStaleWatchdog(get, set, agentId, displayLevel);
        if (reconnectAttempt > 0) {
          setAgentLiveStatus(set, agentId, "streaming");
        }
      },
      onActivity: () => {
        markStreamActivity(set, agentId);
        scheduleStaleWatchdog(get, set, agentId, displayLevel);
      },
      onEvent: (event) => {
        markStreamActivity(set, agentId);
        enqueueStreamEvent(set, agentId, event);
        scheduleBootstrapRefresh(get);
      },
      onClose: () => scheduleStreamReconnect(get, set, agentId, displayLevel, "event stream closed"),
      onError: (error) => scheduleStreamReconnect(get, set, agentId, displayLevel, error.message),
    });
    if (!subscription) {
      setAgentLiveStatus(set, agentId, "idle");
      return;
    }
    activeEventStreams.set(agentId, subscription);
  },

  stopAgentEventStream: (agentId) => {
    if (!agentId) return;
    stopAgentEventStream(agentId, set);
  },
}));

function emptyAgentSession(): AgentSessionState {
  return {
    loading: false,
    loadingOlder: false,
    liveStatus: "idle",
    sendingPrompt: false,
    detail: null,
    eventsBySeq: {},
    eventSeqs: [],
  };
}

type StoreSet = (
  partial:
    | Partial<RuntimeStoreState>
    | RuntimeStoreState
    | ((state: RuntimeStoreState) => Partial<RuntimeStoreState> | RuntimeStoreState),
  replace?: false,
) => void;

function stopAgentEventStream(agentId: string, set?: StoreSet): void {
  if (set) flushStreamEvents(set, agentId);
  activeEventStreams.get(agentId)?.close();
  activeEventStreams.delete(agentId);
  const flushTimer = streamFlushTimers.get(agentId);
  if (flushTimer != null) {
    window.clearTimeout(flushTimer);
    streamFlushTimers.delete(agentId);
  }
  const timer = reconnectTimers.get(agentId);
  if (timer != null) {
    window.clearTimeout(timer);
    reconnectTimers.delete(agentId);
  }
  const staleTimer = staleTimers.get(agentId);
  if (staleTimer != null) {
    window.clearTimeout(staleTimer);
    staleTimers.delete(agentId);
  }
}

function enqueueStreamEvent(set: StoreSet, agentId: string, event: StreamEventEnvelopeDto): void {
  const pending = pendingStreamEvents.get(agentId);
  if (pending) {
    pending.push(event);
  } else {
    pendingStreamEvents.set(agentId, [event]);
  }

  if (streamFlushTimers.has(agentId)) return;
  const timer = window.setTimeout(() => {
    streamFlushTimers.delete(agentId);
    flushStreamEvents(set, agentId);
  }, STREAM_FLUSH_INTERVAL_MS);
  streamFlushTimers.set(agentId, timer);
}

function flushStreamEvents(set: StoreSet, agentId: string): void {
  const events = pendingStreamEvents.get(agentId);
  if (!events?.length) return;
  pendingStreamEvents.delete(agentId);
  applyStreamEvents(set, agentId, events);
}

function setAgentLiveStatus(set: StoreSet, agentId: string, liveStatus: AgentLiveStatus): void {
  setStreamState(set, agentId, liveStatus);
}

function setStreamState(
  set: StoreSet,
  agentId: string,
  liveStatus: AgentLiveStatus,
  updates: Partial<AgentSessionState> = {},
): void {
  set((state) => ({
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...emptyAgentSession(),
        ...state.sessionsByAgentId[agentId],
        liveStatus,
        ...updates,
      },
    },
  }));
}

function markStreamActivity(set: StoreSet, agentId: string): void {
  set((state) => ({
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...emptyAgentSession(),
        ...state.sessionsByAgentId[agentId],
        lastStreamActivityAt: new Date().toISOString(),
      },
    },
  }));
}

function scheduleStaleWatchdog(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  displayLevel: DisplayLevel,
): void {
  const existing = staleTimers.get(agentId);
  if (existing != null) window.clearTimeout(existing);
  const timer = window.setTimeout(() => {
    if (!activeEventStreams.has(agentId)) return;
    flushStreamEvents(set, agentId);
    setStreamState(set, agentId, "stale", { error: "event stream is stale; reconnecting" });
    activeEventStreams.get(agentId)?.close();
    activeEventStreams.delete(agentId);
    scheduleStreamReconnect(get, set, agentId, displayLevel, "event stream idle timeout");
  }, STREAM_STALE_TIMEOUT_MS);
  staleTimers.set(agentId, timer);
}

function scheduleStreamReconnect(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  displayLevel: DisplayLevel,
  reason: string,
): void {
  flushStreamEvents(set, agentId);
  activeEventStreams.get(agentId)?.close();
  activeEventStreams.delete(agentId);
  const staleTimer = staleTimers.get(agentId);
  if (staleTimer != null) {
    window.clearTimeout(staleTimer);
    staleTimers.delete(agentId);
  }
  if (reconnectTimers.has(agentId)) return;

  const attempt = (get().sessionsByAgentId[agentId]?.reconnectAttempt ?? 0) + 1;
  const delay = reconnectDelayMs(attempt);
  setStreamState(set, agentId, "reconnecting", {
    reconnectAttempt: attempt,
    error: reason,
  });
  const timer = window.setTimeout(() => {
    reconnectTimers.delete(agentId);
    get().startAgentEventStream(agentId, displayLevel);
  }, delay);
  reconnectTimers.set(agentId, timer);
}

function reconnectDelayMs(attempt: number): number {
  const exponential = Math.min(STREAM_RECONNECT_MAX_MS, STREAM_RECONNECT_BASE_MS * 2 ** Math.max(0, attempt - 1));
  const jitter = Math.floor(Math.random() * 500);
  return exponential + jitter;
}

function setSessionModelError(set: StoreSet, agentId: string, error: string | undefined): void {
  set((state) => ({
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...emptyAgentSession(),
        ...state.sessionsByAgentId[agentId],
        modelError: error,
      },
    },
  }));
}

function scheduleBootstrapRefresh(get: () => RuntimeStoreState, delayMs = 1_000): void {
  if (bootstrapRefreshTimer != null) return;
  bootstrapRefreshTimer = window.setTimeout(() => {
    bootstrapRefreshTimer = undefined;
    void get().refreshBootstrap({ background: true });
  }, delayMs);
}

function mergeAgentIntoBootstrap(bootstrap: RuntimeBootstrap, updatedAgent: AgentSummary): RuntimeBootstrap {
  const existingIndex = bootstrap.agents.findIndex((agent) => agent.id === updatedAgent.id);
  const agents =
    existingIndex >= 0
      ? bootstrap.agents.map((agent) => (agent.id === updatedAgent.id ? updatedAgent : agent))
      : [...bootstrap.agents, updatedAgent];

  return {
    ...bootstrap,
    agents,
    attentionCount: countAgentsNeedingAttention(agents),
    metrics: buildBootstrapMetrics(agents),
  };
}

function sortBootstrapAgents(bootstrap: RuntimeBootstrap, rosterActivityByAgentId: Record<string, AgentRosterActivity>): RuntimeBootstrap {
  return {
    ...bootstrap,
    agents: sortAgentsByRosterActivity(bootstrap.agents, rosterActivityByAgentId),
  };
}

function sortAgentsByRosterActivity(
  agents: AgentSummary[],
  rosterActivityByAgentId: Record<string, AgentRosterActivity>,
): AgentSummary[] {
  return [...agents].sort((left, right) => {
    const lifecycle = compareStoppedLast(left, right);
    if (lifecycle !== 0) return lifecycle;
    const leftActivity = rosterActivityByAgentId[left.id];
    const rightActivity = rosterActivityByAgentId[right.id];
    const operator = compareIsoDesc(leftActivity?.operatorAt, rightActivity?.operatorAt);
    if (operator !== 0) return operator;
    const brief = compareIsoDesc(leftActivity?.briefAt, rightActivity?.briefAt);
    if (brief !== 0) return brief;
    return left.id.localeCompare(right.id);
  });
}

function compareStoppedLast(left: AgentSummary, right: AgentSummary): number {
  const leftStopped = isStoppedAgent(left);
  const rightStopped = isStoppedAgent(right);
  if (leftStopped === rightStopped) return 0;
  return leftStopped ? 1 : -1;
}

function isStoppedAgent(agent: AgentSummary): boolean {
  return agent.lifecycle.toLowerCase() === "stopped";
}

function compareIsoDesc(left: string | undefined, right: string | undefined): number {
  const leftTime = sortableTime(left ?? "");
  const rightTime = sortableTime(right ?? "");
  return rightTime - leftTime;
}

function touchRosterActivity(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  kind: "operator" | "brief",
  timestamp: string | undefined,
): Record<string, AgentRosterActivity> {
  if (!timestamp) return current;
  const existing = current[agentId];
  const field = kind === "operator" ? "operatorAt" : "briefAt";
  if (sortableTime(existing?.[field] ?? "") >= sortableTime(timestamp)) return current;
  return {
    ...current,
    [agentId]: {
      ...existing,
      [field]: timestamp,
    },
  };
}

function touchRosterActivityFromEvent(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  event: StreamEventEnvelopeDto,
): Record<string, AgentRosterActivity> {
  if (event.type === "brief_created") {
    return touchRosterActivity(current, agentId, "brief", eventTimestamp(event));
  }
  if (event.type === "message_enqueued" && messageOrigin(event.payload) === "operator") {
    return touchRosterActivity(current, agentId, "operator", eventTimestamp(event));
  }
  return current;
}

function eventTimestamp(event: StreamEventEnvelopeDto): string | undefined {
  const payload = asRecord(event.payload);
  return stringField(payload, "created_at") ?? event.ts;
}

function messageOrigin(payload: unknown): string | undefined {
  const origin = asRecord(asRecord(payload)?.origin);
  return stringField(origin, "kind") ?? stringField(origin, "role") ?? stringField(asRecord(payload), "origin");
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : undefined;
}

function stringField(record: Record<string, unknown> | undefined, key: string): string | undefined {
  const value = record?.[key];
  return typeof value === "string" && value.trim() ? value : undefined;
}

function countAgentsNeedingAttention(agents: AgentSummary[]): number {
  return agents.filter((agent) => agent.pending > 0 || agent.waitingCount > 0).length;
}

function buildBootstrapMetrics(agents: AgentSummary[]): RuntimeBootstrap["metrics"] {
  const attentionCount = countAgentsNeedingAttention(agents);
  const activeTaskCount = agents.reduce((sum, agent) => sum + agent.activeTaskCount, 0);
  const currentWorkCount = agents.filter((agent) => agent.currentWork).length;

  return [
    { label: "Agents", value: String(agents.length) },
    { label: "Needs attention", value: String(attentionCount), tone: attentionCount > 0 ? "attention" : "muted" },
    { label: "Active tasks", value: String(activeTaskCount), tone: activeTaskCount > 0 ? "attention" : "muted" },
    { label: "Current work", value: String(currentWorkCount) },
  ];
}

function updateAgentModelInState(
  state: RuntimeStoreState,
  agentId: string,
  modelPatch: Pick<AgentSummary, "model"> & Partial<Pick<AgentSummary, "modelSource">>,
): Partial<RuntimeStoreState> {
  const session = state.sessionsByAgentId[agentId];
  const detail = session?.detail
    ? {
        ...session.detail,
        agent: {
          ...session.detail.agent,
          ...modelPatch,
        },
      }
    : session?.detail;

  return {
    bootstrap: {
      ...state.bootstrap,
      agents: state.bootstrap.agents.map((agent) => (agent.id === agentId ? { ...agent, ...modelPatch } : agent)),
    },
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...emptyAgentSession(),
        ...session,
        detail,
      },
    },
  };
}

function mergeAgentDetailIntoSession(state: RuntimeStoreState, agentId: string, detail: AgentDetail): Partial<RuntimeStoreState> {
  const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  const pageEvents = detail.events ?? [];
  const eventsBySeq = {
    ...current.eventsBySeq,
    ...eventsBySeqFromPage(pageEvents),
  };
  const eventSeqs = Array.from(new Set([...current.eventSeqs, ...eventSeqsFromPage(pageEvents)])).sort((left, right) => left - right);
  const events = eventSeqs.map((eventSeq) => eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
  const pageTimeline = reduceAgentSessionTimeline({
    transcript: [],
    briefs: [],
    events: { events: pageEvents },
    eventDisplayLevel: "debug",
  });
  const liveDetailIsNewer = (current.newestSeq ?? 0) > Math.max(detail.eventCursorSeq ?? 0, detail.newestEventSeq ?? 0);
  const agent = liveDetailIsNewer && current.detail ? preserveLiveAgentRunState(detail.agent, current.detail.agent) : detail.agent;
  const mergedDetail: AgentDetail = {
    ...detail,
    agent,
    timeline: mergeTimeline(pageTimeline, current.detail?.timeline ?? []),
    events,
    newestEventSeq: Math.max(detail.newestEventSeq ?? 0, current.detail?.newestEventSeq ?? 0, highestSeq(eventSeqs) ?? 0),
    oldestEventSeq: detail.oldestEventSeq ?? current.detail?.oldestEventSeq ?? eventSeqs[0],
    hasOlderEvents: detail.hasOlderEvents,
  };
  const newestSeq = Math.max(detail.newestEventSeq ?? 0, current.newestSeq ?? 0, highestSeq(eventSeqs) ?? 0);

  return {
    bootstrap:
      detail.source === "http" && !detail.error
        ? sortBootstrapAgents(mergeAgentIntoBootstrap(state.bootstrap, agent), state.rosterActivityByAgentId)
        : state.bootstrap,
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...emptyAgentSession(),
        ...current,
        detail: mergedDetail,
        loading: false,
        liveStatus: detail.error ? "error" : current.liveStatus,
        eventsBySeq,
        eventSeqs,
        newestSeq: newestSeq || undefined,
        oldestSeq: detail.oldestEventSeq ?? current.oldestSeq ?? eventSeqs[0],
        hasOlder: detail.hasOlderEvents,
        error: detail.error,
      },
    },
  };
}

async function catchUpAgentEvents(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  _displayLevel: DisplayLevel,
): Promise<void> {
  const afterSeq = get().sessionsByAgentId[agentId]?.newestSeq;
  const page = await runtimeClient.getAgentEvents(agentId, {
    afterSeq,
    limit: 100,
    order: "asc",
  });
  set((state) =>
    mergeEventPageIntoSession(state, agentId, page.events ?? [], page.oldest_seq, page.has_older, "debug", {
      newestSeq: page.cursor_seq ?? page.newest_seq,
      append: true,
    }),
  );
}

function applyStreamEvents(set: StoreSet, agentId: string, events: StreamEventEnvelopeDto[]): void {
  const incomingEvents = events.filter((event) => event.event_seq != null);
  if (!incomingEvents.length) return;

  set((state) => {
    const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
    const uniqueEvents = incomingEvents.filter((event) => !current.eventsBySeq[event.event_seq as number]);
    if (!uniqueEvents.length) {
      return {
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...current,
            liveStatus: "streaming",
            error: undefined,
          },
        },
      };
    }
    const rosterActivityByAgentId = uniqueEvents.reduce(
      (activityByAgentId, event) => touchRosterActivityFromEvent(activityByAgentId, agentId, event),
      state.rosterActivityByAgentId,
    );
    const eventsBySeq = {
      ...current.eventsBySeq,
      ...eventsBySeqFromPage(uniqueEvents),
    };
    const eventSeqs = Array.from(new Set([...current.eventSeqs, ...eventSeqsFromPage(uniqueEvents)])).sort((left, right) => left - right);
    const liveTimelineDelta = reduceAgentSessionTimeline({
      transcript: [],
      briefs: [],
      events: { events: uniqueEvents },
      eventDisplayLevel: "debug",
    });
    const detailEvents = eventSeqs.map((eventSeq) => eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
    const baseDetail = current.detail ?? createLiveAgentDetail(state.bootstrap.agents.find((agent) => agent.id === agentId));
    const highestIncomingSeq = highestSeq(eventSeqs) ?? 0;
    const runPatch = agentRunPatchFromEvents(uniqueEvents);
    const patchedBaseDetail = baseDetail && runPatch ? patchAgentDetailRunState(baseDetail, runPatch) : baseDetail;
    const detail = patchedBaseDetail
      ? {
          ...patchedBaseDetail,
          timeline: mergeTimeline(patchedBaseDetail.timeline, liveTimelineDelta),
          events: detailEvents,
          newestEventSeq: Math.max(highestIncomingSeq, patchedBaseDetail.newestEventSeq ?? 0),
          oldestEventSeq: patchedBaseDetail.oldestEventSeq ?? eventSeqs[0],
        }
      : patchedBaseDetail;

    return {
      bootstrap: sortBootstrapAgents(
        runPatch ? patchBootstrapAgentRunState(state.bootstrap, agentId, runPatch) : state.bootstrap,
        rosterActivityByAgentId,
      ),
      rosterActivityByAgentId,
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...current,
          detail,
          eventsBySeq,
          eventSeqs,
          newestSeq: Math.max(highestIncomingSeq, current.newestSeq ?? 0),
          oldestSeq: current.oldestSeq ?? eventSeqs[0],
          liveStatus: "streaming",
          error: undefined,
        },
      },
    };
  });
}

function agentRunPatchFromEvents(events: StreamEventEnvelopeDto[]): Pick<AgentSummary, "currentRunId" | "lifecycle"> | undefined {
  let patch: Pick<AgentSummary, "currentRunId" | "lifecycle"> | undefined;
  for (const event of events) {
    if (event.type === "message_processing_started") {
      patch = {
        currentRunId: runIdFromPayload(event.payload) ?? `event:${event.event_seq ?? event.id ?? "message_processing_started"}`,
        lifecycle: "awake-running",
      };
    }
    if (
      event.type === "turn_terminal" ||
      event.type === "turn_terminal_aborted" ||
      event.type === "message_processing_aborted" ||
      event.type === "runtime_error"
    ) {
      patch = {
        currentRunId: null,
        lifecycle: "awake-idle",
      };
    }
  }
  return patch;
}

function runIdFromPayload(payload: unknown): string | undefined {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) return undefined;
  const value = (payload as Record<string, unknown>).run_id;
  return typeof value === "string" && value.trim() ? value : undefined;
}

function patchBootstrapAgentRunState(
  bootstrap: RuntimeBootstrap,
  agentId: string,
  patch: Pick<AgentSummary, "currentRunId" | "lifecycle">,
): RuntimeBootstrap {
  return {
    ...bootstrap,
    agents: bootstrap.agents.map((agent) => (agent.id === agentId ? { ...agent, ...patch } : agent)),
  };
}

function patchAgentDetailRunState(
  detail: AgentDetail,
  patch: Pick<AgentSummary, "currentRunId" | "lifecycle">,
): AgentDetail {
  return {
    ...detail,
    agent: {
      ...detail.agent,
      ...patch,
    },
  };
}

function mergeEventPageIntoSession(
  state: RuntimeStoreState,
  agentId: string,
  pageEvents: StreamEventEnvelopeDto[],
  pageOldestSeq: number | undefined,
  pageHasOlder: boolean | undefined,
  displayLevel: DisplayLevel,
  options: { newestSeq?: number; append?: boolean } = {},
): Partial<RuntimeStoreState> {
  const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  const eventsBySeq = {
    ...current.eventsBySeq,
    ...eventsBySeqFromPage(pageEvents),
  };
  const eventSeqs = Array.from(new Set([...current.eventSeqs, ...eventSeqsFromPage(pageEvents)])).sort((left, right) => left - right);
  const pageTimeline = reduceAgentSessionTimeline({
    transcript: [],
    briefs: [],
    events: { events: pageEvents },
    eventDisplayLevel: displayLevel,
  });
  const events = eventSeqs.map((eventSeq) => eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
  const detail = current.detail
    ? {
        ...current.detail,
        timeline: options.append ? mergeTimeline(current.detail.timeline, pageTimeline) : mergeTimeline(pageTimeline, current.detail.timeline),
        events,
        newestEventSeq: Math.max(options.newestSeq ?? 0, current.detail.newestEventSeq ?? 0, highestSeq(eventSeqs) ?? 0),
        oldestEventSeq: pageOldestSeq ?? eventSeqs[0] ?? current.detail.oldestEventSeq,
        hasOlderEvents: pageHasOlder,
      }
    : current.detail;

  return {
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...current,
        detail,
        eventsBySeq,
        eventSeqs,
        newestSeq: Math.max(options.newestSeq ?? 0, current.newestSeq ?? 0, highestSeq(eventSeqs) ?? 0) || undefined,
        oldestSeq: pageOldestSeq ?? eventSeqs[0] ?? current.oldestSeq,
        hasOlder: pageHasOlder,
        loadingOlder: false,
        historyError: undefined,
      },
    },
  };
}

function eventsBySeq(events: StreamEventEnvelopeDto[]): Record<number, unknown> {
  return eventsBySeqFromPage(events);
}

function eventsBySeqFromPage(events: StreamEventEnvelopeDto[]): Record<number, unknown> {
  return Object.fromEntries(events.filter((event) => event.event_seq != null).map((event) => [event.event_seq, event]));
}

function eventSeqs(events: StreamEventEnvelopeDto[]): number[] {
  return eventSeqsFromPage(events);
}

function eventSeqsFromPage(events: StreamEventEnvelopeDto[]): number[] {
  return events
    .map((event) => event.event_seq)
    .filter((seq): seq is number => seq != null)
    .sort((left, right) => left - right);
}

function highestSeq(eventSeqs: number[]): number | undefined {
  return eventSeqs.length ? eventSeqs[eventSeqs.length - 1] : undefined;
}

function isStreamEventEnvelope(event: unknown): event is StreamEventEnvelopeDto {
  return typeof event === "object" && event !== null;
}

function mergeTimeline(existing: AgentTimelineItem[], incoming: AgentTimelineItem[]): AgentTimelineItem[] {
  const sorted = mergeAgentTimelineItems(existing, incoming).sort(
    (left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp),
  );
  return compactAgentTimelineItems(sorted);
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isNaN(timestamp) ? 0 : timestamp;
}
