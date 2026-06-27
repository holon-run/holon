import { create } from "zustand";

import { createRuntimeClient, type AgentEventStreamSubscription, type StreamEventEnvelopeDto } from "./client";
import { cacheClearRemote } from "./idb-cache";
import {
  currentRemoteKey,
  hydrateAllSessions,
  initSessionCache,
  SessionCacheWriter,
} from "./session-cache";
import type { AgentSessionState as AgentSessionStateBase } from "./runtime-store-helpers";
import {
  compactAgentTimelineItems,
  mergeAgentTimelineItems,
  reduceAgentSessionTimeline,
  briefIdForPayload,
  transcriptEntryIdForPayload,
} from "./session-reducer";
import type {
  AddSkillInput,
  AgentDetail,
  AgentSummary,
  AgentTimelineActivity,
  AgentTimelineItem,
  DisplayLevel,
  MemorySourceContent,
  RightPanelView,
  RouteKey,
  RuntimeBootstrap,
  RuntimeConnectionConfig,
  TaskSummary,
  RuntimeConnectionProfile,
  RuntimeConfigState,
  CodexDeviceLoginState,
  CredentialProfileStatus,
  CredentialStoreState,
  RuntimeBriefRecord,
  RuntimeTaskOutputResult,
  RuntimeMessageEnvelope,
  RuntimeModelCatalog,
  RuntimeSearchOptions,
  SkillCatalogState,
  SkillDetailState,
  RuntimeTranscriptEntry,
  RuntimeToolExecutionRecord,
  WorkItemSummary,
  SearchResponse,
  WorkspaceDirectoryListing,
  WorkspaceFileContent,
} from "./types";

import type { AgentLiveStatus, AgentSessionState, WorkItemDetailState, TaskDetailState } from "./runtime-store-helpers";
export type { AgentLiveStatus, AgentSessionState };

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
    // Tasks come from the live /state endpoint, not the /agents/list bootstrap.
    // When httpAgent.tasks is empty it may mean "no tasks" (from /state) or
    // "tasks not included" (from /agents/list). Only overwrite cached tasks
    // when the HTTP source actually carries task data.
    tasks: httpAgent.tasks?.length ? httpAgent.tasks : (cachedAgent.tasks ?? []),
    activeTaskCount: httpAgent.tasks?.length ? httpAgent.activeTaskCount : Math.max(cachedAgent.activeTaskCount ?? 0, httpAgent.activeTaskCount ?? 0),
    waitingCount: Math.max(httpAgent.waitingCount, cachedAgent.waitingCount),
    pending: Math.max(httpAgent.pending, cachedAgent.pending),
    workspaceSummary: cachedAgent.workspaceSummary ?? httpAgent.workspaceSummary,
    // attachedWorkspaces come from the /state endpoint, not /agents/list bootstrap.
    // Bootstrap only includes the active workspace as fallback; preserve cached
    // entries when the HTTP source carries fewer workspaces than the cache.
    attachedWorkspaces: (httpAgent.attachedWorkspaces?.length ?? 0) >= (cachedAgent.attachedWorkspaces?.length ?? 0)
      ? httpAgent.attachedWorkspaces
      : cachedAgent.attachedWorkspaces,
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

export interface AgentRosterActivity {
  operatorAt?: string;
  briefAt?: string;
  unreadCount?: number;
  lastUnreadSeq?: number;
  lastReadSeq?: number;
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
  selectedSkillId: string;
  displayLevel: DisplayLevel;
  displayLevelsByAgentId: Record<string, DisplayLevel>;
  rightPanelOpen: boolean;
  rightPanelView?: RightPanelView;
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
  skillCatalog: SkillCatalogState;
  skillCatalogLoading: boolean;
  skillCatalogError?: string;
  skillDetailById: Record<string, SkillDetailState>;
  skillDetailLoadingById: Record<string, boolean>;
  skillDetailErrorById: Record<string, string | undefined>;
  agentSkillCatalogByAgentId: Record<string, SkillCatalogState>;
  agentSkillCatalogLoadingByAgentId: Record<string, boolean>;
  agentSkillCatalogErrorByAgentId: Record<string, string | undefined>;
  credentialStore: CredentialStoreState;
  credentialStoreLoading: boolean;
  credentialStoreError?: string;
  codexDeviceLogin: CodexDeviceLoginState;
  search: SearchResponse | null;
  searchLoading: boolean;
  searchError?: string;
  searchResultContentBySourceRef: Record<string, MemorySourceContent>;
  searchResultContentLoadingBySourceRef: Record<string, boolean>;
  searchResultContentErrorBySourceRef: Record<string, string | undefined>;
  rosterActivityByAgentId: Record<string, AgentRosterActivity>;
  sessionsByAgentId: Record<string, AgentSessionState>;
  skillInstallJobs: SkillInstallJob[];

  setRoute: (route: RouteKey) => void;
  openAgent: (agentId: string, targetEventSeq?: number) => void;
  openSkill: (skillId: string) => void;
  setDisplayLevel: (displayLevel: DisplayLevel, agentId?: string) => void;
  setRightPanelOpen: (open: boolean) => void;
  showAgentOverview: (agentId?: string) => void;
  showWorkItemDetail: (agentId: string, workItem: WorkItemSummary) => void;
  showTaskDetail: (agentId: string, task: TaskSummary) => void;
  inspectActivity: (agentId: string, activity: AgentTimelineActivity) => void;
  showFileBrowser: (agentId: string, workspaceId: string, initialPath?: string, executionRootId?: string) => void;
  browseWorkspaceDir: (workspaceId: string, path?: string, executionRootId?: string) => Promise<WorkspaceDirectoryListing>;
  readWorkspaceFile: (workspaceId: string, path: string, executionRootId?: string) => Promise<WorkspaceFileContent>;
  workspaceFileUrl: (workspaceId: string, path: string, download?: boolean, executionRootId?: string) => string;
  toggleRightPanel: () => void;
  toggleNavCollapsed: () => void;
  setRuntimeConnection: (config: RuntimeConnectionConfig) => Promise<void>;
  refreshBootstrap: (options?: BootstrapRefreshOptions) => Promise<void>;
  refreshModelCatalog: () => Promise<void>;
  refreshRuntimeConfig: () => Promise<void>;
  updateRuntimeConfig: (updates: Array<{ key: string; value?: unknown; unset?: boolean }>) => Promise<RuntimeConfigState | undefined>;
  refreshSkillCatalog: () => Promise<void>;
  refreshSkillDetail: (skillId: string | undefined) => Promise<void>;
  addSkillToCatalog: (input: AddSkillInput) => Promise<boolean>;
  removeSkillFromCatalog: (name: string) => Promise<boolean>;
  updateSkillCatalog: (name?: string) => Promise<boolean>;
  checkSkillCatalog: (name?: string) => Promise<boolean>;
  refreshAgentSkillCatalog: (agentId: string | undefined) => Promise<void>;
  enableAgentSkill: (agentId: string | undefined, name: string) => Promise<boolean>;
  disableAgentSkill: (agentId: string | undefined, name: string) => Promise<boolean>;
  refreshCredentialStore: () => Promise<void>;
  setCredential: (profile: string, kind: string, material: string) => Promise<CredentialProfileStatus | undefined>;
  deleteCredential: (profile: string) => Promise<void>;
  startCodexDeviceLogin: () => Promise<void>;
  clearCodexDeviceLogin: () => void;
  runSearch: (query: string, options?: RuntimeSearchOptions) => Promise<void>;
  loadSearchResultContent: (sourceRef: string) => Promise<void>;
  refreshAgentDetail: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  refreshAgentWorkItems: (agentId: string | undefined) => Promise<void>;
  refreshAgentState: (agentId: string | undefined) => Promise<void>;
  loadAgentWorkItemDetail: (agentId: string | undefined, workItemId: string | undefined) => Promise<void>;
  loadAgentTaskDetail: (agentId: string | undefined, taskId: string | undefined) => Promise<void>;
  loadOlderAgentEvents: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  sendOperatorPrompt: (agentId: string | undefined, text: string, displayLevel: DisplayLevel) => Promise<void>;
  setAgentModel: (agentId: string | undefined, model: string, displayLevel: DisplayLevel, reasoningEffort?: string) => Promise<void>;
  clearAgentModel: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  startAgentEventStream: (agentId: string | undefined, displayLevel: DisplayLevel) => void;
  stopAgentEventStream: (agentId: string | undefined) => void;
  startGlobalEventStream: () => void;
  stopGlobalEventStream: () => void;
  registerAgentForEvents: (agentId: string) => void;
  unregisterAgentForEvents: (agentId: string) => void;
}

const LEGACY_RUNTIME_CONNECTION_STORAGE_KEY = "holon.webGui.runtimeConnection.v1";
const ACTIVE_RUNTIME_CONNECTION_STORAGE_KEY = "holon.webGui.activeRuntimeConnection.v1";
const RUNTIME_CONNECTION_PROFILES_STORAGE_KEY = "holon.webGui.runtimeConnectionProfiles.v1";
const DISPLAY_LEVEL_STORAGE_KEY = "holon.webGui.displayLevelsByAgentId.v1";
const ROSTER_ACTIVITY_STORAGE_KEY = "holon.webGui.rosterActivityByRemote.v1";
let runtimeConnectionConfig = readStoredRuntimeConnectionConfig();
let runtimeClient = createRuntimeClient(runtimeClientOptions(runtimeConnectionConfig));
const activeEventStreams = new Map<string, AgentEventStreamSubscription>();
export interface SkillInstallJob {
  jobId: string;
  source: string;
  status: "queued" | "running" | "completed" | "failed";
  error?: string;
}

const SKILL_INSTALL_JOBS_STORAGE_KEY = "holon.webGui.skillInstallJobs.v1";

function loadSkillInstallJobs(): SkillInstallJob[] {
  try {
    const raw = localStorage.getItem(SKILL_INSTALL_JOBS_STORAGE_KEY);
    return raw ? (JSON.parse(raw) as SkillInstallJob[]) : [];
  } catch {
    return [];
  }
}

function saveSkillInstallJobs(jobs: SkillInstallJob[]): void {
  try {
    const active = jobs.filter((j) => j.status === "queued" || j.status === "running");
    if (active.length) {
      localStorage.setItem(SKILL_INSTALL_JOBS_STORAGE_KEY, JSON.stringify(active));
    } else {
      localStorage.removeItem(SKILL_INSTALL_JOBS_STORAGE_KEY);
    }
  } catch {
    // localStorage unavailable; state is in-memory only
  }
}

const pendingStreamEvents = new Map<string, StreamEventEnvelopeDto[]>();
const streamFlushTimers = new Map<string, number>();
const reconnectTimers = new Map<string, number>();
const staleTimers = new Map<string, number>();
let globalEventStream: AgentEventStreamSubscription | undefined;
let globalStreamReconnectTimer: number | undefined;
let globalStreamStaleTimer: number | undefined;
let globalStreamReconnectAttempt = 0;
const globalStreamSubscribedAgents = new Set<string>();
const agentLastSeenSeq = new Map<string, number>();
const backfillInFlight = new Set<string>();
const messageHydrationInFlight = new Map<string, Set<string>>();
const transcriptHydrationInFlight = new Map<string, Set<string>>();
const briefHydrationInFlight = new Map<string, Set<string>>();
const inspectorDetailInFlight = new Set<string>();
const workItemRefreshInFlight = new Set<string>();
const workItemDetailInFlight = new Set<string>();
const taskDetailInFlight = new Set<string>();
const agentStateRefreshInFlight = new Set<string>();
let bootstrapRefreshInFlight: Promise<void> | undefined;
let bootstrapRefreshTimer: number | undefined;
const STREAM_FLUSH_INTERVAL_MS = 100;
const STREAM_STALE_TIMEOUT_MS = 45_000;
const STREAM_RECONNECT_BASE_MS = 1_000;
const STREAM_RECONNECT_MAX_MS = 15_000;
const GLOBAL_STREAM_STALE_TIMEOUT_MS = 45_000;
const GLOBAL_BACKFILL_LIMIT = 100;

// ─── Session cache (IndexedDB persistence) ──────────────────────────
let sessionCacheWriter: SessionCacheWriter | null = null;
let sessionCacheInitPromise: Promise<void> | null = null;

function runtimeClientOptions(config: RuntimeConnectionConfig) {
  return config.mode === "remote"
    ? { mode: "remote" as const, baseUrl: config.baseUrl, token: config.token }
    : { mode: "local" as const, token: config.token };
}

export function readStoredRuntimeConnectionConfig(): RuntimeConnectionConfig {
  if (typeof window === "undefined") return { mode: "local" };
  const activeConfig = coerceRuntimeConnectionConfig(readStoredJson(window.sessionStorage, ACTIVE_RUNTIME_CONNECTION_STORAGE_KEY));
  if (activeConfig) return remoteConfigAllowed(activeConfig) ? withStoredRemoteProfileToken(activeConfig) : { mode: "local" };

  const legacyConfig = coerceRuntimeConnectionConfig(readStoredJson(window.localStorage, LEGACY_RUNTIME_CONNECTION_STORAGE_KEY));
  if (legacyConfig?.mode === "remote") {
    if (!canUseRemoteRuntimeConnections()) return { mode: "local" };
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
    const allowedConfig = remoteConfigAllowed(config) ? config : { mode: "local" as const };
    writeActiveRuntimeConnectionConfig(allowedConfig);
    if (allowedConfig.mode === "remote") writeStoredRemoteProfile(allowedConfig);
  } catch {
    // Ignore storage failures; the in-memory connection still applies.
  }
}

function coerceRuntimeConnectionConfig(value: unknown): RuntimeConnectionConfig | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const parsed = value as Partial<RuntimeConnectionConfig>;
  if (parsed.mode === "local") {
    return {
      mode: "local",
      token: typeof parsed.token === "string" && parsed.token.trim() ? parsed.token.trim() : undefined,
    };
  }
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

export function readStoredRemoteConnectionProfiles(): RuntimeConnectionProfile[] {
  if (!canUseRemoteRuntimeConnections()) return [];
  return Object.values(readStoredRemoteProfiles())
    .filter((profile): profile is RuntimeConnectionConfig & { mode: "remote"; baseUrl: string } => profile.mode === "remote" && Boolean(profile.baseUrl))
    .map((profile) => ({
      baseUrl: profile.baseUrl,
      hasToken: Boolean(profile.token),
    }))
    .sort((left, right) => left.baseUrl.localeCompare(right.baseUrl));
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

function remoteConfigAllowed(config: RuntimeConnectionConfig): boolean {
  return config.mode !== "remote" || canUseRemoteRuntimeConnections();
}

export function canUseRemoteRuntimeConnections(): boolean {
  if (typeof window === "undefined") return false;
  return isLoopbackWebHostname(window.location?.hostname);
}

export function isLoopbackWebHostname(hostname: string | undefined): boolean {
  if (!hostname) return false;
  const normalized = hostname.trim().toLowerCase().replace(/^\[(.*)\]$/, "$1");
  if (!normalized) return false;
  if (normalized === "localhost" || normalized.endsWith(".localhost")) return true;
  if (normalized === "::1") return true;
  return /^127(?:\.\d{1,3}){3}$/.test(normalized);
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

export function readStoredRosterActivity(remoteKey: string): Record<string, AgentRosterActivity> {
  if (typeof window === "undefined") return {};
  try {
    const parsed = readStoredJson(window.localStorage, ROSTER_ACTIVITY_STORAGE_KEY);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const byRemote = parsed as Record<string, unknown>;
    const rawActivity = byRemote[remoteKey];
    if (!rawActivity || typeof rawActivity !== "object" || Array.isArray(rawActivity)) return {};
    const activityByAgentId: Record<string, AgentRosterActivity> = {};
    for (const [agentId, value] of Object.entries(rawActivity)) {
      if (typeof agentId !== "string" || !agentId || !value || typeof value !== "object" || Array.isArray(value)) continue;
      const activity = coerceRosterActivity(value);
      if (activity) activityByAgentId[agentId] = activity;
    }
    return activityByAgentId;
  } catch {
    return {};
  }
}

function writeStoredRosterActivity(remoteKey: string, activityByAgentId: Record<string, AgentRosterActivity>): void {
  if (typeof window === "undefined") return;
  try {
    const parsed = readStoredJson(window.localStorage, ROSTER_ACTIVITY_STORAGE_KEY);
    const byRemote =
      parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? (parsed as Record<string, Record<string, AgentRosterActivity>>)
        : {};
    byRemote[remoteKey] = activityByAgentId;
    window.localStorage.setItem(ROSTER_ACTIVITY_STORAGE_KEY, JSON.stringify(byRemote));
  } catch {
    // Ignore storage failures; unread state falls back to memory-only.
  }
}

function coerceRosterActivity(value: unknown): AgentRosterActivity | undefined {
  const parsed = value as Partial<AgentRosterActivity>;
  const activity: AgentRosterActivity = {};
  if (typeof parsed.operatorAt === "string") activity.operatorAt = parsed.operatorAt;
  if (typeof parsed.briefAt === "string") activity.briefAt = parsed.briefAt;
  if (typeof parsed.unreadCount === "number" && Number.isFinite(parsed.unreadCount) && parsed.unreadCount > 0) {
    activity.unreadCount = Math.floor(parsed.unreadCount);
  }
  if (typeof parsed.lastUnreadSeq === "number" && Number.isFinite(parsed.lastUnreadSeq)) {
    activity.lastUnreadSeq = Math.floor(parsed.lastUnreadSeq);
  }
  if (typeof parsed.lastReadSeq === "number" && Number.isFinite(parsed.lastReadSeq)) {
    activity.lastReadSeq = Math.floor(parsed.lastReadSeq);
  }
  return Object.keys(activity).length ? activity : undefined;
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
      hasToken: Boolean(config.token?.trim()),
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

const emptySkillCatalog: SkillCatalogState = {
  source: "fixture",
  catalog: [],
};

/**
 * Initialize session cache for the current remote and hydrate any cached
 * sessions into the store. Called on initial load and remote switch.
 */
function initSessionCacheForRemote(set: StoreSet): void {
  if (sessionCacheInitPromise) return;
  const remoteKey = currentRemoteKey(runtimeConnectionConfig);

  sessionCacheInitPromise = (async () => {
    const ok = await initSessionCache();
    if (!ok) {
      sessionCacheWriter = null;
      return;
    }

    // Set up writer for this remote.
    sessionCacheWriter?.cancel();
    sessionCacheWriter = new SessionCacheWriter(remoteKey);

    // Hydrate cached sessions into store.
    const cached = await hydrateAllSessions(remoteKey);
    if (Object.keys(cached).length === 0) return;

    set((state) => {
      const sessionsByAgentId = { ...state.sessionsByAgentId };
      for (const [agentId, partial] of Object.entries(cached)) {
        sessionsByAgentId[agentId] = {
          ...emptyAgentSession(),
          ...partial,
        };
      }
      return { sessionsByAgentId };
    });
  })();
}

export const useRuntimeStore = create<RuntimeStoreState>((set, get) => ({
  route: "dashboard",
  selectedAgentId: "",
  selectedSkillId: "",
  displayLevel: "info",
  displayLevelsByAgentId: readStoredDisplayLevels(),
  rightPanelOpen: true,
  rightPanelView: undefined,
  navCollapsed: false,

  bootstrap: pendingBootstrap(runtimeConnectionConfig),
  bootstrapLoading: true,
  modelCatalog: emptyModelCatalog,
  modelCatalogLoading: false,
  runtimeConfig: emptyRuntimeConfig,
  runtimeConfigLoading: false,
  runtimeConfigSaving: false,
  skillCatalog: emptySkillCatalog,
  skillCatalogLoading: false,
  skillDetailById: {},
  skillDetailLoadingById: {},
  skillDetailErrorById: {},
  agentSkillCatalogByAgentId: {},
  agentSkillCatalogLoadingByAgentId: {},
  agentSkillCatalogErrorByAgentId: {},
  search: null,
  searchLoading: false,
  searchResultContentBySourceRef: {},
  searchResultContentLoadingBySourceRef: {},
  searchResultContentErrorBySourceRef: {},
  credentialStore: { profiles: [] },
  credentialStoreLoading: false,
  credentialStoreError: undefined,
  codexDeviceLogin: { status: "idle" as const },
  rosterActivityByAgentId: readStoredRosterActivity(currentRemoteKey(runtimeConnectionConfig)),
  sessionsByAgentId: {},
  skillInstallJobs: loadSkillInstallJobs(),

  setRoute: (route) => set({ route }),
  openSkill: (skillId) => set({ route: "skillDetail", selectedSkillId: skillId }),
  openAgent: (agentId, targetEventSeq) =>
    set((state) => {
      const currentSession = state.sessionsByAgentId[agentId];
      const rosterActivityByAgentId = markAgentRead(
        state.rosterActivityByAgentId,
        agentId,
        currentSession?.newestSeq,
      );
      if (rosterActivityByAgentId !== state.rosterActivityByAgentId) {
        writeStoredRosterActivity(currentRemoteKey(runtimeConnectionConfig), rosterActivityByAgentId);
      }
      return {
        selectedAgentId: agentId,
        route: "agent",
        displayLevel: state.displayLevelsByAgentId[agentId] ?? "info",
        rosterActivityByAgentId,
        sessionsByAgentId:
          targetEventSeq == null
            ? state.sessionsByAgentId
            : {
                ...state.sessionsByAgentId,
                [agentId]: {
                  ...emptyAgentSession(),
                  ...currentSession,
                  targetEventSeq,
                  historyError: undefined,
                },
              },
      };
    }),
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
  setRightPanelOpen: (open) => set({ rightPanelOpen: open }),
  showAgentOverview: (agentId) =>
    set((state) => ({
      rightPanelOpen: true,
      rightPanelView: { kind: "agent_overview", agentId: agentId ?? state.selectedAgentId },
    })),
  showWorkItemDetail: (agentId, workItem) =>
    set({
      rightPanelOpen: true,
      rightPanelView: { kind: "work_item_detail", agentId, workItem },
    }),
  showTaskDetail: (agentId, task) =>
    set({
      rightPanelOpen: true,
      rightPanelView: { kind: "task_detail", agentId, task },
    }),
  showFileBrowser: (agentId, workspaceId, initialPath, executionRootId) =>
    set({
      rightPanelOpen: true,
      rightPanelView: { kind: "file_browser", agentId, workspaceId, initialPath, executionRootId },
    }),
  browseWorkspaceDir: (workspaceId, path, executionRootId) => runtimeClient.browseWorkspaceDir(workspaceId, path, executionRootId),
  readWorkspaceFile: (workspaceId, path, executionRootId) => runtimeClient.readWorkspaceFile(workspaceId, path, executionRootId),
  workspaceFileUrl: (workspaceId, path, download, executionRootId) => runtimeClient.workspaceFileUrl(workspaceId, path, download, executionRootId),
  inspectActivity: (agentId, activity) => {
    set({
      rightPanelOpen: true,
      rightPanelView: { kind: "activity_inspector", agentId, activity },
    });
    hydrateInspectorActivityDetail(get, set, agentId, activity);
  },
  toggleRightPanel: () => set((state) => ({ rightPanelOpen: !state.rightPanelOpen })),
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
        ? canUseRemoteRuntimeConnections()
          ? withStoredRemoteProfileToken({
              mode: "remote",
              baseUrl: normalizedBaseUrl,
              token: config.token?.trim() || retainedToken,
            })
          : { mode: "local", token: config.token?.trim() || undefined }
        : { mode: "local", token: config.token?.trim() || undefined };
    runtimeConnectionConfig = normalizedConfig;
    runtimeClient = createRuntimeClient(runtimeClientOptions(normalizedConfig));
    writeStoredRuntimeConnectionConfig(normalizedConfig);
    bootstrapRefreshInFlight = undefined;
    for (const subscription of activeEventStreams.values()) subscription.close();
    activeEventStreams.clear();
    pendingStreamEvents.clear();
    globalEventStream?.close();
    globalEventStream = undefined;
    if (globalStreamReconnectTimer != null) {
      window.clearTimeout(globalStreamReconnectTimer);
      globalStreamReconnectTimer = undefined;
    }
    if (globalStreamStaleTimer != null) {
      window.clearTimeout(globalStreamStaleTimer);
      globalStreamStaleTimer = undefined;
    }
    globalStreamSubscribedAgents.clear();
    agentLastSeenSeq.clear();
    backfillInFlight.clear();
    globalStreamReconnectAttempt = 0;
    messageHydrationInFlight.clear();
    transcriptHydrationInFlight.clear();
    briefHydrationInFlight.clear();
    inspectorDetailInFlight.clear();
    for (const timer of streamFlushTimers.values()) window.clearTimeout(timer);
    for (const timer of reconnectTimers.values()) window.clearTimeout(timer);
    for (const timer of staleTimers.values()) window.clearTimeout(timer);
    streamFlushTimers.clear();
    reconnectTimers.clear();
    staleTimers.clear();
    // Flush pending cache writes for the old remote before switching.
    sessionCacheWriter?.flush();
    sessionCacheWriter = null;
    sessionCacheInitPromise = null;
    set({
      bootstrap: pendingBootstrap(normalizedConfig),
      bootstrapLoading: true,
      bootstrapError: undefined,
      modelCatalog: emptyModelCatalog,
      modelCatalogError: undefined,
      runtimeConfig: emptyRuntimeConfig,
      runtimeConfigError: undefined,
      skillCatalog: emptySkillCatalog,
      skillCatalogLoading: false,
      skillCatalogError: undefined,
      skillDetailById: {},
      skillDetailLoadingById: {},
      skillDetailErrorById: {},
      agentSkillCatalogByAgentId: {},
      agentSkillCatalogLoadingByAgentId: {},
      agentSkillCatalogErrorByAgentId: {},
      credentialStore: { profiles: [] },
      credentialStoreLoading: false,
      credentialStoreError: undefined,
      codexDeviceLogin: { status: "idle" as const },
      search: null,
      searchError: undefined,
      sessionsByAgentId: {},
      rosterActivityByAgentId: readStoredRosterActivity(currentRemoteKey(normalizedConfig)),
      selectedAgentId: "",
      selectedSkillId: "",
      route: "dashboard",
    });
    await get().refreshBootstrap();
    // Initialize cache for the new remote (async, non-blocking).
    initSessionCacheForRemote(set);
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
        syncGlobalEventRoster(get, set);
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
      if (runtimeConfig.changed && !runtimeConfig.error) {
        set({ modelCatalogLoading: true, modelCatalogError: undefined });
        try {
          const modelCatalog = await runtimeClient.getModels();
          set({ modelCatalog, modelCatalogLoading: false, modelCatalogError: modelCatalog.error });
        } catch (modelError) {
          const message = modelError instanceof Error ? modelError.message : String(modelError);
          set((state) => ({
            modelCatalog: { ...state.modelCatalog, error: message },
            modelCatalogLoading: false,
            modelCatalogError: message,
          }));
        }
      }
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

  refreshSkillCatalog: async () => {
    set({ skillCatalogLoading: true, skillCatalogError: undefined });
    try {
      const skillCatalog = await runtimeClient.getSkillCatalog();
      set({ skillCatalog, skillCatalogLoading: false, skillCatalogError: skillCatalog.error });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        skillCatalog: { ...state.skillCatalog, source: "http", error: message },
        skillCatalogLoading: false,
        skillCatalogError: message,
      }));
    }
  },

  refreshSkillDetail: async (skillId) => {
    if (!skillId) return;
    set((state) => ({
      skillDetailLoadingById: { ...state.skillDetailLoadingById, [skillId]: true },
      skillDetailErrorById: { ...state.skillDetailErrorById, [skillId]: undefined },
    }));
    try {
      // The backend resolves scope from the skill_id prefix automatically.
      const detail = await runtimeClient.getSkillDetail(skillId);
      set((state) => ({
        skillDetailById: { ...state.skillDetailById, [skillId]: detail },
        skillDetailLoadingById: { ...state.skillDetailLoadingById, [skillId]: false },
        skillDetailErrorById: { ...state.skillDetailErrorById, [skillId]: detail.error },
      }));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        skillDetailById: {
          ...state.skillDetailById,
          [skillId]: { source: "http", error: message },
        },
        skillDetailLoadingById: { ...state.skillDetailLoadingById, [skillId]: false },
        skillDetailErrorById: { ...state.skillDetailErrorById, [skillId]: message },
      }));
    }
  },

  addSkillToCatalog: async (input) => {
    set({ skillCatalogError: undefined });
    try {
      const jobId = await runtimeClient.addSkillToCatalog(input);
      const source = "package" in input ? input.package : "path" in input ? input.path : "name" in input ? input.name : "unknown";
      const job: SkillInstallJob = { jobId, source, status: "queued" };
      set((state) => {
        const jobs = [...state.skillInstallJobs, job];
        saveSkillInstallJobs(jobs);
        return { skillInstallJobs: jobs };
      });
      void pollSkillInstallJob(set, get, jobId);
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ skillCatalogError: message });
      return false;
    }
  },

  removeSkillFromCatalog: async (name) => {
    set({ skillCatalogLoading: true, skillCatalogError: undefined });
    try {
      await runtimeClient.removeSkillFromCatalog(name);
      const skillCatalog = await runtimeClient.getSkillCatalog();
      set({ skillCatalog, skillCatalogLoading: false, skillCatalogError: skillCatalog.error });
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        skillCatalog: { ...state.skillCatalog, error: message },
        skillCatalogLoading: false,
        skillCatalogError: message,
      }));
      return false;
    }
  },

  updateSkillCatalog: async (name) => {
    set({ skillCatalogLoading: true, skillCatalogError: undefined });
    try {
      await runtimeClient.updateSkillCatalog(name);
      const skillCatalog = await runtimeClient.getSkillCatalog();
      set({ skillCatalog, skillCatalogLoading: false, skillCatalogError: skillCatalog.error });
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        skillCatalog: { ...state.skillCatalog, error: message },
        skillCatalogLoading: false,
        skillCatalogError: message,
      }));
      return false;
    }
  },

  checkSkillCatalog: async (name) => {
    set({ skillCatalogLoading: true, skillCatalogError: undefined });
    try {
      await runtimeClient.checkSkillCatalog(name);
      const skillCatalog = await runtimeClient.getSkillCatalog();
      set({ skillCatalog, skillCatalogLoading: false, skillCatalogError: skillCatalog.error });
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        skillCatalog: { ...state.skillCatalog, error: message },
        skillCatalogLoading: false,
        skillCatalogError: message,
      }));
      return false;
    }
  },

  refreshAgentSkillCatalog: async (agentId) => {
    if (!agentId) return;
    set((state) => ({
      agentSkillCatalogLoadingByAgentId: {
        ...state.agentSkillCatalogLoadingByAgentId,
        [agentId]: true,
      },
      agentSkillCatalogErrorByAgentId: {
        ...state.agentSkillCatalogErrorByAgentId,
        [agentId]: undefined,
      },
    }));
    try {
      const catalog = await runtimeClient.getSkillCatalog(agentId);
      set((state) => ({
        agentSkillCatalogByAgentId: {
          ...state.agentSkillCatalogByAgentId,
          [agentId]: catalog,
        },
        agentSkillCatalogLoadingByAgentId: {
          ...state.agentSkillCatalogLoadingByAgentId,
          [agentId]: false,
        },
      }));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        agentSkillCatalogLoadingByAgentId: {
          ...state.agentSkillCatalogLoadingByAgentId,
          [agentId]: false,
        },
        agentSkillCatalogErrorByAgentId: {
          ...state.agentSkillCatalogErrorByAgentId,
          [agentId]: message,
        },
      }));
    }
  },

  enableAgentSkill: async (agentId, name) => {
    if (!agentId) return false;
    set((state) => ({
      agentSkillCatalogLoadingByAgentId: {
        ...state.agentSkillCatalogLoadingByAgentId,
        [agentId]: true,
      },
      agentSkillCatalogErrorByAgentId: {
        ...state.agentSkillCatalogErrorByAgentId,
        [agentId]: undefined,
      },
    }));
    try {
      await runtimeClient.enableAgentSkill(agentId, name);
      const catalog = await runtimeClient.getSkillCatalog(agentId);
      set((state) => ({
        agentSkillCatalogByAgentId: {
          ...state.agentSkillCatalogByAgentId,
          [agentId]: catalog,
        },
        agentSkillCatalogLoadingByAgentId: {
          ...state.agentSkillCatalogLoadingByAgentId,
          [agentId]: false,
        },
      }));
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        agentSkillCatalogLoadingByAgentId: {
          ...state.agentSkillCatalogLoadingByAgentId,
          [agentId]: false,
        },
        agentSkillCatalogErrorByAgentId: {
          ...state.agentSkillCatalogErrorByAgentId,
          [agentId]: message,
        },
      }));
      return false;
    }
  },

  disableAgentSkill: async (agentId, name) => {
    if (!agentId) return false;
    set((state) => ({
      agentSkillCatalogLoadingByAgentId: {
        ...state.agentSkillCatalogLoadingByAgentId,
        [agentId]: true,
      },
      agentSkillCatalogErrorByAgentId: {
        ...state.agentSkillCatalogErrorByAgentId,
        [agentId]: undefined,
      },
    }));
    try {
      await runtimeClient.disableAgentSkill(agentId, name);
      const catalog = await runtimeClient.getSkillCatalog(agentId);
      set((state) => ({
        agentSkillCatalogByAgentId: {
          ...state.agentSkillCatalogByAgentId,
          [agentId]: catalog,
        },
        agentSkillCatalogLoadingByAgentId: {
          ...state.agentSkillCatalogLoadingByAgentId,
          [agentId]: false,
        },
      }));
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        agentSkillCatalogLoadingByAgentId: {
          ...state.agentSkillCatalogLoadingByAgentId,
          [agentId]: false,
        },
        agentSkillCatalogErrorByAgentId: {
          ...state.agentSkillCatalogErrorByAgentId,
          [agentId]: message,
        },
      }));
      return false;
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
      const [credentialStore, runtimeConfig, modelCatalog] = await Promise.all([
        runtimeClient.listCredentials(),
        runtimeClient.getRuntimeConfig(),
        runtimeClient.getModels(),
      ]);
      set({
        credentialStore,
        credentialStoreError: undefined,
        runtimeConfig,
        runtimeConfigError: runtimeConfig.error,
        modelCatalog,
        modelCatalogError: modelCatalog.error,
      });
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
      const [credentialStore, runtimeConfig, modelCatalog] = await Promise.all([
        runtimeClient.listCredentials(),
        runtimeClient.getRuntimeConfig(),
        runtimeClient.getModels(),
      ]);
      set({
        credentialStore,
        credentialStoreError: undefined,
        runtimeConfig,
        runtimeConfigError: runtimeConfig.error,
        modelCatalog,
        modelCatalogError: modelCatalog.error,
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ credentialStoreError: message });
    }
  },
  startCodexDeviceLogin: async () => {
    set({ codexDeviceLogin: { status: "starting" } });
    try {
      const resp = await runtimeClient.startCodexDeviceLogin();
      set({
        codexDeviceLogin: {
          status: "waiting",
          verificationUrl: resp.verificationUrl,
          userCode: resp.userCode,
          jobId: resp.jobId,
          expiresAt: resp.expiresAt,
        },
      });

      const jobId = resp.jobId;
      const pollInterval = Math.max((resp.interval ?? 5) * 1000, 3000);
      const expiresAt = resp.expiresAt ? new Date(resp.expiresAt).getTime() : Date.now() + 300_000;

      const poll = async (): Promise<void> => {
        const current = get().codexDeviceLogin;
        if (current.status !== "waiting" || current.jobId !== jobId) return;
        if (Date.now() > expiresAt) {
          set({ codexDeviceLogin: { status: "failed", error: "Device login expired." } });
          return;
        }
        try {
          const job = await runtimeClient.getJob(jobId);
          if (job.status === "completed") {
            const [credentialStore, runtimeConfig, modelCatalog] = await Promise.all([
              runtimeClient.listCredentials(),
              runtimeClient.getRuntimeConfig(),
              runtimeClient.getModels(),
            ]);
            set({
              codexDeviceLogin: { status: "completed" },
              credentialStore,
              credentialStoreError: undefined,
              runtimeConfig,
              runtimeConfigError: runtimeConfig.error,
              modelCatalog,
              modelCatalogError: modelCatalog.error,
            });
            return;
          }
          if (job.status === "failed") {
            set({ codexDeviceLogin: { status: "failed", error: job.error || job.summary || "Device login failed." } });
            return;
          }
        } catch {
          // Transient error — continue polling.
        }
        setTimeout(() => { void poll(); }, pollInterval);
      };

      setTimeout(() => { void poll(); }, pollInterval);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ codexDeviceLogin: { status: "failed", error: message } });
    }
  },
  clearCodexDeviceLogin: () => {
    set({ codexDeviceLogin: { status: "idle" } });
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
      set({
        search,
        searchLoading: false,
        searchResultContentBySourceRef: {},
        searchResultContentLoadingBySourceRef: {},
        searchResultContentErrorBySourceRef: {},
      });
    } catch (error) {
      set({ searchLoading: false, searchError: error instanceof Error ? error.message : String(error) });
    }
  },
  loadSearchResultContent: async (sourceRef) => {
    const trimmed = sourceRef.trim();
    if (!trimmed) return;
    const state = get();
    if (state.searchResultContentBySourceRef[trimmed] || state.searchResultContentLoadingBySourceRef[trimmed]) {
      return;
    }
    set((current) => ({
      searchResultContentLoadingBySourceRef: {
        ...current.searchResultContentLoadingBySourceRef,
        [trimmed]: true,
      },
      searchResultContentErrorBySourceRef: {
        ...current.searchResultContentErrorBySourceRef,
        [trimmed]: undefined,
      },
    }));
    try {
      const content = await runtimeClient.getMemorySource(trimmed);
      set((current) => ({
        searchResultContentBySourceRef: {
          ...current.searchResultContentBySourceRef,
          [trimmed]: content,
        },
        searchResultContentLoadingBySourceRef: {
          ...current.searchResultContentLoadingBySourceRef,
          [trimmed]: false,
        },
      }));
    } catch (error) {
      set((current) => ({
        searchResultContentLoadingBySourceRef: {
          ...current.searchResultContentLoadingBySourceRef,
          [trimmed]: false,
        },
        searchResultContentErrorBySourceRef: {
          ...current.searchResultContentErrorBySourceRef,
          [trimmed]: error instanceof Error ? error.message : String(error),
        },
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
      await loadTargetAgentEventWindow(get, set, agentId, displayLevel);
      scheduleMessageHydration(get, set, agentId, displayLevel);
      scheduleTranscriptHydration(get, set, agentId, displayLevel);
      scheduleBriefHydration(get, set, agentId, displayLevel);
      scheduleCacheWrite(get, agentId);
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

  refreshAgentWorkItems: async (agentId) => {
    if (!agentId || workItemRefreshInFlight.has(agentId)) return;
    workItemRefreshInFlight.add(agentId);
    try {
      const workItems = await runtimeClient.getAgentWorkItems(agentId, { limit: 50 });
      set((state) => mergeAgentWorkItemsIntoState(state, agentId, workItems));
    } catch (error) {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            historyError: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    } finally {
      workItemRefreshInFlight.delete(agentId);
    }
  },

  refreshAgentState: async (agentId) => {
    if (!agentId || agentStateRefreshInFlight.has(agentId)) return;
    agentStateRefreshInFlight.add(agentId);
    try {
      const freshAgent = await runtimeClient.getAgentState(agentId);
      set((state) => mergeAgentStateIntoState(state, agentId, freshAgent));
    } catch {
      // Swallow — state refresh is best-effort; the next full detail refresh will recover.
    } finally {
      agentStateRefreshInFlight.delete(agentId);
    }
  },

  loadAgentWorkItemDetail: async (agentId, workItemId) => {
    if (!agentId || !workItemId) return;
    const key = `${agentId}:${workItemId}`;
    const cached = get().sessionsByAgentId[agentId]?.workItemDetailsById[workItemId];
    if (cached?.workItem || cached?.loading || workItemDetailInFlight.has(key)) return;
    workItemDetailInFlight.add(key);
    setWorkItemDetailState(set, agentId, workItemId, { loading: true, error: undefined });
    try {
      const workItem = await runtimeClient.getAgentWorkItem(agentId, workItemId);
      setWorkItemDetailState(set, agentId, workItemId, { loading: false, workItem });
    } catch (error) {
      setWorkItemDetailState(set, agentId, workItemId, {
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      workItemDetailInFlight.delete(key);
    }
  },

  loadAgentTaskDetail: async (agentId, taskId) => {
    if (!agentId || !taskId) return;
    const key = `${agentId}:${taskId}`;
    const cached = get().sessionsByAgentId[agentId]?.taskDetailsById[taskId];
    if (cached?.output || cached?.loading || taskDetailInFlight.has(key)) return;
    taskDetailInFlight.add(key);
    setTaskDetailState(set, agentId, taskId, { loading: true, error: undefined });
    try {
      const output = await runtimeClient.getTaskOutput(agentId, taskId);
      setTaskDetailState(set, agentId, taskId, { loading: false, output });
    } catch (error) {
      setTaskDetailState(set, agentId, taskId, {
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      taskDetailInFlight.delete(key);
      const selection = get().rightPanelView;
      if (selection?.kind === "task_detail" && selection.agentId === agentId && selection.task.id === taskId) {
        const detail = get().sessionsByAgentId[agentId]?.taskDetailsById[taskId];
        if (detail) {
          set({ rightPanelView: { ...selection, detailState: detail } });
        }
      }
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
      if (rosterActivityByAgentId !== state.rosterActivityByAgentId) {
        writeStoredRosterActivity(currentRemoteKey(runtimeConnectionConfig), rosterActivityByAgentId);
      }
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
  startGlobalEventStream: () => {
    startGlobalEventStream(get, set);
  },
  stopGlobalEventStream: () => {
    stopGlobalEventStream(set);
  },
  registerAgentForEvents: (agentId) => {
    registerAgentForEvents(get, set, agentId);
  },
  unregisterAgentForEvents: (agentId) => {
    unregisterAgentForEvents(agentId);
  },
}));

// Initialize session cache on first load.
if (typeof window !== "undefined") {
  initSessionCacheForRemote((partial) => useRuntimeStore.setState(partial));
}

// Resume polling for any skill install jobs persisted from a previous session.
if (typeof window !== "undefined") {
  for (const job of useRuntimeStore.getState().skillInstallJobs) {
    if (job.status === "queued" || job.status === "running") {
      void pollSkillInstallJob(useRuntimeStore.setState, useRuntimeStore.getState, job.jobId);
    }
  }
}

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
    workItemDetailsById: {},
    taskDetailsById: {},
  };
}

type StoreSet = (
  partial:
    | Partial<RuntimeStoreState>
    | RuntimeStoreState
    | ((state: RuntimeStoreState) => Partial<RuntimeStoreState> | RuntimeStoreState),
  replace?: false,
) => void;

/**
 * Schedule a debounced cache write for the given agent's session.
 * Best-effort: silently skips if the cache writer isn't initialized.
 */
function scheduleCacheWrite(get: () => RuntimeStoreState, agentId: string): void {
  if (!sessionCacheWriter) return;
  const session = get().sessionsByAgentId[agentId];
  if (!session) return;
  sessionCacheWriter.scheduleWrite(agentId, session);
}

// ─── Global event stream ────────────────────────────────────────────

function startGlobalEventStream(get: () => RuntimeStoreState, set: StoreSet): void {
  if (globalEventStream) return;

  const subscription = runtimeClient.streamGlobalEvents({
    onOpen: () => {
      globalStreamReconnectAttempt = 0;
      scheduleGlobalStaleWatchdog(get, set);
      // Backfill all registered agents on (re)connect.
      for (const agentId of globalStreamSubscribedAgents) {
        void backfillAgentEvents(set, agentId);
      }
    },
    onActivity: () => {
      scheduleGlobalStaleWatchdog(get, set);
    },
    onEvent: (event) => {
      scheduleGlobalStaleWatchdog(get, set);
      dispatchGlobalStreamEvent(set, event);
    },
    onClose: () => scheduleGlobalStreamReconnect(get, set, "global event stream closed"),
    onError: (error) => scheduleGlobalStreamReconnect(get, set, error.message),
  });
  if (!subscription) return;
  globalEventStream = subscription;
}

function stopGlobalEventStream(set: StoreSet): void {
  globalEventStream?.close();
  globalEventStream = undefined;
  if (globalStreamReconnectTimer != null) {
    window.clearTimeout(globalStreamReconnectTimer);
    globalStreamReconnectTimer = undefined;
  }
  if (globalStreamStaleTimer != null) {
    window.clearTimeout(globalStreamStaleTimer);
    globalStreamStaleTimer = undefined;
  }
  globalStreamReconnectAttempt = 0;
  // Flush any pending events for all agents.
  for (const agentId of globalStreamSubscribedAgents) {
    flushStreamEvents(set, agentId);
  }
}

function registerAgentForEvents(get: () => RuntimeStoreState, set: StoreSet, agentId: string): void {
  const wasSubscribed = globalStreamSubscribedAgents.has(agentId);
  globalStreamSubscribedAgents.add(agentId);
  // Initialize seq tracking from existing session state.
  const session = wasSubscribed ? undefined : get().sessionsByAgentId[agentId];
  if (session && !agentLastSeenSeq.has(agentId)) {
    const lastSeq = highestSeq(session.eventSeqs) ?? session.newestSeq;
    if (lastSeq != null) {
      agentLastSeenSeq.set(agentId, lastSeq);
    }
  }
  // Start global stream if not running.
  startGlobalEventStream(get, set);
  // Initial backfill from the last known seq.
  if (!wasSubscribed) void backfillAgentEvents(set, agentId);
}

function unregisterAgentForEvents(agentId: string): void {
  globalStreamSubscribedAgents.delete(agentId);
  agentLastSeenSeq.delete(agentId);
}

function syncGlobalEventRoster(get: () => RuntimeStoreState, set: StoreSet): void {
  const agentIds = new Set(get().bootstrap.agents.map((agent) => agent.id));
  for (const agentId of Array.from(globalStreamSubscribedAgents)) {
    if (!agentIds.has(agentId)) unregisterAgentForEvents(agentId);
  }
  for (const agentId of agentIds) {
    registerAgentForEvents(get, set, agentId);
  }
}

function dispatchGlobalStreamEvent(set: StoreSet, event: StreamEventEnvelopeDto): void {
  const agentId = event.agent_id;
  if (!agentId || !globalStreamSubscribedAgents.has(agentId)) return;

  const seq = event.event_seq;
  if (seq != null) {
    const lastSeq = agentLastSeenSeq.get(agentId);
    if (lastSeq != null && seq > lastSeq + 1) {
      // Gap detected — trigger backfill.
      void backfillAgentEvents(set, agentId);
    }
    agentLastSeenSeq.set(agentId, Math.max(seq, lastSeq ?? 0));
  }

  enqueueStreamEvent(set, agentId, event);
  scheduleBootstrapRefresh(useRuntimeStore.getState);
}

async function backfillAgentEvents(set: StoreSet, agentId: string): Promise<void> {
  if (backfillInFlight.has(agentId)) return;
  const afterSeq = agentLastSeenSeq.get(agentId);
  if (afterSeq == null) return; // No seq baseline yet; initial fetch handles this.
  backfillInFlight.add(agentId);
  try {
    let cursor = afterSeq;
    let hasMore = true;
    while (hasMore) {
      const page = await runtimeClient.getAgentEvents(agentId, {
        afterSeq: cursor,
        order: "asc",
        limit: GLOBAL_BACKFILL_LIMIT,
      });
      const events = (page.events ?? []).filter((e) => e.event_seq != null);
      if (!events.length) break;
      // Convert EventEnvelopeDto to StreamEventEnvelopeDto for the reducer.
      const streamEvents: StreamEventEnvelopeDto[] = events.map((e) => ({
        id: e.id,
        event_seq: e.event_seq,
        ts: e.ts,
        agent_id: agentId,
        type: e.type,
        payload: e.payload,
      }));
      applyStreamEvents(set, agentId, streamEvents);
      const maxSeq = events.reduce((max, e) => Math.max(max, e.event_seq!), 0);
      agentLastSeenSeq.set(agentId, Math.max(maxSeq, agentLastSeenSeq.get(agentId) ?? 0));
      cursor = maxSeq;
      hasMore = events.length >= GLOBAL_BACKFILL_LIMIT;
    }
  } catch {
    // Silently ignore backfill errors; the stream will retry.
  } finally {
    backfillInFlight.delete(agentId);
  }
}

function scheduleGlobalStaleWatchdog(get: () => RuntimeStoreState, set: StoreSet): void {
  if (globalStreamStaleTimer != null) window.clearTimeout(globalStreamStaleTimer);
  globalStreamStaleTimer = window.setTimeout(() => {
    if (!globalEventStream) return;
    for (const agentId of globalStreamSubscribedAgents) {
      flushStreamEvents(set, agentId);
    }
    scheduleGlobalStreamReconnect(get, set, "global event stream idle timeout");
  }, GLOBAL_STREAM_STALE_TIMEOUT_MS);
}

function scheduleGlobalStreamReconnect(
  get: () => RuntimeStoreState,
  set: StoreSet,
  reason: string,
): void {
  globalEventStream?.close();
  globalEventStream = undefined;
  if (globalStreamStaleTimer != null) {
    window.clearTimeout(globalStreamStaleTimer);
    globalStreamStaleTimer = undefined;
  }
  if (globalStreamReconnectTimer != null) return;

  globalStreamReconnectAttempt += 1;
  const delay = reconnectDelayMs(globalStreamReconnectAttempt);
  for (const agentId of globalStreamSubscribedAgents) {
    setStreamState(set, agentId, "reconnecting", {
      reconnectAttempt: globalStreamReconnectAttempt,
      error: reason,
    });
  }
  globalStreamReconnectTimer = window.setTimeout(() => {
    globalStreamReconnectTimer = undefined;
    startGlobalEventStream(get, set);
  }, delay);
}

// ─── End global event stream ────────────────────────────────────────

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

function hydrateInspectorActivityDetail(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  activity: AgentTimelineActivity,
): void {
  const refs = inspectorDetailRefs(activity);
  if (!refs.toolExecutionId && !refs.taskId) return;

  const key = `${agentId}:${activity.id}:${refs.toolExecutionId ?? ""}:${refs.taskId ?? ""}`;
  if (inspectorDetailInFlight.has(key)) return;
  inspectorDetailInFlight.add(key);
  setInspectorActivityDetailState(set, agentId, activity.id, { loading: true });

  void Promise.all([
    refs.toolExecutionId ? runtimeClient.getToolExecution(agentId, refs.toolExecutionId) : Promise.resolve(undefined),
    refs.taskId ? runtimeClient.getTaskOutput(agentId, refs.taskId) : Promise.resolve(undefined),
  ])
    .then(([toolExecution, taskOutput]) => {
      setInspectorActivityDetailState(set, agentId, activity.id, {
        loading: false,
        toolExecution,
        taskOutput,
      });
    })
    .catch((error) => {
      setInspectorActivityDetailState(set, agentId, activity.id, {
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    })
    .finally(() => {
      inspectorDetailInFlight.delete(key);
      const selection = get().rightPanelView;
      if (selection?.kind === "activity_inspector" && selection.agentId === agentId && selection.activity.id === activity.id) {
        set({ rightPanelView: selection });
      }
    });
}

function setInspectorActivityDetailState(
  set: StoreSet,
  agentId: string,
  activityId: string,
  detailState: {
    loading?: boolean;
    error?: string;
    toolExecution?: RuntimeToolExecutionRecord;
    taskOutput?: RuntimeTaskOutputResult;
  },
): void {
  set((state) => {
    const selection = state.rightPanelView;
    if (selection?.kind !== "activity_inspector" || selection.agentId !== agentId || selection.activity.id !== activityId) return {};
    return {
      rightPanelView: {
        ...selection,
        detailState: {
          ...selection.detailState,
          ...detailState,
        },
      },
    };
  });
}

function setWorkItemDetailState(
  set: StoreSet,
  agentId: string,
  workItemId: string,
  detailState: WorkItemDetailState,
): void {
  set((state) => {
    const session = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
    const previous = session.workItemDetailsById[workItemId] ?? {};
    return {
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...session,
          workItemDetailsById: {
            ...session.workItemDetailsById,
            [workItemId]: {
              ...previous,
              ...detailState,
            },
          },
        },
      },
    };
  });
}

function setTaskDetailState(
  set: StoreSet,
  agentId: string,
  taskId: string,
  detailState: TaskDetailState,
): void {
  set((state) => {
    const session = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
    const previous = session.taskDetailsById[taskId] ?? {};
    return {
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...session,
          taskDetailsById: {
            ...session.taskDetailsById,
            [taskId]: {
              ...previous,
              ...detailState,
            },
          },
        },
      },
    };
  });
}

function inspectorDetailRefs(activity: AgentTimelineActivity): { toolExecutionId?: string; taskId?: string } {
  const rawEvent = asRecord(activity.rawEvent);
  const payload = asRecord(rawEvent?.payload) ?? asRecord(activity.rawEvent);
  return {
    toolExecutionId: firstStringField(payload, ["tool_execution_id", "toolExecutionId"]),
    taskId: firstStringField(payload, ["task_id", "taskId"]),
  };
}

function firstStringField(record: Record<string, unknown> | undefined, keys: string[]): string | undefined {
  for (const key of keys) {
    const value = stringField(record, key);
    if (value) return value;
  }
  return undefined;
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

const SKILL_JOB_POLL_INTERVAL_MS = 1_000;
const SKILL_JOB_POLL_TIMEOUT_MS = 180_000;

async function pollSkillInstallJob(
  set: StoreSet,
  get: () => RuntimeStoreState,
  jobId: string,
): Promise<void> {
  const deadline = Date.now() + SKILL_JOB_POLL_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      await new Promise((resolve) => globalThis.setTimeout(resolve, SKILL_JOB_POLL_INTERVAL_MS));
      const job = await runtimeClient.getJob(jobId);
      if (job.status === "completed") {
        removeSkillInstallJob(set, get, jobId);
        await get().refreshSkillCatalog();
        return;
      }
      if (job.status === "failed") {
        updateSkillInstallJob(set, jobId, "failed", job.error || job.summary);
        removeSkillInstallJobAfterDelay(set, get, jobId, 10_000);
        return;
      }
      updateSkillInstallJob(set, jobId, job.status === "running" ? "running" : "queued");
    } catch {
      // Network error — keep retrying until deadline
    }
  }
  updateSkillInstallJob(set, jobId, "failed", "Timed out waiting for skill install.");
  removeSkillInstallJobAfterDelay(set, get, jobId, 10_000);
}

function updateSkillInstallJob(set: StoreSet, jobId: string, status: SkillInstallJob["status"], error?: string): void {
  set((state) => {
    const jobs = state.skillInstallJobs.map((j) => j.jobId === jobId ? { ...j, status, error } : j);
    saveSkillInstallJobs(jobs);
    return { skillInstallJobs: jobs };
  });
}

function removeSkillInstallJob(set: StoreSet, get: () => RuntimeStoreState, jobId: string): void {
  set((state) => {
    const jobs = state.skillInstallJobs.filter((j) => j.jobId !== jobId);
    saveSkillInstallJobs(jobs);
    return { skillInstallJobs: jobs };
  });
}

function removeSkillInstallJobAfterDelay(set: StoreSet, get: () => RuntimeStoreState, jobId: string, delayMs: number): void {
  window.setTimeout(() => removeSkillInstallJob(set, get, jobId), delayMs);
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

function mergeAgentWorkItemsIntoState(state: RuntimeStoreState, agentId: string, workItems: WorkItemSummary[]): Partial<RuntimeStoreState> {
  const session = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  const detail = session.detail
    ? {
        ...session.detail,
        agent: patchAgentWorkItems(session.detail.agent, workItems),
      }
    : session.detail;
  const agents = state.bootstrap.agents.map((agent) => (agent.id === agentId ? patchAgentWorkItems(agent, workItems) : agent));

  return {
    bootstrap: sortBootstrapAgents(
      {
        ...state.bootstrap,
        agents,
        metrics: buildBootstrapMetrics(agents),
      },
      state.rosterActivityByAgentId,
    ),
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

function patchAgentWorkItems(agent: AgentSummary, workItems: WorkItemSummary[]): AgentSummary {
  const currentWork = workItems.find((item) => item.current);
  return {
    ...agent,
    currentWork,
    workItems,
  };
}

function mergeAgentStateIntoState(state: RuntimeStoreState, agentId: string, freshAgent: AgentSummary): Partial<RuntimeStoreState> {
  const session = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  // Preserve cached work items from existing detail — those are managed by
  // refreshAgentWorkItems. Tasks come from the state endpoint and are always trusted.
  const cachedDetail = session.detail;
  const mergedAgent: AgentSummary = cachedDetail
    ? {
        ...freshAgent,
        // Tasks come from the live state endpoint, so always trust fresh data
        // to ensure cancelled/completed tasks are removed promptly.
        tasks: freshAgent.tasks,
        // Work items are managed by a separate endpoint, preserve cached data.
        workItems: cachedDetail.agent.workItems?.length ? cachedDetail.agent.workItems : freshAgent.workItems,
        currentWork: cachedDetail.agent.currentWork ?? freshAgent.currentWork,
        lastBrief: cachedDetail.agent.lastBrief || freshAgent.lastBrief,
      }
    : freshAgent;
  const detail = cachedDetail ? { ...cachedDetail, agent: mergedAgent } : cachedDetail;
  const agents = state.bootstrap.agents.map((agent) => (agent.id === agentId ? mergedAgent : agent));

  return {
    bootstrap: sortBootstrapAgents(
      {
        ...state.bootstrap,
        agents,
        metrics: buildBootstrapMetrics(agents),
      },
      state.rosterActivityByAgentId,
    ),
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

function markAgentRead(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  newestSeq: number | undefined,
): Record<string, AgentRosterActivity> {
  const existing = current[agentId];
  if (!existing?.unreadCount && (newestSeq == null || existing?.lastReadSeq === newestSeq)) return current;
  return {
    ...current,
    [agentId]: {
      ...existing,
      unreadCount: 0,
      lastReadSeq: Math.max(newestSeq ?? 0, existing?.lastUnreadSeq ?? 0, existing?.lastReadSeq ?? 0),
    },
  };
}

export function touchRosterActivityFromEvent(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  event: StreamEventEnvelopeDto,
  selectedAgentId: string,
): Record<string, AgentRosterActivity> {
  let next = current;
  if (event.type === "brief_created") {
    next = touchRosterActivity(next, agentId, "brief", eventTimestamp(event));
  }
  if (event.type === "message_enqueued" && messageOrigin(event.payload) === "operator") {
    next = touchRosterActivity(next, agentId, "operator", eventTimestamp(event));
  }
  if (isUnreadEvent(event) && agentId !== selectedAgentId) {
    next = incrementUnreadFromEvent(next, agentId, event);
  }
  return next;
}

function isUnreadEvent(event: StreamEventEnvelopeDto): boolean {
  if (event.type === "brief_created") return true;
  return event.type === "message_enqueued" && messageOrigin(event.payload) !== "operator";
}

function incrementUnreadFromEvent(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  event: StreamEventEnvelopeDto,
): Record<string, AgentRosterActivity> {
  const existing = current[agentId];
  const seq = event.event_seq;
  if (seq != null && existing?.lastReadSeq != null && seq <= existing.lastReadSeq) return current;
  if (seq != null && existing?.lastUnreadSeq != null && seq <= existing.lastUnreadSeq) return current;
  return {
    ...current,
    [agentId]: {
      ...existing,
      unreadCount: (existing?.unreadCount ?? 0) + 1,
      lastUnreadSeq: seq ?? existing?.lastUnreadSeq,
    },
  };
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
  if ((page.events ?? []).some(isWorkItemCacheInvalidationEvent)) {
    void useRuntimeStore.getState().refreshAgentWorkItems(agentId);
  }
  if ((page.events ?? []).some(isAgentStateCacheInvalidationEvent)) {
    void useRuntimeStore.getState().refreshAgentState(agentId);
  }
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
      (activityByAgentId, event) =>
        touchRosterActivityFromEvent(activityByAgentId, agentId, event, state.route === "agent" ? state.selectedAgentId : ""),
      state.rosterActivityByAgentId,
    );
    if (rosterActivityByAgentId !== state.rosterActivityByAgentId) {
      writeStoredRosterActivity(currentRemoteKey(runtimeConnectionConfig), rosterActivityByAgentId);
    }
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
  scheduleCacheWrite(useRuntimeStore.getState, agentId);
  if (events.some(isWorkItemCacheInvalidationEvent)) {
    void useRuntimeStore.getState().refreshAgentWorkItems(agentId);
  }
  if (events.some(isAgentStateCacheInvalidationEvent)) {
    void useRuntimeStore.getState().refreshAgentState(agentId);
  }
}

function isWorkItemCacheInvalidationEvent(event: StreamEventEnvelopeDto): boolean {
  if (event.type !== "work_item_written") return false;
  const action = stringField(asRecord(event.payload), "action");
  return action === "created" || action === "completed";
}

function isAgentStateCacheInvalidationEvent(event: StreamEventEnvelopeDto): boolean {
  return (
    event.type === "agent_state_changed" ||
    event.type === "state_changed" ||
    event.type === "work_item_written" ||
    event.type === "task_created" ||
    event.type === "task_status_updated" ||
    event.type === "task_result_received"
  );
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

async function loadTargetAgentEventWindow(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  displayLevel: DisplayLevel,
): Promise<void> {
  const session = get().sessionsByAgentId[agentId];
  const targetEventSeq = session?.targetEventSeq;
  if (targetEventSeq == null || session?.eventsBySeq[targetEventSeq]) return;

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
      afterSeq: targetEventSeq - 1,
      limit: 80,
      order: "asc",
      displayLevel,
    });
    set((state) =>
      mergeEventPageIntoSession(state, agentId, page.events ?? [], page.oldest_seq, page.has_older, displayLevel, {
        newestSeq: page.cursor_seq ?? page.newest_seq,
        append: true,
      }),
    );
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
  }
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
