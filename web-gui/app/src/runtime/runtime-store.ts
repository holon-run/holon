import i18next from "i18next";
import { hydrateInputActivity } from "./conversation-input";
import { readPanelPreferences, writePanelPreferences, rememberPanelView } from "./panel-preferences";
import { panelLayout, PANEL_DEFAULT } from "../features/right-panel/panel-layout";
import { clearConversationCaches } from "./conversation-cache-lifecycle";
import type { CurrentUser } from "./client";
import { create } from "zustand";

import {
  createRuntimeClient,
  isProjectionBusyError,
  isTimeoutAbortError,
  projectRosterAgents,
  RuntimeHttpError,
  type BriefReadStateDto,
  type AgentEventStreamSubscription,
  type OperatorPromptAttachment,
  type AgentRosterSnapshotDto,
  type StreamEventEnvelopeDto,
} from "./client";
import {
  AgentSessionRepository,
  isSessionCacheContextCurrent,
  snapshotRepairFromClient,
} from "./agent-session-repository";
import {
  applyProjectionAction,
  emptyAgentSession,
  eventLogEpochFromEvents,
  hasEventIdentityConflict,
  materializeProjectionDetail,
  mergeEventPageIntoConversation,
  resetSessionForEventConflict,
  sessionForEventLogEpoch,
} from "./conversation-store";
import {
  backfillRetryDelayMs,
  GlobalSyncCoordinator,
  runWithConcurrencyLimit,
  streamEventFromBackfill,
} from "./global-sync-coordinator";
import { ReadStateBus } from "./event-ledger/read-state-bus";
import {
  readStoredRosterActivity,
  touchRosterActivity,
  touchRosterActivityFromEvent,
  writeStoredRosterActivity,
  evaluateLedgerReadMarkerGate,
  type AgentRosterActivity,
} from "./read-state";
import {
  CACHE_SCHEMA_VERSION,
  cacheGetModelCatalog,
  cachePutModelCatalog,
} from "./idb-cache";
import { ResumeReconciliationCoordinator } from "./resume-reconciliation";
import { currentRemoteKey } from "./session-cache";
import { resolveConversationScopeKey, conversationScopeSnapshot } from "./conversation-scope-store";
import {
  createRuntimeTrace,
  installRuntimeTraceDebugApi,
  startRuntimeSpan,
  type RuntimeTraceContext,
} from "./runtime-trace";
import type { AgentSessionState as AgentSessionStateBase } from "./runtime-store-helpers";
import {
  briefIdForPayload,
} from "./session-reducer";
import { generateUuid } from "./uuid";
import {
  deriveSessionTimeline,
  projectionEvents,
  type SessionProjectionAction,
} from "./session-projection";
import { canApplySessionEvent } from "./session-events";
import type {
  AddSkillInput,
  AgentDetail,
  AgentControlAction,
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
  RuntimeConfigState,
  RuntimeLocalOnnxPresetStatus,
  TaskStatusSnapshot,
  CodexDeviceLoginState,
  CredentialMutationResult,
  CredentialStoreState,
  RuntimeBriefRecord,
  RuntimeTaskOutputResult,
  RuntimeMessageEnvelope,
  RuntimeModelCatalog,
  RosterDiscoveryIdentity,
  RosterDiscoveryState,
  RuntimeSearchOptions,
  SkillCatalogState,
  SkillDetailState,
  RuntimeTranscriptEntry,
  RuntimeToolExecutionRecord,
  FileReference,
  ResolveFileReferencesResponse,
  WorkItemSummary,
  SearchResponse,
  WorkspaceBrowserLocation,
  FileOpenTarget,
  WorkspaceDirectoryListing,
  WorkspaceFileContent,
  WorkspaceFileLocation,
  WorkspacePathInfo,
  ToolExecutionArtifactContent,
} from "./types";

import type {
  AgentLiveStatus,
  AgentSessionState,
  TimelineEventsState,
  WorkItemDetailState,
  TaskDetailState,
  ToolExecutionDetailState,
} from "./runtime-store-helpers";
export type { AgentLiveStatus, AgentSessionState, TimelineEventsState };
export {
  hasEventIdentityConflict,
  materializeProjectionDetail,
  sessionForEventLogEpoch,
} from "./conversation-store";
export {
  readStoredRosterActivity,
  touchRosterActivityFromEvent,
} from "./read-state";
export {
  backfillRetryDelayMs,
  runWithConcurrencyLimit,
  streamEventFromBackfill,
} from "./global-sync-coordinator";
export { isSessionCacheContextCurrent } from "./agent-session-repository";
export type { AgentRosterActivity } from "./read-state";

export interface BootstrapRefreshOptions {
  background?: boolean;
  syncEvents?: boolean;
  trace?: RuntimeTraceContext;
  trigger?: string;
}

export interface AgentDetailRefreshOptions {
  retry?: boolean;
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

const OPTIMISTIC_OPERATOR_PROMPT_SOURCE = "pending-operator-prompt";
const OPTIMISTIC_OPERATOR_CLIENT_PREFIX = "operator-prompt-client:";
const OPTIMISTIC_OPERATOR_MESSAGE_PREFIX = "operator-prompt-message:";
const MAX_SEMANTIC_HISTORY_PAGES_PER_LOAD = 5;

export function appendOptimisticOperatorPrompt(
  detail: AgentDetail | null,
  agent: AgentSummary | undefined,
  prompt: string,
  clientId: string,
  senderName: string | undefined,
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
        senderName,
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
  selectedSkillAgentId: string;
  selectedTemplateId: string;
  rightPanelViewStack: RightPanelView[];
  rightPanelOpen: boolean;
  rightPanelMode: "normal" | "expanded";
  // navCollapsed snapshot restored when the expanded panel returns to normal.
  rightPanelExpandedNavWasCollapsed?: boolean;
  rightPanelView?: RightPanelView;
  timelineEventsByAgentId: Record<string, TimelineEventsState>;
  navCollapsed: boolean;

  bootstrap: RuntimeBootstrap;
  bootstrapLoading: boolean;
  /** Identity behind the current session, used to attribute pending operator prompts. */
  currentUser?: CurrentUser;
  currentUserLoaded: boolean;
  bootstrapError?: string;
  globalStreamStatus: "idle" | "connecting" | "catching_up" | "streaming" | "reconnecting";
  discovery: RosterDiscoveryState;
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
  /** Server-authoritative unread state keyed by public agent id. */
  briefReadStateByAgentId: Record<string, BriefReadStateDto>;
  briefReadStatesLoading: boolean;
  briefReadStatesError?: string;
  sessionsByAgentId: Record<string, AgentSessionState>;
  skillInstallJobs: SkillInstallJob[];
  resumeRevision: number;

  setRoute: (route: RouteKey) => void;
  openAgent: (agentId: string) => void;
  markAgentConversationRead: (agentId: string) => void;
  refreshBriefReadStates: () => Promise<void>;
  openSkill: (skillId: string, agentId?: string) => void;
  openTemplate: (catalogId: string) => void;
  disableDeveloperDiagnosticsUi: () => void;
  setRightPanelOpen: (open: boolean) => void;
  toggleRightPanelExpanded: () => void;
  showAgentOverview: (agentId?: string) => void;
  showTimelineEvents: (agentId: string) => void;
  refreshTimelineEvents: (agentId: string) => Promise<void>;
  loadOlderTimelineEvents: (agentId: string) => Promise<void>;
  showWorkItemDetail: (agentId: string, workItem: WorkItemSummary) => void;
  showTaskDetail: (agentId: string, task: TaskSummary) => void;
  showToolExecutionDetail: (agentId: string, toolExecutionId: string, toolName?: string, relatedStateObjectRef?: TimelineStateObjectRef) => void;
  inspectActivity: (agentId: string, activity: AgentTimelineActivity) => void;
  showFileBrowser: (agentId: string, location: WorkspaceBrowserLocation) => void;
  openResolvedFile: (agentId: string, location: FileOpenTarget) => void;
  resolveFileReferences: (references: FileReference[]) => Promise<ResolveFileReferencesResponse>;
  browseWorkspaceDir: (location: WorkspaceFileLocation) => Promise<WorkspaceDirectoryListing>;
  readWorkspaceFile: (location: WorkspaceFileLocation) => Promise<WorkspaceFileContent>;
  fetchWorkspacePath: (location: WorkspaceFileLocation) => Promise<WorkspacePathInfo>;
  readToolExecutionArtifact: (
    agentId: string,
    toolExecutionId: string,
    artifactIndex: number,
  ) => Promise<ToolExecutionArtifactContent>;
  fetchWorkspaceFileBlob: (location: WorkspaceFileLocation, options?: { download?: boolean; timeoutMs?: number }) => Promise<Blob>;
  workspaceFileUrl: (location: WorkspaceFileLocation, options?: { download?: boolean }) => string;
  navigateBack: () => void;
  restoreRightPanelView: (view: RightPanelView) => void;
  toggleRightPanel: () => void;
  toggleNavCollapsed: () => void;
  setRuntimeConnection: (config: RuntimeConnectionConfig) => Promise<void>;
  refreshBootstrap: (options?: BootstrapRefreshOptions) => Promise<void>;
  reconcileAfterResume: () => Promise<void>;
  refreshModelCatalog: (options?: { refresh: boolean }) => Promise<void>;
  refreshRuntimeConfig: () => Promise<void>;
  updateRuntimeConfig: (updates: Array<{ key: string; value?: unknown; unset?: boolean }>) => Promise<RuntimeConfigState | undefined>;
  downloadLocalOnnxPreset: (preset: string) => Promise<RuntimeLocalOnnxPresetStatus>;
  cancelLocalOnnxPreset: (preset: string) => Promise<RuntimeLocalOnnxPresetStatus>;
  refreshSkillCatalog: () => Promise<void>;
  refreshSkillDetail: (skillId: string | undefined, agentId?: string) => Promise<void>;
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
  setCredential: (profile: string, kind: string, material: string) => Promise<CredentialMutationResult | undefined>;
  deleteCredential: (profile: string) => Promise<void>;
  startCodexDeviceLogin: (providerId?: string) => Promise<void>;
  clearCodexDeviceLogin: () => void;
  runSearch: (query: string, options?: RuntimeSearchOptions) => Promise<void>;
  loadSearchResultContent: (sourceRef: string) => Promise<void>;
  refreshAgentDetail: (agentId: string | undefined, options?: AgentDetailRefreshOptions) => Promise<void>;
  refreshAgentWorkItems: (agentId: string | undefined) => Promise<void>;
  refreshAgentState: (agentId: string | undefined) => Promise<void>;
  loadAgentWorkItemDetail: (agentId: string | undefined, workItemId: string | undefined) => Promise<void>;
  loadAgentTaskDetail: (agentId: string | undefined, taskId: string | undefined, force?: boolean) => Promise<void>;
  loadAgentToolExecutionDetail: (agentId: string | undefined, toolExecutionId: string | undefined, fallbackActivity?: AgentTimelineActivity, conversationRevision?: number) => Promise<void>;
  sendOperatorPrompt: (agentId: string | undefined, text: string, attachments?: OperatorPromptAttachment[]) => Promise<void>;
  abortCurrentRun: (agentId: string | undefined, runId: string | null | undefined) => Promise<void>;
  setAgentModel: (agentId: string | undefined, model: string, reasoningEffort?: string) => Promise<void>;
  clearAgentModel: (agentId: string | undefined) => Promise<void>;
  controlAgent: (agentId: string | undefined, action: AgentControlAction) => Promise<void>;
  deleteAgent: (agentId: string | undefined, cascadePrivateChildren?: boolean) => Promise<void>;
  renameAgent: (agentId: string | undefined, name: string) => Promise<void>;
  // Debug inspector compatibility path; globalSyncCoordinator owns default correctness.
  startAgentEventStream: (agentId: string | undefined, displayLevel: DisplayLevel) => void;
  stopAgentEventStream: (agentId: string | undefined) => void;
  startGlobalEventStream: () => void;
  stopGlobalEventStream: () => void;
  registerAgentForEvents: (agentId: string) => void;
  unregisterAgentForEvents: (agentId: string) => void;
  retryAgentSync: (agentId: string) => void;
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
    // Drop skill-catalog error placeholders so the fetch effect re-runs against
    // the new client generation; clean catalogs survive as last-known-good.
    agentSkillCatalogByAgentId: Object.fromEntries(
      Object.entries(state.agentSkillCatalogByAgentId).filter(([, catalog]) => !catalog.error),
    ),
    credentialStoreLoading: false,
    codexDeviceLogin: { status: "idle" },
    searchLoading: false,
    searchResultContentLoadingBySourceRef: resetBooleanMap(state.searchResultContentLoadingBySourceRef),
    rightPanelView: resetRightPanelLoading(state.rightPanelView),
    rightPanelViewStack: state.rightPanelViewStack.map(resetRightPanelLoading).filter((view): view is RightPanelView => view !== undefined),
    timelineEventsByAgentId: Object.fromEntries(
      Object.entries(state.timelineEventsByAgentId).map(([agentId, timelineEvents]) => [
        agentId,
        { ...timelineEvents, loading: false, loadingOlder: false },
      ]),
    ),
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

/**
 * Current runtime connection config (module-level source of truth). Auxiliary
 * clients such as the conversation SDK scope store read this to build
 * transport with identical base URL and bearer semantics.
 */
export function getRuntimeConnectionConfig(): RuntimeConnectionConfig {
  return runtimeConnectionConfig;
}

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

const reconnectTimers = new Map<string, number>();
const staleTimers = new Map<string, number>();
const inspectorDetailInFlight = new Set<string>();
const workItemRefreshInFlight = new Set<string>();
const workItemDetailInFlight = new Set<string>();
const taskDetailInFlight = new Set<string>();
const toolExecutionDetailInFlight = new Set<string>();
const agentStateRefreshInFlight = new Map<string, number>();
const agentDetailRefreshInFlight = new Map<string, { generation: number; promise: Promise<void> }>();
const agentDetailRequestSequence = new Map<string, number>();
const agentDetailRetryTimers = new Map<string, number>();
const agentDetailRetryAttempts = new Map<string, number>();
const agentSkillCatalogRetryTimers = new Map<string, number>();
const agentSkillCatalogRetryAttempts = new Map<string, number>();
let bootstrapRefreshInFlight: Promise<void> | undefined;
let bootstrapRefreshTimer: number | undefined;
let clientGeneration = 0;
let resumeReconciliationInFlight: Promise<void> | undefined;
let resumeReconciliationCoordinator: ResumeReconciliationCoordinator | undefined;
const STREAM_STALE_TIMEOUT_MS = 45_000;
const STREAM_RECONNECT_BASE_MS = 1_000;
const STREAM_RECONNECT_MAX_MS = 15_000;
const AGENT_VALIDATION_TTL_MS = 60_000;
const RESUME_RECONCILIATION_THRESHOLD_MS = 60_000;
const AGENT_DETAIL_RETRY_DELAYS_MS = [2_000, 5_000, 15_000] as const;
const AGENT_SKILL_CATALOG_RETRY_DELAYS_MS = [2_000, 5_000, 15_000] as const;

function nextClientGeneration(): number {
  clientGeneration += 1;
  return clientGeneration;
}

function isCurrentClientGeneration(generation: number): boolean {
  return generation === clientGeneration;
}

type RuntimeClient = ReturnType<typeof createRuntimeClient>;

export function skillDetailCacheKey(skillId: string, agentId?: string): string {
  return agentId ? `${agentId}\u0000${skillId}` : skillId;
}

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

async function observeRuntimeConfigReload(
  request: ClientRequest,
  targetGeneration: number,
): Promise<RuntimeConfigState> {
  let delayMs = 100;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const runtimeConfig = await request.client.getRuntimeConfig();
    const reload = runtimeConfig.reload;
    if (
      targetGeneration === 0
      || !reload
      || reload.completedGeneration >= targetGeneration
      || reload.state === "failed"
    ) {
      return runtimeConfig;
    }
    await new Promise((resolve) => globalThis.setTimeout(resolve, delayMs));
    delayMs = Math.min(delayMs * 2, 1_000);
  }
  return request.client.getRuntimeConfig();
}

function cancelClientGenerationWork(): void {
  bootstrapRefreshInFlight = undefined;
  if (bootstrapRefreshTimer != null) {
    window.clearTimeout(bootstrapRefreshTimer);
    bootstrapRefreshTimer = undefined;
  }
  agentStateRefreshInFlight.clear();
  agentDetailRefreshInFlight.clear();
  agentDetailRequestSequence.clear();
  for (const timer of agentDetailRetryTimers.values()) window.clearTimeout(timer);
  agentDetailRetryTimers.clear();
  agentDetailRetryAttempts.clear();
  for (const timer of agentSkillCatalogRetryTimers.values()) window.clearTimeout(timer);
  agentSkillCatalogRetryTimers.clear();
  agentSkillCatalogRetryAttempts.clear();
  for (const timer of readMarkerRetryTimers.values()) window.clearTimeout(timer);
  readMarkerRetryTimers.clear();
  readMarkerRetryAttempts.clear();
  pendingReadMarkerAgentIds.clear();
  pendingReadMarkerCursorByAgentId.clear();
  readMarkerAdvanceQueued.clear();
  readMarkerAdvanceInFlight.clear();
  globalSyncCoordinator.cancelClientGenerationWork();
  inspectorDetailInFlight.clear();
  workItemRefreshInFlight.clear();
  workItemDetailInFlight.clear();
  taskDetailInFlight.clear();
  toolExecutionDetailInFlight.clear();
}


function closeEventStreamsForResume(set: StoreSet): void {
  globalSyncCoordinator.closeForResume(set);
  for (const agentId of Array.from(activeEventStreams.keys())) {
    stopAgentEventStream(agentId, set);
  }
  for (const timer of reconnectTimers.values()) window.clearTimeout(timer);
  reconnectTimers.clear();
  for (const timer of staleTimers.values()) window.clearTimeout(timer);
  staleTimers.clear();
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
        sendingPrompt: false,
        abortingRun: false,
        liveStatus: "stale" as const,
        reconnectAttempt: 0,
        briefHydrationById: Object.fromEntries(
          Object.entries(session.briefHydrationById).map(([briefId, hydration]) => [
            briefId,
            hydration.status === "loading" ? { ...hydration, status: "pending" as const } : hydration,
          ]),
        ),
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

function runtimeClientOptions(config: RuntimeConnectionConfig) {
  return config.mode === "remote"
    ? { mode: "remote" as const, baseUrl: config.baseUrl, token: config.token }
    : { mode: "local" as const, token: config.token };
}

// The GUI always talks to the server that serves this page. Old remote
// selections and their credentials must never be applied to the current site.
function sameOriginConnection(config: RuntimeConnectionConfig | undefined): RuntimeConnectionConfig {
  return config?.mode === "local"
    ? { mode: "local", token: typeof config.token === "string" ? config.token.trim() || undefined : undefined }
    : { mode: "local" };
}

export function readStoredRuntimeConnectionConfig(): RuntimeConnectionConfig {
  if (typeof window === "undefined") return { mode: "local" };
  const active = readStoredJson(window.sessionStorage, ACTIVE_RUNTIME_CONNECTION_STORAGE_KEY) as RuntimeConnectionConfig | undefined;
  const legacy = readStoredJson(window.localStorage, LEGACY_RUNTIME_CONNECTION_STORAGE_KEY) as RuntimeConnectionConfig | undefined;
  const config = sameOriginConnection(active ?? legacy);
  writeStoredRuntimeConnectionConfig(config);
  return config;
}

export function writeStoredRuntimeConnectionConfig(config: RuntimeConnectionConfig): void {
  if (typeof window === "undefined") return;
  removeStoredItem(window.localStorage, LEGACY_RUNTIME_CONNECTION_STORAGE_KEY);
  try {
    window.sessionStorage.setItem(ACTIVE_RUNTIME_CONNECTION_STORAGE_KEY, JSON.stringify(sameOriginConnection(config)));
  } catch {
    // Ignore storage failures; the in-memory connection still applies.
  }
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

/** Drop stale bearer credentials when browser session authentication takes over. */
export function clearStoredRuntimeConnectionToken(): void {
  writeStoredRuntimeConnectionConfig({ mode: "local" });
}

const emptyBootstrap: RuntimeBootstrap = {
  attentionCount: 0,
  connection: {
    mode: "local",
    source: "fixture",
    summary: "Connecting to this Holon server…",
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
      summary: config.mode === "remote" ? "Connecting to remote runtime…" : "Connecting to this Holon server…",
    },
  };
}

const emptyModelCatalog: RuntimeModelCatalog = {
  source: "fixture",
  options: [],
};

function freshModelCatalog(catalog: RuntimeModelCatalog): RuntimeModelCatalog {
  return {
    ...catalog,
    source: catalog.source === "fixture" ? "fixture" : "http",
    stale: false,
    cachedAt: Date.now(),
    error: undefined,
  };
}

function persistModelCatalog(config: RuntimeConnectionConfig, catalog: RuntimeModelCatalog): void {
  if (catalog.source !== "http" || catalog.error) return;
  void cachePutModelCatalog({
    remoteKey: modelCatalogCacheKey(config),
    schemaVersion: CACHE_SCHEMA_VERSION,
    options: catalog.options,
    cachedAt: catalog.cachedAt ?? Date.now(),
  });
}

async function hydrateCachedModelCatalog(
  config: RuntimeConnectionConfig,
  generation: number,
): Promise<void> {
  const cached = await cacheGetModelCatalog(modelCatalogCacheKey(config));
  if (!cached || !isCurrentClientGeneration(generation)) return;
  useRuntimeStore.setState({
    modelCatalog: {
      source: "cache",
      options: cached.options as RuntimeModelCatalog["options"],
      stale: true,
      cachedAt: cached.cachedAt,
    },
    modelCatalogError: undefined,
  });
}

export function modelCatalogCacheKey(config: RuntimeConnectionConfig): string {
  const token = config.token?.trim();
  if (!token) return `${currentRemoteKey(config)}#anonymous`;
  let hash = 0x811c9dc5;
  for (let index = 0; index < token.length; index += 1) {
    hash ^= token.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return `${currentRemoteKey(config)}#auth-${(hash >>> 0).toString(16).padStart(8, "0")}`;
}

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
 * Atomically apply one authoritative roster snapshot (W4 discovery):
 * validate identity against the last applied roster, replace the roster,
 * reset sessions when the runtime/scope/epoch rotated, and purge the
 * cache of agents the authoritative roster omitted. Returns the omitted
 * agent ids so the coordinator can unregister them.
 */
async function applyRosterSnapshotToStore(
  set: StoreSet,
  snapshot: AgentRosterSnapshotDto,
  context: { previousIdentity?: RosterDiscoveryIdentity },
): Promise<string[]> {
  const identity: RosterDiscoveryIdentity = {
    runtimeId: snapshot.runtime_id,
    visibilityScopeId: snapshot.visibility_scope_id,
    eventLogEpoch: snapshot.event_log_epoch,
  };
  const identityReset =
    context.previousIdentity != null &&
    (context.previousIdentity.runtimeId !== identity.runtimeId ||
      context.previousIdentity.visibilityScopeId !== identity.visibilityScopeId ||
      context.previousIdentity.eventLogEpoch !== identity.eventLogEpoch);
  let omittedAgentIds: string[] = [];
  set((state) => {
    const cachedAgentsById = cachedAgentsByIdFromState(state);
    const rosterAgents = projectRosterAgents(snapshot).map((agent) => {
      const cached = cachedAgentsById[agent.id];
      return cached ? mergeBootstrapAgentState(agent, cached) : agent;
    });
    const rosterIds = new Set(rosterAgents.map((agent) => agent.id));
    const previousIds = state.bootstrap.agents.map((agent) => agent.id);
    omittedAgentIds = previousIds.filter((id) => !rosterIds.has(id));
    const dropIds = new Set(identityReset ? previousIds : omittedAgentIds);
    const sessionsByAgentId = dropIds.size
      ? Object.fromEntries(
          Object.entries(state.sessionsByAgentId).filter(([id]) => !dropIds.has(id)),
        )
      : state.sessionsByAgentId;
    const rosterActivityByAgentId = dropIds.size
      ? Object.fromEntries(
          Object.entries(state.rosterActivityByAgentId).filter(([id]) => !dropIds.has(id)),
        )
      : state.rosterActivityByAgentId;
    if (rosterActivityByAgentId !== state.rosterActivityByAgentId) {
      writeStoredRosterActivity(currentRemoteKey(runtimeConnectionConfig), rosterActivityByAgentId);
    }
    const briefReadStateByAgentId = dropIds.size
      ? Object.fromEntries(
          Object.entries(state.briefReadStateByAgentId).filter(([id]) => !dropIds.has(id)),
        )
      : state.briefReadStateByAgentId;
    const timelineEventsByAgentId = dropIds.size
      ? Object.fromEntries(
          Object.entries(state.timelineEventsByAgentId).filter(([id]) => !dropIds.has(id)),
        )
      : state.timelineEventsByAgentId;
    const attentionCount = countAgentsNeedingAttention(rosterAgents);
    return {
      bootstrap: sortBootstrapAgents(
        {
          ...state.bootstrap,
          agents: rosterAgents,
          attentionCount,
          metrics: buildBootstrapMetrics(rosterAgents),
        },
        rosterActivityByAgentId,
      ),
      discovery: {
        ...state.discovery,
        mode: "authoritative",
        identity,
      },
      sessionsByAgentId,
      rosterActivityByAgentId,
      briefReadStateByAgentId,
      timelineEventsByAgentId,
    };
  });
  if (identityReset) {
    // Old-identity ledger scopes must never join the new scope's data.
    try {
      await agentSessionRepository.clearLedgerScopesNotMatching(identity);
    } catch (error) {
      console.warn("Failed to clear old ledger scopes.", error);
    }
  }
  for (const agentId of omittedAgentIds) {
    stopAgentEventStream(agentId, set);
    void agentSessionRepository
      .purgeAgentLedger(agentId)
      .catch((error) => console.warn("Failed to purge omitted agent ledger.", error));
  }
  return omittedAgentIds;
}

const globalSyncCoordinator = new GlobalSyncCoordinator<RuntimeStoreState>({
  applyStreamEvents,
  captureClientRequest,
  isCurrentClientRequest,
  setAgentLiveStatus,
  setStreamState,
  fetchRosterSnapshot: async (request) => {
    return request.client.getAgentRosterSnapshot();
  },
  applyRosterSnapshot: (set, snapshot, context) =>
    applyRosterSnapshotToStore(set, snapshot, {
      previousIdentity: context.previousIdentity,
    }),
  registerAgentRecovery: (agentId, hint) => {
    void agentSessionRepository
      .syncAgentRecovery(agentId, hint)
      .catch((error) => console.warn(`Agent ledger recovery failed for ${agentId}.`, error));
  },
});

let agentSessionRepository!: AgentSessionRepository<RuntimeStoreState>;

/** Read-only observer-sync status used by the e2e diagnostics bridge. */
export function ledgerStatusForDiagnostics(agentId: string) {
  return agentSessionRepository.sessionLedgerStatus(agentId);
}

export interface ObserverSyncAgentDiagnostics {
  agentId: string;
  durability: string;
  state: string;
  ingestedThroughSeq?: number;
  projectionReadyThroughSeq?: number;
  observedEventHeadSeq?: number;
  pendingHydrationJobs: number;
  failedHydrationJobs: number;
  resetReason?: string;
  readCertainty: "exact" | "truncated" | "stale_sync_error" | "unavailable";
}

export interface ObserverSyncDiagnostics {
  discovery: {
    mode: RosterDiscoveryState["mode"];
    freshness: RosterDiscoveryState["freshness"];
    runtimeId?: string;
    visibilityScopeId?: string;
    eventLogEpoch?: string;
  };
  agents: ObserverSyncAgentDiagnostics[];
}

/**
 * Safe observer-sync diagnostics for the current authorized roster only.
 * Deliberately excludes connection credentials, event payloads, Brief text,
 * and any Agent not present in the applied roster.
 */
export function observerSyncDiagnostics(): ObserverSyncDiagnostics {
  const state = useRuntimeStore.getState();
  return {
    discovery: {
      mode: state.discovery.mode,
      freshness: state.discovery.freshness,
      runtimeId: state.discovery.identity?.runtimeId,
      visibilityScopeId: state.discovery.identity?.visibilityScopeId,
      eventLogEpoch: state.discovery.identity?.eventLogEpoch,
    },
    agents: state.bootstrap.agents.map((agent) => {
      const status = agentSessionRepository.sessionLedgerStatus(agent.id);
      return {
        agentId: agent.id,
        durability: status?.durability ?? "unavailable",
        state: status?.state ?? "unavailable",
        ingestedThroughSeq: status?.ingestedThroughSeq,
        projectionReadyThroughSeq: status?.projectionReadyThroughSeq,
        observedEventHeadSeq: status?.observedEventHeadSeq,
        pendingHydrationJobs: status?.pendingHydrationJobs ?? 0,
        failedHydrationJobs: status?.failedHydrationJobs ?? 0,
        resetReason: agentSessionRepository.sessionLedgerResetReason(agent.id),
        readCertainty: state.briefReadStateByAgentId[agent.id] ? "exact" : "unavailable",
      };
    }),
  };
}

let briefReadStatesRefreshInFlight: Promise<void> | undefined;
let briefReadStatesRefreshGeneration: number | undefined;
const pendingReadMarkerAgentIds = new Set<string>();
const pendingReadMarkerCursorByAgentId = new Map<string, number>();
const readMarkerAdvanceInFlight = new Set<string>();
const readMarkerAdvanceQueued = new Set<string>();
const readMarkerRetryTimers = new Map<string, ReturnType<typeof setTimeout>>();
const readMarkerRetryAttempts = new Map<string, number>();
let readStateBus: ReadStateBus | null = null;

function ensureReadStateBus(): ReadStateBus {
  if (!readStateBus) {
    readStateBus = new ReadStateBus((message) => {
      if (message.remoteKey !== currentRemoteKey(runtimeConnectionConfig)) return;
      void refreshBriefReadStatesInView().then(() => refreshBriefReadStatesInView());
    });
  }
  return readStateBus;
}

function publishReadStateRevalidation(agentId: string): void {
  ensureReadStateBus().publish({
    kind: "server_revalidation_required",
    remoteKey: currentRemoteKey(runtimeConnectionConfig),
    agentId,
  });
}

async function refreshBriefReadStatesInView(): Promise<void> {
  // The initial local connection uses the module-level runtime config and
  // does not pass through setRuntimeConnection. Ensure dashboard-only tabs
  // subscribe before another tab publishes a read-state revalidation hint.
  ensureReadStateBus();
  const generation = clientGeneration;
  if (briefReadStatesRefreshInFlight && briefReadStatesRefreshGeneration === generation) {
    return briefReadStatesRefreshInFlight;
  }
  let request!: Promise<void>;
  request = (async () => {
    useRuntimeStore.setState({ briefReadStatesLoading: true, briefReadStatesError: undefined });
    try {
      const states = await runtimeClient.getBriefReadStates();
      if (!isCurrentClientGeneration(generation)) return;
      const currentStates = useRuntimeStore.getState().briefReadStateByAgentId;
      const nextStates = Object.fromEntries(states.map((state) => [state.agent_id, state]));
      for (const incoming of states) {
        const current = currentStates[incoming.agent_id];
        const pendingCursor = pendingReadMarkerCursorByAgentId.get(incoming.agent_id);
        if (
          current &&
          current.event_log_epoch === incoming.event_log_epoch &&
          current.visibility_scope_id === incoming.visibility_scope_id &&
          (incoming.read_through_event_seq < current.read_through_event_seq ||
            (incoming.read_through_event_seq === current.read_through_event_seq &&
              incoming.revision < current.revision &&
              incoming.event_head_seq <= current.event_head_seq))
        ) {
          nextStates[incoming.agent_id] = current;
          continue;
        }
        if (
          current &&
          pendingCursor != null &&
          incoming.read_through_event_seq < pendingCursor &&
          current.event_log_epoch === incoming.event_log_epoch &&
          current.visibility_scope_id === incoming.visibility_scope_id
        ) {
          nextStates[incoming.agent_id] = {
            ...incoming,
            read_through_event_seq: pendingCursor,
            unread_count: 0,
            revision: Math.max(incoming.revision, current.revision),
          };
        }
      }
      useRuntimeStore.setState({
        briefReadStateByAgentId: nextStates,
        briefReadStatesLoading: false,
        briefReadStatesError: undefined,
      });
    } catch (error) {
      if (!isCurrentClientGeneration(generation)) return;
      useRuntimeStore.setState({
        briefReadStatesLoading: false,
        briefReadStatesError: error instanceof Error ? error.message : String(error),
      });
    } finally {
      if (briefReadStatesRefreshInFlight === request) {
        briefReadStatesRefreshInFlight = undefined;
        briefReadStatesRefreshGeneration = undefined;
      }
    }
  })();
  briefReadStatesRefreshInFlight = request;
  briefReadStatesRefreshGeneration = generation;
  return request;
}

export async function retryPendingReadMarker(agentId: string): Promise<void> {
  if (!pendingReadMarkerAgentIds.has(agentId)) return;
  if (readMarkerAdvanceInFlight.has(agentId)) {
    readMarkerAdvanceQueued.add(agentId);
    return;
  }
  const trace = createRuntimeTrace("read_marker.advance", {
    agentId,
    trigger: "conversation.read",
  });
  const span = startRuntimeSpan(trace, "read_marker.advance");
  const generation = clientGeneration;
  const state = useRuntimeStore.getState();
  const candidateSeq = state.briefReadStateByAgentId[agentId]?.event_head_seq;
  if (candidateSeq == null) {
    span.end("skipped", { reason: "candidate_unavailable" });
    return;
  }
  const scopeKey = resolveConversationScopeKey(
    currentRemoteKey(runtimeConnectionConfig),
    agentId,
    state.currentUser,
  );
  const scope = conversationScopeSnapshot(scopeKey);
  const covered = state.rightPanelOpen && (
    state.rightPanelMode === "expanded" ||
    (typeof window !== "undefined" &&
      panelLayout(window.innerWidth, true, false, state.navCollapsed, PANEL_DEFAULT).full)
  );
  const decision = evaluateLedgerReadMarkerGate(
    {
      route: state.route,
      selectedAgentId: state.selectedAgentId,
      documentVisible: typeof document !== "undefined" && document.visibilityState === "visible",
      conversationReady:
        scope.status.kind === "ready" &&
        scope.view?.scope != null &&
        scope.view.reset_reason === null,
      conversationVisible: !covered,
      discoveryFresh: state.discovery.freshness === "fresh",
      readiness: agentSessionRepository.sessionLedgerReadiness(agentId),
    },
    agentId,
  );
  if (!decision.mayAdvance || decision.candidateSeq == null) {
    if (decision.reason === "not_selected") {
      pendingReadMarkerAgentIds.delete(agentId);
    }
    span.end("skipped", { reason: decision.reason ?? "candidate_unavailable" });
    return;
  }

  readMarkerAdvanceInFlight.add(agentId);
  try {
    const result = await runtimeClient.markBriefRead(agentId, decision.candidateSeq);
    if (!isCurrentClientGeneration(generation)) return;
    if (result.state.read_through_event_seq >= result.state.event_head_seq) {
      pendingReadMarkerAgentIds.delete(agentId);
    }
    pendingReadMarkerCursorByAgentId.delete(agentId);
    readMarkerRetryAttempts.delete(agentId);
    const retryTimer = readMarkerRetryTimers.get(agentId);
    if (retryTimer) {
      clearTimeout(retryTimer);
      readMarkerRetryTimers.delete(agentId);
    }
    useRuntimeStore.setState((current) => ({
      briefReadStateByAgentId: {
        ...current.briefReadStateByAgentId,
        [agentId]: result.state,
      },
      briefReadStatesError: undefined,
    }));
    publishReadStateRevalidation(agentId);
    span.end("ok", {
      candidateSeq,
    });
  } catch (error) {
    if (!isCurrentClientGeneration(generation)) return;
    span.end("error", {
      candidateSeq,
      error: error instanceof Error ? error.message : String(error),
    });
    useRuntimeStore.setState({
      briefReadStatesError: error instanceof Error ? error.message : String(error),
    });
    const attempt = (readMarkerRetryAttempts.get(agentId) ?? 0) + 1;
    readMarkerRetryAttempts.set(agentId, attempt);
    if (!readMarkerRetryTimers.has(agentId)) {
      const delayMs = Math.min(30_000, 1_000 * 2 ** Math.min(attempt - 1, 5));
      const retryTimer = setTimeout(() => {
        readMarkerRetryTimers.delete(agentId);
        void retryPendingReadMarker(agentId);
      }, delayMs);
      readMarkerRetryTimers.set(agentId, retryTimer);
    }
  } finally {
    readMarkerAdvanceInFlight.delete(agentId);
    if (readMarkerAdvanceQueued.delete(agentId)) {
      pendingReadMarkerAgentIds.add(agentId);
      void retryPendingReadMarker(agentId);
    }
  }
}

function collapseRightPanelExpansion(state: {
  navCollapsed: boolean;
  rightPanelExpandedNavWasCollapsed?: boolean;
}) {
  return {
    rightPanelMode: "normal" as const,
    navCollapsed: state.rightPanelExpandedNavWasCollapsed ?? false,
    rightPanelExpandedNavWasCollapsed: undefined,
  };
}

const panelPreferences = readPanelPreferences();

export const useRuntimeStore = create<RuntimeStoreState>((set, get) => {
  agentSessionRepository = new AgentSessionRepository<RuntimeStoreState>({
    get,
    set,
    getClient: () => runtimeClient,
    getConnectionConfig: () => runtimeConnectionConfig,
    getGeneration: () => clientGeneration,
    ledgerIngestion: {
      // Stable runtime identity (runtime id + visibility scope) is learned
      // from the per-Agent projection snapshot during W3 recovery; the
      // repository also seeds it from the durable restart scan. The
      // registry stays empty for remotes without the snapshot capability,
      // so the in-memory path is unchanged until the W4/W6 cutover.
      resolveScope: (agentId) => agentSessionRepository.knownLedgerScope(agentId),
      snapshotRepair: snapshotRepairFromClient((agentId) =>
        runtimeClient.getAgentProjectionSnapshot(agentId),
      ),
      fetchers: {
        fetchCanonicalRecords: async (agentId, recordKind, recordIds) => {
          if (recordKind === "message") {
            const response = await runtimeClient.getAgentMessagesBatch(agentId, recordIds);
            const recordsById: Record<string, { record: unknown }> = {};
            for (const message of response.messages ?? []) {
              if (message?.id) recordsById[message.id] = { record: message };
            }
            return { recordsById, missingIds: response.missing_message_ids ?? [] };
          }
          if (recordKind === "transcript_entry") {
            const response = await runtimeClient.getAgentTranscriptEntriesBatch(
              agentId,
              recordIds,
            );
            const recordsById: Record<string, { record: unknown }> = {};
            for (const entry of response.entries ?? []) {
              if (entry?.id) recordsById[entry.id] = { record: entry };
            }
            return { recordsById, missingIds: response.missing_entry_ids ?? [] };
          }
          const response = await runtimeClient.getAgentBriefsById(agentId, recordIds);
          const recordsById: Record<string, { record: unknown }> = {};
          for (const [briefId, record] of Object.entries(response.recordsById ?? {})) {
            recordsById[briefId] = { record };
          }
          return { recordsById, missingIds: response.notFoundIds ?? [] };
        },
      },
      onStatus: (status) => {
        void refreshBriefReadStatesInView();
        void retryPendingReadMarker(status.scope.agentId);
      },
    },
  });

  return ({
  route: "dashboard",
  selectedAgentId: "",
  selectedSkillId: "",
  selectedSkillAgentId: "",
  selectedTemplateId: "",
  rightPanelOpen: panelPreferences.open,
  rightPanelMode: panelPreferences.mode,
  rightPanelView: panelPreferences.view,
  rightPanelViewStack: [],
  timelineEventsByAgentId: {},
  navCollapsed: false,

  bootstrap: pendingBootstrap(runtimeConnectionConfig),
  bootstrapLoading: true,
  currentUser: undefined,
  currentUserLoaded: false,
  globalStreamStatus: "idle",
  discovery: {
    mode: "pending",
    freshness: "fresh",
    retryAttempt: 0,
  } satisfies RosterDiscoveryState,
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
  briefReadStateByAgentId: {},
  briefReadStatesLoading: false,
  briefReadStatesError: undefined,
  sessionsByAgentId: {},
  skillInstallJobs: loadSkillInstallJobs(),
  resumeRevision: 0,

  setRoute: (route) => set({ route }),
  openSkill: (skillId, agentId) =>
    set({
      route: "skillDetail",
      selectedSkillId: skillId,
      selectedSkillAgentId: agentId ?? "",
    }),
  openTemplate: (catalogId) => set({ route: "templateDetail", selectedTemplateId: catalogId }),
  openAgent: (agentId) =>
    set((state) => ({
      selectedAgentId: agentId,
      route: "agent",
    })),
  markAgentConversationRead: (agentId) => {
    const current = get().briefReadStateByAgentId[agentId];
    if (current) {
      pendingReadMarkerCursorByAgentId.set(
        agentId,
        Math.max(pendingReadMarkerCursorByAgentId.get(agentId) ?? 0, current.event_head_seq),
      );
      set((state) => ({
        briefReadStateByAgentId: {
          ...state.briefReadStateByAgentId,
          [agentId]: {
            ...current,
            read_through_event_seq: Math.max(current.read_through_event_seq, current.event_head_seq),
            unread_count: 0,
          },
        },
      }));
    }
    pendingReadMarkerAgentIds.add(agentId);
    void retryPendingReadMarker(agentId);
  },
  refreshBriefReadStates: async () => {
    await refreshBriefReadStatesInView();
  },
  disableDeveloperDiagnosticsUi: () =>
    set((state) => {
      const timelineEventsView =
        state.rightPanelView?.kind === "timeline_events" ? state.rightPanelView : undefined;
      return {
        rightPanelOpen: timelineEventsView ? false : state.rightPanelOpen,
        rightPanelView: timelineEventsView
          ? { kind: "agent_overview", agentId: timelineEventsView.agentId }
          : state.rightPanelView,
        rightPanelViewStack: state.rightPanelViewStack.filter((view) => view.kind !== "timeline_events"),
      };
    }),
  setRightPanelOpen: (open) =>
    set((state) => {
      if (!open && state.rightPanelMode === "expanded") {
        return { ...collapseRightPanelExpansion(state), rightPanelOpen: false };
      }
      return { rightPanelOpen: open };
    }),
  toggleRightPanelExpanded: () =>
    set((state) => {
      if (state.rightPanelMode === "expanded") {
        return collapseRightPanelExpansion(state);
      }
      return {
        rightPanelMode: "expanded",
        rightPanelOpen: true,
        rightPanelExpandedNavWasCollapsed: state.navCollapsed,
        navCollapsed: true,
      };
    }),
  showAgentOverview: (agentId) =>
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: { kind: "agent_overview", agentId: agentId ?? state.selectedAgentId },
      };
    }),
  showTimelineEvents: (agentId) => {
    set((state) => {
      if (state.rightPanelView?.kind === "timeline_events" && state.rightPanelView.agentId === agentId) {
        return { rightPanelOpen: true };
      }
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
        rightPanelViewStack: stack,
        rightPanelOpen: true,
        rightPanelView: { kind: "timeline_events", agentId },
      };
    });
    const timelineEvents = get().timelineEventsByAgentId[agentId];
    if (!timelineEvents || (!timelineEvents.loading && timelineEvents.eventSeqs.length === 0)) {
      void get().refreshTimelineEvents(agentId);
    }
  },
  refreshTimelineEvents: async (agentId) => {
    const request = captureClientRequest();
    set((state) => ({
      timelineEventsByAgentId: {
        ...state.timelineEventsByAgentId,
        [agentId]: {
          ...emptyTimelineEventsState(),
          ...state.timelineEventsByAgentId[agentId],
          loading: true,
          error: undefined,
        },
      },
    }));
    try {
      const page = await request.client.getAgentEvents(agentId, {
        limit: 80,
        order: "desc",
        displayLevel: "debug",
      });
      if (!isCurrentClientRequest(request)) return;
      set((state) => {
        const latest = state.timelineEventsByAgentId[agentId] ?? emptyTimelineEventsState();
        const preserveLoadedEvents = latest.eventSeqs.length > 0;
        return {
          timelineEventsByAgentId: {
            ...state.timelineEventsByAgentId,
            [agentId]: mergeTimelineEventPage(
              latest,
              page.events ?? [],
              page.event_log_epoch,
              preserveLoadedEvents ? latest.hasOlder && page.has_older : page.has_older,
              preserveLoadedEvents,
            ),
          },
        };
      });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      set((state) => ({
        timelineEventsByAgentId: {
          ...state.timelineEventsByAgentId,
          [agentId]: {
            ...emptyTimelineEventsState(),
            ...state.timelineEventsByAgentId[agentId],
            loading: false,
            error: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    }
  },
  loadOlderTimelineEvents: async (agentId) => {
    const current = get().timelineEventsByAgentId[agentId];
    if (!current || current.loadingOlder || !current.hasOlder || current.oldestSeq == null) return;
    const request = captureClientRequest();
    set((state) => ({
      timelineEventsByAgentId: {
        ...state.timelineEventsByAgentId,
        [agentId]: {
          ...(state.timelineEventsByAgentId[agentId] ?? current),
          loadingOlder: true,
          error: undefined,
        },
      },
    }));
    try {
      const page = await request.client.getAgentEvents(agentId, {
        beforeSeq: current.oldestSeq,
        limit: 80,
        order: "desc",
        displayLevel: "debug",
      });
      if (!isCurrentClientRequest(request)) return;
      set((state) => {
        const latest = state.timelineEventsByAgentId[agentId] ?? emptyTimelineEventsState();
        return {
          timelineEventsByAgentId: {
            ...state.timelineEventsByAgentId,
            [agentId]: mergeTimelineEventPage(
              latest,
              page.events ?? [],
              page.event_log_epoch,
              page.has_older,
              true,
            ),
          },
        };
      });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      set((state) => ({
        timelineEventsByAgentId: {
          ...state.timelineEventsByAgentId,
          [agentId]: {
            ...emptyTimelineEventsState(),
            ...state.timelineEventsByAgentId[agentId],
            loadingOlder: false,
            error: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    }
  },
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
  showFileBrowser: (agentId, location) =>
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: {
        kind: "file_browser",
        agentId,
        workspaceId: location.workspaceId,
        initialPath: location.path,
        executionRootId: location.executionRootId,
        initialFilePath: location.initialFilePath,
        fragment: location.fragment,
      },
      };
    }),
  openResolvedFile: (agentId, location) =>
    get().showFileBrowser(agentId, {
      workspaceId: location.workspaceId,
      executionRootId: location.executionRootId,
      path: location.kind === "directory"
        ? location.path
        : location.path.includes("/")
          ? location.path.slice(0, location.path.lastIndexOf("/"))
          : "",
      initialFilePath: location.kind === "file" ? location.path : undefined,
      fragment: location.fragment,
    }),
  resolveFileReferences: (references) => runtimeClient.resolveFileReferences(references),
  browseWorkspaceDir: (location) => runtimeClient.browseWorkspaceDir(location),
  readWorkspaceFile: (location) => runtimeClient.readWorkspaceFile(location),
  fetchWorkspacePath: (location) => runtimeClient.fetchWorkspacePath(location),
  readToolExecutionArtifact: (agentId, toolExecutionId, artifactIndex) =>
    runtimeClient.readToolExecutionArtifact(agentId, toolExecutionId, artifactIndex),
  fetchWorkspaceFileBlob: (location, options) => runtimeClient.fetchWorkspaceFileBlob(location, options),
  workspaceFileUrl: (location, options) => runtimeClient.workspaceFileUrl(location, options),
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
  restoreRightPanelView: (view) => set((state) => ({
    rightPanelOpen: true,
    rightPanelView: view,
    rightPanelViewStack: state.rightPanelView ? [...state.rightPanelViewStack.slice(-31), state.rightPanelView] : state.rightPanelViewStack,
  })),
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
    await clearConversationCaches();
    nextClientGeneration();
    const generation = clientGeneration;
    cancelClientGenerationWork();
    const normalizedConfig = sameOriginConnection(config);
    runtimeConnectionConfig = normalizedConfig;
    runtimeClient = createRuntimeClient(runtimeClientOptions(normalizedConfig));
    ensureReadStateBus();
    writeStoredRuntimeConnectionConfig(normalizedConfig);
    bootstrapRefreshInFlight = undefined;
    briefReadStatesRefreshInFlight = undefined;
    briefReadStatesRefreshGeneration = undefined;
    resumeReconciliationInFlight = undefined;
    for (const subscription of activeEventStreams.values()) subscription.close();
    activeEventStreams.clear();
    globalSyncCoordinator.resetForClientChange();
    agentSessionRepository.switchRemote();
    inspectorDetailInFlight.clear();
    for (const timer of reconnectTimers.values()) window.clearTimeout(timer);
    for (const timer of staleTimers.values()) window.clearTimeout(timer);
    reconnectTimers.clear();
    staleTimers.clear();
    set({
      currentUser: undefined,
      currentUserLoaded: false,
      bootstrap: pendingBootstrap(normalizedConfig),
      bootstrapLoading: true,
      bootstrapError: undefined,
      discovery: {
        mode: "pending",
        freshness: "fresh",
        retryAttempt: 0,
      },
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
      briefReadStateByAgentId: {},
      briefReadStatesLoading: false,
      briefReadStatesError: undefined,
      selectedAgentId: "",
      selectedSkillId: "",
      selectedSkillAgentId: "",
      selectedTemplateId: "",
      route: "dashboard",
      resumeRevision: get().resumeRevision + 1,
    });
    await hydrateCachedModelCatalog(normalizedConfig, generation);
    await get().refreshBootstrap();
    // Initialize cache for the new remote (async, non-blocking).
    agentSessionRepository.initializeCache();
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
        // Current-user identity is advisory: resolve it in the background so
        // pending prompts can be attributed without blocking the bootstrap.
        void runtimeClient.getCurrentUser().then((currentUser) => {
          if (!isCurrentClientGeneration(generation)) return;
          set({ currentUser: currentUser ?? undefined, currentUserLoaded: true });
        });
        set((state) => {
          if (bootstrap.connection.source === "fixture" && state.bootstrap.connection.source === "http") {
            return {
              bootstrap: state.bootstrap,
              bootstrapLoading: false,
              bootstrapError: bootstrap.connection.error,
            };
          }
          if (state.discovery.mode === "authoritative") {
            // Authoritative discovery owns roster membership; keep the
            // applied roster and refresh only connection metadata here.
            return {
              bootstrap: {
                ...state.bootstrap,
                connection: bootstrap.connection,
                capabilities: bootstrap.capabilities,
              },
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
        void refreshBriefReadStatesInView();
        if (options.syncEvents !== false) {
          if (get().discovery.mode === "authoritative") {
            globalSyncCoordinator.refreshRoster(get, set);
          } else {
            globalSyncCoordinator.syncRoster(get, set);
          }
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
        await runWithConcurrencyLimit(
          get()
            .bootstrap.agents.map((agent) => agent.id)
            .filter((agentId) => agentId === selectedAgentId),
          4,
          async (agentId) => {
            await get().refreshAgentState(agentId);
          },
          () => isCurrentClientGeneration(generation),
        );
        if (!isCurrentClientGeneration(generation)) return;
        globalSyncCoordinator.syncRoster(get, set);
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

  refreshModelCatalog: async (options) => {
    const request = captureClientRequest();
    set({ modelCatalogLoading: true, modelCatalogError: undefined });
    try {
      const modelCatalog = freshModelCatalog(await (options?.refresh === false
        ? request.client.getModels()
        : request.client.refreshModels()));
      if (!isCurrentClientRequest(request)) return;
      set({ modelCatalog, modelCatalogLoading: false, modelCatalogError: modelCatalog.error });
      persistModelCatalog(runtimeConnectionConfig, modelCatalog);
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        modelCatalog: { ...state.modelCatalog, stale: state.modelCatalog.options.length > 0, error: message },
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
      const response = await request.client.updateRuntimeConfig(updates);
      if (!isCurrentClientRequest(request)) return undefined;
      const runtimeConfig = response.changed && response.reload
        ? {
            ...response,
            ...(await observeRuntimeConfigReload(request, response.reload.requestedGeneration)),
            changed: response.changed,
            results: response.results,
          }
        : response;
      if (!isCurrentClientRequest(request)) return undefined;
      set({ runtimeConfig, runtimeConfigSaving: false, runtimeConfigError: runtimeConfig.error });
      if (runtimeConfig.changed && !runtimeConfig.error) {
        set({ modelCatalogLoading: true, modelCatalogError: undefined });
        try {
          const modelCatalog = freshModelCatalog(await request.client.getModels());
          if (!isCurrentClientRequest(request)) return runtimeConfig;
          set({ modelCatalog, modelCatalogLoading: false, modelCatalogError: modelCatalog.error });
          persistModelCatalog(runtimeConnectionConfig, modelCatalog);
        } catch (modelError) {
          if (!isCurrentClientRequest(request)) return runtimeConfig;
          const message = modelError instanceof Error ? modelError.message : String(modelError);
          set((state) => ({
            modelCatalog: { ...state.modelCatalog, stale: state.modelCatalog.options.length > 0, error: message },
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

  downloadLocalOnnxPreset: async (preset) => {
    const request = captureClientRequest();
    return request.client.downloadLocalOnnxPreset(preset);
  },

  cancelLocalOnnxPreset: async (preset) => {
    const request = captureClientRequest();
    return request.client.cancelLocalOnnxPreset(preset);
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

  refreshSkillDetail: async (skillId, agentId) => {
    if (!skillId) return;
    const cacheKey = skillDetailCacheKey(skillId, agentId);
    const request = captureClientRequest();
    set((state) => ({
      skillDetailLoadingById: { ...state.skillDetailLoadingById, [cacheKey]: true },
      skillDetailErrorById: { ...state.skillDetailErrorById, [cacheKey]: undefined },
    }));
    try {
      const detail = await request.client.getSkillDetail(skillId, agentId);
      if (!isCurrentClientRequest(request)) return;
      set((state) => ({
        skillDetailById: { ...state.skillDetailById, [cacheKey]: detail },
        skillDetailLoadingById: { ...state.skillDetailLoadingById, [cacheKey]: false },
        skillDetailErrorById: { ...state.skillDetailErrorById, [cacheKey]: detail.error },
      }));
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        skillDetailById: {
          ...state.skillDetailById,
          [cacheKey]: { source: "http", error: message },
        },
        skillDetailLoadingById: { ...state.skillDetailLoadingById, [cacheKey]: false },
        skillDetailErrorById: { ...state.skillDetailErrorById, [cacheKey]: message },
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
      if (get().discovery.mode === "authoritative") {
        // The authoritative roster owns membership; refresh the snapshot
        // so the new agent appears through the same atomic apply.
        globalSyncCoordinator.refreshRoster(get, set);
        return true;
      }
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
      clearAgentSkillCatalogRetry(agentId);
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        // Keep an error-carrying (or last-known-good) catalog in the map so
        // the App-level fetch effect terminates instead of hot-retrying;
        // recovery retries from the store with bounded backoff.
        agentSkillCatalogByAgentId: {
          ...state.agentSkillCatalogByAgentId,
          [agentId]: {
            ...(state.agentSkillCatalogByAgentId[agentId] ?? emptySkillCatalog),
            source: "http",
            error: message,
          },
        },
        agentSkillCatalogLoadingByAgentId: {
          ...state.agentSkillCatalogLoadingByAgentId,
          [agentId]: false,
        },
        agentSkillCatalogErrorByAgentId: {
          ...state.agentSkillCatalogErrorByAgentId,
          [agentId]: message,
        },
      }));
      scheduleAgentSkillCatalogRetry(get, agentId);
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
      if (!isCurrentClientRequest(request)) return undefined;
      if (result.profile) {
        set((state) => ({
          credentialStore: {
            ...state.credentialStore,
            profiles: [
              ...state.credentialStore.profiles.filter((entry) => entry.profile !== result.profile?.profile),
              result.profile!,
            ],
          },
          credentialStoreError: undefined,
        }));
      }
      void (async () => {
        const [credentialResult, runtimeConfigResult] = await Promise.allSettled([
          request.client.listCredentials(),
          observeRuntimeConfigReload(request, result.reloadGeneration),
        ]);
        if (!isCurrentClientRequest(request)) return;
        if (credentialResult.status === "fulfilled") {
          set({ credentialStore: credentialResult.value, credentialStoreError: undefined });
        } else {
          set({
            credentialStoreError: credentialResult.reason instanceof Error
              ? credentialResult.reason.message
              : String(credentialResult.reason),
          });
        }
        if (runtimeConfigResult.status === "fulfilled") {
          const runtimeConfig = runtimeConfigResult.value;
          set({ runtimeConfig, runtimeConfigError: runtimeConfig.error });
          if (runtimeConfig.reload?.state === "completed") {
            try {
              const modelCatalog = freshModelCatalog(await request.client.refreshModels());
              if (!isCurrentClientRequest(request)) return;
              set({ modelCatalog, modelCatalogError: modelCatalog.error });
              persistModelCatalog(runtimeConnectionConfig, modelCatalog);
            } catch (error) {
              if (isCurrentClientRequest(request)) {
                set({ modelCatalogError: error instanceof Error ? error.message : String(error) });
              }
            }
          }
        } else {
          set({
            runtimeConfigError: runtimeConfigResult.reason instanceof Error
              ? runtimeConfigResult.reason.message
              : String(runtimeConfigResult.reason),
          });
        }
      })();
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
      const result = await request.client.deleteCredential(profile);
      if (!isCurrentClientRequest(request)) return;
      set((state) => ({
        credentialStore: {
          ...state.credentialStore,
          profiles: state.credentialStore.profiles.filter((entry) => entry.profile !== profile),
        },
        credentialStoreError: undefined,
      }));
      void (async () => {
        const [credentialResult, runtimeConfigResult] = await Promise.allSettled([
          request.client.listCredentials(),
          observeRuntimeConfigReload(request, result.reloadGeneration),
        ]);
        if (!isCurrentClientRequest(request)) return;
        if (credentialResult.status === "fulfilled") {
          set({ credentialStore: credentialResult.value, credentialStoreError: undefined });
        } else {
          set({
            credentialStoreError: credentialResult.reason instanceof Error
              ? credentialResult.reason.message
              : String(credentialResult.reason),
          });
        }
        if (runtimeConfigResult.status === "fulfilled") {
          const runtimeConfig = runtimeConfigResult.value;
          set({ runtimeConfig, runtimeConfigError: runtimeConfig.error });
          if (runtimeConfig.reload?.state === "completed") {
            try {
              const modelCatalog = freshModelCatalog(await request.client.refreshModels());
              if (!isCurrentClientRequest(request)) return;
              set({ modelCatalog, modelCatalogError: modelCatalog.error });
              persistModelCatalog(runtimeConnectionConfig, modelCatalog);
            } catch (error) {
              if (isCurrentClientRequest(request)) {
                set({ modelCatalogError: error instanceof Error ? error.message : String(error) });
              }
            }
          }
        } else {
          set({
            runtimeConfigError: runtimeConfigResult.reason instanceof Error
              ? runtimeConfigResult.reason.message
              : String(runtimeConfigResult.reason),
          });
        }
      })();
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
            const [credentialStore, runtimeConfig, fetchedModelCatalog] = await Promise.all([
              request.client.listCredentials(),
              request.client.getRuntimeConfig(),
              request.client.refreshModels(),
            ]);
            if (!isCurrentClientRequest(request)) return;
            const modelCatalog = freshModelCatalog(fetchedModelCatalog);
            set({
              codexDeviceLogin: { status: "completed" },
              credentialStore,
              credentialStoreError: undefined,
              runtimeConfig,
              runtimeConfigError: runtimeConfig.error,
              modelCatalog,
              modelCatalogError: modelCatalog.error,
            });
            persistModelCatalog(runtimeConnectionConfig, modelCatalog);
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

  refreshAgentDetail: async (agentId, options = {}) => {
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
    const span = startRuntimeSpan(trace, "agent.detail", { retry: Boolean(options.retry) });
    const key = agentId;
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
        const detail = await request.client.getAgentDetail(agentId);
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
        if (get().sessionsByAgentId[agentId]?.syncStatus === "reconnecting") {
          // The detail fetch failed but the merge preserved the last-known-good
          // detail: retry with bounded backoff instead of hydrating placeholder
          // data, so the session recovers automatically once the API responds.
          scheduleAgentDetailRetry(get, agentId);
          span.end("error", { recovery: "last-known-good" });
          return;
        }
        clearAgentDetailRetry(agentId);
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
        const currentSession = get().sessionsByAgentId[agentId];
        const canRecover =
          get().globalStreamStatus === "streaming" &&
          Boolean(currentSession?.eventSeqs.length);
        if (canRecover) {
          set((state) => ({
            sessionsByAgentId: {
              ...state.sessionsByAgentId,
              [agentId]: {
                ...emptyAgentSession(),
                ...state.sessionsByAgentId[agentId],
                loading: false,
                syncStatus: "recovering",
                contentStatus: state.sessionsByAgentId[agentId]?.eventSeqs.length
                  ? "available"
                  : "unknown",
                error: error instanceof Error ? error.message : String(error),
              },
            },
          }));
          span.end("error", { recovery: true });
          scheduleAgentDetailRetry(get, agentId);
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
        span.end("error", { errorKind: agentDetailErrorKind(error), phase: "fetch" });
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
            error: error instanceof Error ? error.message : String(error),
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

  loadAgentToolExecutionDetail: async (agentId, toolExecutionId, fallbackActivity, conversationRevision) => {
    if (!agentId || !toolExecutionId) return;
    const request = captureClientRequest();
    const key = `${agentId}:${toolExecutionId}`;
    const cached = get().sessionsByAgentId[agentId]?.toolExecutionDetailsById[toolExecutionId];
    const currentRevision = conversationRevision === undefined || (cached?.conversationRevision ?? -1) >= conversationRevision;
    if ((cached?.toolExecution && currentRevision) || cached?.loading || toolExecutionDetailInFlight.has(key)) return;
    toolExecutionDetailInFlight.add(key);
    setToolExecutionDetailState(set, agentId, toolExecutionId, { loading: true, error: undefined });
    try {
      const toolExecution = await request.client.getToolExecution(agentId, toolExecutionId);
      if (!isCurrentClientRequest(request)) return;
      setToolExecutionDetailState(set, agentId, toolExecutionId, {
        loading: false, toolExecution,
        ...(conversationRevision !== undefined ? { conversationRevision } : {}),
      });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      setToolExecutionDetailState(set, agentId, toolExecutionId, {
        loading: false,
        error: error instanceof Error ? error.message : String(error),
        ...(conversationRevision !== undefined ? { conversationRevision } : {}),
      });
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

  sendOperatorPrompt: async (agentId, text, attachments = []) => {
    const prompt = text.trim();
    if (!agentId || (!prompt && attachments.length === 0)) {
      return;
    }

    const request = captureClientRequest();
    try {
      const clientId = generateUuid();
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
                get().currentUser?.displayName,
              ),
            },
          },
        };
      });

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

  abortCurrentRun: async (agentId, runId) => {
    if (!agentId || !runId) return;
    if (get().sessionsByAgentId[agentId]?.abortingRun) return;

    const request = captureClientRequest();
    set((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...emptyAgentSession(),
          ...state.sessionsByAgentId[agentId],
          abortingRun: true,
          abortError: undefined,
        },
      },
    }));

    try {
      await request.client.abortCurrentRun(agentId, runId);
      if (!isCurrentClientRequest(request)) return;
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            abortingRun: false,
            abortError: undefined,
          },
        },
      }));
      scheduleBootstrapRefresh(get, 250);
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      // The run already ended or the run_id expired: the event stream
      // converges the composer state, so a conflict is not surfaced as an
      // operator-facing failure.
      const conflict =
        error instanceof RuntimeHttpError &&
        error.status === 409 &&
        (error.code === "stale_run_id" || error.code === "no_current_run");
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            abortingRun: false,
            abortError: conflict ? undefined : message,
          },
        },
      }));
      if (!conflict) throw error;
    }
  },

  setAgentModel: async (agentId, model, reasoningEffort) => {
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
          modelSelection: modelState?.effective_model,
          runtimeDefaultModel: modelState?.runtime_default_model,
          modelSource: modelState?.source ?? "agent_override",
          modelReasoningEffort: modelState?.override_reasoning_effort ?? undefined,
        }),
      );
      await get().refreshAgentDetail(agentId);
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

  clearAgentModel: async (agentId) => {
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
          modelSelection: modelState?.effective_model,
          runtimeDefaultModel: modelState?.runtime_default_model,
          modelSource: modelState?.source ?? "runtime_default",
          modelReasoningEffort: modelState?.override_reasoning_effort ?? undefined,
        }),
      );
      await get().refreshAgentDetail(agentId);
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

  controlAgent: async (agentId, action) => {
    if (!agentId) return;
    const request = captureClientRequest();
    try {
      await request.client.controlAgent(agentId, action);
      if (!isCurrentClientRequest(request)) return;
      await get().refreshBootstrap({ background: true });
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      throw error;
    }
  },
  renameAgent: async (agentId, name) => {
    if (!agentId) return;
    const request = captureClientRequest();
    try {
      await request.client.renameAgent(agentId, name);
      if (!isCurrentClientRequest(request)) return;
      await get().refreshAgentDetail(agentId, { trigger: "agent.rename" });
      if (get().discovery.mode === "authoritative") {
        // The authoritative roster owns the displayed identity; one snapshot
        // refresh applies the new name (and any concurrent roster change).
        globalSyncCoordinator.refreshRoster(get, set);
      } else {
        await get().refreshBootstrap({ background: true });
      }
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      throw error;
    }
  },
  deleteAgent: async (agentId, cascadePrivateChildren = false) => {
    if (!agentId) return;
    const request = captureClientRequest();
    try {
      await request.client.deleteAgent(agentId, cascadePrivateChildren);
      if (!isCurrentClientRequest(request)) return;
      await agentSessionRepository.deleteCachedSession(agentId);
      globalSyncCoordinator.unregister(agentId);
      stopAgentEventStream(agentId, set);
      set((state) => {
        const sessionsByAgentId = { ...state.sessionsByAgentId };
        const rosterActivityByAgentId = { ...state.rosterActivityByAgentId };
        delete sessionsByAgentId[agentId];
        delete rosterActivityByAgentId[agentId];
        writeStoredRosterActivity(
          currentRemoteKey(runtimeConnectionConfig),
          rosterActivityByAgentId,
        );
        return {
          sessionsByAgentId,
          rosterActivityByAgentId,
        };
      });
      if (get().discovery.mode === "authoritative") {
        // The authoritative roster owns membership; one snapshot refresh
        // applies the deletion (and any concurrent roster change) atomically.
        globalSyncCoordinator.refreshRoster(get, set);
      } else {
        await get().refreshBootstrap({ background: true });
      }
      // If we deleted the currently selected agent, clean up and reset to dashboard
      // so the UI doesn't stay on a stale agent page that errors on refresh.
      if (get().selectedAgentId === agentId) {
        set({
          selectedAgentId: "",
          route: "dashboard",
          rightPanelView: undefined,
          rightPanelViewStack: [],
        });
      }
    } catch (error) {
      if (!isCurrentClientRequest(request)) return;
      throw error;
    }
  },

  // Agent-scoped SSE is intentionally retained only for Debug inspector compatibility.
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
        globalSyncCoordinator.enqueueStreamEvent(set, agentId, event);
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
    globalSyncCoordinator.start(get, set);
  },
  stopGlobalEventStream: () => {
    globalSyncCoordinator.stop(set);
  },
  registerAgentForEvents: (agentId) => {
    globalSyncCoordinator.register(get, set, agentId);
  },
  unregisterAgentForEvents: (agentId) => {
    globalSyncCoordinator.unregister(agentId);
  },
  retryAgentSync: (agentId) => {
    if (!agentId) return;
    globalSyncCoordinator.retryAgentSync(get, set, agentId);
  },
  });
});

// Initialize session cache on first load.
if (typeof window !== "undefined") {
  installRuntimeTraceDebugApi();
  const modelCatalogHydrationGeneration = clientGeneration;
  useRuntimeStore.setState({ modelCatalogLoading: true });
  void hydrateCachedModelCatalog(runtimeConnectionConfig, modelCatalogHydrationGeneration).finally(() => {
    if (isCurrentClientGeneration(modelCatalogHydrationGeneration)) {
      useRuntimeStore.setState({ modelCatalogLoading: false });
    }
  });
  agentSessionRepository.initializeCache();
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

function emptyTimelineEventsState(): TimelineEventsState {
  return {
    eventsBySeq: {},
    eventSeqs: [],
    hasOlder: false,
    loading: false,
    loadingOlder: false,
  };
}

export function mergeTimelineEventPage(
  current: TimelineEventsState,
  incomingEvents: StreamEventEnvelopeDto[],
  eventLogEpoch: string | undefined,
  hasOlder: boolean,
  append: boolean,
): TimelineEventsState {
  const epochChanged = Boolean(
    eventLogEpoch
    && current.eventLogEpoch
    && eventLogEpoch !== current.eventLogEpoch,
  );
  const base = !append || epochChanged ? emptyTimelineEventsState() : current;
  const eventsBySeq = { ...base.eventsBySeq };
  for (const event of incomingEvents) {
    if (event.event_seq == null) continue;
    if (eventLogEpoch && event.event_log_epoch && event.event_log_epoch !== eventLogEpoch) continue;
    const existing = eventsBySeq[event.event_seq];
    if (existing && timelineEventIdentity(existing) !== timelineEventIdentity(event)) {
      return mergeTimelineEventPage(emptyTimelineEventsState(), incomingEvents, eventLogEpoch, hasOlder, false);
    }
    eventsBySeq[event.event_seq] = event;
  }
  const eventSeqs = Object.keys(eventsBySeq).map(Number).sort((left, right) => left - right);
  return {
    eventLogEpoch: eventLogEpoch || base.eventLogEpoch,
    eventsBySeq,
    eventSeqs,
    oldestSeq: eventSeqs[0],
    newestSeq: eventSeqs.at(-1),
    hasOlder,
    loading: false,
    loadingOlder: false,
    error: undefined,
  };
}

function timelineEventIdentity(event: { id?: string; type?: string; event_seq?: number }): string {
  return `${event.event_seq ?? ""}:${event.id ?? ""}:${event.type ?? ""}`;
}

type StoreSet = (
  partial:
    | Partial<RuntimeStoreState>
    | RuntimeStoreState
    | ((state: RuntimeStoreState) => Partial<RuntimeStoreState> | RuntimeStoreState),
  replace?: false,
) => void;

function scheduleAgentDetailRetry(
  get: () => RuntimeStoreState,
  agentId: string,
): void {
  if (agentDetailRetryTimers.has(agentId)) return;
  const attempt = agentDetailRetryAttempts.get(agentId) ?? 0;
  if (attempt >= AGENT_DETAIL_RETRY_DELAYS_MS.length) return;
  const delay = AGENT_DETAIL_RETRY_DELAYS_MS[attempt];
  agentDetailRetryAttempts.set(agentId, attempt + 1);
  const timer = window.setTimeout(() => {
    agentDetailRetryTimers.delete(agentId);
    void get().refreshAgentDetail(agentId, { retry: true });
  }, delay);
  agentDetailRetryTimers.set(agentId, timer);
}

function clearAgentDetailRetry(agentId: string): void {
  const timer = agentDetailRetryTimers.get(agentId);
  if (timer != null) window.clearTimeout(timer);
  agentDetailRetryTimers.delete(agentId);
  agentDetailRetryAttempts.delete(agentId);
}

function scheduleAgentSkillCatalogRetry(get: () => RuntimeStoreState, agentId: string): void {
  if (agentSkillCatalogRetryTimers.has(agentId)) return;
  const attempt = agentSkillCatalogRetryAttempts.get(agentId) ?? 0;
  if (attempt >= AGENT_SKILL_CATALOG_RETRY_DELAYS_MS.length) return;
  agentSkillCatalogRetryAttempts.set(agentId, attempt + 1);
  armAgentSkillCatalogRetry(
    get,
    agentId,
    AGENT_SKILL_CATALOG_RETRY_DELAYS_MS[attempt] + retryJitterMs(),
  );
}

function armAgentSkillCatalogRetry(get: () => RuntimeStoreState, agentId: string, delayMs: number): void {
  const timer = window.setTimeout(() => {
    agentSkillCatalogRetryTimers.delete(agentId);
    const streamStatus = get().globalStreamStatus;
    if (streamStatus !== "streaming" && streamStatus !== "catching_up") {
      // Connection down: pause without fetching or consuming another attempt
      // so the retry resumes once the stream reconnects.
      armAgentSkillCatalogRetry(
        get,
        agentId,
        AGENT_SKILL_CATALOG_RETRY_DELAYS_MS[AGENT_SKILL_CATALOG_RETRY_DELAYS_MS.length - 1] + retryJitterMs(),
      );
      return;
    }
    void get().refreshAgentSkillCatalog(agentId);
  }, delayMs);
  agentSkillCatalogRetryTimers.set(agentId, timer);
}

function clearAgentSkillCatalogRetry(agentId: string): void {
  const timer = agentSkillCatalogRetryTimers.get(agentId);
  if (timer != null) window.clearTimeout(timer);
  agentSkillCatalogRetryTimers.delete(agentId);
  agentSkillCatalogRetryAttempts.delete(agentId);
}

function retryJitterMs(): number {
  return Math.floor(Math.random() * 500);
}


function stopAgentEventStream(agentId: string, set?: StoreSet): void {
  if (set) globalSyncCoordinator.stopBatchingAgent(set, agentId);
  activeEventStreams.get(agentId)?.close();
  activeEventStreams.delete(agentId);
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
  if (activity.messageId) {
    const request = captureClientRequest();
    setInspectorActivityDetailState(set, agentId, activity.id, { loading: true, error: undefined });
    void request.client.getAgentMessagesBatch(agentId, [activity.messageId]).then((response) => {
      if (!isCurrentClientRequest(request)) return;
      const message = response.messages?.find((item) => item.id === activity.messageId);
      if (!message) throw new Error(i18next.t("agentPage.eventUnavailable"));
      set((state) => {
        const selected = state.rightPanelView;
        if (selected?.kind !== "activity_inspector" || selected.agentId !== agentId || selected.activity.id !== activity.id) return {};
        return { rightPanelView: { ...selected, activity: hydrateInputActivity(activity, message), detailState: { loading: false } } };
      });
    }).catch((error) => {
      if (!isCurrentClientRequest(request)) return;
      setInspectorActivityDetailState(set, agentId, activity.id, { loading: false, error: error instanceof Error ? error.message : String(error) });
    });
    return;
  }
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
    globalSyncCoordinator.flushStreamEvents(set, agentId);
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
  globalSyncCoordinator.flushStreamEvents(set, agentId);
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
  modelPatch: Pick<AgentSummary, "model"> & Partial<Pick<AgentSummary, "modelSource" | "modelReasoningEffort" | "modelSelection" | "runtimeDefaultModel">>,
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

function mergeAgentDetailIntoSession(
  state: RuntimeStoreState,
  agentId: string,
  detail: AgentDetail,
): Partial<RuntimeStoreState> {
  const epochSession = sessionForEventLogEpoch(
    state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
    detail.eventLogEpoch,
  );
  const pageEvents = detail.events ?? [];
  const current = hasEventIdentityConflict(epochSession, pageEvents)
    ? resetSessionForEventConflict(epochSession, detail.eventLogEpoch)
    : epochSession;
  if (detail.error && current.detail) {
    // The client returns a disconnected placeholder (error set) when the
    // detail fetch fails. With a last-known-good detail on hand, keep
    // rendering it and mark the session reconnecting instead of letting the
    // placeholder overwrite real data (model "unavailable", "!" badge).
    return {
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...current,
          loading: false,
          syncStatus: "reconnecting",
          error: detail.error,
        },
      },
    };
  }
  const liveDetailIsNewer = (current.newestSeq ?? 0) > Math.max(detail.eventCursorSeq ?? 0, detail.newestEventSeq ?? 0);
  const agent = liveDetailIsNewer && current.detail ? mergeNewerLiveAgentState(detail.agent, current.detail.agent) : detail.agent;
  const detailBase: AgentDetail = {
    ...detail,
    agent,
    timeline: current.detail?.timeline ?? detail.timeline,
    hasOlderEvents: undefined,
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
        contentStatus: detail.error
          ? projected.eventSeqs.length
            ? "available"
            : "unknown"
          : projected.eventSeqs.length
            ? "available"
            : "confirmed-empty",
        syncStatus: detail.error ? "error" : "idle",
        lastValidatedAt: detail.error ? current.lastValidatedAt : Date.now(),
        detailValidatedAt: detail.error ? current.detailValidatedAt : Date.now(),
        newestSeq: newestSeq || undefined,
        oldestSeq: detail.oldestEventSeq ?? projected.oldestSeq,
        error: detail.error,
      },
    },
  };
}

export function applyStreamEvents(set: StoreSet, agentId: string, events: StreamEventEnvelopeDto[]): void {
  const incomingEpoch = eventLogEpochFromEvents(events);
  const incomingEvents = events.filter(
    (event) =>
      event.event_seq != null &&
      (!incomingEpoch || !event.event_log_epoch || event.event_log_epoch === incomingEpoch),
  );
  if (!incomingEvents.length) return;
  const currentSnapshot = useRuntimeStore.getState().sessionsByAgentId[agentId];
  if (currentSnapshot && hasEventIdentityConflict(currentSnapshot, incomingEvents)) {
    globalSyncCoordinator.discardPendingEvents(agentId);
    globalSyncCoordinator.unregisterRecovery(agentId);
    set((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: resetSessionForEventConflict(
          state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
          incomingEpoch,
        ),
      },
    }));
    void useRuntimeStore.getState().refreshAgentDetail(agentId);
    return;
  }
  const liveStatus = globalSyncCoordinator.isRecovering(agentId) ? "recovering" : "streaming";

  const currentSession = sessionForEventLogEpoch(
    currentSnapshot ?? emptyAgentSession(),
    incomingEpoch,
  );
  const uniqueEvents = incomingEvents.filter(
    (event) => !currentSession.eventsBySeq[event.event_seq as number],
  );
  const offerToLedger = (): void => {
    void agentSessionRepository
      .ingestSessionEvents(agentId, incomingEvents)
      .catch((error) => console.warn(`Agent ledger ingestion failed for ${agentId}.`, error));
  };
  // Duplicate stream events are common while gap recovery overlaps the live
  // stream; they must not commit a new session object on every flush. The
  // ledger is still offered every envelope so its exactness guarantees hold.
  if (!uniqueEvents.length) {
    if (currentSession.liveStatus !== liveStatus || currentSession.error !== undefined) {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...sessionForEventLogEpoch(
              state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
              incomingEpoch,
            ),
            liveStatus,
            error: undefined,
          },
        },
      }));
    }
    offerToLedger();
    return;
  }

  set((state) => {
    const current = sessionForEventLogEpoch(
      state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
      incomingEpoch,
    );
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
    const timelineEvents = state.timelineEventsByAgentId[agentId];

    return {
      bootstrap: sortBootstrapAgents(
        patchBootstrapAgent(state.bootstrap, agentId, runPatch, briefPatch),
        rosterActivityByAgentId,
      ),
      rosterActivityByAgentId,
      timelineEventsByAgentId: timelineEvents
        ? {
            ...state.timelineEventsByAgentId,
            [agentId]: mergeTimelineEventPage(
              timelineEvents,
              incomingEvents,
              incomingEpoch,
              timelineEvents.hasOlder,
              true,
            ),
          }
        : state.timelineEventsByAgentId,
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
  offerToLedger();
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

export function agentDetailErrorKind(error: unknown): string {
  if (error instanceof Error) {
    if (error.name === "RuntimeHttpError") return "http_error";
    if (error.name === "SyntaxError") return "parse_error";
    if (error.name === "TypeError") return "network_error";
  }
  if (isTimeoutAbortError(error)) return "timeout";
  return "unknown";
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
  options: {
    newestSeq?: number;
    append?: boolean;
    eventLogEpoch?: string;
    historyDisplayLevel?: DisplayLevel;
    historyLoading?: boolean;
  } = {},
): Partial<RuntimeStoreState> {
  return {
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: mergeEventPageIntoConversation(
        state.sessionsByAgentId[agentId],
        pageEvents,
        pageOldestSeq,
        pageHasOlder,
        displayLevel,
        options,
      ),
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

useRuntimeStore.subscribe((state, previous) => {
  if (state.rightPanelView !== previous.rightPanelView) rememberPanelView(state.rightPanelView);
  if (state.rightPanelOpen !== previous.rightPanelOpen || state.rightPanelMode !== previous.rightPanelMode) {
    writePanelPreferences({ ...readPanelPreferences(), open: state.rightPanelOpen, mode: state.rightPanelMode });
  }
});
