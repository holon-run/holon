import { create } from "zustand";

import { createRuntimeClient, type AgentEventStreamSubscription, type StreamEventEnvelopeDto } from "./client";
import {
  compactAgentTimelineItems,
  mergeAgentTimelineItems,
  reduceAgentSessionTimeline,
  briefIdForPayload,
  transcriptEntryIdForPayload,
} from "./session-reducer";
import type {
  AgentDetail,
  AgentSummary,
  AgentTimelineActivity,
  AgentTimelineItem,
  DisplayLevel,
  InspectorSelection,
  RouteKey,
  RuntimeBootstrap,
  RuntimeConnectionConfig,
  RuntimeConfigState,
  CredentialProfileStatus,
  CredentialStoreState,
  RuntimeBriefRecord,
  RuntimeMessageEnvelope,
  RuntimeModelCatalog,
  RuntimeTranscriptEntry,
  SearchResponse,
} from "./types";

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

function mergeCachedAgentState(httpAgent: AgentSummary, cachedAgent: AgentSummary): AgentSummary {
  const merged: AgentSummary = {
    ...httpAgent,
    currentWork: cachedAgent.currentWork ?? httpAgent.currentWork,
    workItems: cachedAgent.workItems?.length ? cachedAgent.workItems : httpAgent.workItems,
    tasks: cachedAgent.tasks?.length ? cachedAgent.tasks : httpAgent.tasks,
    activeTaskCount: Math.max(httpAgent.activeTaskCount, cachedAgent.activeTaskCount),
    waitingCount: Math.max(httpAgent.waitingCount, cachedAgent.waitingCount),
    pending: Math.max(httpAgent.pending, cachedAgent.pending),
    workspaceSummary: cachedAgent.workspaceSummary ?? httpAgent.workspaceSummary,
  };

  if (!isLiveRunningAgent(cachedAgent)) return merged;
  return {
    ...merged,
    currentRunId: cachedAgent.currentRunId,
    lifecycle: cachedAgent.lifecycle,
  };
}

function isLiveRunningAgent(agent: AgentSummary): boolean {
  return Boolean(agent.currentRunId) || agent.lifecycle.toLowerCase() === "awake-running";
}

function isAgentEventStreamActive(agentId: string, liveStatus: AgentLiveStatus | undefined): boolean {
  return activeEventStreams.has(agentId) && (liveStatus === "streaming" || liveStatus === "recovering");
}

function cachedAgentsByIdFromState(state: RuntimeStoreState): Record<string, AgentSummary> {
  const agentsById: Record<string, AgentSummary> = Object.fromEntries(state.bootstrap.agents.map((agent) => [agent.id, agent]));
  for (const session of Object.values(state.sessionsByAgentId)) {
    const agent = session.detail?.agent;
    if (!agent) continue;
    agentsById[agent.id] = agentsById[agent.id] ? mergeCachedAgentState(agentsById[agent.id], agent) : agent;
  }
  return agentsById;
}

export interface AgentSessionState {
  loading: boolean;
  loadingOlder: boolean;
  liveStatus: AgentLiveStatus;
  sendingPrompt: boolean;
  detail: AgentDetail | null;
  eventsBySeq: Record<number, unknown>;
  eventSeqs: number[];
  messagesById: Record<string, RuntimeMessageEnvelope>;
  missingMessageIds: Record<string, true>;
  transcriptEntriesById: Record<string, RuntimeTranscriptEntry>;
  missingTranscriptEntryIds: Record<string, true>;
  briefRecordsById: Record<string, RuntimeBriefRecord>;
  missingBriefIds: Record<string, true>;
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

function markOptimisticOperatorPromptsSent(detail: AgentDetail | null): AgentDetail | null {
  if (!detail) return detail;
  let changed = false;
  const timeline = detail.timeline.map((item) => {
    if (item.kind !== "operator" || item.meta !== "sending" || !item.sourceIds.includes("pending-operator-prompt")) {
      return item;
    }
    changed = true;
    return {
      ...item,
      meta: "Sent",
    };
  });
  return changed ? { ...detail, timeline } : detail;
}

export interface RuntimeStoreState {
  route: RouteKey;
  selectedAgentId: string;
  displayLevel: DisplayLevel;
  displayLevelsByAgentId: Record<string, DisplayLevel>;
  inspectorOpen: boolean;
  inspectorSelection?: InspectorSelection;
  navCollapsed: boolean;

  bootstrap: RuntimeBootstrap;
  bootstrapLoading: boolean;
  bootstrapError?: string;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  modelCatalogError?: string;
  runtimeConfig: RuntimeConfigState;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  runtimeConfigError?: string;
  credentialStore: CredentialStoreState;
  credentialStoreLoading: boolean;
  credentialStoreError?: string;
  search: SearchResponse | null;
  searchLoading: boolean;
  searchError?: string;
  rosterActivityByAgentId: Record<string, AgentRosterActivity>;
  sessionsByAgentId: Record<string, AgentSessionState>;

  setRoute: (route: RouteKey) => void;
  openAgent: (agentId: string) => void;
  setDisplayLevel: (displayLevel: DisplayLevel, agentId?: string) => void;
  setInspectorOpen: (open: boolean) => void;
  inspectActivity: (agentId: string, activity: AgentTimelineActivity) => void;
  clearInspectorSelection: () => void;
  toggleInspector: () => void;
  toggleNavCollapsed: () => void;
  setRuntimeConnection: (config: RuntimeConnectionConfig) => Promise<void>;
  refreshBootstrap: (options?: BootstrapRefreshOptions) => Promise<void>;
  refreshModelCatalog: () => Promise<void>;
  refreshRuntimeConfig: () => Promise<void>;
  updateRuntimeConfig: (updates: Array<{ key: string; value?: unknown; unset?: boolean }>) => Promise<RuntimeConfigState | undefined>;
  refreshCredentialStore: () => Promise<void>;
  setCredential: (profile: string, kind: string, material: string) => Promise<CredentialProfileStatus | undefined>;
  deleteCredential: (profile: string) => Promise<void>;
  runSearch: (query: string, options?: { agentIds?: string[]; limit?: number }) => Promise<void>;
  refreshAgentDetail: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  loadOlderAgentEvents: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  sendOperatorPrompt: (agentId: string | undefined, text: string, displayLevel: DisplayLevel) => Promise<void>;
  setAgentModel: (agentId: string | undefined, model: string, displayLevel: DisplayLevel, reasoningEffort?: string) => Promise<void>;
  clearAgentModel: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  startAgentEventStream: (agentId: string | undefined, displayLevel: DisplayLevel) => void;
  stopAgentEventStream: (agentId: string | undefined) => void;
}

const LEGACY_RUNTIME_CONNECTION_STORAGE_KEY = "holon.webGui.runtimeConnection.v1";
const ACTIVE_RUNTIME_CONNECTION_STORAGE_KEY = "holon.webGui.activeRuntimeConnection.v1";
const RUNTIME_CONNECTION_PROFILES_STORAGE_KEY = "holon.webGui.runtimeConnectionProfiles.v1";
const DISPLAY_LEVEL_STORAGE_KEY = "holon.webGui.displayLevelsByAgentId.v1";
let runtimeConnectionConfig = readStoredRuntimeConnectionConfig();
let runtimeClient = createRuntimeClient(runtimeClientOptions(runtimeConnectionConfig));
const activeEventStreams = new Map<string, AgentEventStreamSubscription>();
const pendingStreamEvents = new Map<string, StreamEventEnvelopeDto[]>();
const streamFlushTimers = new Map<string, number>();
const reconnectTimers = new Map<string, number>();
const staleTimers = new Map<string, number>();
const messageHydrationInFlight = new Map<string, Set<string>>();
const transcriptHydrationInFlight = new Map<string, Set<string>>();
const briefHydrationInFlight = new Map<string, Set<string>>();
let bootstrapRefreshInFlight: Promise<void> | undefined;
let bootstrapRefreshTimer: number | undefined;
const STREAM_FLUSH_INTERVAL_MS = 100;
const STREAM_STALE_TIMEOUT_MS = 45_000;
const STREAM_RECONNECT_BASE_MS = 1_000;
const STREAM_RECONNECT_MAX_MS = 15_000;

function runtimeClientOptions(config: RuntimeConnectionConfig) {
  return config.mode === "remote" ? { mode: "remote" as const, baseUrl: config.baseUrl, token: config.token } : { mode: "local" as const };
}

export function readStoredRuntimeConnectionConfig(): RuntimeConnectionConfig {
  if (typeof window === "undefined") return { mode: "local" };
  const activeConfig = coerceRuntimeConnectionConfig(readStoredJson(window.sessionStorage, ACTIVE_RUNTIME_CONNECTION_STORAGE_KEY));
  if (activeConfig) return withStoredRemoteProfileToken(activeConfig);

  const legacyConfig = coerceRuntimeConnectionConfig(readStoredJson(window.localStorage, LEGACY_RUNTIME_CONNECTION_STORAGE_KEY));
  if (legacyConfig?.mode === "remote") {
    writeStoredRuntimeConnectionConfig(legacyConfig);
    removeStoredItem(window.localStorage, LEGACY_RUNTIME_CONNECTION_STORAGE_KEY);
    return withStoredRemoteProfileToken(legacyConfig);
  }

  if (legacyConfig?.mode === "local") {
    writeActiveRuntimeConnectionConfig(legacyConfig);
    removeStoredItem(window.localStorage, LEGACY_RUNTIME_CONNECTION_STORAGE_KEY);
  }

  return { mode: "local" };
}

export function writeStoredRuntimeConnectionConfig(config: RuntimeConnectionConfig): void {
  try {
    removeStoredItem(window.localStorage, LEGACY_RUNTIME_CONNECTION_STORAGE_KEY);
    writeActiveRuntimeConnectionConfig(config);
    if (config.mode === "remote") writeStoredRemoteProfile(config);
  } catch {
    // Ignore storage failures; the in-memory connection still applies.
  }
}

function coerceRuntimeConnectionConfig(value: unknown): RuntimeConnectionConfig | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const parsed = value as Partial<RuntimeConnectionConfig>;
  if (parsed.mode === "local") return { mode: "local" };
  if (parsed.mode !== "remote") return undefined;
  const baseUrl = normalizeConnectionBaseUrl(parsed.baseUrl);
  if (!baseUrl) return undefined;
  return {
    mode: "remote",
    baseUrl,
    token: typeof parsed.token === "string" && parsed.token.trim() ? parsed.token.trim() : undefined,
  };
}

function readStoredJson(storage: Storage, key: string): unknown {
  try {
    const raw = storage.getItem(key);
    return raw ? JSON.parse(raw) : undefined;
  } catch {
    return undefined;
  }
}

function removeStoredItem(storage: Storage, key: string): void {
  try {
    storage.removeItem(key);
  } catch {
    // Ignore storage failures; the in-memory connection still applies.
  }
}

function writeActiveRuntimeConnectionConfig(config: RuntimeConnectionConfig): void {
  if (typeof window === "undefined") return;
  const activeConfig = coerceRuntimeConnectionConfig(config) ?? { mode: "local" };
  try {
    window.sessionStorage.setItem(ACTIVE_RUNTIME_CONNECTION_STORAGE_KEY, JSON.stringify(activeConfig));
  } catch {
    // Ignore storage failures; the in-memory connection still applies.
  }
}

function readStoredRemoteProfiles(): Record<string, RuntimeConnectionConfig> {
  if (typeof window === "undefined") return {};
  const parsed = readStoredJson(window.localStorage, RUNTIME_CONNECTION_PROFILES_STORAGE_KEY);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
  const profiles: Record<string, RuntimeConnectionConfig> = {};
  for (const [key, value] of Object.entries(parsed)) {
    const profile = coerceRuntimeConnectionConfig(value);
    const profileBaseUrl = profile?.baseUrl;
    if (profile?.mode === "remote" && profileBaseUrl && key === remoteProfileKey(profileBaseUrl)) {
      profiles[key] = profile;
    }
  }
  return profiles;
}

function writeStoredRemoteProfile(config: RuntimeConnectionConfig): void {
  if (typeof window === "undefined" || config.mode !== "remote") return;
  const profile = coerceRuntimeConnectionConfig(config);
  if (profile?.mode !== "remote") return;
  const profileBaseUrl = profile.baseUrl;
  if (!profileBaseUrl) return;
  const profiles = readStoredRemoteProfiles();
  const key = remoteProfileKey(profileBaseUrl);
  const existingProfile = profiles[key];
  profiles[key] = {
    ...profile,
    token: profile.token ?? (existingProfile?.mode === "remote" ? existingProfile.token : undefined),
  };
  try {
    window.localStorage.setItem(RUNTIME_CONNECTION_PROFILES_STORAGE_KEY, JSON.stringify(profiles));
  } catch {
    // Ignore storage failures; the in-memory connection still applies.
  }
}

function normalizeConnectionBaseUrl(value: string | undefined): string {
  return value?.trim().replace(/\/+$/, "") ?? "";
}

function remoteProfileKey(baseUrl: string): string {
  return normalizeConnectionBaseUrl(baseUrl);
}

function withStoredRemoteProfileToken(config: RuntimeConnectionConfig): RuntimeConnectionConfig {
  if (config.mode !== "remote" || config.token) return config;
  const baseUrl = config.baseUrl;
  if (!baseUrl) return config;
  const profile = readStoredRemoteProfiles()[remoteProfileKey(baseUrl)];
  if (profile?.mode !== "remote" || !profile.token) return config;
  return { ...config, token: profile.token };
}

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

function pendingBootstrap(config: RuntimeConnectionConfig): RuntimeBootstrap {
  return {
    ...emptyBootstrap,
    connection: {
      mode: config.mode,
      source: "fixture",
      baseUrl: config.mode === "remote" ? config.baseUrl : undefined,
      hasToken: config.mode === "remote" ? Boolean(config.token?.trim()) : false,
      summary: config.mode === "remote" ? "Connecting to remote runtime…" : "Connecting to local runtime…",
    },
  };
}

const emptyModelCatalog: RuntimeModelCatalog = {
  source: "fixture",
  options: [],
};

const emptyRuntimeConfig: RuntimeConfigState = {
  source: "fixture",
};

export const useRuntimeStore = create<RuntimeStoreState>((set, get) => ({
  route: "dashboard",
  selectedAgentId: "",
  displayLevel: "info",
  displayLevelsByAgentId: readStoredDisplayLevels(),
  inspectorOpen: true,
  inspectorSelection: undefined,
  navCollapsed: false,

  bootstrap: pendingBootstrap(runtimeConnectionConfig),
  bootstrapLoading: true,
  modelCatalog: emptyModelCatalog,
  modelCatalogLoading: false,
  runtimeConfig: emptyRuntimeConfig,
  runtimeConfigLoading: false,
  runtimeConfigSaving: false,
  search: null,
  searchLoading: false,
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
  inspectActivity: (agentId, activity) =>
    set({
      inspectorOpen: true,
      inspectorSelection: { kind: "activity", agentId, activity },
    }),
  clearInspectorSelection: () => set({ inspectorSelection: undefined }),
  toggleInspector: () => set((state) => ({ inspectorOpen: !state.inspectorOpen })),
  toggleNavCollapsed: () => set((state) => ({ navCollapsed: !state.navCollapsed })),

  setRuntimeConnection: async (config) => {
    const normalizedBaseUrl = config.mode === "remote" ? normalizeConnectionBaseUrl(config.baseUrl) : "";
    const retainedToken =
      config.mode === "remote" &&
      config.token === undefined &&
      runtimeConnectionConfig.mode === "remote" &&
      normalizeConnectionBaseUrl(runtimeConnectionConfig.baseUrl) === normalizedBaseUrl
        ? runtimeConnectionConfig.token
        : undefined;
    const normalizedConfig: RuntimeConnectionConfig =
      config.mode === "remote"
        ? {
            mode: "remote",
            baseUrl: normalizedBaseUrl,
            token: config.token?.trim() || retainedToken,
          }
        : { mode: "local" };
    runtimeConnectionConfig = normalizedConfig;
    runtimeClient = createRuntimeClient(runtimeClientOptions(normalizedConfig));
    writeStoredRuntimeConnectionConfig(normalizedConfig);
    bootstrapRefreshInFlight = undefined;
    for (const subscription of activeEventStreams.values()) subscription.close();
    activeEventStreams.clear();
    pendingStreamEvents.clear();
    messageHydrationInFlight.clear();
    transcriptHydrationInFlight.clear();
    briefHydrationInFlight.clear();
    for (const timer of streamFlushTimers.values()) window.clearTimeout(timer);
    for (const timer of reconnectTimers.values()) window.clearTimeout(timer);
    for (const timer of staleTimers.values()) window.clearTimeout(timer);
    streamFlushTimers.clear();
    reconnectTimers.clear();
    staleTimers.clear();
    set({
      bootstrap: pendingBootstrap(normalizedConfig),
      bootstrapLoading: true,
      bootstrapError: undefined,
      modelCatalog: emptyModelCatalog,
      modelCatalogError: undefined,
      runtimeConfig: emptyRuntimeConfig,
      runtimeConfigError: undefined,
      credentialStore: { profiles: [] },
      credentialStoreLoading: false,
      credentialStoreError: undefined,
      search: null,
      searchError: undefined,
      sessionsByAgentId: {},
      selectedAgentId: "",
      route: "dashboard",
    });
    await get().refreshBootstrap();
  },

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
          const cachedAgentsById = cachedAgentsByIdFromState(state);
          const agents = bootstrap.agents.map((agent) => {
            const cachedAgent = cachedAgentsById[agent.id];
            return cachedAgent ? mergeCachedAgentState(agent, cachedAgent) : agent;
          });
          return {
            bootstrap: sortBootstrapAgents(
              {
                ...bootstrap,
                agents,
                attentionCount: countAgentsNeedingAttention(agents),
                metrics: buildBootstrapMetrics(agents),
              },
              state.rosterActivityByAgentId,
            ),
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

  refreshRuntimeConfig: async () => {
    set({ runtimeConfigLoading: true, runtimeConfigError: undefined });
    try {
      const runtimeConfig = await runtimeClient.getRuntimeConfig();
      set({ runtimeConfig, runtimeConfigLoading: false, runtimeConfigError: runtimeConfig.error });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        runtimeConfig: { ...state.runtimeConfig, error: message },
        runtimeConfigLoading: false,
        runtimeConfigError: message,
      }));
    }
  },

  updateRuntimeConfig: async (updates) => {
    set({ runtimeConfigSaving: true, runtimeConfigError: undefined });
    try {
      const runtimeConfig = await runtimeClient.updateRuntimeConfig(updates);
      set({ runtimeConfig, runtimeConfigSaving: false, runtimeConfigError: runtimeConfig.error });
      return runtimeConfig;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        runtimeConfig: { ...state.runtimeConfig, error: message },
        runtimeConfigSaving: false,
        runtimeConfigError: message,
      }));
      return undefined;
    }
  },


  refreshCredentialStore: async () => {
    set({ credentialStoreLoading: true, credentialStoreError: undefined });
    try {
      const credentialStore = await runtimeClient.listCredentials();
      set({ credentialStore, credentialStoreLoading: false });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ credentialStoreLoading: false, credentialStoreError: message });
    }
  },

  setCredential: async (profile, kind, material) => {
    try {
      const result = await runtimeClient.setCredential(profile, kind, material);
      const credentialStore = await runtimeClient.listCredentials();
      set({ credentialStore, credentialStoreError: undefined });
      return result;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ credentialStoreError: message });
      return undefined;
    }
  },

  deleteCredential: async (profile) => {
    try {
      await runtimeClient.deleteCredential(profile);
      const credentialStore = await runtimeClient.listCredentials();
      set({ credentialStore, credentialStoreError: undefined });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ credentialStoreError: message });
    }
  },
  runSearch: async (query, options = {}) => {
    const trimmed = query.trim();
    if (!trimmed) {
      set({ search: null, searchLoading: false, searchError: undefined });
      return;
    }
    set({ searchLoading: true, searchError: undefined });
    try {
      const search = await runtimeClient.search(trimmed, options);
      set({ search, searchLoading: false });
    } catch (error) {
      set({ searchLoading: false, searchError: error instanceof Error ? error.message : String(error) });
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
      scheduleMessageHydration(get, set, agentId, displayLevel);
      scheduleTranscriptHydration(get, set, agentId, displayLevel);
      scheduleBriefHydration(get, set, agentId, displayLevel);
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
      scheduleMessageHydration(get, set, agentId, displayLevel);
      scheduleTranscriptHydration(get, set, agentId, displayLevel);
      scheduleBriefHydration(get, set, agentId, displayLevel);
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
            detail: markOptimisticOperatorPromptsSent(state.sessionsByAgentId[agentId]?.detail ?? null),
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
          modelReasoningEffort: modelState?.override_reasoning_effort ?? undefined,
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
          modelReasoningEffort: modelState?.override_reasoning_effort ?? undefined,
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
    messagesById: {},
    missingMessageIds: {},
    transcriptEntriesById: {},
    missingTranscriptEntryIds: {},
    briefRecordsById: {},
    missingBriefIds: {},
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

function messageIdFromEventPayload(payload: unknown): string | undefined {
  return stringField(asRecord(payload), "message_id");
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : undefined;
}

function stringField(record: Record<string, unknown> | undefined, key: string): string | undefined {
  const value = record?.[key];
  return typeof value === "string" && value.trim() ? value : undefined;
}

function transcriptEntryText(entry: RuntimeTranscriptEntry | undefined): string | undefined {
  const data = asRecord(entry?.data);
  const text = stringField(data, "text");
  if (text) return text;
  const blocks = Array.isArray(data?.blocks) ? data.blocks : [];
  const parts = blocks.flatMap((block) => {
    const record = asRecord(block);
    return stringField(record, "text") ?? stringField(record, "content") ?? [];
  });
  return parts.filter(Boolean).join("\n\n") || undefined;
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
  modelPatch: Pick<AgentSummary, "model"> & Partial<Pick<AgentSummary, "modelSource" | "modelReasoningEffort">>,
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
  const briefRecordsById = {
    ...current.briefRecordsById,
    ...(detail.briefRecordsById ?? {}),
  };
  const eventSeqs = Array.from(new Set([...current.eventSeqs, ...eventSeqsFromPage(pageEvents)])).sort((left, right) => left - right);
  const events = eventSeqs.map((eventSeq) => eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
  const pageTimeline = reduceAgentSessionTimeline({
    events: { events: pageEvents },
    eventDisplayLevel: "debug",
    messagesById: current.messagesById,
    transcriptEntriesById: current.transcriptEntriesById,
    briefRecordsById,
  });
  const liveDetailIsNewer = (current.newestSeq ?? 0) > Math.max(detail.eventCursorSeq ?? 0, detail.newestEventSeq ?? 0);
  const agent = liveDetailIsNewer && current.detail ? mergeCachedAgentState(detail.agent, current.detail.agent) : detail.agent;
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
        briefRecordsById,
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
  scheduleMessageHydration(get, set, agentId, "debug");
  scheduleTranscriptHydration(get, set, agentId, "debug");
  scheduleBriefHydration(get, set, agentId, "debug");
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
      events: { events: uniqueEvents },
      eventDisplayLevel: "debug",
      messagesById: current.messagesById,
      transcriptEntriesById: current.transcriptEntriesById,
      briefRecordsById: current.briefRecordsById,
    });
    const detailEvents = eventSeqs.map((eventSeq) => eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
    const baseDetail = current.detail ?? createLiveAgentDetail(state.bootstrap.agents.find((agent) => agent.id === agentId));
    const highestIncomingSeq = highestSeq(eventSeqs) ?? 0;
    const runPatch = agentRunPatchFromEvents(uniqueEvents);
    const briefPatch = agentBriefPatchFromEvents(uniqueEvents, current.transcriptEntriesById, current.briefRecordsById);
    const patchedBaseDetail = patchAgentDetail(baseDetail, runPatch, briefPatch);
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
        patchBootstrapAgent(state.bootstrap, agentId, runPatch, briefPatch),
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
  scheduleMessageHydration(useRuntimeStore.getState, set, agentId, useRuntimeStore.getState().displayLevel);
  scheduleTranscriptHydration(useRuntimeStore.getState, set, agentId, useRuntimeStore.getState().displayLevel);
  scheduleBriefHydration(useRuntimeStore.getState, set, agentId, useRuntimeStore.getState().displayLevel);
}

function scheduleMessageHydration(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  displayLevel: DisplayLevel,
): void {
  const session = get().sessionsByAgentId[agentId];
  const messageIds = missingMessageIdsForHydration(session);
  if (!messageIds.length) return;

  let inFlight = messageHydrationInFlight.get(agentId);
  if (!inFlight) {
    inFlight = new Set<string>();
    messageHydrationInFlight.set(agentId, inFlight);
  }
  const requestIds = messageIds.filter((messageId) => !inFlight.has(messageId));
  if (!requestIds.length) return;
  requestIds.forEach((messageId) => inFlight.add(messageId));

  void runtimeClient
    .getAgentMessagesBatch(agentId, requestIds)
    .then((response) => {
      set((state) => mergeHydratedMessagesIntoSession(state, agentId, response.messages ?? [], response.missing_message_ids ?? [], displayLevel));
    })
    .catch((error) => {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...(state.sessionsByAgentId[agentId] ?? emptyAgentSession()),
            historyError: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    })
    .finally(() => {
      const current = messageHydrationInFlight.get(agentId);
      if (!current) return;
      requestIds.forEach((messageId) => current.delete(messageId));
      if (!current.size) messageHydrationInFlight.delete(agentId);
    });
}

function scheduleTranscriptHydration(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  displayLevel: DisplayLevel,
): void {
  const session = get().sessionsByAgentId[agentId];
  const entryIds = missingTranscriptEntryIdsForHydration(session);
  if (!entryIds.length) return;

  let inFlight = transcriptHydrationInFlight.get(agentId);
  if (!inFlight) {
    inFlight = new Set<string>();
    transcriptHydrationInFlight.set(agentId, inFlight);
  }
  const requestIds = entryIds.filter((entryId) => !inFlight.has(entryId));
  if (!requestIds.length) return;
  requestIds.forEach((entryId) => inFlight.add(entryId));

  void runtimeClient
    .getAgentTranscriptEntriesBatch(agentId, requestIds)
    .then((response) => {
      set((state) =>
        mergeHydratedTranscriptEntriesIntoSession(
          state,
          agentId,
          response.entries ?? [],
          response.missing_entry_ids ?? [],
          displayLevel,
        ),
      );
    })
    .catch((error) => {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...(state.sessionsByAgentId[agentId] ?? emptyAgentSession()),
            historyError: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    })
    .finally(() => {
      const current = transcriptHydrationInFlight.get(agentId);
      if (!current) return;
      requestIds.forEach((entryId) => current.delete(entryId));
      if (!current.size) transcriptHydrationInFlight.delete(agentId);
    });
}

function scheduleBriefHydration(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  displayLevel: DisplayLevel,
): void {
  const session = get().sessionsByAgentId[agentId];
  const briefIds = missingBriefIdsForHydration(session);
  if (!briefIds.length) return;

  let inFlight = briefHydrationInFlight.get(agentId);
  if (!inFlight) {
    inFlight = new Set<string>();
    briefHydrationInFlight.set(agentId, inFlight);
  }
  const requestIds = briefIds.filter((briefId) => !inFlight.has(briefId));
  if (!requestIds.length) return;
  requestIds.forEach((briefId) => inFlight.add(briefId));

  void runtimeClient
    .getAgentBriefsById(agentId, requestIds)
    .then((recordsById) => {
      set((state) => mergeHydratedBriefRecordsIntoSession(state, agentId, recordsById, requestIds, displayLevel));
    })
    .catch((error) => {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...(state.sessionsByAgentId[agentId] ?? emptyAgentSession()),
            historyError: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    })
    .finally(() => {
      const current = briefHydrationInFlight.get(agentId);
      if (!current) return;
      requestIds.forEach((briefId) => current.delete(briefId));
      if (!current.size) briefHydrationInFlight.delete(agentId);
    });
}

function agentBriefPatchFromEvents(
  events: StreamEventEnvelopeDto[],
  transcriptEntriesById: Record<string, RuntimeTranscriptEntry> = {},
  briefRecordsById: Record<string, RuntimeBriefRecord> = {},
): Pick<AgentSummary, "lastBrief" | "lastTurnTime"> | undefined {
  let patch: Pick<AgentSummary, "lastBrief" | "lastTurnTime"> | undefined;
  for (const event of events) {
    if (event.type !== "brief_created") continue;
    const payload = asRecord(event.payload);
    const entryId = transcriptEntryIdForPayload(payload);
    const briefId = briefIdForPayload(payload);
    const text = (entryId ? transcriptEntryText(transcriptEntriesById[entryId]) : undefined) ?? stringField(payload, "text");
    const fallbackText = briefId ? briefRecordsById[briefId]?.text : undefined;
    const resolvedText = text ?? fallbackText;
    if (!resolvedText) continue;
    const createdAt = stringField(payload, "created_at") ?? event.ts;
    patch = {
      lastBrief: resolvedText,
      lastTurnTime: formatTime(createdAt),
    };
  }
  return patch;
}

function missingMessageIdsForHydration(session: AgentSessionState | undefined): string[] {
  if (!session) return [];
  const seen = new Set<string>();
  const missing: string[] = [];
  for (const eventSeq of session.eventSeqs) {
    const event = session.eventsBySeq[eventSeq];
    if (!isStreamEventEnvelope(event) || event.type !== "message_enqueued") continue;
    const messageId = messageIdFromEventPayload(event.payload);
    if (!messageId || seen.has(messageId) || session.messagesById[messageId] || session.missingMessageIds[messageId]) continue;
    seen.add(messageId);
    missing.push(messageId);
  }
  return missing;
}

function missingTranscriptEntryIdsForHydration(session: AgentSessionState | undefined): string[] {
  if (!session) return [];
  const seen = new Set<string>();
  const missing: string[] = [];
  for (const eventSeq of session.eventSeqs) {
    const event = session.eventsBySeq[eventSeq];
    if (!isStreamEventEnvelope(event)) continue;
    const entryId = transcriptEntryIdForPayload(asRecord(event.payload));
    if (
      !entryId ||
      seen.has(entryId) ||
      session.transcriptEntriesById[entryId] ||
      session.missingTranscriptEntryIds[entryId]
    ) {
      continue;
    }
    seen.add(entryId);
    missing.push(entryId);
  }
  return missing;
}

function missingBriefIdsForHydration(session: AgentSessionState | undefined): string[] {
  if (!session) return [];
  const seen = new Set<string>();
  const missing: string[] = [];
  for (const eventSeq of session.eventSeqs) {
    const event = session.eventsBySeq[eventSeq];
    if (!isStreamEventEnvelope(event) || event.type !== "brief_created") continue;
    const payload = asRecord(event.payload);
    const briefId = briefIdForPayload(payload);
    if (!briefId || seen.has(briefId) || session.briefRecordsById[briefId] || session.missingBriefIds[briefId]) {
      continue;
    }
    const entryId = transcriptEntryIdForPayload(payload);
    const hydratedByTranscript = entryId ? transcriptEntryText(session.transcriptEntriesById[entryId]) : undefined;
    if (hydratedByTranscript || stringField(payload, "text")) continue;
    seen.add(briefId);
    missing.push(briefId);
  }
  return missing;
}

function mergeHydratedMessagesIntoSession(
  state: RuntimeStoreState,
  agentId: string,
  messages: RuntimeMessageEnvelope[],
  missingMessageIds: string[],
  displayLevel: DisplayLevel,
): Partial<RuntimeStoreState> {
  const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  const messagesById = { ...current.messagesById };
  let changed = false;
  for (const message of messages) {
    const messageId = typeof message.id === "string" && message.id.trim() ? message.id : undefined;
    if (!messageId) continue;
    messagesById[messageId] = message;
    changed = true;
  }

  const missingById = { ...current.missingMessageIds };
  for (const messageId of missingMessageIds) {
    if (!messageId) continue;
    missingById[messageId] = true;
    changed = true;
  }
  if (!changed) return {};

  const events = current.eventSeqs.map((eventSeq) => current.eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
  const timeline = reduceAgentSessionTimeline({
    events: { events },
    eventDisplayLevel: displayLevel,
    messagesById,
    transcriptEntriesById: current.transcriptEntriesById,
    briefRecordsById: current.briefRecordsById,
  });
  const detail = current.detail
    ? {
        ...current.detail,
        timeline,
      }
    : current.detail;

  return {
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...current,
        detail,
        messagesById,
        missingMessageIds: missingById,
      },
    },
  };
}

function mergeHydratedTranscriptEntriesIntoSession(
  state: RuntimeStoreState,
  agentId: string,
  entries: RuntimeTranscriptEntry[],
  missingEntryIds: string[],
  displayLevel: DisplayLevel,
): Partial<RuntimeStoreState> {
  const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  const transcriptEntriesById = { ...current.transcriptEntriesById };
  let changed = false;
  for (const entry of entries) {
    const entryId = typeof entry.id === "string" && entry.id.trim() ? entry.id : undefined;
    if (!entryId) continue;
    transcriptEntriesById[entryId] = entry;
    changed = true;
  }

  const missingById = { ...current.missingTranscriptEntryIds };
  for (const entryId of missingEntryIds) {
    if (!entryId) continue;
    missingById[entryId] = true;
    changed = true;
  }
  if (!changed) return {};

  const events = current.eventSeqs.map((eventSeq) => current.eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
  const timeline = reduceAgentSessionTimeline({
    events: { events },
    eventDisplayLevel: displayLevel,
    messagesById: current.messagesById,
    transcriptEntriesById,
    briefRecordsById: current.briefRecordsById,
  });
  const briefPatch = agentBriefPatchFromEvents(events, transcriptEntriesById, current.briefRecordsById);
  const detail = current.detail
    ? patchAgentDetail(
        {
          ...current.detail,
          timeline,
        },
        undefined,
        briefPatch,
      )
    : current.detail;

  return {
    bootstrap: patchBootstrapAgent(state.bootstrap, agentId, undefined, briefPatch),
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...current,
        detail,
        transcriptEntriesById,
        missingTranscriptEntryIds: missingById,
      },
    },
  };
}

function mergeHydratedBriefRecordsIntoSession(
  state: RuntimeStoreState,
  agentId: string,
  recordsById: Record<string, RuntimeBriefRecord>,
  requestedBriefIds: string[],
  displayLevel: DisplayLevel,
): Partial<RuntimeStoreState> {
  const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  const briefRecordsById = { ...current.briefRecordsById };
  let changed = false;
  for (const [briefId, record] of Object.entries(recordsById)) {
    if (!briefId || !record?.text) continue;
    briefRecordsById[briefId] = record;
    changed = true;
  }

  const missingById = { ...current.missingBriefIds };
  for (const briefId of requestedBriefIds) {
    if (!briefId || recordsById[briefId]) continue;
    missingById[briefId] = true;
    changed = true;
  }
  if (!changed) return {};

  const events = current.eventSeqs.map((eventSeq) => current.eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
  const timeline = reduceAgentSessionTimeline({
    events: { events },
    eventDisplayLevel: displayLevel,
    messagesById: current.messagesById,
    transcriptEntriesById: current.transcriptEntriesById,
    briefRecordsById,
  });
  const briefPatch = agentBriefPatchFromEvents(events, current.transcriptEntriesById, briefRecordsById);
  const detail = current.detail
    ? patchAgentDetail(
        {
          ...current.detail,
          timeline,
          briefRecordsById,
        },
        undefined,
        briefPatch,
      )
    : current.detail;

  return {
    bootstrap: patchBootstrapAgent(state.bootstrap, agentId, undefined, briefPatch),
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...current,
        detail,
        briefRecordsById,
        missingBriefIds: missingById,
      },
    },
  };
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

function patchBootstrapAgent(
  bootstrap: RuntimeBootstrap,
  agentId: string,
  runPatch: Pick<AgentSummary, "currentRunId" | "lifecycle"> | undefined,
  briefPatch: Pick<AgentSummary, "lastBrief" | "lastTurnTime"> | undefined,
): RuntimeBootstrap {
  if (!runPatch && !briefPatch) return bootstrap;
  return {
    ...bootstrap,
    agents: bootstrap.agents.map((agent) => (agent.id === agentId ? { ...agent, ...runPatch, ...briefPatch } : agent)),
  };
}

function patchAgentDetail(
  detail: AgentDetail | null,
  runPatch: Pick<AgentSummary, "currentRunId" | "lifecycle"> | undefined,
  briefPatch: Pick<AgentSummary, "lastBrief" | "lastTurnTime"> | undefined,
): AgentDetail | null {
  if (!detail || (!runPatch && !briefPatch)) return detail;
  return {
    ...detail,
    agent: {
      ...detail.agent,
      ...runPatch,
      ...briefPatch,
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
    events: { events: pageEvents },
    eventDisplayLevel: displayLevel,
    messagesById: current.messagesById,
    transcriptEntriesById: current.transcriptEntriesById,
    briefRecordsById: current.briefRecordsById,
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

function formatTime(value: string | null | undefined): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  return new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" }).format(date);
}
