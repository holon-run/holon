import { create } from "zustand";

import {
  createRuntimeClient,
  isProjectionBusyError,
  type AgentEventStreamSubscription,
  type OperatorPromptAttachment,
  type StreamEventEnvelopeDto,
} from "./client";
import { EventGapRecoveryTracker, recoverEventGap } from "./event-gap-recovery";
import { cacheClearRemote } from "./idb-cache";
import { ResumeReconciliationCoordinator } from "./resume-reconciliation";
import {
  currentRemoteKey,
  hydrateAgentSession,
  hydrateAllSessions,
  initSessionCache,
  SessionCacheWriter,
} from "./session-cache";
import {
  createRuntimeTrace,
  installRuntimeTraceDebugApi,
  startRuntimeSpan,
  type RuntimeTraceContext,
} from "./runtime-trace";
import type { AgentSessionState as AgentSessionStateBase } from "./runtime-store-helpers";
import {
  compactAgentTimelineItems,
  briefIdForPayload,
} from "./session-reducer";
import {
  briefIdsForProjectionHydration,
  createSessionProjectionState,
  deriveSessionTimeline,
  eventIdentityConflicts,
  messageIdsForProjectionHydration,
  projectionEvents,
  reduceSessionProjection,
  transcriptEntryIdsForProjectionHydration,
  type SessionProjectionAction,
  type SessionProjectionState,
} from "./session-projection";
import { canApplySessionEvent } from "./session-events";
import type {
  AddSkillInput,
  AgentDetail,
  AgentTemplateCatalogDiagnostic,
  AgentSummary,
  AgentTemplateCatalogState,
  AgentTemplateDetailState,
  AgentTimelineActivity,
  AgentTimelineItem,
  TimelineStateObjectRef,
  DisplayLevel,
  MemorySourceContent,
  RightPanelView,
  RouteKey,
  RuntimeBootstrap,
  RuntimeConnectionConfig,
  TaskSummary,
  RuntimeConnectionProfile,
  RuntimeConfigState,
  TaskStatusSnapshot,
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
  ToolExecutionArtifactContent,
} from "./types";

import type { AgentLiveStatus, AgentSessionState, WorkItemDetailState, TaskDetailState, ToolExecutionDetailState } from "./runtime-store-helpers";
export type { AgentLiveStatus, AgentSessionState };

export interface BootstrapRefreshOptions {
  background?: boolean;
  syncEvents?: boolean;
  trace?: RuntimeTraceContext;
  trigger?: string;
}

export interface AgentDetailRefreshOptions {
  force?: boolean;
  trace?: RuntimeTraceContext;
  trigger?: string;
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

export function mergeBootstrapAgentState(httpAgent: AgentSummary, cachedAgent: AgentSummary): AgentSummary {
  return {
    ...httpAgent,
    currentWork: cachedAgent.currentWork ?? httpAgent.currentWork,
    workItems: cachedAgent.workItems?.length ? cachedAgent.workItems : httpAgent.workItems,
    // Tasks come from the live /state endpoint, not the /agents/list bootstrap.
    // When httpAgent.tasks is empty it may mean "no tasks" (from /state) or
    // "tasks not included" (from /agents/list). Only overwrite cached tasks
    // when the HTTP source actually carries task data.
    tasks: httpAgent.tasks?.length ? httpAgent.tasks : (cachedAgent.tasks ?? []),
    // Trust HTTP state for workspace — it changes via UseWorkspace and must reflect fresh data.
    workspaceSummary: httpAgent.workspaceSummary ?? cachedAgent.workspaceSummary,
    // Unified workspaces array from /state endpoint; fall back to cache when
    // the HTTP source is stale (e.g., bootstrap list without full state).
    attachedWorkspaces: httpAgent.attachedWorkspaces?.length
      ? httpAgent.attachedWorkspaces
      : cachedAgent.attachedWorkspaces,
  };
}

function mergeNewerLiveAgentState(httpAgent: AgentSummary, cachedAgent: AgentSummary): AgentSummary {
  const merged = mergeBootstrapAgentState(httpAgent, cachedAgent);
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
    agentsById[agent.id] = agentsById[agent.id] ? mergeBootstrapAgentState(agentsById[agent.id], agent) : agent;
  }
  return agentsById;
}

/**
 * Build a provisional AgentDetail from IndexedDB-cached events when the real
 * HTTP detail hasn't loaded yet. This eliminates the blank/flash period when
 * switching agents that have cached data.
 */
function rebuildProvisionalDetailsWithAgents(
  agents: AgentSummary[],
  sessionsByAgentId: Record<string, AgentSessionState>,
): Record<string, AgentSessionState> | null {
  const agentsById = Object.fromEntries(agents.map((agent) => [agent.id, agent]));
  let changed = false;
  const updated = { ...sessionsByAgentId };
  for (const [agentId, session] of Object.entries(updated)) {
    if (session.detail || session.eventSeqs.length === 0) continue;
    const agent = agentsById[agentId];
    if (!agent) continue;
    const events = session.eventSeqs
      .map((seq) => session.eventsBySeq[seq])
      .filter(isStreamEventEnvelope);
    if (events.length === 0) continue;
    updated[agentId] = {
      ...session,
      detail: {
        agent,
        timeline: deriveSessionTimeline(session, "debug"),
        source: "http",
        events,
        newestEventSeq: highestSeq(session.eventSeqs),
        oldestEventSeq: session.eventSeqs[0],
      },
    };
    changed = true;
  }
  return changed ? updated : null;
}

export interface AgentRosterActivity {
  operatorAt?: string;
  briefAt?: string;
  unreadCount?: number;
  lastUnreadSeq?: number;
  lastReadSeq?: number;
}

const OPTIMISTIC_OPERATOR_PROMPT_SOURCE = "pending-operator-prompt";
const OPTIMISTIC_OPERATOR_CLIENT_PREFIX = "operator-prompt-client:";
const OPTIMISTIC_OPERATOR_MESSAGE_PREFIX = "operator-prompt-message:";

function appendOptimisticOperatorPrompt(
  detail: AgentDetail | null,
  agent: AgentSummary | undefined,
  prompt: string,
  clientId: string,
): AgentDetail | null {
  const baseDetail = detail ?? createLiveAgentDetail(agent);
  if (!baseDetail) return null;
  const timestamp = new Date().toISOString();
  return {
    ...baseDetail,
    timeline: [
      ...baseDetail.timeline,
      {
        id: `operator-prompt:pending:${clientId}`,
        kind: "operator",
        label: "Operator input",
        body: prompt,
        timestamp,
        meta: "sending",
        minDisplayLevel: "info",
        sourceIds: [OPTIMISTIC_OPERATOR_PROMPT_SOURCE, `${OPTIMISTIC_OPERATOR_CLIENT_PREFIX}${clientId}`],
      },
    ],
  };
}

function confirmOptimisticOperatorPrompt(
  detail: AgentDetail | null,
  clientId: string,
  messageId: string,
): AgentDetail | null {
  if (!detail) return detail;
  let changed = false;
  const timeline = detail.timeline.map((item) => {
    if (
      item.kind !== "operator" ||
      !item.sourceIds.includes(`${OPTIMISTIC_OPERATOR_CLIENT_PREFIX}${clientId}`)
    ) {
      return item;
    }
    changed = true;
    return {
      ...item,
      meta: "Sent",
      sourceIds: [...item.sourceIds, `${OPTIMISTIC_OPERATOR_MESSAGE_PREFIX}${messageId}`],
    };
  });
  return changed ? { ...detail, timeline } : detail;
}

export interface RuntimeStoreState {
  route: RouteKey;
  selectedAgentId: string;
  selectedSkillId: string;
  selectedTemplateId: string;
  displayLevel: DisplayLevel;
  displayLevelsByAgentId: Record<string, DisplayLevel>;
  rightPanelViewStack: RightPanelView[];
  rightPanelOpen: boolean;
  rightPanelView?: RightPanelView;
  navCollapsed: boolean;

  bootstrap: RuntimeBootstrap;
  bootstrapLoading: boolean;
  bootstrapError?: string;
  globalStreamStatus: "idle" | "connecting" | "streaming" | "reconnecting";
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
  templateCatalog: AgentTemplateCatalogState;
  templateCatalogLoading: boolean;
  templateCatalogError?: string;
  dismissedTemplateDiagnostics: string[];
  templateSyncInProgress: boolean;
  templateSyncMessage?: string;
  templateDetailById: Record<string, AgentTemplateDetailState>;
  templateDetailLoadingById: Record<string, boolean>;
  templateDetailErrorById: Record<string, string | undefined>;
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
  resumeRevision: number;

  setRoute: (route: RouteKey) => void;
  openAgent: (agentId: string, targetEventSeq?: number) => void;
  openSkill: (skillId: string) => void;
  openTemplate: (catalogId: string) => void;
  setDisplayLevel: (displayLevel: DisplayLevel, agentId?: string) => void;
  setRightPanelOpen: (open: boolean) => void;
  showAgentOverview: (agentId?: string) => void;
  showWorkItemDetail: (agentId: string, workItem: WorkItemSummary) => void;
  showTaskDetail: (agentId: string, task: TaskSummary) => void;
  showToolExecutionDetail: (agentId: string, toolExecutionId: string, toolName?: string, relatedStateObjectRef?: TimelineStateObjectRef) => void;
  inspectActivity: (agentId: string, activity: AgentTimelineActivity) => void;
  showFileBrowser: (agentId: string, workspaceId: string, initialPath?: string, executionRootId?: string, initialFilePath?: string) => void;
  browseWorkspaceDir: (workspaceId: string, path?: string, executionRootId?: string) => Promise<WorkspaceDirectoryListing>;
  readWorkspaceFile: (workspaceId: string, path: string, executionRootId?: string) => Promise<WorkspaceFileContent>;
  readToolExecutionArtifact: (
    agentId: string,
    toolExecutionId: string,
    artifactIndex: number,
  ) => Promise<ToolExecutionArtifactContent>;
  fetchWorkspaceFileBlob: (
    workspaceId: string,
    path: string,
    executionRootId?: string,
    options?: { download?: boolean; timeoutMs?: number },
  ) => Promise<Blob>;
  navigateBack: () => void;
  toggleRightPanel: () => void;
  toggleNavCollapsed: () => void;
  setRuntimeConnection: (config: RuntimeConnectionConfig) => Promise<void>;
  refreshBootstrap: (options?: BootstrapRefreshOptions) => Promise<void>;
  reconcileAfterResume: () => Promise<void>;
  refreshModelCatalog: () => Promise<void>;
  refreshRuntimeConfig: () => Promise<void>;
  updateRuntimeConfig: (updates: Array<{ key: string; value?: unknown; unset?: boolean }>) => Promise<RuntimeConfigState | undefined>;
  refreshSkillCatalog: () => Promise<void>;
  refreshSkillDetail: (skillId: string | undefined) => Promise<void>;
  refreshTemplateCatalog: () => Promise<void>;
  refreshTemplateDetail: (catalogId: string | undefined) => Promise<void>;
  installTemplate: (githubUrl: string) => Promise<boolean>;
  removeTemplate: (templateId: string) => Promise<boolean>;
  syncTemplateRemoteSources: () => Promise<boolean>;
  dismissTemplateDiagnostics: () => void;
  dismissTemplateError: () => void;
  createAgentFromTemplate: (agentId: string, template: string) => Promise<boolean>;
  addSkillToCatalog: (input: AddSkillInput) => Promise<boolean>;
  removeSkillFromCatalog: (name: string) => Promise<boolean>;
  updateSkillCatalog: (name?: string) => Promise<boolean>;
  dismissSkillJob: (jobId: string) => void;
  checkSkillCatalog: (name?: string) => Promise<boolean>;
  refreshAgentSkillCatalog: (agentId: string | undefined) => Promise<void>;
  enableAgentSkill: (agentId: string | undefined, name: string) => Promise<boolean>;
  disableAgentSkill: (agentId: string | undefined, name: string) => Promise<boolean>;
  refreshCredentialStore: () => Promise<void>;
  setCredential: (profile: string, kind: string, material: string) => Promise<CredentialProfileStatus | undefined>;
  deleteCredential: (profile: string) => Promise<void>;
  startCodexDeviceLogin: (providerId?: string) => Promise<void>;
  clearCodexDeviceLogin: () => void;
  runSearch: (query: string, options?: RuntimeSearchOptions) => Promise<void>;
  loadSearchResultContent: (sourceRef: string) => Promise<void>;
  ensureAgentSession: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  refreshAgentDetail: (
    agentId: string | undefined,
    displayLevel: DisplayLevel,
    options?: AgentDetailRefreshOptions,
  ) => Promise<void>;
  refreshAgentWorkItems: (agentId: string | undefined) => Promise<void>;
  refreshAgentState: (agentId: string | undefined) => Promise<void>;
  loadAgentWorkItemDetail: (agentId: string | undefined, workItemId: string | undefined) => Promise<void>;
  loadAgentTaskDetail: (agentId: string | undefined, taskId: string | undefined, force?: boolean) => Promise<void>;
  loadAgentToolExecutionDetail: (agentId: string | undefined, toolExecutionId: string | undefined, fallbackActivity?: AgentTimelineActivity) => Promise<void>;
  loadOlderAgentEvents: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  sendOperatorPrompt: (agentId: string | undefined, text: string, displayLevel: DisplayLevel, attachments?: OperatorPromptAttachment[]) => Promise<void>;
  setAgentModel: (agentId: string | undefined, model: string, displayLevel: DisplayLevel, reasoningEffort?: string) => Promise<void>;
  clearAgentModel: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  startAgentEventStream: (agentId: string | undefined, displayLevel: DisplayLevel) => void;
  stopAgentEventStream: (agentId: string | undefined) => void;
  startGlobalEventStream: () => void;
  stopGlobalEventStream: () => void;
  registerAgentForEvents: (agentId: string) => void;
  unregisterAgentForEvents: (agentId: string) => void;
}

export function resetTransientRuntimeStateForResume(
  state: RuntimeStoreState,
): Partial<RuntimeStoreState> {
  return {
    modelCatalogLoading: false,
    runtimeConfigLoading: false,
    runtimeConfigSaving: false,
    skillCatalogLoading: false,
    skillDetailLoadingById: resetBooleanMap(state.skillDetailLoadingById),
    templateCatalogLoading: false,
    templateSyncInProgress: false,
    templateDetailLoadingById: resetBooleanMap(state.templateDetailLoadingById),
    agentSkillCatalogLoadingByAgentId: resetBooleanMap(state.agentSkillCatalogLoadingByAgentId),
    credentialStoreLoading: false,
    codexDeviceLogin: { status: "idle" },
    searchLoading: false,
    searchResultContentLoadingBySourceRef: resetBooleanMap(state.searchResultContentLoadingBySourceRef),
    rightPanelView: resetRightPanelLoading(state.rightPanelView),
    rightPanelViewStack: state.rightPanelViewStack.map(resetRightPanelLoading).filter((view): view is RightPanelView => view !== undefined),
    sessionsByAgentId: resetSessionsForResume(state.sessionsByAgentId),
  };
}

function resetBooleanMap(values: Record<string, boolean>): Record<string, boolean> {
  return Object.fromEntries(Object.keys(values).map((key) => [key, false]));
}

function resetRightPanelLoading(view: RightPanelView | undefined): RightPanelView | undefined {
  if (!view || !("detailState" in view) || !view.detailState?.loading) return view;
  return { ...view, detailState: { ...view.detailState, loading: false } };
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
  kind?: "install" | "update";
  status: "queued" | "running" | "completed" | "failed";
  summary?: string;
  error?: string;
}

const SKILL_INSTALL_JOBS_STORAGE_KEY = "holon.webGui.skillInstallJobs.v1";
const SKILL_JOB_TERMINAL_RETENTION = 20;

function retainSkillJobs(jobs: SkillInstallJob[]): SkillInstallJob[] {
  let terminalToDrop = Math.max(
    0,
    jobs.filter((job) => job.status === "completed" || job.status === "failed").length
      - SKILL_JOB_TERMINAL_RETENTION,
  );
  return jobs.filter((job) => {
    if (job.status === "queued" || job.status === "running" || terminalToDrop === 0) {
      return true;
    }
    terminalToDrop -= 1;
    return false;
  });
}

function loadSkillInstallJobs(): SkillInstallJob[] {
  try {
    const raw = localStorage.getItem(SKILL_INSTALL_JOBS_STORAGE_KEY);
    return raw ? retainSkillJobs(JSON.parse(raw) as SkillInstallJob[]) : [];
  } catch {
    return [];
  }
}

function saveSkillInstallJobs(jobs: SkillInstallJob[]): void {
  try {
    const retainedJobs = retainSkillJobs(jobs);
    if (retainedJobs.length) {
      localStorage.setItem(SKILL_INSTALL_JOBS_STORAGE_KEY, JSON.stringify(retainedJobs));
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
const globalEventRecovery = new EventGapRecoveryTracker();
const messageHydrationInFlight = new Map<string, Set<string>>();
const transcriptHydrationInFlight = new Map<string, Set<string>>();
const briefHydrationInFlight = new Map<string, Set<string>>();
const inspectorDetailInFlight = new Set<string>();
const workItemRefreshInFlight = new Set<string>();
const workItemDetailInFlight = new Set<string>();
const taskDetailInFlight = new Set<string>();
const toolExecutionDetailInFlight = new Set<string>();
const agentStateRefreshInFlight = new Map<string, number>();
const agentEventCatchUpInFlight = new Map<string, Promise<void>>();
const agentDetailRefreshInFlight = new Map<string, { generation: number; promise: Promise<void> }>();
const agentDetailRequestSequence = new Map<string, number>();
let bootstrapRefreshInFlight: Promise<void> | undefined;
let bootstrapRefreshTimer: number | undefined;
let clientGeneration = 0;
let resumeReconciliationInFlight: Promise<void> | undefined;
let resumeReconciliationCoordinator: ResumeReconciliationCoordinator | undefined;
const STREAM_FLUSH_INTERVAL_MS = 100;
const STREAM_STALE_TIMEOUT_MS = 45_000;
const STREAM_RECONNECT_BASE_MS = 1_000;
const STREAM_RECONNECT_MAX_MS = 15_000;
const GLOBAL_STREAM_STALE_TIMEOUT_MS = 45_000;
const GLOBAL_BACKFILL_LIMIT = 100;
const AGENT_VALIDATION_TTL_MS = 60_000;
const RESUME_RECONCILIATION_THRESHOLD_MS = 60_000;

function nextClientGeneration(): number {
  clientGeneration += 1;
  return clientGeneration;
}

function isCurrentClientGeneration(generation: number): boolean {
  return generation === clientGeneration;
}

type RuntimeClient = ReturnType<typeof createRuntimeClient>;

interface ClientRequest {
  client: RuntimeClient;
  generation: number;
}

function captureClientRequest(): ClientRequest {
  return { client: runtimeClient, generation: clientGeneration };
}

function isCurrentClientRequest(request: ClientRequest): boolean {
  return request.client === runtimeClient && isCurrentClientGeneration(request.generation);
}

function clearInFlightHydration(): void {
  messageHydrationInFlight.clear();
  transcriptHydrationInFlight.clear();
  briefHydrationInFlight.clear();
}

function cancelClientGenerationWork(): void {
  bootstrapRefreshInFlight = undefined;
  if (bootstrapRefreshTimer != null) {
    window.clearTimeout(bootstrapRefreshTimer);
    bootstrapRefreshTimer = undefined;
  }
  agentStateRefreshInFlight.clear();
  agentEventCatchUpInFlight.clear();
  agentDetailRefreshInFlight.clear();
  agentDetailRequestSequence.clear();
  inspectorDetailInFlight.clear();
  workItemRefreshInFlight.clear();
  workItemDetailInFlight.clear();
  taskDetailInFlight.clear();
  toolExecutionDetailInFlight.clear();
  clearInFlightHydration();
}

export async function runWithConcurrencyLimit<T>(
  values: readonly T[],
  limit: number,
  run: (value: T) => Promise<void>,
  shouldContinue: () => boolean = () => true,
): Promise<void> {
  const workerCount = Math.min(values.length, Math.max(1, Math.floor(limit)));
  let nextIndex = 0;
  const worker = async () => {
    while (shouldContinue()) {
      const index = nextIndex;
      nextIndex += 1;
      if (index >= values.length) return;
      await run(values[index]);
    }
  };
  await Promise.all(Array.from({ length: workerCount }, worker));
}

export function buildResumeRefreshes(
  agentIds: readonly string[],
  selectedAgentId: string,
): Array<{ agentId: string; detail: boolean }> {
  return agentIds
    .filter((agentId) => agentId === selectedAgentId)
    .map((agentId) => ({ agentId, detail: true }));
}

function closeEventStreamsForResume(set: StoreSet): void {
  stopGlobalEventStream(set);
  for (const agentId of Array.from(activeEventStreams.keys())) {
    stopAgentEventStream(agentId, set);
  }
  for (const timer of streamFlushTimers.values()) window.clearTimeout(timer);
  streamFlushTimers.clear();
  pendingStreamEvents.clear();
  for (const timer of reconnectTimers.values()) window.clearTimeout(timer);
  reconnectTimers.clear();
  for (const timer of staleTimers.values()) window.clearTimeout(timer);
  staleTimers.clear();
  globalStreamSubscribedAgents.clear();
  globalEventRecovery.clear();
}

export function resetSessionsForResume(
  sessionsByAgentId: Record<string, AgentSessionState>,
): Record<string, AgentSessionState> {
  return Object.fromEntries(
    Object.entries(sessionsByAgentId).map(([agentId, session]) => [
      agentId,
      {
        ...session,
        loading: false,
        loadingOlder: false,
        sendingPrompt: false,
        liveStatus: "stale" as const,
        reconnectAttempt: 0,
        workItemDetailsById: resetDetailLoading(session.workItemDetailsById),
        taskDetailsById: resetDetailLoading(session.taskDetailsById),
        toolExecutionDetailsById: resetDetailLoading(session.toolExecutionDetailsById),
      },
    ]),
  );
}

function resetDetailLoading<T extends { loading?: boolean }>(detailsById: Record<string, T>): Record<string, T> {
  return Object.fromEntries(
    Object.entries(detailsById).map(([id, detail]) => [id, detail.loading ? { ...detail, loading: false } : detail]),
  );
}

// ─── Session cache (IndexedDB persistence) ──────────────────────────
let sessionCacheWriter: SessionCacheWriter | null = null;
let sessionCacheInitPromise: Promise<void> | null = null;

interface SessionCacheContext {
  remoteKey: string;
  generation: number;
}

export function isSessionCacheContextCurrent(
  context: SessionCacheContext,
  remoteKey: string,
  generation: number,
): boolean {
  return context.remoteKey === remoteKey && context.generation === generation;
}

function currentSessionCacheContext(): SessionCacheContext {
  return {
    remoteKey: currentRemoteKey(runtimeConnectionConfig),
    generation: clientGeneration,
  };
}

function sessionCacheContextIsCurrent(context: SessionCacheContext): boolean {
  const current = currentSessionCacheContext();
  return isSessionCacheContextCurrent(context, current.remoteKey, current.generation);
}

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

const emptyTemplateCatalog: AgentTemplateCatalogState = {
  source: "fixture",
  catalog: [],
  sources: [],
  diagnostics: [],
};

function diagnosticSignature(d: AgentTemplateCatalogDiagnostic): string {
  return `${d.sourceId ?? "catalog"}:${d.message}`;
}

/**
 * Filter out diagnostics the user has previously dismissed so they don't
 * reappear on refresh when the server still has them stored.
 */
function filterDismissedDiagnostics(
  catalog: AgentTemplateCatalogState,
  dismissed: string[],
): AgentTemplateCatalogState {
  if (!dismissed.length || !catalog.diagnostics.length) return catalog;
  const dismissedSet = new Set(dismissed);
  return {
    ...catalog,
    diagnostics: catalog.diagnostics.filter((d) => !dismissedSet.has(diagnosticSignature(d))),
  };
}

/**
 * Initialize session cache for the current remote and hydrate any cached
 * sessions into the store. Called on initial load and remote switch.
 */
function initSessionCacheForRemote(set: StoreSet, get?: () => RuntimeStoreState): void {
  if (sessionCacheInitPromise) return;
  const context = currentSessionCacheContext();
  const initialization = (async () => {
    try {
      const ok = await initSessionCache();
      if (!sessionCacheContextIsCurrent(context)) return;
      if (!ok) {
        sessionCacheWriter = null;
        set((state) => ({
          sessionsByAgentId: Object.fromEntries(
            Object.entries(state.sessionsByAgentId).map(([agentId, session]) => [
              agentId,
              session.cacheStatus === "unchecked"
                ? { ...session, cacheStatus: "unavailable" as const }
                : session,
            ]),
          ),
        }));
        return;
      }

      // Set up writer for this remote.
      sessionCacheWriter?.cancel();
      const writer = new SessionCacheWriter(context.remoteKey);
      sessionCacheWriter = writer;

      // Hydrate cached sessions into store.
      const cached = await hydrateAllSessions(context.remoteKey);
      if (!sessionCacheContextIsCurrent(context) || sessionCacheWriter !== writer) return;
      if (Object.keys(cached).length === 0) return;

      const restoredAgentIds: string[] = [];
      set((state) => {
        const sessionsByAgentId = { ...state.sessionsByAgentId };
        for (const [agentId, partial] of Object.entries(cached)) {
          const current = sessionsByAgentId[agentId] ?? emptyAgentSession();
          const restored = mergeCachedSessionIntoCurrent(current, partial);
          if (restored === current) continue;
          sessionsByAgentId[agentId] = restored;
          restoredAgentIds.push(agentId);
        }
        const withProvisional = rebuildProvisionalDetailsWithAgents(
          state.bootstrap.agents,
          sessionsByAgentId,
        );
        return { sessionsByAgentId: withProvisional ?? sessionsByAgentId };
      });

      if (get && sessionCacheContextIsCurrent(context)) {
        const displayLevel = get().displayLevel;
        const selectedAgentId = get().selectedAgentId;
        if (selectedAgentId && restoredAgentIds.includes(selectedAgentId)) {
          scheduleMessageHydration(get, set, selectedAgentId, displayLevel);
          scheduleTranscriptHydration(get, set, selectedAgentId, displayLevel);
          scheduleBriefHydration(get, set, selectedAgentId, displayLevel);
        }
      }
    } catch {
      if (sessionCacheContextIsCurrent(context)) sessionCacheWriter = null;
    }
  })();
  sessionCacheInitPromise = initialization;
  void initialization.then(
    () => {
      if (sessionCacheInitPromise === initialization) sessionCacheInitPromise = null;
    },
    () => {
      if (sessionCacheInitPromise === initialization) sessionCacheInitPromise = null;
    },
  );
}

export function mergeCachedSessionIntoCurrent(
  current: AgentSessionState,
  cached: Partial<AgentSessionState>,
): AgentSessionState {
  if (
    current.detail ||
    current.eventSeqs.length > 0 ||
    Object.keys(current.messagesById).length > 0 ||
    Object.keys(current.transcriptEntriesById).length > 0 ||
    Object.keys(current.briefRecordsById).length > 0
  ) {
    return current;
  }
  return {
    ...current,
    ...cached,
    loading: current.loading,
    loadingOlder: current.loadingOlder,
    liveStatus: current.liveStatus,
    sendingPrompt: current.sendingPrompt,
  };
}

export const useRuntimeStore = create<RuntimeStoreState>((set, get) => ({
  route: "dashboard",
  selectedAgentId: "",
  selectedSkillId: "",
  selectedTemplateId: "",
  displayLevel: "info",
  displayLevelsByAgentId: readStoredDisplayLevels(),
  rightPanelOpen: true,
  rightPanelView: undefined,
  rightPanelViewStack: [],
  navCollapsed: false,

  bootstrap: pendingBootstrap(runtimeConnectionConfig),
  bootstrapLoading: true,
  globalStreamStatus: "idle",
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
  templateCatalog: emptyTemplateCatalog,
  templateCatalogLoading: false,
  templateCatalogError: undefined,
  dismissedTemplateDiagnostics: [],
  templateSyncInProgress: false,
  templateSyncMessage: undefined,
  templateDetailById: {},
  templateDetailLoadingById: {},
  templateDetailErrorById: {},
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
  resumeRevision: 0,

  setRoute: (route) => set({ route }),
  openSkill: (skillId) => set({ route: "skillDetail", selectedSkillId: skillId }),
  openTemplate: (catalogId) => set({ route: "templateDetail", selectedTemplateId: catalogId }),
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
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: { kind: "agent_overview", agentId: agentId ?? state.selectedAgentId },
      };
    }),
  showWorkItemDetail: (agentId, workItem) =>
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: { kind: "work_item_detail", agentId, workItem },
      };
    }),
  showTaskDetail: (agentId, task) =>
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: { kind: "task_detail", agentId, task },
      };
    }),
  showToolExecutionDetail: (agentId, toolExecutionId, toolName, relatedStateObjectRef) =>
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: { kind: "tool_execution_detail", agentId, toolExecutionId, toolName, relatedStateObjectRef },
      };
    }),
  showFileBrowser: (agentId, workspaceId, initialPath, executionRootId, initialFilePath) =>
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: { kind: "file_browser", agentId, workspaceId, initialPath, executionRootId, initialFilePath },
      };
    }),
  browseWorkspaceDir: (workspaceId, path, executionRootId) => runtimeClient.browseWorkspaceDir(workspaceId, path, executionRootId),
  readWorkspaceFile: (workspaceId, path, executionRootId) => runtimeClient.readWorkspaceFile(workspaceId, path, executionRootId),
  readToolExecutionArtifact: (agentId, toolExecutionId, artifactIndex) =>
    runtimeClient.readToolExecutionArtifact(agentId, toolExecutionId, artifactIndex),
  fetchWorkspaceFileBlob: (workspaceId, path, executionRootId, options) =>
    runtimeClient.fetchWorkspaceFileBlob(workspaceId, path, executionRootId, options),
  inspectActivity: (agentId, activity) => {
    // Use relatedStateObjectRef as fallback for task/work_item navigation,
    // since their child activities (status_updated, result_received, etc.)
    // only carry relatedStateObjectRef without their own stateObjectRef.
    const ref = activity.stateObjectRef ?? activity.relatedStateObjectRef;
    if (ref?.kind === "work_item") {
      const workItem: WorkItemSummary = {
        id: ref.id,
        objective: ref.objective ?? activity.body,
        state: ref.state ?? "unknown",
      };
      get().showWorkItemDetail(agentId, workItem);
      void get().loadAgentWorkItemDetail(agentId, ref.id);
      return;
    }

    if (ref?.kind === "task") {
      const taskId = ref.id.replace(/^task:/, "");
      const task: TaskSummary = {
        id: taskId,
        kind: "task",
        status: ref.status ?? "unknown",
        summary: ref.summary ?? activity.body,
      };
      get().showTaskDetail(agentId, task);
      void get().loadAgentTaskDetail(agentId, taskId);
      return;
    }

    if (activity.stateObjectRef?.kind === "tool_execution") {
      get().showToolExecutionDetail(agentId, activity.stateObjectRef.id, activity.stateObjectRef.toolName, activity.relatedStateObjectRef);
      void get().loadAgentToolExecutionDetail(agentId, activity.stateObjectRef.id, activity);
      return;
    }

    // Other stateObjectRef kinds fall through to the
    // activity inspector, which shows structured detail for the event.
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: { kind: "activity_inspector", agentId, activity },
      };
    });
    hydrateInspectorActivityDetail(get, set, agentId, activity);
  },
  navigateBack: () =>
    set((state) => {
      if (state.rightPanelViewStack.length === 0) {
        return {
          rightPanelView: { kind: "agent_overview", agentId: state.selectedAgentId },
          rightPanelViewStack: [],
        };
      }
      const newStack = [...state.rightPanelViewStack];
      const previous = newStack.pop()!;
      return {
        rightPanelView: previous,
        rightPanelViewStack: newStack,
      };
    }),
  toggleRightPanel: () => set((state) => ({ rightPanelOpen: !state.rightPanelOpen })),
  toggleNavCollapsed: () => set((state) => ({ navCollapsed: !state.navCollapsed })),

  setRuntimeConnection: async (config) => {
    nextClientGeneration();
    cancelClientGenerationWork();
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
    resumeReconciliationInFlight = undefined;
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
    globalEventRecovery.clear();
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
      modelCatalogLoading: false,
      modelCatalogError: undefined,
      runtimeConfig: emptyRuntimeConfig,
      runtimeConfigLoading: false,
      runtimeConfigSaving: false,
      runtimeConfigError: undefined,
      skillCatalog: emptySkillCatalog,
      skillCatalogLoading: false,
      skillCatalogError: undefined,
      skillDetailById: {},
      skillDetailLoadingById: {},
      skillDetailErrorById: {},
      templateCatalog: emptyTemplateCatalog,
      templateCatalogLoading: false,
      templateCatalogError: undefined,
      dismissedTemplateDiagnostics: [],
      templateSyncInProgress: false,
      templateSyncMessage: undefined,
      templateDetailById: {},
      templateDetailLoadingById: {},
      templateDetailErrorById: {},
      agentSkillCatalogByAgentId: {},
      agentSkillCatalogLoadingByAgentId: {},
      agentSkillCatalogErrorByAgentId: {},
      credentialStore: { profiles: [] },
      credentialStoreLoading: false,
      credentialStoreError: undefined,
      codexDeviceLogin: { status: "idle" as const },
      search: null,
      searchLoading: false,
      searchError: undefined,
      searchResultContentBySourceRef: {},
      searchResultContentLoadingBySourceRef: {},
      searchResultContentErrorBySourceRef: {},
      sessionsByAgentId: {},
      rosterActivityByAgentId: readStoredRosterActivity(currentRemoteKey(normalizedConfig)),
      selectedAgentId: "",
      selectedSkillId: "",
      selectedTemplateId: "",
      route: "dashboard",
      resumeRevision: get().resumeRevision + 1,
    });
    await get().refreshBootstrap();
    // Initialize cache for the new remote (async, non-blocking).
    initSessionCacheForRemote(set, get);
  },

  refreshBootstrap: async (options = {}) => {
    if (bootstrapRefreshInFlight) return bootstrapRefreshInFlight;
    const generation = clientGeneration;
    const trace =
      options.trace ??
      createRuntimeTrace("bootstrap.refresh", { trigger: options.trigger ?? "bootstrap" });
    const span = startRuntimeSpan(trace, "bootstrap.refresh", {
      background: Boolean(options.background),
    });
    if (options.background) {
      set({ bootstrapError: undefined });
    } else {
      set({ bootstrapLoading: true, bootstrapError: undefined });
    }

    const request = (async () => {
      try {
        const bootstrap = await runtimeClient.getBootstrap();
        if (!isCurrentClientGeneration(generation)) return;
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
            return cachedAgent ? mergeBootstrapAgentState(agent, cachedAgent) : agent;
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
        // After bootstrap agents are available, build provisional details
        // for sessions that have cached events but no HTTP detail yet.
        useRuntimeStore.setState((state) => {
          const updated = rebuildProvisionalDetailsWithAgents(
            state.bootstrap.agents,
            state.sessionsByAgentId,
          );
          return updated ? { sessionsByAgentId: updated } : {};
        });
        if (options.syncEvents !== false) {
          syncGlobalEventRoster(get, set);
        }
        span.end("ok", { agentCount: bootstrap.agents.length });
      } catch (error) {
        if (!isCurrentClientGeneration(generation)) return;
        if (isProjectionBusyError(error)) {
          set({ bootstrapLoading: false });
          span.end("skipped", { reason: "projection_busy" });
          return;
        }
        set({
          bootstrapLoading: false,
          bootstrapError: error instanceof Error ? error.message : String(error),
        });
        span.end("error");
      }
    })();
    bootstrapRefreshInFlight = request;
    void request.then(() => {
      if (bootstrapRefreshInFlight === request) {
        bootstrapRefreshInFlight = undefined;
      }
    });

    return request;
  },

  reconcileAfterResume: async () => {
    if (resumeReconciliationInFlight) return resumeReconciliationInFlight;

    const generation = nextClientGeneration();
    cancelClientGenerationWork();
    closeEventStreamsForResume(set);
    // First invalidate transient layout/loading state before fresh projections arrive.
    set((state) => ({
      ...resetTransientRuntimeStateForResume(state),
      resumeRevision: state.resumeRevision + 1,
    }));
    resumeSkillInstallJobPolling(set, get);

    const trace = createRuntimeTrace("page.resume", { trigger: "page.resume" });
    const request = (async () => {
      try {
        await get().refreshBootstrap({
          background: true,
          syncEvents: false,
          trace,
          trigger: "page.resume",
        });
        if (!isCurrentClientGeneration(generation)) return;
        const selectedAgentId = get().selectedAgentId;
        const refreshes = buildResumeRefreshes(
          get().bootstrap.agents.map((agent) => agent.id),
          selectedAgentId,
        );
        await runWithConcurrencyLimit(
          refreshes,
          4,
          async ({ agentId, detail }) => {
            if (detail) {
              await get().ensureAgentSession(agentId, get().displayLevel);
            } else {
              await get().refreshAgentState(agentId);
            }
          },
          () => isCurrentClientGeneration(generation),
        );
        if (!isCurrentClientGeneration(generation)) return;
        syncGlobalEventRoster(get, set);
      } finally {
        if (isCurrentClientGeneration(generation)) {
          // Then remeasure once more after the reconciled projections and hydration are scheduled.
          set((state) => ({ resumeRevision: state.resumeRevision + 1 }));
        }
      }
    })();
    resumeReconciliationInFlight = request;
    const clearRequest = () => {
      if (resumeReconciliationInFlight === request) {
        resumeReconciliationInFlight = undefined;
      }
    };
    void request.then(clearRequest, clearRequest);

    return request;
  },

  refreshModelCatalog: async () => {
    const request = captureClientRequest();
    set({ modelCatalogLoading: true, modelCatalogError: undefined });
    try {
      const modelCatalog = await request.client.getModels();
      if (!isCurrentClientRequest(request)) return;
      set({ modelCatalog, modelCatalogLoading: false, modelCatalogError: modelCatalog.error });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        modelCatalog: { ...state.modelCatalog, error: message },
        modelCatalogLoading: false,
        modelCatalogError: message,
      }));
    }
  },

  refreshRuntimeConfig: async () => {
    const request = captureClientRequest();
    set({ runtimeConfigLoading: true, runtimeConfigError: undefined });
    try {
      const runtimeConfig = await request.client.getRuntimeConfig();
      if (!isCurrentClientRequest(request)) return;
      set({ runtimeConfig, runtimeConfigLoading: false, runtimeConfigError: runtimeConfig.error });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        runtimeConfig: { ...state.runtimeConfig, error: message },
        runtimeConfigLoading: false,
        runtimeConfigError: message,
      }));
    }
  },

  updateRuntimeConfig: async (updates) => {
    const request = captureClientRequest();
    set({ runtimeConfigSaving: true, runtimeConfigError: undefined });
    try {
      const runtimeConfig = await request.client.updateRuntimeConfig(updates);
      if (!isCurrentClientRequest(request)) return undefined;
      set({ runtimeConfig, runtimeConfigSaving: false, runtimeConfigError: runtimeConfig.error });
      if (runtimeConfig.changed && !runtimeConfig.error) {
        set({ modelCatalogLoading: true, modelCatalogError: undefined });
        try {
          const modelCatalog = await request.client.getModels();
          if (!isCurrentClientRequest(request)) return runtimeConfig;
          set({ modelCatalog, modelCatalogLoading: false, modelCatalogError: modelCatalog.error });
        } catch (modelError) {
          if (!isCurrentClientRequest(request)) return runtimeConfig;
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
      if (!isCurrentClientRequest(request)) return undefined;
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
    const request = captureClientRequest();
    set({ skillCatalogLoading: true, skillCatalogError: undefined });
    try {
      const skillCatalog = await request.client.getSkillCatalog();
      if (!isCurrentClientRequest(request)) return;
      set({ skillCatalog, skillCatalogLoading: false, skillCatalogError: skillCatalog.error });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
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
    const request = captureClientRequest();
    set((state) => ({
      skillDetailLoadingById: { ...state.skillDetailLoadingById, [skillId]: true },
      skillDetailErrorById: { ...state.skillDetailErrorById, [skillId]: undefined },
    }));
    try {
      // The backend resolves scope from the skill_id prefix automatically.
      const detail = await request.client.getSkillDetail(skillId);
      if (!isCurrentClientRequest(request)) return;
      set((state) => ({
        skillDetailById: { ...state.skillDetailById, [skillId]: detail },
        skillDetailLoadingById: { ...state.skillDetailLoadingById, [skillId]: false },
        skillDetailErrorById: { ...state.skillDetailErrorById, [skillId]: detail.error },
      }));
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
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

  refreshTemplateCatalog: async () => {
    const request = captureClientRequest();
    set({ templateCatalogLoading: true, templateCatalogError: undefined });
    try {
      const templateCatalog = filterDismissedDiagnostics(
        await request.client.getTemplateCatalog(), get().dismissedTemplateDiagnostics,
      );
      if (!isCurrentClientRequest(request)) return;
      set({ templateCatalog, templateCatalogLoading: false, templateCatalogError: templateCatalog.error });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        templateCatalog: { ...state.templateCatalog, source: "http", error: message },
        templateCatalogLoading: false,
        templateCatalogError: message,
      }));
    }
  },

  refreshTemplateDetail: async (catalogId) => {
    if (!catalogId) return;
    const request = captureClientRequest();
    set((state) => ({
      templateDetailLoadingById: { ...state.templateDetailLoadingById, [catalogId]: true },
      templateDetailErrorById: { ...state.templateDetailErrorById, [catalogId]: undefined },
    }));
    try {
      const detail = await request.client.getTemplateDetail(catalogId);
      if (!isCurrentClientRequest(request)) return;
      set((state) => ({
        templateDetailById: { ...state.templateDetailById, [catalogId]: detail },
        templateDetailLoadingById: { ...state.templateDetailLoadingById, [catalogId]: false },
        templateDetailErrorById: { ...state.templateDetailErrorById, [catalogId]: detail.error },
      }));
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        templateDetailById: {
          ...state.templateDetailById,
          [catalogId]: { source: "http", error: message },
        },
        templateDetailLoadingById: { ...state.templateDetailLoadingById, [catalogId]: false },
        templateDetailErrorById: { ...state.templateDetailErrorById, [catalogId]: message },
      }));
    }
  },

  installTemplate: async (githubUrl) => {
    const request = captureClientRequest();
    set({ templateCatalogLoading: true, templateCatalogError: undefined });
    try {
      await request.client.installTemplate(githubUrl);
      const templateCatalog = filterDismissedDiagnostics(
        await request.client.getTemplateCatalog(), get().dismissedTemplateDiagnostics,
      );
      if (!isCurrentClientRequest(request)) return false;
      set({ templateCatalog, templateCatalogLoading: false, templateCatalogError: templateCatalog.error });
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request)) return false;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        templateCatalog: { ...state.templateCatalog, error: message },
        templateCatalogLoading: false,
        templateCatalogError: message,
      }));
      return false;
    }
  },

  removeTemplate: async (templateId) => {
    const request = captureClientRequest();
    set({ templateCatalogLoading: true, templateCatalogError: undefined });
    try {
      await request.client.removeTemplate(templateId);
      const templateCatalog = filterDismissedDiagnostics(
        await request.client.getTemplateCatalog(), get().dismissedTemplateDiagnostics,
      );
      if (!isCurrentClientRequest(request)) return false;
      set({ templateCatalog, templateCatalogLoading: false, templateCatalogError: templateCatalog.error });
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request)) return false;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        templateCatalog: { ...state.templateCatalog, error: message },
        templateCatalogLoading: false,
        templateCatalogError: message,
      }));
      return false;
    }
  },

  syncTemplateRemoteSources: async () => {
    const request = captureClientRequest();
    set({ templateCatalogLoading: true, templateCatalogError: undefined, templateSyncMessage: undefined });
    try {
      set({ templateSyncInProgress: true });
      const jobId = await request.client.syncTemplateRemoteSources();
      await pollTemplateSyncJob(request, jobId);
      const templateCatalog = filterDismissedDiagnostics(
        await request.client.getTemplateCatalog(), get().dismissedTemplateDiagnostics,
      );
      if (!isCurrentClientRequest(request)) return false;
      const sourceCount = templateCatalog.sources.length;
      const templateCount = templateCatalog.catalog.length;
      set({ templateCatalog, templateCatalogLoading: false, templateSyncInProgress: false, templateCatalogError: templateCatalog.error, templateSyncMessage: `Synced ${sourceCount} source(s), ${templateCount} template(s).` });
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request)) return false;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        templateCatalog: { ...state.templateCatalog, error: message },
        templateCatalogLoading: false,
        templateSyncInProgress: false,
        templateCatalogError: message,
        templateSyncMessage: undefined,
      }));
      return false;
    }
  },

  createAgentFromTemplate: async (agentId, template) => {
    const request = captureClientRequest();
    set({ templateCatalogError: undefined });
    try {
      await request.client.createAgentFromTemplate(agentId, template);
      if (!isCurrentClientRequest(request)) return false;
      await get().refreshBootstrap({ background: true });
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request)) return false;
      const message = error instanceof Error ? error.message : String(error);
      set({ templateCatalogError: message });
      return false;
    }
  },

  dismissTemplateDiagnostics: () => {
    set((state) => ({
      dismissedTemplateDiagnostics: [
        ...state.dismissedTemplateDiagnostics,
        ...state.templateCatalog.diagnostics.map(diagnosticSignature),
      ],
      templateCatalog: { ...state.templateCatalog, diagnostics: [] },
    }));
  },

  dismissTemplateError: () => {
    set((state) => ({
      templateCatalog: { ...state.templateCatalog, error: undefined },
      templateCatalogError: undefined,
    }));
  },

  addSkillToCatalog: async (input) => {
    const request = captureClientRequest();
    set({ skillCatalogError: undefined });
    try {
      const jobId = await request.client.addSkillToCatalog(input);
      if (!isCurrentClientRequest(request)) return false;
      const source = "package" in input ? input.package : "path" in input ? input.path : "name" in input ? input.name : "unknown";
      const job: SkillInstallJob = { jobId, source, status: "queued" };
      set((state) => {
        const jobs = retainSkillJobs([...state.skillInstallJobs, job]);
        saveSkillInstallJobs(jobs);
        return { skillInstallJobs: jobs };
      });
      void pollSkillInstallJob(set, get, request, jobId);
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request)) return false;
      const message = error instanceof Error ? error.message : String(error);
      set({ skillCatalogError: message });
      return false;
    }
  },

  removeSkillFromCatalog: async (name) => {
    const request = captureClientRequest();
    set({ skillCatalogLoading: true, skillCatalogError: undefined });
    try {
      await request.client.removeSkillFromCatalog(name);
      const skillCatalog = await request.client.getSkillCatalog();
      if (!isCurrentClientRequest(request)) return false;
      set({ skillCatalog, skillCatalogLoading: false, skillCatalogError: skillCatalog.error });
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request)) return false;
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
    const request = captureClientRequest();
    set({ skillCatalogError: undefined });
    try {
      const jobId = await request.client.updateSkillCatalog(name);
      if (!isCurrentClientRequest(request)) return false;
      const job: SkillInstallJob = {
        jobId,
        source: name ?? "all skills",
        kind: "update",
        status: "queued",
      };
      set((state) => {
        const jobs = retainSkillJobs([...state.skillInstallJobs, job]);
        saveSkillInstallJobs(jobs);
        return { skillInstallJobs: jobs };
      });
      void pollSkillInstallJob(set, get, request, jobId);
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request)) return false;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        skillCatalog: { ...state.skillCatalog, error: message },
        skillCatalogError: message,
      }));
      return false;
    }
  },

  dismissSkillJob: (jobId) => removeSkillInstallJob(set, get, jobId),

  checkSkillCatalog: async (name) => {
    const request = captureClientRequest();
    set({ skillCatalogLoading: true, skillCatalogError: undefined });
    try {
      await request.client.checkSkillCatalog(name);
      const skillCatalog = await request.client.getSkillCatalog();
      if (!isCurrentClientRequest(request)) return false;
      set({ skillCatalog, skillCatalogLoading: false, skillCatalogError: skillCatalog.error });
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request)) return false;
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
    const request = captureClientRequest();
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
      const catalog = await request.client.getSkillCatalog(agentId);
      if (!isCurrentClientRequest(request)) return;
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
      if (!isCurrentClientRequest(request)) return;
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
    const request = captureClientRequest();
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
      await request.client.enableAgentSkill(agentId, name);
      const catalog = await request.client.getSkillCatalog(agentId);
      if (!isCurrentClientRequest(request)) return false;
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
      if (!isCurrentClientRequest(request)) return false;
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
    const request = captureClientRequest();
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
      await request.client.disableAgentSkill(agentId, name);
      const catalog = await request.client.getSkillCatalog(agentId);
      if (!isCurrentClientRequest(request)) return false;
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
      if (!isCurrentClientRequest(request)) return false;
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
    const request = captureClientRequest();
    set({ credentialStoreLoading: true, credentialStoreError: undefined });
    try {
      const credentialStore = await request.client.listCredentials();
      if (!isCurrentClientRequest(request)) return;
      set({ credentialStore, credentialStoreLoading: false });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set({ credentialStoreLoading: false, credentialStoreError: message });
    }
  },

  setCredential: async (profile, kind, material) => {
    const request = captureClientRequest();
    try {
      const result = await request.client.setCredential(profile, kind, material);
      const [credentialStore, runtimeConfig, modelCatalog] = await Promise.all([
        request.client.listCredentials(),
        request.client.getRuntimeConfig(),
        request.client.getModels(),
      ]);
      if (!isCurrentClientRequest(request)) return undefined;
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
      if (!isCurrentClientRequest(request)) return undefined;
      const message = error instanceof Error ? error.message : String(error);
      set({ credentialStoreError: message });
      return undefined;
    }
  },

  deleteCredential: async (profile) => {
    const request = captureClientRequest();
    try {
      await request.client.deleteCredential(profile);
      const [credentialStore, runtimeConfig, modelCatalog] = await Promise.all([
        request.client.listCredentials(),
        request.client.getRuntimeConfig(),
        request.client.getModels(),
      ]);
      if (!isCurrentClientRequest(request)) return;
      set({
        credentialStore,
        credentialStoreError: undefined,
        runtimeConfig,
        runtimeConfigError: runtimeConfig.error,
        modelCatalog,
        modelCatalogError: modelCatalog.error,
      });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set({ credentialStoreError: message });
    }
  },
  startCodexDeviceLogin: async (providerId = "openai-codex") => {
    const request = captureClientRequest();
    set({ codexDeviceLogin: { status: "starting" } });
    try {
      const resp = await request.client.startCodexDeviceLogin(providerId);
      if (!isCurrentClientRequest(request)) return;
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
        if (!isCurrentClientRequest(request)) return;
        const current = get().codexDeviceLogin;
        if (current.status !== "waiting" || current.jobId !== jobId) return;
        if (Date.now() > expiresAt) {
          set({ codexDeviceLogin: { status: "failed", error: "Device login expired." } });
          return;
        }
        try {
          const job = await request.client.getJob(jobId);
          if (!isCurrentClientRequest(request)) return;
          if (job.status === "completed") {
            const [credentialStore, runtimeConfig, modelCatalog] = await Promise.all([
              request.client.listCredentials(),
              request.client.getRuntimeConfig(),
              request.client.getModels(),
            ]);
            if (!isCurrentClientRequest(request)) return;
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
      if (!isCurrentClientRequest(request)) return;
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
    const request = captureClientRequest();
    set({ searchLoading: true, searchError: undefined });
    try {
      const search = await request.client.search(trimmed, options);
      if (!isCurrentClientRequest(request)) return;
      set({
        search,
        searchLoading: false,
        searchResultContentBySourceRef: {},
        searchResultContentLoadingBySourceRef: {},
        searchResultContentErrorBySourceRef: {},
      });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      set({ searchLoading: false, searchError: error instanceof Error ? error.message : String(error) });
    }
  },
  loadSearchResultContent: async (sourceRef) => {
    const trimmed = sourceRef.trim();
    if (!trimmed) return;
    const request = captureClientRequest();
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
      const content = await request.client.getMemorySource(trimmed);
      if (!isCurrentClientRequest(request)) return;
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
      if (!isCurrentClientRequest(request)) return;
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

  ensureAgentSession: async (agentId, displayLevel) => {
    if (!agentId) return;
    const trace = createRuntimeTrace("agent.open", { agentId, trigger: "agent.open" });
    let session = get().sessionsByAgentId[agentId] ?? emptyAgentSession();

    if (session.cacheStatus === "unchecked") {
      if (sessionCacheInitPromise) await sessionCacheInitPromise;
      session = get().sessionsByAgentId[agentId] ?? emptyAgentSession();
    }
    if (session.cacheStatus === "unchecked") {
      const cacheSpan = startRuntimeSpan(trace, "cache.read");
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            cacheStatus: "loading",
            loading: !state.sessionsByAgentId[agentId]?.detail,
          },
        },
      }));
      const cached = await hydrateAgentSession(
        currentRemoteKey(runtimeConnectionConfig),
        agentId,
      );
      set((state) => {
        const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
        const restored = cached ? mergeCachedSessionIntoCurrent(current, cached) : current;
        const available = Boolean(restored.detail?.timeline.length || restored.eventSeqs.length);
        return {
          sessionsByAgentId: {
            ...state.sessionsByAgentId,
            [agentId]: {
              ...restored,
              cacheStatus: cached ? "hit" : "miss",
              contentStatus: available ? "available" : "unknown",
              syncStatus: cached ? "stale" : "refreshing",
              loading: !restored.detail,
            },
          },
        };
      });
      startRuntimeSpan(trace, "ui.session_state_transition", {
        state: cached ? "cache_hit/stale" : "cache_miss/refreshing",
      }).end("ok");
      cacheSpan.end("ok", { cacheHit: Boolean(cached) });
    }

    session = get().sessionsByAgentId[agentId] ?? emptyAgentSession();
    const hasCachedContent = Boolean(session.detail && session.eventSeqs.length);
    const fresh =
      session.lastValidatedAt != null &&
      Date.now() - session.lastValidatedAt < AGENT_VALIDATION_TTL_MS;
    if (hasCachedContent && fresh && get().globalStreamStatus === "streaming") {
      startRuntimeSpan(trace, "agent.validate", { reason: "fresh_stream" }).end("skipped");
      return;
    }
    if (hasCachedContent) {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...state.sessionsByAgentId[agentId],
            loading: false,
            syncStatus: "refreshing",
          },
        },
      }));
      try {
        await catchUpAgentEvents(get, set, agentId, displayLevel, trace);
        if (!fresh) void get().refreshAgentState(agentId);
        set((state) => ({
          sessionsByAgentId: {
            ...state.sessionsByAgentId,
            [agentId]: {
              ...state.sessionsByAgentId[agentId],
              loading: false,
              syncStatus: get().globalStreamStatus === "streaming" ? "streaming" : "idle",
              contentStatus: "available",
              lastValidatedAt: Date.now(),
            },
          },
        }));
        startRuntimeSpan(trace, "ui.session_state_transition", {
          state: `${get().sessionsByAgentId[agentId]?.contentStatus ?? "unknown"}/${
            get().sessionsByAgentId[agentId]?.syncStatus ?? "idle"
          }`,
        }).end("ok");
      } catch (error) {
        set((state) => ({
          sessionsByAgentId: {
            ...state.sessionsByAgentId,
            [agentId]: {
              ...state.sessionsByAgentId[agentId],
              loading: false,
              syncStatus: "error",
              error: error instanceof Error ? error.message : String(error),
            },
          },
        }));
      }
      return;
    }
    await get().refreshAgentDetail(agentId, displayLevel, { trace, trigger: "agent.open" });
  },

  refreshAgentDetail: async (agentId, displayLevel, options = {}) => {
    if (!agentId) {
      return;
    }

    const request = captureClientRequest();
    const trace =
      options.trace ??
      createRuntimeTrace("manual.refresh", {
        agentId,
        trigger: options.trigger ?? "manual.refresh",
      });
    const span = startRuntimeSpan(trace, "agent.detail", { force: Boolean(options.force) });
    const key = `${agentId}:${displayLevel}`;
    const existing = agentDetailRefreshInFlight.get(key);
    if (existing?.generation === request.generation) {
      span.end("deduped");
      return existing.promise;
    }
    const sequence = (agentDetailRequestSequence.get(agentId) ?? 0) + 1;
    agentDetailRequestSequence.set(agentId, sequence);
    set((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...emptyAgentSession(),
          ...state.sessionsByAgentId[agentId],
          loading: !state.sessionsByAgentId[agentId]?.detail,
          syncStatus: "refreshing",
          error: undefined,
        },
      },
    }));

    let promise!: Promise<void>;
    promise = (async () => {
      try {
        const detail = await request.client.getAgentDetail(agentId, displayLevel);
        if (
          !isCurrentClientRequest(request) ||
          agentDetailRequestSequence.get(agentId) !== sequence
        ) return;
        set((state) => mergeAgentDetailIntoSession(state, agentId, detail));
        startRuntimeSpan(trace, "ui.session_state_transition", {
          state: `${get().sessionsByAgentId[agentId]?.contentStatus ?? "unknown"}/${
            get().sessionsByAgentId[agentId]?.syncStatus ?? "idle"
          }`,
        }).end(detail.error ? "error" : "ok");
        await loadTargetAgentEventWindow(get, set, agentId, displayLevel);
        if (
          !isCurrentClientRequest(request) ||
          agentDetailRequestSequence.get(agentId) !== sequence
        ) return;
        scheduleMessageHydration(get, set, agentId, displayLevel);
        scheduleTranscriptHydration(get, set, agentId, displayLevel);
        scheduleBriefHydration(get, set, agentId, displayLevel);
        scheduleCacheWrite(get, agentId);
        span.end(detail.error ? "error" : "ok", {
          eventCount: detail.events?.length ?? 0,
        });
      } catch (error) {
        if (
          !isCurrentClientRequest(request) ||
          agentDetailRequestSequence.get(agentId) !== sequence
        ) return;
        if (isProjectionBusyError(error)) {
          set((state) => ({
            sessionsByAgentId: {
              ...state.sessionsByAgentId,
              [agentId]: {
                ...emptyAgentSession(),
                ...state.sessionsByAgentId[agentId],
                loading: false,
                syncStatus: "stale",
              },
            },
          }));
          span.end("skipped", { reason: "projection_busy" });
          return;
        }
        set((state) => ({
          sessionsByAgentId: {
            ...state.sessionsByAgentId,
            [agentId]: {
              ...emptyAgentSession(),
              ...state.sessionsByAgentId[agentId],
              loading: false,
              liveStatus: "error",
              syncStatus: "error",
              error: error instanceof Error ? error.message : String(error),
            },
          },
        }));
        span.end("error");
      } finally {
        const current = agentDetailRefreshInFlight.get(key);
        if (current?.promise === promise) {
          agentDetailRefreshInFlight.delete(key);
        }
      }
    })();
    agentDetailRefreshInFlight.set(key, { generation: request.generation, promise });
    return promise;
  },

  refreshAgentWorkItems: async (agentId) => {
    if (!agentId || workItemRefreshInFlight.has(agentId)) return;
    const request = captureClientRequest();
    workItemRefreshInFlight.add(agentId);
    try {
      const workItems = await request.client.getAgentWorkItems(agentId, { limit: 50 });
      if (!isCurrentClientRequest(request)) return;
      set((state) => mergeAgentWorkItemsIntoState(state, agentId, workItems));
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
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
      if (isCurrentClientRequest(request)) {
        workItemRefreshInFlight.delete(agentId);
      }
    }
  },

  refreshAgentState: async (agentId) => {
    if (!agentId || agentStateRefreshInFlight.has(agentId)) return;
    const request = captureClientRequest();
    agentStateRefreshInFlight.set(agentId, request.generation);
    try {
      const freshAgent = await request.client.getAgentState(agentId);
      if (!isCurrentClientRequest(request)) return;
      set((state) => mergeAgentStateIntoState(state, agentId, freshAgent));
    } catch {
      // Swallow — state refresh is best-effort; the next full detail refresh will recover.
    } finally {
      if (
        isCurrentClientRequest(request) &&
        agentStateRefreshInFlight.get(agentId) === request.generation
      ) {
        agentStateRefreshInFlight.delete(agentId);
      }
    }
  },

  loadAgentWorkItemDetail: async (agentId, workItemId) => {
    if (!agentId || !workItemId) return;
    const request = captureClientRequest();
    const key = `${agentId}:${workItemId}`;
    const cached = get().sessionsByAgentId[agentId]?.workItemDetailsById[workItemId];
    if (cached?.workItem || cached?.loading || workItemDetailInFlight.has(key)) return;
    workItemDetailInFlight.add(key);
    setWorkItemDetailState(set, agentId, workItemId, { loading: true, error: undefined });
    try {
      const workItem = await request.client.getAgentWorkItem(agentId, workItemId);
      if (!isCurrentClientRequest(request)) return;
      setWorkItemDetailState(set, agentId, workItemId, { loading: false, workItem });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      setWorkItemDetailState(set, agentId, workItemId, {
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      if (isCurrentClientRequest(request)) {
        workItemDetailInFlight.delete(key);
      }
    }
  },

  loadAgentTaskDetail: async (agentId, taskId, force) => {
    if (!agentId || !taskId) return;
    const request = captureClientRequest();
    const key = `${agentId}:${taskId}`;
    const cached = get().sessionsByAgentId[agentId]?.taskDetailsById[taskId];
    if (!force && (cached?.output || cached?.loading || taskDetailInFlight.has(key))) return;
    taskDetailInFlight.add(key);
    setTaskDetailState(set, agentId, taskId, { loading: !cached?.output, error: undefined });
    try {
      const [statusResult, outputResult] = await Promise.allSettled([
        request.client.getTaskStatus(agentId, taskId),
        request.client.getTaskOutput(agentId, taskId),
      ]);
      if (!isCurrentClientRequest(request)) return;
      const status = statusResult.status === "fulfilled" ? statusResult.value : undefined;
      const output = outputResult.status === "fulfilled" ? outputResult.value : undefined;
      const error = statusResult.status === "rejected" && outputResult.status === "rejected"
        ? (statusResult.reason instanceof Error ? statusResult.reason.message : String(statusResult.reason))
        : undefined;
      setTaskDetailState(set, agentId, taskId, { loading: false, status, output, error });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      setTaskDetailState(set, agentId, taskId, {
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      if (!isCurrentClientRequest(request)) {
        return;
      }
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

  loadAgentToolExecutionDetail: async (agentId, toolExecutionId, fallbackActivity) => {
    if (!agentId || !toolExecutionId) return;
    const request = captureClientRequest();
    const key = `${agentId}:${toolExecutionId}`;
    const cached = get().sessionsByAgentId[agentId]?.toolExecutionDetailsById[toolExecutionId];
    if (cached?.toolExecution || cached?.loading || toolExecutionDetailInFlight.has(key)) return;
    toolExecutionDetailInFlight.add(key);
    setToolExecutionDetailState(set, agentId, toolExecutionId, { loading: true, error: undefined });
    try {
      const toolExecution = await request.client.getToolExecution(agentId, toolExecutionId);
      if (!isCurrentClientRequest(request)) return;
      setToolExecutionDetailState(set, agentId, toolExecutionId, { loading: false, toolExecution });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      // If the tool execution record doesn't exist (e.g. historical events
      // without tool_execution_id), fall back to the activity inspector
      // which renders structured detail from the raw event payload.
      if (fallbackActivity) {
        set((state) => {
          const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
          return {
            rightPanelViewStack: stack,
            rightPanelView: { kind: "activity_inspector", agentId, activity: fallbackActivity },
          };
        });
        void hydrateInspectorActivityDetail(get, set, agentId, fallbackActivity);
        return;
      }
      setToolExecutionDetailState(set, agentId, toolExecutionId, {
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      if (!isCurrentClientRequest(request)) {
        return;
      }
      toolExecutionDetailInFlight.delete(key);
      const selection = get().rightPanelView;
      if (selection?.kind === "tool_execution_detail" && selection.agentId === agentId && selection.toolExecutionId === toolExecutionId) {
        const detail = get().sessionsByAgentId[agentId]?.toolExecutionDetailsById[toolExecutionId];
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

    const request = captureClientRequest();
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
      const page = await request.client.getAgentEvents(agentId, {
        beforeSeq: session.oldestSeq,
        limit: 80,
        order: "desc",
        displayLevel,
      });
      if (!isCurrentClientRequest(request)) return;

      set((state) =>
        mergeEventPageIntoSession(
          state,
          agentId,
          page.events ?? [],
          page.oldest_seq ?? undefined,
          page.has_older,
          displayLevel,
          { eventLogEpoch: page.event_log_epoch },
        ),
      );
      scheduleMessageHydration(get, set, agentId, displayLevel);
      scheduleTranscriptHydration(get, set, agentId, displayLevel);
      scheduleBriefHydration(get, set, agentId, displayLevel);
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
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

  sendOperatorPrompt: async (agentId, text, displayLevel, attachments = []) => {
    const prompt = text.trim();
    if (!agentId || (!prompt && attachments.length === 0)) {
      return;
    }

    const request = captureClientRequest();
    const clientId = crypto.randomUUID();
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
              clientId,
            ),
          },
        },
      };
    });

    try {
      const { messageId } = await request.client.sendOperatorPrompt(agentId, prompt, attachments);
      if (!isCurrentClientRequest(request)) return;
      scheduleBootstrapRefresh(get, 250);
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            sendingPrompt: false,
            promptError: undefined,
            detail: confirmOptimisticOperatorPrompt(
              state.sessionsByAgentId[agentId]?.detail ?? null,
              clientId,
              messageId,
            ),
          },
        },
      }));
      if (!isAgentEventStreamActive(agentId, get().sessionsByAgentId[agentId]?.liveStatus)) {
        void catchUpAgentEvents(get, set, agentId, displayLevel);
      }
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
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
    const request = captureClientRequest();
    const previousAgent = get().sessionsByAgentId[agentId]?.detail?.agent;
    setSessionModelError(set, agentId, undefined);
    try {
      const modelState = await request.client.setAgentModel(agentId, model, reasoningEffort);
      if (!isCurrentClientRequest(request)) return;
      set((state) =>
        updateAgentModelInState(state, agentId, {
          model: modelState?.active_model ?? modelState?.effective_model ?? model,
          modelSource: modelState?.source ?? "agent_override",
          modelReasoningEffort: modelState?.override_reasoning_effort ?? undefined,
        }),
      );
      await get().refreshAgentDetail(agentId, displayLevel);
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
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
    const request = captureClientRequest();
    const previousAgent = get().sessionsByAgentId[agentId]?.detail?.agent;
    setSessionModelError(set, agentId, undefined);
    try {
      const modelState = await request.client.clearAgentModel(agentId);
      if (!isCurrentClientRequest(request)) return;
      set((state) =>
        updateAgentModelInState(state, agentId, {
          model: modelState?.active_model ?? modelState?.effective_model ?? "runtime default",
          modelSource: modelState?.source ?? "runtime_default",
          modelReasoningEffort: modelState?.override_reasoning_effort ?? undefined,
        }),
      );
      await get().refreshAgentDetail(agentId, displayLevel);
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
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
    const request = captureClientRequest();
    const session = get().sessionsByAgentId[agentId] ?? emptyAgentSession();
    if (session.detail?.error) return;

    const reconnectAttempt = session.reconnectAttempt ?? 0;
    setStreamState(set, agentId, reconnectAttempt > 0 ? "reconnecting" : "connecting", {
      reconnectAttempt,
      error: undefined,
    });
    const subscription = request.client.streamAgentEvents(agentId, {
      afterSeq: highestSeq(session.eventSeqs) ?? session.newestSeq ?? 0,
      limit: 100,
      onOpen: () => {
        if (!isCurrentClientRequest(request)) return;
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
        if (!isCurrentClientRequest(request)) return;
        markStreamActivity(set, agentId);
        scheduleStaleWatchdog(get, set, agentId, displayLevel);
      },
      onEvent: (event) => {
        if (!isCurrentClientRequest(request)) return;
        markStreamActivity(set, agentId);
        enqueueStreamEvent(set, agentId, event);
      },
      onClose: () => {
        if (isCurrentClientRequest(request)) {
          scheduleStreamReconnect(get, set, agentId, displayLevel, "event stream closed");
        }
      },
      onError: (error) => {
        if (isCurrentClientRequest(request)) {
          scheduleStreamReconnect(get, set, agentId, displayLevel, error.message);
        }
      },
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
    registerAgentForEvents(get, set, agentId, false);
  },
  unregisterAgentForEvents: (agentId) => {
    unregisterAgentForEvents(agentId);
  },
}));

// Initialize session cache on first load.
if (typeof window !== "undefined") {
  installRuntimeTraceDebugApi();
  initSessionCacheForRemote((partial) => useRuntimeStore.setState(partial), () => useRuntimeStore.getState());
  resumeReconciliationCoordinator = installResumeReconciliationListeners();
  void resumeReconciliationCoordinator;
}

// Resume polling for any skill install jobs persisted from a previous session.
if (typeof window !== "undefined") {
  resumeSkillInstallJobPolling(useRuntimeStore.setState, useRuntimeStore.getState);
}

function installResumeReconciliationListeners(): ResumeReconciliationCoordinator {
  const coordinator = new ResumeReconciliationCoordinator(
    () => useRuntimeStore.getState().reconcileAfterResume(),
    window,
    100,
  );
  let hiddenAt: number | undefined;
  const scheduleIfVisible = () => {
    if (document.visibilityState === "hidden") {
      hiddenAt = Date.now();
      return;
    }
    if (
      document.visibilityState === "visible" &&
      hiddenAt != null &&
      Date.now() - hiddenAt >= RESUME_RECONCILIATION_THRESHOLD_MS
    ) {
      coordinator.schedule();
    }
    hiddenAt = undefined;
  };
  const schedulePageShow = (event: PageTransitionEvent) => {
    if (event.persisted) coordinator.schedule();
  };
  const scheduleOnline = () => coordinator.schedule();
  document.addEventListener("visibilitychange", scheduleIfVisible);
  window.addEventListener("pageshow", schedulePageShow);
  window.addEventListener("online", scheduleOnline);
  return coordinator;
}

function emptyAgentSession(): AgentSessionState {
  return {
    ...createSessionProjectionState(),
    loading: false,
    loadingOlder: false,
    liveStatus: "idle",
    cacheStatus: "unchecked",
    contentStatus: "unknown",
    syncStatus: "idle",
    sendingPrompt: false,
    detail: null,
    workItemDetailsById: {},
    taskDetailsById: {},
    toolExecutionDetailsById: {},
  };
}

function applyProjectionAction(
  current: AgentSessionState,
  action: SessionProjectionAction,
  displayLevel: DisplayLevel = "debug",
  detailBase: AgentDetail | null = current.detail,
): AgentSessionState {
  const projection = reduceSessionProjection(current, action);
  return {
    ...current,
    ...projection,
    detail: materializeProjectionDetail(detailBase, projection, displayLevel),
  };
}

export function materializeProjectionDetail(
  detail: AgentDetail | null,
  projection: SessionProjectionState,
  displayLevel: DisplayLevel,
): AgentDetail | null {
  if (!detail) return null;
  const projectedTimeline = deriveSessionTimeline(projection, displayLevel);
  const projectedMessageIds = new Set(
    projectedTimeline.flatMap((item) =>
      item.kind === "operator" && item.id.startsWith("message:")
        ? [item.id.slice("message:".length)]
        : [],
    ),
  );
  const optimisticItems = detail.timeline.filter((item) => {
    if (!item.sourceIds.includes(OPTIMISTIC_OPERATOR_PROMPT_SOURCE)) return false;
    const canonicalMessageId = item.sourceIds
      .find((sourceId) => sourceId.startsWith(OPTIMISTIC_OPERATOR_MESSAGE_PREFIX))
      ?.slice(OPTIMISTIC_OPERATOR_MESSAGE_PREFIX.length);
    return !canonicalMessageId || !projectedMessageIds.has(canonicalMessageId);
  });
  const timeline = compactAgentTimelineItems([
    ...projectedTimeline,
    ...optimisticItems,
  ]).sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
  return {
    ...detail,
    timeline,
    events: projectionEvents(projection),
    eventLogEpoch: projection.eventLogEpoch,
    newestEventSeq: projection.newestSeq,
    oldestEventSeq: projection.oldestSeq,
    briefRecordsById: projection.briefRecordsById,
  };
}

function eventLogEpochFromEvents(events: StreamEventEnvelopeDto[]): string | undefined {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const epoch = events[index]?.event_log_epoch;
    if (epoch) return epoch;
  }
  return undefined;
}

function shouldResetForEventLogEpoch(
  current: AgentSessionState,
  incomingEpoch: string | undefined,
): boolean {
  if (!incomingEpoch) return false;
  return (
    (current.eventLogEpoch != null || current.eventSeqs.length > 0) &&
    current.eventLogEpoch !== incomingEpoch
  );
}

export function sessionForEventLogEpoch(
  current: AgentSessionState,
  incomingEpoch: string | undefined,
): AgentSessionState {
  if (!incomingEpoch) return current;
  if (!shouldResetForEventLogEpoch(current, incomingEpoch)) {
    return current.eventLogEpoch === incomingEpoch
      ? current
      : { ...current, eventLogEpoch: incomingEpoch };
  }
  const reset = applyProjectionAction(current, { type: "reset", eventLogEpoch: incomingEpoch });
  return {
    ...reset,
    hasOlder: undefined,
    detail: reset.detail
      ? {
          ...reset.detail,
          eventCursorSeq: undefined,
          hasOlderEvents: undefined,
        }
      : null,
  };
}

export function hasEventIdentityConflict(
  current: AgentSessionState,
  incomingEvents: StreamEventEnvelopeDto[],
): boolean {
  return eventIdentityConflicts(current, incomingEvents);
}

function resetSessionForEventConflict(
  current: AgentSessionState,
  eventLogEpoch?: string,
): AgentSessionState {
  return {
    ...applyProjectionAction(current, {
      type: "reset",
      eventLogEpoch: eventLogEpoch ?? current.eventLogEpoch,
      reason: "event_identity_conflict",
    }),
    liveStatus: "stale",
    hasOlder: undefined,
    error: "runtime event identity conflict; refreshing projection",
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

  set({ globalStreamStatus: "connecting" });
  const trace = createRuntimeTrace("stream.connect", { trigger: "stream.connect" });
  const connectSpan = startRuntimeSpan(trace, "sse.connect");
  const request = captureClientRequest();
  const subscription = request.client.streamGlobalEvents({
    onOpen: () => {
      if (!isCurrentClientRequest(request)) return;
      globalStreamReconnectAttempt = 0;
      set({ globalStreamStatus: "streaming" });
      connectSpan.end("ok");
      scheduleGlobalStaleWatchdog(get, set);
    },
    onActivity: () => {
      if (!isCurrentClientRequest(request)) return;
      scheduleGlobalStaleWatchdog(get, set);
    },
    onEvent: (event) => {
      if (!isCurrentClientRequest(request)) return;
      scheduleGlobalStaleWatchdog(get, set);
      dispatchGlobalStreamEvent(set, event);
    },
    onClose: () => {
      if (isCurrentClientRequest(request)) {
        scheduleGlobalStreamReconnect(get, set, "global event stream closed");
      }
    },
    onError: (error) => {
      if (isCurrentClientRequest(request)) {
        scheduleGlobalStreamReconnect(get, set, error.message);
      }
    },
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
  set({ globalStreamStatus: "idle" });
  // Flush any pending events for all agents.
  for (const agentId of globalStreamSubscribedAgents) {
    flushStreamEvents(set, agentId);
  }
}

function registerAgentForEvents(
  get: () => RuntimeStoreState,
  set: StoreSet,
  agentId: string,
  backfill = true,
): void {
  const wasSubscribed = globalStreamSubscribedAgents.has(agentId);
  globalStreamSubscribedAgents.add(agentId);
  // Initialize seq tracking from existing session state.
  const session = wasSubscribed ? undefined : get().sessionsByAgentId[agentId];
  if (session && !globalEventRecovery.snapshotFor(agentId)) {
    const lastSeq = highestSeq(session.eventSeqs) ?? session.newestSeq;
    globalEventRecovery.register(agentId, lastSeq, session.eventLogEpoch);
  }
  // Start global stream if not running.
  startGlobalEventStream(get, set);
  // Initial backfill from the last known seq.
  if (!wasSubscribed && backfill) void backfillAgentEvents(set, agentId);
}

function unregisterAgentForEvents(agentId: string): void {
  globalStreamSubscribedAgents.delete(agentId);
  globalEventRecovery.unregister(agentId);
}

function syncGlobalEventRoster(get: () => RuntimeStoreState, set: StoreSet): void {
  const agentIds = new Set(get().bootstrap.agents.map((agent) => agent.id));
  for (const agentId of Array.from(globalStreamSubscribedAgents)) {
    if (!agentIds.has(agentId)) unregisterAgentForEvents(agentId);
  }
  for (const agentId of agentIds) {
    registerAgentForEvents(get, set, agentId, false);
  }
}

function dispatchGlobalStreamEvent(set: StoreSet, event: StreamEventEnvelopeDto): void {
  const agentId = event.agent_id;
  if (!agentId || !globalStreamSubscribedAgents.has(agentId)) return;

  const incomingEpoch = event.event_log_epoch || undefined;
  const session = useRuntimeStore.getState().sessionsByAgentId[agentId];
  if (session && shouldResetForEventLogEpoch(session, incomingEpoch)) {
    pendingStreamEvents.delete(agentId);
  }
  const seq = event.event_seq;
  if (seq != null) {
    const recovery = globalEventRecovery.observe(agentId, seq, incomingEpoch);
    if (recovery.recovering) {
      setAgentLiveStatus(set, agentId, "recovering");
      void backfillAgentEvents(set, agentId);
    }
  }

  enqueueStreamEvent(set, agentId, event);
}

async function backfillAgentEvents(set: StoreSet, agentId: string, force = false): Promise<void> {
  const generation = clientGeneration;
  const span = startRuntimeSpan(
    createRuntimeTrace("stream.reconnect", { agentId, trigger: "events.backfill" }),
    "events.backfill",
    { force },
  );
  let eventCount = 0;
  try {
    await recoverEventGap(globalEventRecovery, agentId, {
      force,
      limit: GLOBAL_BACKFILL_LIMIT,
      fetchPage: async (afterSeq) => {
        const page = await runtimeClient.getAgentEvents(agentId, {
          afterSeq,
          order: "asc",
          limit: GLOBAL_BACKFILL_LIMIT,
        });
        if (!isCurrentClientGeneration(generation)) return { eventLogEpoch: page.event_log_epoch, events: [] };
        return {
          eventLogEpoch: page.event_log_epoch,
          events: (page.events ?? [])
            .filter((event) => event.event_seq != null)
            .map((event) => streamEventFromBackfill(event, agentId, page.event_log_epoch)),
        };
      },
      applyEvents: (events) => {
        eventCount += events.length;
        if (isCurrentClientGeneration(generation)) {
          applyStreamEvents(set, agentId, events);
        }
      },
    });
    span.end("ok", { eventCount });
  } catch {
    span.end("error");
    // Silently ignore backfill errors; the stream will retry.
  }
}

export function streamEventFromBackfill(
  event: StreamEventEnvelopeDto,
  agentId: string,
  pageEventLogEpoch: string,
): StreamEventEnvelopeDto {
  return {
    ...event,
    event_log_epoch: event.event_log_epoch || pageEventLogEpoch,
    agent_id: agentId,
  };
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
  set({ globalStreamStatus: "reconnecting" });
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
  const request = captureClientRequest();
  inspectorDetailInFlight.add(key);
  setInspectorActivityDetailState(set, agentId, activity.id, { loading: true });

  // Use allSettled so a 404 on one fetch (e.g. historical tool execution
  // without a persisted record) doesn't wipe out the other detail.
  void Promise.allSettled([
    refs.toolExecutionId ? request.client.getToolExecution(agentId, refs.toolExecutionId) : Promise.resolve(undefined),
    refs.taskId ? request.client.getTaskOutput(agentId, refs.taskId) : Promise.resolve(undefined),
  ])
    .then(([toolExecResult, taskOutputResult]) => {
      if (!isCurrentClientRequest(request)) return;
      const toolExecution = toolExecResult.status === "fulfilled" ? toolExecResult.value : undefined;
      const taskOutput = taskOutputResult.status === "fulfilled" ? taskOutputResult.value : undefined;
      setInspectorActivityDetailState(set, agentId, activity.id, {
        loading: false,
        toolExecution,
        taskOutput,
      });
    })
    .catch((error) => {
      if (!isCurrentClientRequest(request)) return;
      setInspectorActivityDetailState(set, agentId, activity.id, {
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    })
    .finally(() => {
      if (!isCurrentClientRequest(request)) return;
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

function setToolExecutionDetailState(
  set: StoreSet,
  agentId: string,
  toolExecutionId: string,
  detailState: ToolExecutionDetailState,
): void {
  set((state) => {
    const session = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
    const previous = session.toolExecutionDetailsById[toolExecutionId] ?? {};
    return {
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...session,
          toolExecutionDetailsById: {
            ...session.toolExecutionDetailsById,
            [toolExecutionId]: {
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
    toolExecutionId:
      activity.stateObjectRef?.kind === "tool_execution"
        ? activity.stateObjectRef.id
        : firstStringField(payload, ["tool_execution_id", "toolExecutionId"]),
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
  const incomingEpoch = event.event_log_epoch || undefined;
  const pendingEpoch = pending ? eventLogEpochFromEvents(pending) : undefined;
  if (pending && incomingEpoch && pendingEpoch && incomingEpoch !== pendingEpoch) {
    pendingStreamEvents.set(agentId, [event]);
    return;
  }
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

function resumeSkillInstallJobPolling(
  set: StoreSet,
  get: () => RuntimeStoreState,
): void {
  const request = captureClientRequest();
  for (const job of get().skillInstallJobs) {
    if (job.status === "queued" || job.status === "running") {
      void pollSkillInstallJob(set, get, request, job.jobId);
    }
  }
}

async function pollSkillInstallJob(
  set: StoreSet,
  get: () => RuntimeStoreState,
  request: ClientRequest,
  jobId: string,
): Promise<void> {
  const deadline = Date.now() + SKILL_JOB_POLL_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (!isCurrentClientRequest(request)) return;
    try {
      await new Promise((resolve) => globalThis.setTimeout(resolve, SKILL_JOB_POLL_INTERVAL_MS));
      if (!isCurrentClientRequest(request)) return;
      const job = await request.client.getJob(jobId);
      if (!isCurrentClientRequest(request)) return;
      if (job.status === "completed") {
        updateSkillInstallJob(set, jobId, "completed", undefined, job.summary);
        await get().refreshSkillCatalog();
        return;
      }
      if (job.status === "failed") {
        updateSkillInstallJob(set, jobId, "failed", job.error || job.summary, job.summary);
        return;
      }
      updateSkillInstallJob(
        set,
        jobId,
        job.status === "running" ? "running" : "queued",
        undefined,
        job.summary,
      );
    } catch {
      // Network error — keep retrying until deadline
    }
  }
  updateSkillInstallJob(set, jobId, "failed", "Timed out waiting for skill job.");
}

const TEMPLATE_SYNC_POLL_INTERVAL_MS = 1_000;
const TEMPLATE_SYNC_POLL_TIMEOUT_MS = 120_000;

/**
 * Poll the daemon job created by `POST /templates/remote-sources/sync` until
 * it completes, fails, or times out. Throws on failure so the caller can
 * surface the error via `templateCatalogError`.
 */
async function pollTemplateSyncJob(
  request: ClientRequest,
  jobId: string,
): Promise<void> {
  const deadline = Date.now() + TEMPLATE_SYNC_POLL_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (!isCurrentClientRequest(request)) return;
    await new Promise((resolve) => globalThis.setTimeout(resolve, TEMPLATE_SYNC_POLL_INTERVAL_MS));
    if (!isCurrentClientRequest(request)) return;
    const job = await request.client.getJob(jobId);
    if (!isCurrentClientRequest(request)) return;
    if (job.status === "completed") {
      return;
    }
    if (job.status === "failed") {
      throw new Error(job.error || job.summary || "Template remote source sync failed.");
    }
    // status is "queued" or "running" — continue polling
  }
  throw new Error("Timed out waiting for template remote source sync.");
}

function updateSkillInstallJob(
  set: StoreSet,
  jobId: string,
  status: SkillInstallJob["status"],
  error?: string,
  summary?: string,
): void {
  set((state) => {
    const jobs = retainSkillJobs(
      state.skillInstallJobs.map((j) =>
        j.jobId === jobId ? { ...j, status, error, summary } : j
      ),
    );
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
  if (!canApplySessionEvent(event)) return current;
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
    { label: "dashboard.agents", value: String(agents.length) },
    { label: "dashboard.needsAttention", value: String(attentionCount), tone: attentionCount > 0 ? "attention" : "muted" },
    { label: "dashboard.activeTasks", value: String(activeTaskCount), tone: activeTaskCount > 0 ? "attention" : "muted" },
    { label: "dashboard.currentWork", value: String(currentWorkCount) },
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
  const epochSession = sessionForEventLogEpoch(
    state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
    detail.eventLogEpoch,
  );
  const pageEvents = detail.events ?? [];
  const current = hasEventIdentityConflict(epochSession, pageEvents)
    ? resetSessionForEventConflict(epochSession, detail.eventLogEpoch)
    : epochSession;
  const liveDetailIsNewer = (current.newestSeq ?? 0) > Math.max(detail.eventCursorSeq ?? 0, detail.newestEventSeq ?? 0);
  const agent = liveDetailIsNewer && current.detail ? mergeNewerLiveAgentState(detail.agent, current.detail.agent) : detail.agent;
  const detailBase: AgentDetail = {
    ...detail,
    agent,
    timeline: current.detail?.timeline ?? detail.timeline,
    hasOlderEvents: detail.hasOlderEvents,
  };
  let projected = applyProjectionAction(current, {
    type: "events_received",
    events: pageEvents,
    eventLogEpoch: detail.eventLogEpoch,
  }, "debug", detailBase);
  if (detail.transcriptEntriesById) {
    projected = applyProjectionAction(projected, {
      type: "transcripts_hydrated",
      entries: Object.values(detail.transcriptEntriesById),
      missingIds: [],
    }, "debug", projected.detail);
  }
  if (detail.briefRecordsById) {
    projected = applyProjectionAction(projected, {
      type: "briefs_hydrated",
      recordsById: detail.briefRecordsById,
      missingIds: [],
    }, "debug", projected.detail);
  }
  const newestSeq = Math.max(detail.newestEventSeq ?? 0, projected.newestSeq ?? 0);

  return {
    bootstrap:
      detail.source === "http" && !detail.error
        ? sortBootstrapAgents(mergeAgentIntoBootstrap(state.bootstrap, agent), state.rosterActivityByAgentId)
        : state.bootstrap,
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...projected,
        loading: false,
        liveStatus: detail.error ? "error" : current.liveStatus,
        cacheStatus: current.cacheStatus === "unchecked" ? "miss" : current.cacheStatus,
        contentStatus: detail.error
          ? projected.eventSeqs.length
            ? "available"
            : "unknown"
          : projected.eventSeqs.length
            ? "available"
            : "confirmed-empty",
        syncStatus: detail.error ? "error" : "idle",
        lastValidatedAt: detail.error ? current.lastValidatedAt : Date.now(),
        newestSeq: newestSeq || undefined,
        oldestSeq: detail.oldestEventSeq ?? projected.oldestSeq,
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
  trace = createRuntimeTrace("events.catch_up", { agentId, trigger: "events.catch_up" }),
): Promise<void> {
  const existing = agentEventCatchUpInFlight.get(agentId);
  if (existing) {
    startRuntimeSpan(trace, "events.catch_up").end("deduped");
    return existing;
  }
  const span = startRuntimeSpan(trace, "events.catch_up");
  const request = (async () => {
    const generation = clientGeneration;
    const afterSeq = get().sessionsByAgentId[agentId]?.newestSeq;
    const page = await runtimeClient.getAgentEvents(agentId, {
      afterSeq,
      limit: 100,
      order: "asc",
    });
    if (!isCurrentClientGeneration(generation)) {
      span.end("cancelled");
      return;
    }
    set((state) =>
      mergeEventPageIntoSession(state, agentId, page.events ?? [], page.oldest_seq ?? undefined, page.has_older, "debug", {
        newestSeq: page.cursor_seq ?? page.newest_seq ?? undefined,
        append: true,
        eventLogEpoch: page.event_log_epoch,
      }),
    );
    if ((page.events ?? []).some(isWorkItemCacheInvalidationEvent)) {
      void useRuntimeStore.getState().refreshAgentWorkItems(agentId);
    }
    if ((page.events ?? []).some(isAgentStateCacheInvalidationEvent)) {
      void useRuntimeStore.getState().refreshAgentState(agentId);
    }
    if (get().selectedAgentId === agentId) {
      scheduleMessageHydration(get, set, agentId, "debug");
      scheduleTranscriptHydration(get, set, agentId, "debug");
      scheduleBriefHydration(get, set, agentId, "debug");
    }
    scheduleCacheWrite(get, agentId);
    span.end("ok", {
      afterSeq,
      eventCount: page.events?.length ?? 0,
    });
  })().catch((error) => {
    span.end("error");
    throw error;
  }).finally(() => {
    if (agentEventCatchUpInFlight.get(agentId) === request) {
      agentEventCatchUpInFlight.delete(agentId);
    }
  });
  agentEventCatchUpInFlight.set(agentId, request);
  return request;
}

function applyStreamEvents(set: StoreSet, agentId: string, events: StreamEventEnvelopeDto[]): void {
  const incomingEpoch = eventLogEpochFromEvents(events);
  const incomingEvents = events.filter(
    (event) =>
      event.event_seq != null &&
      (!incomingEpoch || !event.event_log_epoch || event.event_log_epoch === incomingEpoch),
  );
  if (!incomingEvents.length) return;
  const currentSnapshot = useRuntimeStore.getState().sessionsByAgentId[agentId];
  if (currentSnapshot && hasEventIdentityConflict(currentSnapshot, incomingEvents)) {
    pendingStreamEvents.delete(agentId);
    globalEventRecovery.unregister(agentId);
    set((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: resetSessionForEventConflict(
          state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
          incomingEpoch,
        ),
      },
    }));
    void useRuntimeStore.getState().refreshAgentDetail(agentId, useRuntimeStore.getState().displayLevel);
    return;
  }
  const liveStatus = globalEventRecovery.snapshotFor(agentId)?.recovering ? "recovering" : "streaming";

  set((state) => {
    const current = sessionForEventLogEpoch(
      state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
      incomingEpoch,
    );
    const uniqueEvents = incomingEvents.filter((event) => !current.eventsBySeq[event.event_seq as number]);
    if (!uniqueEvents.length) {
      return {
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...current,
            liveStatus,
            error: undefined,
          },
        },
      };
    }
    const projectionEvents = uniqueEvents.filter(canApplySessionEvent);
    const rosterActivityByAgentId = projectionEvents.reduce(
      (activityByAgentId, event) =>
        touchRosterActivityFromEvent(activityByAgentId, agentId, event, state.route === "agent" ? state.selectedAgentId : ""),
      state.rosterActivityByAgentId,
    );
    if (rosterActivityByAgentId !== state.rosterActivityByAgentId) {
      writeStoredRosterActivity(currentRemoteKey(runtimeConnectionConfig), rosterActivityByAgentId);
    }
    const baseDetail = current.detail ?? createLiveAgentDetail(state.bootstrap.agents.find((agent) => agent.id === agentId));
    const runPatch = agentRunPatchFromEvents(projectionEvents);
    const briefPatch = agentBriefPatchFromEvents(projectionEvents, current.briefRecordsById);
    const patchedBaseDetail = patchAgentDetail(baseDetail, runPatch, briefPatch);
    const projected = applyProjectionAction(current, {
      type: "events_received",
      events: uniqueEvents,
      eventLogEpoch: incomingEpoch,
    }, "debug", patchedBaseDetail);

    return {
      bootstrap: sortBootstrapAgents(
        patchBootstrapAgent(state.bootstrap, agentId, runPatch, briefPatch),
        rosterActivityByAgentId,
      ),
      rosterActivityByAgentId,
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...projected,
          liveStatus,
          error: undefined,
        },
      },
    };
  });
  if (useRuntimeStore.getState().selectedAgentId === agentId) {
    scheduleMessageHydration(useRuntimeStore.getState, set, agentId, useRuntimeStore.getState().displayLevel);
    scheduleTranscriptHydration(useRuntimeStore.getState, set, agentId, useRuntimeStore.getState().displayLevel);
    scheduleBriefHydration(useRuntimeStore.getState, set, agentId, useRuntimeStore.getState().displayLevel);
  }
  scheduleCacheWrite(useRuntimeStore.getState, agentId);
  if (events.some((event) => canApplySessionEvent(event) && isWorkItemCacheInvalidationEvent(event))) {
    void useRuntimeStore.getState().refreshAgentWorkItems(agentId);
  }
  if (events.some((event) => canApplySessionEvent(event) && isAgentStateCacheInvalidationEvent(event))) {
    void useRuntimeStore.getState().refreshAgentState(agentId);
  }
}

function isWorkItemCacheInvalidationEvent(event: StreamEventEnvelopeDto): boolean {
  // Match all work_item_written events so updated/picked actions also refresh the cache.
  return event.type === "work_item_written";
}

function isAgentStateCacheInvalidationEvent(event: StreamEventEnvelopeDto): boolean {
  return (
    event.type === "agent_state_changed" ||
    event.type === "state_changed" ||
    event.type === "message_processing_started" ||
    event.type === "turn_terminal" ||
    event.type === "work_item_written" ||
    event.type === "task_created" ||
    event.type === "task_status_updated" ||
    event.type === "task_result_received" ||
    event.type === "workspace_entered" ||
    event.type === "workspace_used" ||
    event.type === "workspace_attached" ||
    event.type === "workspace_detached" ||
    event.type === "workspace_exited" ||
    event.type === "worktree_entered" ||
    event.type === "worktree_exited"
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

  const hydrationSpan = startRuntimeSpan(
    createRuntimeTrace("object.hydration", { agentId, trigger: "message.hydration" }),
    "object.hydration",
    { resource: "message", idCount: requestIds.length },
  );
  const generation = clientGeneration;
  void runtimeClient
    .getAgentMessagesBatch(agentId, requestIds)
    .then((response) => {
      if (!isCurrentClientGeneration(generation)) return;
      set((state) => mergeHydratedMessagesIntoSession(state, agentId, response.messages ?? [], response.missing_message_ids ?? [], displayLevel));
      hydrationSpan.end("ok", { returnedCount: response.messages?.length ?? 0 });
    })
    .catch((error) => {
      if (!isCurrentClientGeneration(generation)) return;
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
      if (!isCurrentClientGeneration(generation)) return;
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

  const hydrationSpan = startRuntimeSpan(
    createRuntimeTrace("object.hydration", { agentId, trigger: "transcript.hydration" }),
    "object.hydration",
    { resource: "transcript", idCount: requestIds.length },
  );
  const generation = clientGeneration;
  void runtimeClient
    .getAgentTranscriptEntriesBatch(agentId, requestIds)
    .then((response) => {
      if (!isCurrentClientGeneration(generation)) return;
      set((state) =>
        mergeHydratedTranscriptEntriesIntoSession(
          state,
          agentId,
          response.entries ?? [],
          response.missing_entry_ids ?? [],
          displayLevel,
        ),
      );
      hydrationSpan.end("ok", { returnedCount: response.entries?.length ?? 0 });
    })
    .catch((error) => {
      if (!isCurrentClientGeneration(generation)) return;
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
      if (!isCurrentClientGeneration(generation)) return;
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

  const hydrationSpan = startRuntimeSpan(
    createRuntimeTrace("object.hydration", { agentId, trigger: "brief.hydration" }),
    "object.hydration",
    { resource: "brief", idCount: requestIds.length },
  );
  const generation = clientGeneration;
  void runtimeClient
    .getAgentBriefsById(agentId, requestIds)
    .then(({ recordsById, notFoundIds }) => {
      if (!isCurrentClientGeneration(generation)) return;
      set((state) => mergeHydratedBriefRecordsIntoSession(state, agentId, recordsById, notFoundIds, displayLevel));
      hydrationSpan.end("ok", { returnedCount: Object.keys(recordsById).length });
    })
    .catch((error) => {
      if (!isCurrentClientGeneration(generation)) return;
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
      if (!isCurrentClientGeneration(generation)) return;
      const current = briefHydrationInFlight.get(agentId);
      if (!current) return;
      requestIds.forEach((briefId) => current.delete(briefId));
      if (!current.size) briefHydrationInFlight.delete(agentId);
    });
}

export function agentBriefPatchFromEvents(
  events: StreamEventEnvelopeDto[],
  briefRecordsById: Record<string, RuntimeBriefRecord> = {},
): Pick<AgentSummary, "lastBrief" | "lastTurnTime"> | undefined {
  let patch: Pick<AgentSummary, "lastBrief" | "lastTurnTime"> | undefined;
  for (const event of events) {
    if (!canApplySessionEvent(event)) continue;
    if (event.type !== "brief_created") continue;
    const payload = asRecord(event.payload);
    const briefId = briefIdForPayload(payload);
    const resolvedText = (briefId ? briefRecordsById[briefId]?.text : undefined) ?? stringField(payload, "text");
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
  return session ? messageIdsForProjectionHydration(session).slice(-100) : [];
}

function missingTranscriptEntryIdsForHydration(session: AgentSessionState | undefined): string[] {
  return session ? transcriptEntryIdsForProjectionHydration(session).slice(-100) : [];
}

export function missingBriefIdsForHydration(session: AgentSessionState | undefined): string[] {
  return session ? briefIdsForProjectionHydration(session).slice(-100) : [];
}

function mergeHydratedMessagesIntoSession(
  state: RuntimeStoreState,
  agentId: string,
  messages: RuntimeMessageEnvelope[],
  missingMessageIds: string[],
  displayLevel: DisplayLevel,
): Partial<RuntimeStoreState> {
  const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  if (!messages.length && !missingMessageIds.length) return {};
  const projected = applyProjectionAction(current, {
    type: "messages_hydrated",
    messages,
    missingIds: missingMessageIds,
  }, displayLevel);

  return {
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: projected,
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
  if (!entries.length && !missingEntryIds.length) return {};
  let projected = applyProjectionAction(current, {
    type: "transcripts_hydrated",
    entries,
    missingIds: missingEntryIds,
  }, displayLevel);
  const briefPatch = agentBriefPatchFromEvents(projectionEvents(projected), projected.briefRecordsById);
  projected = { ...projected, detail: patchAgentDetail(projected.detail, undefined, briefPatch) };

  return {
    bootstrap: patchBootstrapAgent(state.bootstrap, agentId, undefined, briefPatch),
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: projected,
    },
  };
}

function mergeHydratedBriefRecordsIntoSession(
  state: RuntimeStoreState,
  agentId: string,
  recordsById: Record<string, RuntimeBriefRecord>,
  notFoundBriefIds: string[],
  displayLevel: DisplayLevel,
): Partial<RuntimeStoreState> {
  const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  if (!Object.keys(recordsById).length && !notFoundBriefIds.length) return {};
  let projected = applyProjectionAction(current, {
    type: "briefs_hydrated",
    recordsById,
    missingIds: notFoundBriefIds,
  }, displayLevel);
  const briefPatch = agentBriefPatchFromEvents(projectionEvents(projected), projected.briefRecordsById);
  projected = { ...projected, detail: patchAgentDetail(projected.detail, undefined, briefPatch) };

  return {
    bootstrap: patchBootstrapAgent(state.bootstrap, agentId, undefined, briefPatch),
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: projected,
    },
  };
}

function agentRunPatchFromEvents(events: StreamEventEnvelopeDto[]): Pick<AgentSummary, "currentRunId" | "lifecycle"> | undefined {
  let patch: Pick<AgentSummary, "currentRunId" | "lifecycle"> | undefined;
  for (const event of events) {
    if (!canApplySessionEvent(event)) continue;
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
  options: { newestSeq?: number; append?: boolean; eventLogEpoch?: string } = {},
): Partial<RuntimeStoreState> {
  const epochSession = sessionForEventLogEpoch(
    state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
    options.eventLogEpoch,
  );
  const current = hasEventIdentityConflict(epochSession, pageEvents)
    ? resetSessionForEventConflict(epochSession, options.eventLogEpoch)
    : epochSession;
  const detailBase = current.detail
    ? {
        ...current.detail,
        hasOlderEvents: pageHasOlder,
      }
    : current.detail;
  const projected = applyProjectionAction(current, {
    type: "events_received",
    events: pageEvents,
    eventLogEpoch: options.eventLogEpoch,
  }, displayLevel, detailBase);

  return {
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...projected,
        newestSeq: Math.max(options.newestSeq ?? 0, projected.newestSeq ?? 0) || undefined,
        oldestSeq: pageOldestSeq ?? projected.oldestSeq,
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
  const generation = clientGeneration;
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
    if (!isCurrentClientGeneration(generation)) return;
    set((state) =>
      mergeEventPageIntoSession(
        state,
        agentId,
        page.events ?? [],
        page.oldest_seq ?? undefined,
        page.has_older,
        displayLevel,
        {
          newestSeq: page.cursor_seq ?? page.newest_seq ?? undefined,
          append: true,
          eventLogEpoch: page.event_log_epoch,
        },
      ),
    );
  } catch (error) {
    if (!isCurrentClientGeneration(generation)) return;
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
