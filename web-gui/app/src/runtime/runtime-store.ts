import i18next from "i18next";
import {
  bootstrapProgress,
  validCreationIdentity,
  validCreationResponse,
  type AgentCreationLookup,
  type AgentCreationProgress,
  type AgentCreationResult,
} from "./agent-creation";
import { hydrateInputActivity, inputPresentation } from "./conversation-input";
import { readPanelPreferences, writePanelPreferences, rememberPanelView } from "./panel-preferences";
import { panelLayout, PANEL_DEFAULT } from "../features/right-panel/panel-layout";
import { clearConversationCaches } from "./conversation-cache-lifecycle";
import { refreshSelectedAgentResources } from "./selected-agent-resource-invalidation";
import { setResumeAdmissionStatus, useResumeAdmission } from "./selected-agent-refresh-context";
import type { CurrentUser } from "./client";
import { create } from "zustand";

import {
  createRuntimeClient,
  isProjectionBusyError,
  isTimeoutAbortError,
  projectRosterAgents,
  RuntimeHttpError,
  type BriefReadStateDto,
  type OperatorPromptAttachment,
  type AgentRosterSnapshotDto,
  type StreamEventEnvelopeDto,
} from "./client";
import {
  AgentSessionRepository,
  isSessionCacheContextCurrent,
} from "./agent-session-repository";
import {
  emptyAgentSession,
  eventLogEpochFromEvents,
  materializeProjectionDetail,
  mergeEventPageIntoConversation,
  sessionForEventLogEpoch,
} from "./conversation-store";
import {
  backfillRetryDelayMs,
  GlobalSyncCoordinator,
  runWithConcurrencyLimit,
} from "./global-sync-coordinator";
import { ReadStateBus } from "./read-state-bus";
import {
  readStoredRosterActivity,
  touchRosterActivity,
  touchRosterActivityFromEvent,
  writeStoredRosterActivity,
  evaluateConversationReadMarkerGate,
  type AgentRosterActivity,
} from "./read-state";
import {
  CACHE_SCHEMA_VERSION,
  cacheGetModelCatalog,
  cachePutModelCatalog,
} from "./idb-cache";
import { ResumeReconciliationCoordinator } from "./resume-reconciliation";
import { currentRemoteKey } from "./session-cache";
import { agentPresentationCacheKey, readCachedAgents, writeCachedAgents } from "./agent-presentation-cache";
import { resolveConversationScopeKey, conversationScopeSnapshot, disposeAllConversationScopes } from "./conversation-scope-store";
import { clearAgentAccessRecords } from "./agent-access-records";
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
  PendingOperatorPrompt,
  WorkItemDetailState,
  TaskDetailState,
  ToolExecutionDetailState,
} from "./runtime-store-helpers";
export type { AgentLiveStatus, AgentSessionState, PendingOperatorPrompt };
export {
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
  incarnation?: number;
  retry?: boolean;
  trace?: RuntimeTraceContext;
  trigger?: string;
}

export interface AgentResourceRefreshOptions {
  incarnation?: number;
  signal?: AbortSignal;
  /** The selected scope's scheduler owns retry/backoff when set. */
  throwOnError?: boolean;
}

export interface AgentObjectDetailRefreshOptions extends AgentResourceRefreshOptions {
  force?: boolean;
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
    currentWork: cachedAgent.workItems !== undefined ? cachedAgent.currentWork : httpAgent.currentWork,
    workItems: cachedAgent.workItems ?? httpAgent.workItems,
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

const MAX_SEMANTIC_HISTORY_PAGES_PER_LOAD = 5;
/** Opportunistic age-out applied whenever a prune pass runs. */
const PENDING_OPERATOR_PROMPT_TTL_MS = 5 * 60_000;

function markPendingOperatorPromptConfirmed(
  prompts: readonly PendingOperatorPrompt[],
  clientId: string,
  messageId: string,
): PendingOperatorPrompt[] {
  return prompts.map((entry) =>
    entry.clientId === clientId ? { ...entry, messageId } : entry,
  );
}

function retainPendingOperatorPrompts(
  prompts: readonly PendingOperatorPrompt[],
  confirmedMessageIds: ReadonlySet<string>,
  now: number,
): PendingOperatorPrompt[] | null {
  const retained = prompts.filter((entry) =>
    (entry.messageId === undefined || !confirmedMessageIds.has(entry.messageId))
    && now - Date.parse(entry.createdAt) < PENDING_OPERATOR_PROMPT_TTL_MS);
  return retained.length === prompts.length ? null : retained;
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
  navCollapsed: boolean;

  bootstrap: RuntimeBootstrap;
  bootstrapLoading: boolean;
  /** Persisted summaries are presentation only, not an authoritative roster. */
  agentRosterSource?: "cache" | "http";
  /** Identity behind the current session, used to attribute pending operator prompts. */
  currentUser?: CurrentUser;
  currentUserLoaded: boolean;
  bootstrapError?: string;
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
  agentCreations: Record<string, AgentCreationProgress>;
  agentCreationContext: number;
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
  markAgentConversationRead: (agentId: string, scopeKey?: string) => void;
  refreshBriefReadStates: () => Promise<void>;
  openSkill: (skillId: string, agentId?: string) => void;
  openTemplate: (catalogId: string) => void;
  setRightPanelOpen: (open: boolean) => void;
  toggleRightPanelExpanded: () => void;
  showAgentOverview: (agentId?: string) => void;
  showWorkItemDetail: (agentId: string, workItem: WorkItemSummary) => void;
  showTaskDetail: (agentId: string, task: TaskSummary) => void;
  showToolExecutionDetail: (agentId: string, toolExecutionId: string, toolName?: string, relatedStateObjectRef?: TimelineStateObjectRef, fallbackActivity?: AgentTimelineActivity) => void;
  inspectActivity: (agentId: string, activity: AgentTimelineActivity) => void;
  /** Full display text of one message, beyond the byte-bounded read-model preview. */
  fetchAgentMessageText: (agentId: string, messageId: string) => Promise<string | null>;
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
  queryAgentRecentTasks: (agentId: string, options?: AgentResourceRefreshOptions) => Promise<TaskSummary[]>;
  queryAgentTimers: (agentId: string, options?: AgentResourceRefreshOptions) => Promise<import("./generated/openapi").components["schemas"]["TimerRecord"][]>;
  cancelAgentTimer: (agentId: string, timerId: string) => Promise<import("./generated/openapi").components["schemas"]["TimerRecord"]>;
  reconcileAfterResume: () => Promise<void>;
  refreshModelCatalog: (options?: { refresh: boolean }) => Promise<void>;
  refreshRuntimeConfig: () => Promise<void>;
  updateRuntimeConfig: (updates: Array<{ key: string; value?: unknown; unset?: boolean }>) => Promise<RuntimeConfigState | undefined>;
  downloadLocalOnnxPreset: (preset: string) => Promise<RuntimeLocalOnnxPresetStatus>;
  cancelLocalOnnxPreset: (preset: string) => Promise<RuntimeLocalOnnxPresetStatus>;
  refreshSkillCatalog: (options?: { force?: boolean }) => Promise<void>;
  refreshSkillDetail: (skillId: string | undefined, agentId?: string) => Promise<void>;
  refreshTemplateCatalog: () => Promise<void>;
  refreshTemplateDetail: (catalogId: string | undefined) => Promise<void>;
  installTemplate: (githubUrl: string) => Promise<boolean>;
  removeTemplate: (templateId: string) => Promise<boolean>;
  syncTemplateRemoteSources: () => Promise<boolean>;
  dismissTemplateDiagnostics: () => void;
  dismissTemplateError: () => void;
  createAgentFromTemplate: (agentId: string, template: string, name?: string) => Promise<AgentCreationResult>;
  lookupCreatedAgent: (agentId: string) => Promise<AgentCreationLookup>;
  refreshAgentCreation: (agentId: string) => Promise<void>;
  dismissAgentCreation: (agentId: string) => void;
  addSkillToCatalog: (input: AddSkillInput) => Promise<boolean>;
  removeSkillFromCatalog: (name: string) => Promise<boolean>;
  updateSkillCatalog: (name?: string) => Promise<boolean>;
  dismissSkillJob: (jobId: string) => void;
  checkSkillCatalog: (name?: string) => Promise<boolean>;
  refreshAgentSkillCatalog: (agentId: string | undefined, options?: { force?: boolean }) => Promise<void>;
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
  refreshAgentWorkItems: (agentId: string | undefined, options?: AgentResourceRefreshOptions) => Promise<void>;
  refreshAgentState: (agentId: string | undefined, options?: AgentResourceRefreshOptions) => Promise<void>;
  loadAgentWorkItemDetail: (agentId: string | undefined, workItemId: string | undefined, options?: AgentObjectDetailRefreshOptions) => Promise<void>;
  loadAgentTaskDetail: (agentId: string | undefined, taskId: string | undefined, force?: boolean, options?: AgentObjectDetailRefreshOptions) => Promise<void>;
  loadAgentToolExecutionDetail: (agentId: string | undefined, toolExecutionId: string | undefined, fallbackActivity?: AgentTimelineActivity, conversationRevision?: number, options?: AgentObjectDetailRefreshOptions) => Promise<void>;
  sendOperatorPrompt: (agentId: string | undefined, text: string, attachments?: OperatorPromptAttachment[], expectedIncarnation?: number) => Promise<void>;
  /** Drop locally echoed prompts once the server view carries them. */
  pruneOperatorPrompts: (agentId: string, confirmedMessageIds: readonly string[]) => void;
  abortCurrentRun: (agentId: string | undefined, runId: string | null | undefined) => Promise<void>;
  setAgentModel: (agentId: string | undefined, model: string, reasoningEffort?: string, serviceTier?: string) => Promise<void>;
  clearAgentModel: (agentId: string | undefined) => Promise<void>;
  controlAgent: (agentId: string | undefined, action: AgentControlAction) => Promise<void>;
  deleteAgent: (agentId: string | undefined, cascadePrivateChildren?: boolean) => Promise<void>;
  renameAgent: (agentId: string | undefined, name: string) => Promise<void>;
  /** Operator-triggered per-agent sync retry over the roster HTTP cycle (#3808 slice 2). */
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

const inspectorDetailInFlight = new Set<string>();
const workItemRefreshInFlight = new Map<string, AgentResourceRequest>();
const objectDetailInFlight = new Map<string, AgentResourceRequest>();
const agentStateRefreshInFlight = new Map<string, AgentResourceRequest>();
let agentResourceReadSequence = 0;
const agentDetailRefreshInFlight = new Map<string, {
  generation: number; contextKey: string; incarnation?: number; promise: Promise<void>;
}>();
const agentDetailRequestSequence = new Map<string, number>();
const agentDetailRetryTimers = new Map<string, number>();
const agentDetailRetryAttempts = new Map<string, number>();
const agentSkillCatalogRetryTimers = new Map<string, number>();
const agentSkillCatalogRetryAttempts = new Map<string, number>();
const skillCatalogInFlight = new Map<string, ClientRequest & { scopeKey: string; promise: Promise<void> }>();
const SKILL_CATALOG_TTL_MS = 60_000;
let bootstrapRefreshInFlight: Promise<void> | undefined;
let bootstrapRefreshTimer: ReturnType<typeof setTimeout> | undefined;
let clientGeneration = 0;
let runtimeConfigRequestSequence = 0;
let runtimeConfigSaveSequence = 0;
let confirmedPresentationSubjectKey: string | undefined;
let resumeReconciliationInFlight: Promise<void> | undefined;
let resumeReconciliationCoordinator: ResumeReconciliationCoordinator | undefined;
const AGENT_VALIDATION_TTL_MS = 60_000;
const RESUME_RECONCILIATION_THRESHOLD_MS = 60_000;
const AGENT_DETAIL_RETRY_DELAYS_MS = [2_000, 5_000, 15_000] as const;
const AGENT_SKILL_CATALOG_RETRY_DELAYS_MS = [2_000, 5_000, 15_000] as const;

function nextClientGeneration(): number {
  clientGeneration += 1;
  return clientGeneration;
}

export function getRuntimeClientGeneration(): number {
  return clientGeneration;
}

function isCurrentClientGeneration(generation: number): boolean {
  return generation === clientGeneration;
}

let resumeReconciliationSchedules = 0;
let resumeAdmissionToken = 0;

/**
 * Deferred resume reconciliation replaces the client generation and resets
 * transient resource state. Partitions hold automatic reads while it is
 * pending so each resource performs exactly one post-resume read.
 */
export function markResumeReconciliationScheduled(): void {
  resumeReconciliationSchedules += 1;
  if (useResumeAdmission.getState().status.pending) return;
  resumeAdmissionToken += 1;
  setResumeAdmissionStatus({ pending: true, token: resumeAdmissionToken });
}

function settleResumeReconciliationAdmission(schedulesAtStart: number): void {
  // A schedule during the run leaves a coordinator rerun pending; only the
  // final run settles resume admission.
  if (resumeReconciliationSchedules !== schedulesAtStart) return;
  resumeAdmissionToken += 1;
  setResumeAdmissionStatus({ pending: false, token: resumeAdmissionToken });
}

type RuntimeClient = ReturnType<typeof createRuntimeClient>;

export function skillDetailCacheKey(skillId: string, agentId?: string): string {
  return agentId ? `${agentId}\u0000${skillId}` : skillId;
}

interface ClientRequest {
  client: RuntimeClient;
  generation: number;
}

type AgentResource = "state" | "workItems";

interface AgentResourceRequest extends ClientRequest {
  contextKey: string;
  incarnation?: number;
  signal?: AbortSignal;
  sequence: number;
  promise: Promise<void>;
}

function agentResourceContext(state: RuntimeStoreState): string {
  return JSON.stringify([
    state.discovery.identity, state.currentUser, state.currentUserLoaded,
    state.bootstrap.connection.authRequired, state.discovery.freshness === "unauthorized",
  ]);
}

function skillCatalogScopeKey(state: RuntimeStoreState, agentId?: string): string {
  return JSON.stringify([
    clientGeneration, agentResourceContext(state),
    agentId ? state.bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation : null,
  ]);
}

function refreshSkillCatalogResource(
  set: StoreSet,
  get: () => RuntimeStoreState,
  agentId?: string,
  options: { force?: boolean } = {},
): Promise<void> {
  const key = agentId ?? "";
  const scopeKey = skillCatalogScopeKey(get(), agentId);
  const cached = agentId ? get().agentSkillCatalogByAgentId[agentId] : get().skillCatalog;
  if (agentId && cached?.scopeKey !== scopeKey) clearAgentSkillCatalogRetry(agentId);
  const existing = skillCatalogInFlight.get(key);
  if (existing && isCurrentClientRequest(existing) && existing.scopeKey === scopeKey) return existing.promise;
  if (options.force === false && cached?.scopeKey === scopeKey && !cached.stale
    && cached.lastSuccessAt !== undefined && Date.now() - cached.lastSuccessAt < SKILL_CATALOG_TTL_MS) {
    return Promise.resolve();
  }
  const request = { ...captureClientRequest(), scopeKey, promise: Promise.resolve() };
  const current = () => skillCatalogInFlight.get(key) === request && isCurrentClientRequest(request)
    && skillCatalogScopeKey(get(), agentId) === scopeKey;
  const patch = (state: RuntimeStoreState, catalog: SkillCatalogState, loading: boolean): Partial<RuntimeStoreState> =>
    agentId ? {
      agentSkillCatalogByAgentId: { ...state.agentSkillCatalogByAgentId, [agentId]: catalog },
      agentSkillCatalogLoadingByAgentId: { ...state.agentSkillCatalogLoadingByAgentId, [agentId]: loading },
      agentSkillCatalogErrorByAgentId: { ...state.agentSkillCatalogErrorByAgentId, [agentId]: catalog.error },
    } : { skillCatalog: catalog, skillCatalogLoading: loading, skillCatalogError: catalog.error };
  skillCatalogInFlight.set(key, request);
  set((state) => patch(state, {
    ...(cached?.scopeKey === scopeKey ? cached : emptySkillCatalog),
    scopeKey,
    incarnation: agentId ? state.bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation : undefined,
    error: undefined,
  }, true));
  request.promise = Promise.resolve().then(async () => {
    try {
      if (!current()) return;
      const catalog = await request.client.getSkillCatalog(agentId);
      if (!current()) return;
      if (catalog.error) throw new Error(catalog.error);
      set((state) => patch(state, {
        ...catalog, scopeKey, stale: false, lastSuccessAt: Date.now(),
        incarnation: agentId ? state.bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation : undefined,
      }, false));
      if (agentId) clearAgentSkillCatalogRetry(agentId);
    } catch (error) {
      if (!current()) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => patch(state, {
        ...(agentId ? state.agentSkillCatalogByAgentId[agentId] : state.skillCatalog),
        source: "http", scopeKey, stale: true, error: message,
      }, false));
      if (agentId) scheduleAgentSkillCatalogRetry(get, agentId, scopeKey);
    } finally {
      if (skillCatalogInFlight.get(key) === request) {
        const scopeCurrent = current();
        skillCatalogInFlight.delete(key);
        if (!scopeCurrent) {
          set((state) => agentId
            ? { agentSkillCatalogLoadingByAgentId: { ...state.agentSkillCatalogLoadingByAgentId, [agentId]: false } }
            : { skillCatalogLoading: false });
        }
      }
    }
  });
  return request.promise;
}

async function refreshAffectedSkillCatalogs(
  set: StoreSet,
  get: () => RuntimeStoreState,
  agentId?: string,
): Promise<void> {
  const affectedAgentIds = agentId ? [agentId] : Object.keys(get().agentSkillCatalogByAgentId);
  skillCatalogInFlight.delete("");
  for (const id of affectedAgentIds) {
    skillCatalogInFlight.delete(id);
    clearAgentSkillCatalogRetry(id);
  }
  set((state) => ({
    skillCatalog: { ...state.skillCatalog, stale: true },
    skillCatalogLoading: false,
    agentSkillCatalogLoadingByAgentId: {
      ...state.agentSkillCatalogLoadingByAgentId,
      ...Object.fromEntries(affectedAgentIds.map((id) => [id, false])),
    },
    agentSkillCatalogByAgentId: {
      ...state.agentSkillCatalogByAgentId,
      ...Object.fromEntries(affectedAgentIds.map((id) => [
        id, { ...(state.agentSkillCatalogByAgentId[id] ?? emptySkillCatalog), stale: true },
      ])),
    },
  }));
  const visibleAgentId = agentId ?? get().selectedAgentId;
  await Promise.all([
    get().refreshSkillCatalog(),
    visibleAgentId ? get().refreshAgentSkillCatalog(visibleAgentId) : Promise.resolve(),
  ]);
}

/**
 * A resource read completed after its scope was replaced (client generation,
 * authorization context, or agent incarnation changed mid-flight). The result
 * must not settle the resource as fresh: callers such as the refresh
 * scheduler retry instead of coasting on data that was never merged.
 */
export class AgentResourceSupersededError extends Error {
  constructor(agentId: string, resource: string) {
    super(`Agent ${agentId} ${resource} read superseded by a newer scope.`);
    this.name = "AgentResourceSupersededError";
  }
}

function refreshAgentResource(
  set: StoreSet,
  get: () => RuntimeStoreState,
  agentId: string,
  resource: AgentResource,
  options: AgentResourceRefreshOptions,
): Promise<void> {
  const inFlight = resource === "state" ? agentStateRefreshInFlight : workItemRefreshInFlight;
  const clientRequest = captureClientRequest();
  const contextKey = agentResourceContext(get());
  const incarnation = options.incarnation ?? get().bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation;
  const existing = inFlight.get(agentId);
  const result = (promise: Promise<void>) => options.throwOnError ? promise : promise.catch(() => undefined);
  if (existing && isCurrentClientRequest(existing) && existing.contextKey === contextKey
    && existing.incarnation === incarnation && !existing.signal?.aborted) return result(existing.promise);
  const request: AgentResourceRequest = {
    ...clientRequest, contextKey, incarnation, signal: options.signal,
    sequence: ++agentResourceReadSequence, promise: Promise.resolve(),
  };
  const current = () => {
    const agent = get().bootstrap.agents.find((entry) => entry.id === agentId);
    return inFlight.get(agentId) === request && isCurrentClientRequest(request) && !request.signal?.aborted
      && agentResourceContext(get()) === contextKey
      && (agent?.incarnation === undefined || incarnation === undefined || agent.incarnation === incarnation);
  };
  inFlight.set(agentId, request);
  set((state) => {
    const session = state.sessionsByAgentId[agentId];
    const scopeKey = JSON.stringify([request.generation, contextKey, incarnation]);
    const replaced = (session?.resourceScopeKey !== undefined && session.resourceScopeKey !== scopeKey)
      || (incarnation !== undefined && session?.detail?.agent.incarnation !== undefined
        && session.detail.agent.incarnation !== incarnation);
    const scopedState = { ...state, sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: { ...(replaced ? emptyAgentSession() : session ?? emptyAgentSession()), resourceScopeKey: scopeKey },
    } };
    return patchAgentResourceStatus(scopedState, agentId, resource, { loading: true, error: undefined });
  });
  request.promise = Promise.resolve().then(async () => {
    try {
      if (!current()) throw new AgentResourceSupersededError(agentId, resource);
      const data = resource === "state"
        ? await request.client.getAgentState(agentId, request.signal)
        : await request.client.getAgentWorkItems(agentId, { limit: 50, signal: request.signal, incarnation });
      if (!current()) throw new AgentResourceSupersededError(agentId, resource);
      if (resource === "state" && incarnation !== undefined && (data as AgentSummary).incarnation !== incarnation) {
        throw new RuntimeHttpError("GET", `/agents/${agentId}/state`, 409,
          "Agent identity changed.", "agent_incarnation_mismatch");
      }
      set((state) => {
        const merged = resource === "state"
          ? mergeAgentStateIntoState(state, agentId, data as AgentSummary)
          : mergeAgentWorkItemsIntoState(state, agentId, data as WorkItemSummary[]);
        return {
          ...merged,
          ...patchAgentResourceStatus({ ...state, ...merged }, agentId, resource, {
            loading: false, stale: false, error: undefined,
            lastSuccessAt: Date.now(), dataRequestId: request.sequence,
          }),
        };
      });
    } catch (error) {
      if (error instanceof AgentResourceSupersededError) throw error;
      if (!current()) throw new AgentResourceSupersededError(agentId, resource);
      set((state) => patchAgentResourceStatus(state, agentId, resource, {
        loading: false, stale: true, error: error instanceof Error ? error.message : String(error),
      }));
      throw error;
    } finally {
      if (inFlight.get(agentId) === request) inFlight.delete(agentId);
    }
  });
  return result(request.promise);
}

function patchAgentResourceStatus(
  state: RuntimeStoreState,
  agentId: string,
  resource: AgentResource,
  patch: Partial<NonNullable<AgentSessionState["resourceRefresh"]>[AgentResource]>,
): Partial<RuntimeStoreState> {
  const session = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  return { sessionsByAgentId: {
    ...state.sessionsByAgentId,
    [agentId]: { ...session, resourceRefresh: {
      ...session.resourceRefresh,
      [resource]: { loading: false, stale: false, ...session.resourceRefresh?.[resource], ...patch },
    } },
  } };
}

type ObjectDetailKind = "workItem" | "task" | "toolExecution";
type ObjectDetailState = WorkItemDetailState & TaskDetailState & ToolExecutionDetailState;

/** All entry points share one scoped read, including forced reads during loading. */
function refreshAgentObjectDetail(
  set: StoreSet,
  get: () => RuntimeStoreState,
  agentId: string,
  objectId: string,
  kind: ObjectDetailKind,
  options: AgentObjectDetailRefreshOptions = {},
  conversationRevision?: number,
): Promise<void> {
  const clientRequest = captureClientRequest();
  const contextKey = agentResourceContext(get());
  const incarnation = options.incarnation ?? get().bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation;
  const scopeKey = JSON.stringify([clientRequest.generation, contextKey, incarnation]);
  const key = JSON.stringify([agentId, kind, objectId]);
  const result = (promise: Promise<void>) => options.throwOnError ? promise : promise.catch(() => undefined);
  const existing = objectDetailInFlight.get(key);
  if (existing && isCurrentClientRequest(existing) && existing.contextKey === contextKey
    && existing.incarnation === incarnation && !existing.signal?.aborted) return result(existing.promise);
  // Stamp the shared scope before parallel core reads can reset this detail.
  set((state) => {
    const previous = state.sessionsByAgentId[agentId];
    if (previous?.resourceScopeKey === scopeKey) return {};
    const replaced = previous?.resourceScopeKey !== undefined
      || (incarnation !== undefined && previous?.detail?.agent.incarnation !== undefined
        && previous.detail.agent.incarnation !== incarnation);
    return { sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: { ...(replaced ? emptyAgentSession() : previous ?? emptyAgentSession()), resourceScopeKey: scopeKey },
    } };
  });
  const session = get().sessionsByAgentId[agentId];
  const cached: ObjectDetailState | undefined = kind === "workItem" ? session?.workItemDetailsById[objectId]
    : kind === "task" ? session?.taskDetailsById[objectId] : session?.toolExecutionDetailsById[objectId];
  const sameScope = cached?.scopeKey === scopeKey;
  const hasData = sameScope && Boolean(cached?.workItem || cached?.output || cached?.status || cached?.toolExecution);
  const ttl = kind === "workItem" ? 30_000 : 5_000;
  if (!options.force && hasData && !cached?.stale && Date.now() - (cached?.lastSuccessAt ?? 0) < ttl
    && (conversationRevision === undefined || (cached?.conversationRevision ?? -1) >= conversationRevision)) return Promise.resolve();
  const request: AgentResourceRequest = {
    ...clientRequest, contextKey, incarnation, signal: options.signal,
    sequence: ++agentResourceReadSequence, promise: Promise.resolve(),
  };
  const current = () => objectDetailInFlight.get(key) === request && isCurrentClientRequest(request)
    && !request.signal?.aborted && agentResourceContext(get()) === contextKey
    && (incarnation === undefined || get().bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation === incarnation);
  const patch = (value: ObjectDetailState) => {
    const scoped = { ...value, scopeKey };
    if (kind === "workItem") setWorkItemDetailState(set, agentId, objectId, scoped);
    else if (kind === "task") setTaskDetailState(set, agentId, objectId, scoped);
    else setToolExecutionDetailState(set, agentId, objectId, scoped);
  };
  objectDetailInFlight.set(key, request);
  patch({
    ...(!sameScope ? { workItem: undefined, output: undefined, status: undefined, toolExecution: undefined, lastSuccessAt: undefined } : {}),
    loading: !hasData, refreshing: hasData, stale: sameScope && (cached?.stale ?? false), error: undefined,
  });
  request.promise = Promise.resolve().then(async () => {
    try {
      if (!current()) throw new AgentResourceSupersededError(agentId, `detail:${kind}:${objectId}`);
      const readOptions = { incarnation, signal: request.signal };
      let data: ObjectDetailState;
      let failure: unknown;
      if (kind === "workItem") {
        data = { workItem: await request.client.getAgentWorkItem(agentId, objectId, readOptions) };
      } else if (kind === "toolExecution") {
        data = {
          toolExecution: await request.client.getToolExecution(agentId, objectId, readOptions),
          ...(conversationRevision !== undefined ? { conversationRevision } : {}),
        };
      } else {
        const [status, output] = await Promise.allSettled([
          request.client.getTaskStatus(agentId, objectId, readOptions),
          request.client.getTaskOutput(agentId, objectId, readOptions),
        ]);
        data = {
          ...(status.status === "fulfilled" ? { status: status.value } : {}),
          ...(output.status === "fulfilled" ? { output: output.value } : {}),
        };
        failure = status.status === "rejected" ? status.reason : output.status === "rejected" ? output.reason : undefined;
      }
      if (!current()) throw new AgentResourceSupersededError(agentId, `detail:${kind}:${objectId}`);
      // Partial Task success is useful data, not proof that its output is fresh.
      patch({ ...data, dataRequestId: request.sequence });
      if (failure !== undefined) throw failure;
      patch({ loading: false, refreshing: false, stale: false, error: undefined, lastSuccessAt: Date.now() });
    } catch (error) {
      if (!current()) throw new AgentResourceSupersededError(agentId, `detail:${kind}:${objectId}`);
      patch({
        loading: false, refreshing: false, stale: true, error: error instanceof Error ? error.message : String(error),
        // A mounted conversation row retries only when a newer revision arrives.
        ...(kind === "toolExecution" && conversationRevision !== undefined ? { conversationRevision } : {}),
      });
      throw error;
    } finally {
      if (objectDetailInFlight.get(key) === request) objectDetailInFlight.delete(key);
    }
  });
  return result(request.promise);
}

interface AgentCreationRequest extends ClientRequest {
  creationContext: number;
}

function captureAgentCreationRequest(get: () => RuntimeStoreState): AgentCreationRequest {
  return { ...captureClientRequest(), creationContext: get().agentCreationContext };
}

function isCurrentAgentCreationRequest(get: () => RuntimeStoreState, request: AgentCreationRequest): boolean {
  return isCurrentClientRequest(request) && get().agentCreationContext === request.creationContext
    && !presentationAuthRejected(get());
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
  skillCatalogInFlight.clear();
  if (bootstrapRefreshTimer != null) {
    globalThis.clearTimeout(bootstrapRefreshTimer);
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
  for (const timer of readMarkerAdvanceTimers.values()) window.clearTimeout(timer);
  readMarkerAdvanceTimers.clear();
  readMarkerAdvanceScheduledSeq.clear();
  pendingReadMarkerAgentIds.clear();
  pendingReadMarkerCursorByAgentId.clear();
  readMarkerScopeKeyByAgentId.clear();
  readMarkerAdvanceQueued.clear();
  readMarkerAdvanceInFlight.clear();
  globalSyncCoordinator.cancelClientGenerationWork();
  inspectorDetailInFlight.clear();
  workItemRefreshInFlight.clear();
  objectDetailInFlight.clear();
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
  context: { previousIdentity?: RosterDiscoveryIdentity; request: ClientRequest },
): Promise<string[]> {
  if (!isCurrentClientRequest(context.request)) return [];
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
      if (!cached || identityReset || cached.incarnation !== agent.incarnation) return agent;
      return preserveResourceOwnedAgent(
        mergeBootstrapAgentState(agent, cached), cached,
        state.sessionsByAgentId[agent.id]?.resourceRefresh,
      );
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
      agentRosterSource: "http",
      ...(identityReset ? {
        agentCreations: {},
        agentCreationContext: state.agentCreationContext + 1,
      } : {}),
      sessionsByAgentId,
      rosterActivityByAgentId,
      briefReadStateByAgentId,
      agentSkillCatalogByAgentId: Object.fromEntries(
        Object.entries(state.agentSkillCatalogByAgentId).filter(([id, catalog]) =>
          !identityReset && rosterAgents.some((agent) => agent.id === id && agent.incarnation === catalog.incarnation)),
      ),
    };
  });
  void refreshBriefReadStatesInView(true);
  return omittedAgentIds;
}

const globalSyncCoordinator = new GlobalSyncCoordinator<RuntimeStoreState>({
  captureClientRequest,
  isCurrentClientRequest,
  fetchRosterSnapshot: async (request) => {
    return request.client.getAgentRosterSnapshot();
  },
  applyRosterSnapshot: (set, snapshot, context) =>
    applyRosterSnapshotToStore(set, snapshot, context),
});

let agentSessionRepository!: AgentSessionRepository;

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
      return {
        agentId: agent.id,
        durability: "unavailable",
        state: "retired",
        pendingHydrationJobs: 0,
        failedHydrationJobs: 0,
        readCertainty: state.briefReadStateByAgentId[agent.id] ? "exact" : "unavailable",
      };
    }),
  };
}

let briefReadStatesRefreshInFlight: Promise<void> | undefined;
let briefReadStatesRefreshGeneration: number | undefined;
// Quiet-window debounce for read-cursor advances (#3851): while the observed
// head keeps moving, hold the candidate boundary and only take the write path
// once the head has stayed quiet for the window. Collapses per-event advance
// POSTs on active agents to at most one per window without changing gate,
// visibility, or caught-up semantics.
export const READ_MARKER_ADVANCE_QUIET_WINDOW_MS = 2_000;
const readMarkerAdvanceTimers = new Map<string, ReturnType<typeof setTimeout>>();
const readMarkerAdvanceScheduledSeq = new Map<string, number>();
const pendingReadMarkerAgentIds = new Set<string>();
const pendingReadMarkerCursorByAgentId = new Map<string, number>();
const readMarkerScopeKeyByAgentId = new Map<string, string>();
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

function briefReadStateScopeKey(): string {
  const identity = useRuntimeStore.getState().discovery.identity;
  return JSON.stringify(identity && [
    identity.runtimeId, identity.visibilityScopeId, identity.eventLogEpoch,
  ]);
}

export async function refreshBriefReadStatesInView(afterCurrent = false): Promise<void> {
  // The initial local connection uses the module-level runtime config and
  // does not pass through setRuntimeConnection. Ensure dashboard-only tabs
  // subscribe before another tab publishes a read-state revalidation hint.
  ensureReadStateBus();
  const generation = clientGeneration;
  if (briefReadStatesRefreshInFlight && briefReadStatesRefreshGeneration === generation) {
    const inFlight = briefReadStatesRefreshInFlight;
    return afterCurrent ? inFlight.then(() => refreshBriefReadStatesInView()) : inFlight;
  }
  const scopeKey = briefReadStateScopeKey();
  const isCurrent = () =>
    isCurrentClientGeneration(generation) && briefReadStateScopeKey() === scopeKey;
  let request!: Promise<void>;
  request = (async () => {
    useRuntimeStore.setState({ briefReadStatesLoading: true, briefReadStatesError: undefined });
    try {
      const states = await runtimeClient.getBriefReadStates();
      if (!isCurrent()) return;
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
      if (!isCurrent()) return;
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

function evaluateReadMarkerGate(
  agentId: string,
  state: RuntimeStoreState,
  scope: ReturnType<typeof conversationScopeSnapshot>,
) {
  const covered = state.rightPanelOpen && (
    state.rightPanelMode === "expanded" ||
    (typeof window !== "undefined" &&
      panelLayout(window.innerWidth, true, false, state.navCollapsed, PANEL_DEFAULT).full)
  );
  const identity = state.discovery.identity;
  const viewScope = scope.view?.scope;
  const readState = state.briefReadStateByAgentId[agentId];
  return evaluateConversationReadMarkerGate(
    {
      route: state.route,
      selectedAgentId: state.selectedAgentId,
      documentVisible: typeof document !== "undefined" && document.visibilityState === "visible",
      conversationReady:
        scope.status.kind === "ready" &&
        !scope.navigationActive &&
        !scope.browsing &&
        scope.view?.scope != null &&
        scope.view.reset_reason === null,
      conversationVisible: !covered,
      discoveryFresh: state.discovery.freshness === "fresh",
      scopeCurrent: identity != null && viewScope != null && readState != null &&
        viewScope.agent_id === agentId &&
        viewScope.runtime_id === identity.runtimeId &&
        viewScope.visibility_scope_id === identity.visibilityScopeId &&
        viewScope.event_log_epoch === identity.eventLogEpoch &&
        readState.event_log_epoch === viewScope.event_log_epoch &&
        readState.visibility_scope_id === viewScope.visibility_scope_id,
      throughSeq: scope.view?.through_seq ?? null,
      observedHeadSeq: readState?.event_head_seq,
    },
    agentId,
  );
}

function clearReadMarkerAdvanceDebounce(agentId: string): void {
  const timer = readMarkerAdvanceTimers.get(agentId);
  if (timer != null) {
    clearTimeout(timer);
    readMarkerAdvanceTimers.delete(agentId);
  }
  readMarkerAdvanceScheduledSeq.delete(agentId);
}

let readMarkerPageHideFlushBound = false;

/**
 * A reload or close inside the quiet window would destroy the debounce timer
 * and lose the advance entirely, so flush gate-approved candidates during
 * `pagehide` with a keepalive fetch the browser can still deliver after the
 * page is gone (#3851). The candidates were already gated when recorded; the
 * full gate is intentionally not re-evaluated here because the page is already
 * tearing down (and may already report itself hidden).
 */
function flushPendingReadMarkerAdvancesOnPageHide(): void {
  for (const agentId of [...pendingReadMarkerAgentIds]) {
    const candidate = Math.max(
      readMarkerAdvanceScheduledSeq.get(agentId) ?? 0,
      pendingReadMarkerCursorByAgentId.get(agentId) ?? 0,
    );
    if (candidate <= 0) continue;
    runtimeClient.markBriefReadKeepalive(agentId, candidate);
  }
}

function ensureReadMarkerPageHideFlush(): void {
  if (readMarkerPageHideFlushBound) return;
  if (typeof window === "undefined" || typeof window.addEventListener !== "function") return;
  readMarkerPageHideFlushBound = true;
  window.addEventListener("pagehide", flushPendingReadMarkerAdvancesOnPageHide);
}

/**
 * Postpone the advance POST until the observed head stays quiet for the
 * debounce window. A strictly newer candidate boundary restarts the window
 * (the head is still moving); an equal-or-older one keeps the deadline, so
 * repeated refreshes at an unchanged boundary never re-arm the timer.
 */
function scheduleReadMarkerAdvance(agentId: string, candidateSeq: number): void {
  const scheduledSeq = readMarkerAdvanceScheduledSeq.get(agentId);
  if (scheduledSeq != null && candidateSeq <= scheduledSeq) return;
  readMarkerAdvanceScheduledSeq.set(agentId, candidateSeq);
  const existingTimer = readMarkerAdvanceTimers.get(agentId);
  if (existingTimer != null) clearTimeout(existingTimer);
  const timer = setTimeout(() => {
    readMarkerAdvanceTimers.delete(agentId);
    readMarkerAdvanceScheduledSeq.delete(agentId);
    void retryPendingReadMarker(agentId, { flush: true });
  }, READ_MARKER_ADVANCE_QUIET_WINDOW_MS);
  readMarkerAdvanceTimers.set(agentId, timer);
}

export async function retryPendingReadMarker(
  agentId: string,
  options: { flush?: boolean } = {},
): Promise<void> {
  if (!pendingReadMarkerAgentIds.has(agentId)) return;
  ensureReadMarkerPageHideFlush();
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
  const scopeKey = readMarkerScopeKeyByAgentId.get(agentId) ?? resolveConversationScopeKey(
    currentRemoteKey(runtimeConnectionConfig),
    agentId,
    state.currentUser,
  );
  const scope = conversationScopeSnapshot(scopeKey);
  const decision = evaluateReadMarkerGate(agentId, state, scope);
  if (!decision.mayAdvance || decision.candidateSeq == null) {
    if (decision.reason === "not_selected") {
      pendingReadMarkerAgentIds.delete(agentId);
      clearReadMarkerAdvanceDebounce(agentId);
    }
    span.end("skipped", { reason: decision.reason ?? "candidate_unavailable" });
    return;
  }

  if (options.flush !== true) {
    // Active agents move the observed head with every event batch; advancing
    // immediately would track the event rate. Wait for a quiet window first
    // (#3851). The gate is re-evaluated when the window deadline fires.
    scheduleReadMarkerAdvance(agentId, decision.candidateSeq);
    span.end("skipped", {
      reason: "quiet_window",
      candidateSeq: decision.candidateSeq,
      quietWindowMs: READ_MARKER_ADVANCE_QUIET_WINDOW_MS,
    });
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
  agentSessionRepository = new AgentSessionRepository({
    getConnectionConfig: () => runtimeConnectionConfig,
    getGeneration: () => clientGeneration,
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
  navCollapsed: false,

  bootstrap: pendingBootstrap(runtimeConnectionConfig),
  bootstrapLoading: true,
  agentRosterSource: undefined,
  currentUser: undefined,
  currentUserLoaded: false,
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
  agentCreations: {},
  agentCreationContext: 0,
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
  markAgentConversationRead: (agentId, conversationScopeKey) => {
    if (conversationScopeKey === undefined) readMarkerScopeKeyByAgentId.delete(agentId);
    else readMarkerScopeKeyByAgentId.set(agentId, conversationScopeKey);
    const current = get().briefReadStateByAgentId[agentId];
    const state = get();
    const scopeKey = conversationScopeKey ?? resolveConversationScopeKey(
      currentRemoteKey(runtimeConnectionConfig),
      agentId,
      state.currentUser,
    );
    const decision = evaluateReadMarkerGate(
      agentId,
      state,
      conversationScopeSnapshot(scopeKey),
    );
    if (current && decision.mayAdvance && decision.candidateSeq != null) {
      pendingReadMarkerCursorByAgentId.set(
        agentId,
        Math.max(pendingReadMarkerCursorByAgentId.get(agentId) ?? 0, decision.candidateSeq),
      );
      set((state) => ({
        briefReadStateByAgentId: {
          ...state.briefReadStateByAgentId,
          [agentId]: {
            ...current,
            read_through_event_seq: Math.max(current.read_through_event_seq, decision.candidateSeq!),
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
  showToolExecutionDetail: (agentId, toolExecutionId, toolName, relatedStateObjectRef, fallbackActivity) =>
    set((state) => {
      const stack = state.rightPanelView ? [...state.rightPanelViewStack, state.rightPanelView] : state.rightPanelViewStack;
      return {
      rightPanelViewStack: stack,
      rightPanelOpen: true,
      rightPanelView: { kind: "tool_execution_detail", agentId, toolExecutionId, toolName, relatedStateObjectRef, fallbackActivity },
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
      return;
    }

    if (activity.stateObjectRef?.kind === "tool_execution") {
      get().showToolExecutionDetail(agentId, activity.stateObjectRef.id, activity.stateObjectRef.toolName, activity.relatedStateObjectRef, activity);
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
  fetchAgentMessageText: async (agentId, messageId) => {
    const request = captureClientRequest();
    const response = await request.client.getAgentMessagesBatch(agentId, [messageId]);
    if (!isCurrentClientRequest(request)) return null;
    const message = response.messages?.find((item) => item.id === messageId);
    if (!message) return null;
    const text = inputPresentation(message.body).text;
    return text.length > 0 ? text : null;
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
    nextClientGeneration();
    set((state) => ({ agentCreations: {}, agentCreationContext: state.agentCreationContext + 1 }));
    const generation = clientGeneration;
    cancelClientGenerationWork();
    await clearConversationCaches();
    if (!isCurrentClientGeneration(generation)) return;
    const normalizedConfig = sameOriginConnection(config);
    runtimeConnectionConfig = normalizedConfig;
    runtimeClient = createRuntimeClient(runtimeClientOptions(normalizedConfig));
    ensureReadStateBus();
    writeStoredRuntimeConnectionConfig(normalizedConfig);
    bootstrapRefreshInFlight = undefined;
    briefReadStatesRefreshInFlight = undefined;
    briefReadStatesRefreshGeneration = undefined;
    resumeReconciliationInFlight = undefined;
    globalSyncCoordinator.resetForClientChange();
    agentSessionRepository.switchRemote();
    inspectorDetailInFlight.clear();
    set({
      currentUser: undefined,
      currentUserLoaded: false,
      agentRosterSource: undefined,
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
      agentCreations: {},
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
    if (bootstrapRefreshTimer != null) {
      globalThis.clearTimeout(bootstrapRefreshTimer);
      bootstrapRefreshTimer = undefined;
    }
    let generation = clientGeneration;
    confirmedPresentationSubjectKey = undefined;
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
      // Confirm the subject independently of a potentially slow roster/state
      // projection. Private caches remain hidden until this request succeeds.
      const subjectRequest = runtimeClient.getCurrentUser({ strict: true }).then((currentUser) => {
        if (!isCurrentClientGeneration(generation) || presentationAuthRejected(get())) return;
        const key = agentPresentationCacheKey(currentRemoteKey(runtimeConnectionConfig), currentUser ?? undefined);
        const previous = get();
        const previousKey = agentPresentationCacheKey(currentRemoteKey(runtimeConnectionConfig), previous.currentUser);
        const subjectChanged = previous.currentUserLoaded && previousKey !== key;
        if (subjectChanged) {
          // Fence all old-subject requests and streams before publishing the
          // new subject. Its roster must be requested only after this point.
          generation = nextClientGeneration();
          cancelClientGenerationWork();
          bootstrapRefreshInFlight = request;
          globalSyncCoordinator.resetForClientChange();
          clearAgentAccessRecords();
          disposeAllConversationScopes(false);
        }
        confirmedPresentationSubjectKey = key;
        set((state) => ({
          currentUser: currentUser ?? undefined, currentUserLoaded: true,
          ...(subjectChanged ? {
            agentCreations: {},
            agentCreationContext: state.agentCreationContext + 1,
            agentRosterSource: undefined,
            discovery: { mode: "pending", freshness: "fresh", retryAttempt: 0 },
            sessionsByAgentId: {},
            bootstrap: { ...state.bootstrap, agents: [], attentionCount: 0, metrics: [] },
          } : {}),
        }));
        if (!key || get().agentRosterSource === "http") return true;
        void readCachedAgents(key).then((agents) => {
          if (!agents?.length || !isCurrentClientGeneration(generation)) return;
          set((state) => {
            if (presentationAuthRejected(state) || state.agentRosterSource === "http"
              || agentPresentationCacheKey(currentRemoteKey(runtimeConnectionConfig), state.currentUser) !== key) return {};
            return {
              agentRosterSource: "cache",
              bootstrap: sortBootstrapAgents({
                ...state.bootstrap, agents,
                attentionCount: countAgentsNeedingAttention(agents), metrics: buildBootstrapMetrics(agents),
              }, state.rosterActivityByAgentId),
            };
          });
        }).catch(() => { /* best-effort presentation */ });
        return true;
      }).catch((error: unknown) => {
        if (!isCurrentClientGeneration(generation) || presentationAuthRejected(get())) return;
        if (error instanceof RuntimeHttpError && [401, 403].includes(error.status)) {
          set((state) => ({ bootstrap: {
            ...state.bootstrap, connection: { ...state.bootstrap.connection, authRequired: true },
          } }));
        } else {
          // A transient subject lookup failure is not a logout and cannot
          // authorize a new private cache read. Make the failed initialization
          // visible and retry confirmation instead of leaving an empty shell.
          const message = error instanceof Error ? error.message : String(error);
          set((state) => ({
            currentUserLoaded: true,
            bootstrapError: message,
            bootstrap: { ...state.bootstrap, connection: { ...state.bootstrap.connection, error: message } },
          }));
          scheduleBootstrapRefresh(get, 5_000, { syncEvents: options.syncEvents, trigger: "subject.retry" });
        }
      });
      try {
        // Never label a roster started under an earlier session with the
        // newly confirmed subject. Cached presentation does not wait for it.
        const subjectConfirmed = await subjectRequest;
        if (!isCurrentClientGeneration(generation)) return;
        if (!subjectConfirmed || presentationAuthRejected(get())) {
          set({ bootstrapLoading: false });
          return;
        }
        const bootstrap = await runtimeClient.getBootstrap();
        if (!isCurrentClientGeneration(generation) || presentationAuthRejected(get())) return;
        set((state) => {
          if (bootstrap.connection.source === "fixture"
            && (state.bootstrap.connection.source === "http" || state.agentRosterSource === "cache")) {
            return {
              bootstrap: {
                ...state.bootstrap,
                connection: bootstrap.connection.authRequired ? bootstrap.connection
                  : { ...state.bootstrap.connection, error: bootstrap.connection.error },
              },
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
            agentRosterSource: bootstrap.connection.source === "http" ? "http" : state.agentRosterSource,
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
        // A bootstrap that settled with a connection error (for example a
        // daemon missing the observer-sync contract) must not start the
        // authoritative roster cycle; discovery waits for a healthy connect.
        if (options.syncEvents !== false && bootstrap.connection.error == null) {
          globalSyncCoordinator.refreshRoster(get, set);
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

    if (!useResumeAdmission.getState().status.pending) {
      resumeAdmissionToken += 1;
      setResumeAdmissionStatus({ pending: true, token: resumeAdmissionToken });
    }
    const resumeSchedulesAtStart = resumeReconciliationSchedules;
    const generation = nextClientGeneration();
    cancelClientGenerationWork();
    globalSyncCoordinator.closeForResume();
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
            // The resume generation bump resets the resource scope; a state
            // read alone would render the state's work_items as the whole
            // list. Re-hydrate the list resource this reset invalidated.
            await get().refreshAgentWorkItems(agentId);
          },
          () => isCurrentClientGeneration(generation),
        );
        if (!isCurrentClientGeneration(generation)) return;
        globalSyncCoordinator.refreshRoster(get, set);
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
        settleResumeReconciliationAdmission(resumeSchedulesAtStart);
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
    if (get().runtimeConfigSaving) return;
    const request = captureClientRequest();
    const sequence = ++runtimeConfigRequestSequence;
    set({ runtimeConfigLoading: true, runtimeConfigError: undefined });
    try {
      const runtimeConfig = await request.client.getRuntimeConfig();
      if (!isCurrentClientRequest(request) || sequence !== runtimeConfigRequestSequence) return;
      set({ runtimeConfig, runtimeConfigLoading: false, runtimeConfigError: runtimeConfig.error });
    } catch (error) {
      if (!isCurrentClientRequest(request) || sequence !== runtimeConfigRequestSequence) return;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        runtimeConfig: { ...state.runtimeConfig, error: message },
        runtimeConfigLoading: false,
        runtimeConfigError: message,
      }));
    }
  },

  updateRuntimeConfig: async (updates) => {
    if (get().runtimeConfigSaving) return undefined;
    const request = captureClientRequest();
    const sequence = ++runtimeConfigRequestSequence;
    const saveSequence = ++runtimeConfigSaveSequence;
    set({ runtimeConfigSaving: true, runtimeConfigLoading: false, runtimeConfigError: undefined });
    try {
      const response = await request.client.updateRuntimeConfig(updates);
      if (!isCurrentClientRequest(request)) return undefined;
      if (sequence !== runtimeConfigRequestSequence) return undefined;
      set({ runtimeConfig: response, runtimeConfigLoading: false });
      const runtimeConfig = response.changed && response.reload
        ? {
            ...response,
            ...(await observeRuntimeConfigReload(request, response.reload.requestedGeneration)),
            changed: response.changed,
            results: response.results,
          }
        : response;
      if (!isCurrentClientRequest(request) || sequence !== runtimeConfigRequestSequence) return undefined;
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
      if (!isCurrentClientRequest(request) || sequence !== runtimeConfigRequestSequence) return undefined;
      const message = error instanceof Error ? error.message : String(error);
      set((state) => ({
        runtimeConfig: { ...state.runtimeConfig, error: message },
        runtimeConfigSaving: false,
        runtimeConfigError: message,
      }));
      return undefined;
    } finally {
      // Credential requests can supersede the response, but do not own this save.
      if (isCurrentClientRequest(request) && saveSequence === runtimeConfigSaveSequence) {
        set({ runtimeConfigSaving: false });
      }
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

  refreshSkillCatalog: (options) => refreshSkillCatalogResource(set, get, undefined, options),

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

  createAgentFromTemplate: async (agentId, template, name) => {
    const request = captureAgentCreationRequest(get);
    let created;
    try {
      created = await request.client.createAgentFromTemplate(agentId, template, name);
    } catch (error) {
      if (!isCurrentAgentCreationRequest(get, request)) return { kind: "stale" };
      if (!(error instanceof RuntimeHttpError)) return { kind: "uncertain" };
      // Raw HTTP errors can contain private paths, SQL, or credentials.
      const problem = error.code === "deletion_incomplete" ? "deletion_incomplete"
        : error.code === "already_exists" ? "conflict"
        : error.code === "agent_name_invalid" || error.status === 400 || error.status === 422 ? "invalid"
        : error.status === 401 ? "unauthorized"
        : error.status === 403 ? "forbidden"
        : error.status === 404 ? "not_found"
        : error.status === 429 || error.status === 503 ? "busy"
        : error.status >= 500 ? "server" : "unexpected";
      return { kind: "rejected", problem, status: error.status };
    }
    if (!isCurrentAgentCreationRequest(get, request)) return { kind: "stale" };
    if (!validCreationResponse(created, agentId)) return { kind: "uncertain" };
    const progress: AgentCreationProgress = {
      creation: created,
      status: created.receipt.stage === "provisioning" ? "initializing"
        : created.receipt.stage === "degraded" ? "degraded" : "ready",
      bootstrap: created.receipt.bootstrap,
      observationRevision: 0,
    };
    set((state) => ({ agentCreations: { ...state.agentCreations, [agentId]: progress } }));
    // POST acceptance is final; observation cannot delay navigation or turn
    // an accepted create into a rejected one.
    void observeAgentCreation(set, get, request, agentId, progress);
    return { kind: "accepted", creation: created };
  },

  lookupCreatedAgent: async (agentId) => {
    const request = captureAgentCreationRequest(get);
    try {
      const detail = await request.client.getAgentIdentity(agentId);
      if (!isCurrentAgentCreationRequest(get, request)) return { kind: "stale" };
      return validCreationIdentity(detail.identity, agentId)
        ? { kind: "found", identity: detail.identity } : { kind: "missing" };
    } catch (error) {
      if (!isCurrentAgentCreationRequest(get, request)) return { kind: "stale" };
      return { kind: error instanceof RuntimeHttpError && error.status === 404 ? "missing" : "unavailable" };
    }
  },

  refreshAgentCreation: async (agentId) => {
    const progress = get().agentCreations[agentId];
    if (progress) await reconcileAgentCreation(set, get, captureAgentCreationRequest(get), agentId, progress);
  },

  // The banner states that the agent was created, not that its first turn
  // succeeded. Once the conversation proves a settled successful turn, the
  // banner is redundant and retires; a failed first turn keeps it visible.
  dismissAgentCreation: (agentId) => {
    set((state) => {
      if (!state.agentCreations[agentId]) return {};
      const agentCreations = { ...state.agentCreations };
      delete agentCreations[agentId];
      return { agentCreations };
    });
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
      if (!isCurrentClientRequest(request)) return false;
      await refreshAffectedSkillCatalogs(set, get);
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
      if (!isCurrentClientRequest(request)) return false;
      await refreshAffectedSkillCatalogs(set, get);
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

  refreshAgentSkillCatalog: (agentId, options) => agentId
    ? refreshSkillCatalogResource(set, get, agentId, options)
    : Promise.resolve(),

  enableAgentSkill: async (agentId, name) => {
    if (!agentId) return false;
    const request = captureClientRequest();
    const scopeKey = skillCatalogScopeKey(get(), agentId);
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
      if (!isCurrentClientRequest(request) || skillCatalogScopeKey(get(), agentId) !== scopeKey) return false;
      await refreshAffectedSkillCatalogs(set, get, agentId);
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request) || skillCatalogScopeKey(get(), agentId) !== scopeKey) return false;
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
    const scopeKey = skillCatalogScopeKey(get(), agentId);
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
      if (!isCurrentClientRequest(request) || skillCatalogScopeKey(get(), agentId) !== scopeKey) return false;
      await refreshAffectedSkillCatalogs(set, get, agentId);
      return true;
    } catch (error) {
      if (!isCurrentClientRequest(request) || skillCatalogScopeKey(get(), agentId) !== scopeKey) return false;
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
    const configSequence = ++runtimeConfigRequestSequence;
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
        if (configSequence !== runtimeConfigRequestSequence) return;
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
    const configSequence = ++runtimeConfigRequestSequence;
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
        if (configSequence !== runtimeConfigRequestSequence) return;
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
    const resourceContextKey = agentResourceContext(get());
    const expectedIncarnation = options.incarnation ?? get().bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation;
    const existing = agentDetailRefreshInFlight.get(key);
    if (existing?.generation === request.generation && existing.contextKey === resourceContextKey
      && existing.incarnation === expectedIncarnation) {
      span.end("deduped");
      return existing.promise;
    }
    const sequence = (agentDetailRequestSequence.get(agentId) ?? 0) + 1;
    agentDetailRequestSequence.set(agentId, sequence);
    const resourceSequence = ++agentResourceReadSequence;
    const isCurrentDetailRequest = () => {
      const agent = get().bootstrap.agents.find((entry) => entry.id === agentId);
      return isCurrentClientRequest(request) && agentDetailRequestSequence.get(agentId) === sequence
        && agentResourceContext(get()) === resourceContextKey
        && (expectedIncarnation === undefined || agent?.incarnation === undefined || agent.incarnation === expectedIncarnation);
    };
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
        if (!isCurrentDetailRequest()) return;
        if (!detail.error && expectedIncarnation !== undefined && detail.agent.incarnation !== expectedIncarnation) {
          throw new RuntimeHttpError("GET", `/agents/${agentId}/state`, 409,
            "Agent identity changed.", "agent_incarnation_mismatch");
        }
        set((state) => mergeAgentDetailIntoSession(state, agentId, detail, resourceSequence));
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
        span.end(detail.error ? "error" : "ok");
      } catch (error) {
        if (!isCurrentDetailRequest()) return;
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
        const canRecover = Boolean(currentSession?.eventSeqs.length);
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
    agentDetailRefreshInFlight.set(key, {
      generation: request.generation, contextKey: resourceContextKey, incarnation: expectedIncarnation, promise,
    });
    return promise;
  },

  refreshAgentWorkItems: async (agentId, options = {}) => {
    if (!agentId) return;
    await refreshAgentResource(set, get, agentId, "workItems", options);
  },

  queryAgentRecentTasks: async (agentId, options = {}) => {
    const request = captureClientRequest();
    const tasks = await request.client.getAgentRecentTasks(agentId, options);
    if (!isCurrentClientRequest(request) || options.signal?.aborted
      || (options.incarnation !== undefined && get().bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation !== options.incarnation)) {
      throw new AgentResourceSupersededError(agentId, "recentTasks");
    }
    return tasks;
  },
  queryAgentTimers: async (agentId, options = {}) => {
    const request = captureClientRequest();
    const timers = await request.client.getAgentTimers(agentId, options);
    if (!isCurrentClientRequest(request) || options.signal?.aborted
      || (options.incarnation !== undefined && get().bootstrap.agents.find((agent) => agent.id === agentId)?.incarnation !== options.incarnation)) {
      throw new AgentResourceSupersededError(agentId, "timers");
    }
    return timers;
  },
  cancelAgentTimer: async (agentId, timerId) => {
    const request = captureClientRequest();
    const timer = await request.client.cancelAgentTimer(agentId, timerId);
    if (!isCurrentClientRequest(request)) return timer;
    return timer;
  },

  refreshAgentState: async (agentId, options = {}) => {
    if (!agentId) return;
    await refreshAgentResource(set, get, agentId, "state", options);
  },

  loadAgentWorkItemDetail: async (agentId, workItemId, options = {}) => {
    if (!agentId || !workItemId) return;
    await refreshAgentObjectDetail(set, get, agentId, workItemId, "workItem", options);
  },

  loadAgentTaskDetail: async (agentId, taskId, force, options = {}) => {
    if (!agentId || !taskId) return;
    await refreshAgentObjectDetail(set, get, agentId, taskId, "task", { ...options, force: force ?? options.force });
  },

  loadAgentToolExecutionDetail: async (agentId, toolExecutionId, fallbackActivity, conversationRevision, options = {}) => {
    if (!agentId || !toolExecutionId) return;
    try {
      await refreshAgentObjectDetail(set, get, agentId, toolExecutionId, "toolExecution",
        { ...options, throwOnError: true }, conversationRevision);
    } catch (error) {
      // If the tool execution record doesn't exist (e.g. historical events
      // without tool_execution_id), fall back to the activity inspector
      // which renders structured detail from the raw event payload.
      if (fallbackActivity && error instanceof RuntimeHttpError && error.status === 404) {
        const selection = get().rightPanelView;
        if (selection?.kind !== "tool_execution_detail" || selection.agentId !== agentId
          || selection.toolExecutionId !== toolExecutionId) return;
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
      if (options.throwOnError) throw error;
    }
  },

  sendOperatorPrompt: async (agentId, text, attachments = [], expectedIncarnation) => {
    const prompt = text.trim();
    if (!agentId || (!prompt && attachments.length === 0)) {
      return;
    }

    const request = captureClientRequest();
    // Tracked outside try so a failed enqueue can drop the local echo; left
    // undefined when even the client id could not be generated.
    let echoClientId: string | undefined;
    const createdAt = new Date().toISOString();
    try {
      const clientId = generateUuid();
      echoClientId = clientId;
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
              // Local echo: keep the sent prompt visible until the server
              // conversation view carries it (pending input or turn input).
              pendingOperatorPrompts: [
                ...(state.sessionsByAgentId[agentId]?.pendingOperatorPrompts ?? []),
                {
                  clientId,
                  text: prompt,
                  senderName: get().currentUser?.displayName,
                  createdAt,
                  ...(attachments.length > 0 ? { attachmentCount: attachments.length } : {}),
                },
              ],
            },
          },
        };
      });

      const { messageId } = await request.client.sendOperatorPrompt(agentId, prompt, attachments, expectedIncarnation);
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
            pendingOperatorPrompts: markPendingOperatorPromptConfirmed(
              state.sessionsByAgentId[agentId]?.pendingOperatorPrompts ?? [],
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
            // The composer keeps its draft on failure; the local echo
            // must not duplicate a prompt that never reached the server.
            pendingOperatorPrompts: (state.sessionsByAgentId[agentId]?.pendingOperatorPrompts ?? [])
              .filter((entry) => entry.clientId !== echoClientId),
          },
        },
      }));
      throw error;
    }
  },

  pruneOperatorPrompts: (agentId, confirmedMessageIds) => {
    if (!agentId) return;
    const confirmed = new Set(confirmedMessageIds);
    set((state) => {
      const session = state.sessionsByAgentId[agentId];
      const prompts = session?.pendingOperatorPrompts;
      if (!prompts || prompts.length === 0) return {};
      const retained = retainPendingOperatorPrompts(prompts, confirmed, Date.now());
      if (retained === null) return {};
      return {
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...session,
            pendingOperatorPrompts: retained,
          },
        },
      };
    });
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

  setAgentModel: async (agentId, model, reasoningEffort, serviceTier) => {
    if (!agentId || !model) return;
    const request = captureClientRequest();
    const previousAgent = get().sessionsByAgentId[agentId]?.detail?.agent;
    setSessionModelError(set, agentId, undefined);
    try {
      const modelState = await request.client.setAgentModel(agentId, model, reasoningEffort, serviceTier);
      if (!isCurrentClientRequest(request)) return;
      set((state) =>
        updateAgentModelInState(state, agentId, {
          model: modelState?.active_model ?? modelState?.effective_model ?? model,
          modelSelection: modelState?.effective_model,
          runtimeDefaultModel: modelState?.runtime_default_model,
          modelSource: modelState?.source ?? "agent_override",
          modelReasoningEffort: modelState?.override_reasoning_effort ?? undefined,
          modelServiceTier: modelState?.effective_service_tier ?? undefined,
          modelServiceTierOverride: modelState?.override_service_tier ?? undefined,
        }),
      );
      if (!refreshSelectedAgentResources(agentId)) await get().refreshAgentState(agentId);
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
          modelServiceTier: modelState?.effective_service_tier ?? undefined,
          modelServiceTierOverride: modelState?.override_service_tier ?? undefined,
        }),
      );
      if (!refreshSelectedAgentResources(agentId)) await get().refreshAgentState(agentId);
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
      refreshSelectedAgentResources(agentId);
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
      if (!refreshSelectedAgentResources(agentId)) await get().refreshAgentState(agentId);
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

  retryAgentSync: (agentId) => {
    if (!agentId) return;
    globalSyncCoordinator.retryAgentSync(get, set, agentId);
    void get().refreshAgentState(agentId);
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
      scheduleReconciliation();
    }
    hiddenAt = undefined;
  };
  const schedulePageShow = (event: PageTransitionEvent) => {
    if (event.persisted) scheduleReconciliation();
  };
  const scheduleReconciliation = () => {
    markResumeReconciliationScheduled();
    coordinator.schedule();
  };
  const scheduleOnline = () => scheduleReconciliation();
  document.addEventListener("visibilitychange", scheduleIfVisible);
  window.addEventListener("pageshow", schedulePageShow);
  window.addEventListener("online", scheduleOnline);
  return coordinator;
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

function scheduleAgentSkillCatalogRetry(get: () => RuntimeStoreState, agentId: string, scopeKey: string): void {
  if (agentSkillCatalogRetryTimers.has(agentId)) return;
  const attempt = agentSkillCatalogRetryAttempts.get(agentId) ?? 0;
  if (attempt >= AGENT_SKILL_CATALOG_RETRY_DELAYS_MS.length) return;
  agentSkillCatalogRetryAttempts.set(agentId, attempt + 1);
  armAgentSkillCatalogRetry(
    get,
    agentId,
    AGENT_SKILL_CATALOG_RETRY_DELAYS_MS[attempt] + retryJitterMs(),
    scopeKey,
  );
}

function armAgentSkillCatalogRetry(get: () => RuntimeStoreState, agentId: string, delayMs: number, scopeKey: string): void {
  const timer = window.setTimeout(() => {
    agentSkillCatalogRetryTimers.delete(agentId);
    if (skillCatalogScopeKey(get(), agentId) !== scopeKey) {
      clearAgentSkillCatalogRetry(agentId);
      return;
    }
    const discovery = get().discovery;
    if (discovery.mode === "authoritative" && discovery.freshness !== "fresh") {
      // Discovery down: pause without fetching or consuming another attempt
      // so the retry resumes once a fresh roster snapshot settles.
      armAgentSkillCatalogRetry(
        get,
        agentId,
        AGENT_SKILL_CATALOG_RETRY_DELAYS_MS[AGENT_SKILL_CATALOG_RETRY_DELAYS_MS.length - 1] + retryJitterMs(),
        scopeKey,
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

function scheduleBootstrapRefresh(get: () => RuntimeStoreState, delayMs = 1_000, options: BootstrapRefreshOptions = {}): void {
  if (bootstrapRefreshTimer != null) return;
  bootstrapRefreshTimer = globalThis.setTimeout(() => {
    bootstrapRefreshTimer = undefined;
    void get().refreshBootstrap({ ...options, background: true });
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
        await refreshAffectedSkillCatalogs(set, get);
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

const AGENT_BOOTSTRAP_POLL_INTERVAL_MS = 1_000;
const AGENT_BOOTSTRAP_POLL_TIMEOUT_MS = 120_000;

function isCurrentAgentCreation(
  get: () => RuntimeStoreState,
  request: AgentCreationRequest,
  agentId: string,
  progress: AgentCreationProgress,
): boolean {
  return isCurrentAgentCreationRequest(get, request) && get().agentCreations[agentId]?.creation === progress.creation;
}

async function reconcileAgentCreation(
  set: StoreSet,
  get: () => RuntimeStoreState,
  request: AgentCreationRequest,
  agentId: string,
  progress: AgentCreationProgress,
  observationEnded = false,
): Promise<void> {
  if (!isCurrentAgentCreation(get, request, agentId, progress)) return;
  const observationRevision = get().agentCreations[agentId].observationRevision + 1;
  set((state) => ({ agentCreations: {
    ...state.agentCreations,
    [agentId]: { ...state.agentCreations[agentId], observationRevision },
  } }));
  const current = () => isCurrentAgentCreation(get, request, agentId, progress)
    && get().agentCreations[agentId]?.observationRevision === observationRevision;
  try {
    const detail = await request.client.getAgentIdentity(agentId);
    if (!current()) return;
    if (!validCreationIdentity(detail.identity, agentId)
      || detail.identity.incarnation !== progress.creation.identity.incarnation) {
      set((state) => {
        const agentCreations = { ...state.agentCreations };
        delete agentCreations[agentId];
        return { agentCreations };
      });
      return;
    }
    const bootstrap = detail.bootstrap ?? undefined;
    const status = bootstrapProgress(bootstrap);
    set((state) => ({
      agentCreations: {
        ...state.agentCreations,
        [agentId]: {
          creation: progress.creation,
          observationRevision,
          bootstrap,
          status: observationEnded && status === "initializing" ? "uncertain" : status,
        },
      },
    }));
  } catch {
    if (!current()) return;
    set((state) => ({
      agentCreations: {
        ...state.agentCreations,
        [agentId]: { ...state.agentCreations[agentId], status: "uncertain" },
      },
    }));
  }
}

async function observeAgentCreation(
  set: StoreSet,
  get: () => RuntimeStoreState,
  request: AgentCreationRequest,
  agentId: string,
  progress: AgentCreationProgress,
): Promise<void> {
  const refreshRoster = async () => {
    if (!isCurrentAgentCreation(get, request, agentId, progress)) return;
    if (get().discovery.mode === "authoritative") globalSyncCoordinator.refreshRoster(get, set);
    else await get().refreshBootstrap({ background: true });
  };
  // Roster availability and initialization observation are independent.
  void refreshRoster().catch(() => undefined);
  const jobId = progress.creation.receipt.job_id;
  if (progress.creation.receipt.stage !== "provisioning" || !jobId) {
    if (progress.creation.receipt.stage === "provisioning") {
      await reconcileAgentCreation(set, get, request, agentId, progress, true);
    }
    return;
  }
  const deadline = Date.now() + AGENT_BOOTSTRAP_POLL_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (!isCurrentAgentCreation(get, request, agentId, progress)) return;
    await new Promise((resolve) => globalThis.setTimeout(resolve, AGENT_BOOTSTRAP_POLL_INTERVAL_MS));
    if (!isCurrentAgentCreation(get, request, agentId, progress)) return;
    try {
      const job = await request.client.getJob(jobId);
      if (!isCurrentAgentCreation(get, request, agentId, progress)) return;
      if (job.status === "completed" || job.status === "failed") break;
    } catch {
      // Jobs are volatile; detail's durable bootstrap is authoritative.
      break;
    }
  }
  await reconcileAgentCreation(set, get, request, agentId, progress, true);
  void refreshRoster().catch(() => undefined);
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
  const stateOwnsFocus = session.resourceRefresh?.state?.dataRequestId !== undefined;
  const detail = session.detail
    ? {
        ...session.detail,
        agent: patchAgentWorkItems(session.detail.agent, workItems, stateOwnsFocus),
      }
    : session.detail;
  const agents = state.bootstrap.agents.map((agent) => (
    agent.id === agentId ? patchAgentWorkItems(agent, workItems, stateOwnsFocus) : agent
  ));

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
        workItemsSnapshot: workItems,
      },
    },
  };
}

function patchAgentWorkItems(agent: AgentSummary, workItems: WorkItemSummary[], stateOwnsFocus = false): AgentSummary {
  const focusId = agent.currentWorkItemId === null ? undefined : agent.currentWorkItemId ?? agent.currentWork?.id;
  const currentWork = stateOwnsFocus
    ? workItems.find((item) => item.id === focusId)
      ?? (agent.currentWork?.id === focusId ? agent.currentWork : undefined)
    : workItems.find((item) => item.current);
  return {
    ...agent,
    currentWork,
    workItems: workItems.map((item) => ({ ...item, current: item.id === currentWork?.id })),
  };
}

function preserveResourceOwnedAgent(
  incoming: AgentSummary,
  cached: AgentSummary,
  resources: AgentSessionState["resourceRefresh"],
  afterSequence = 0,
): AgentSummary {
  let agent = (resources?.state?.dataRequestId ?? 0) > afterSequence
    ? { ...cached, name: incoming.name, canRename: incoming.canRename,
        lastBrief: incoming.lastBrief || cached.lastBrief }
    : incoming;
  if ((resources?.workItems?.dataRequestId ?? 0) > afterSequence) {
    agent = patchAgentWorkItems(agent, cached.workItems ?? [], true);
  }
  return agent;
}

function mergeAgentStateIntoState(state: RuntimeStoreState, agentId: string, freshAgent: AgentSummary): Partial<RuntimeStoreState> {
  const session = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
  const cachedDetail = session.detail;
  const cachedAgent = cachedDetail?.agent ?? state.bootstrap.agents.find((agent) => agent.id === agentId);
  const workItems = session.workItemsSnapshot ?? freshAgent.workItems ?? [];
  // State owns focus and active tasks; list membership belongs to /work-items,
  // including a successful empty list. Neither may resurrect a cleared focus.
  const mergedAgent = patchAgentWorkItems({
    ...freshAgent,
    lastBrief: cachedAgent?.lastBrief || freshAgent.lastBrief,
  }, workItems, true);
  const detail = cachedDetail ? { ...cachedDetail, agent: mergedAgent } : createLiveAgentDetail(mergedAgent);
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
  modelPatch: Pick<AgentSummary, "model"> & Partial<Pick<AgentSummary, "modelSource" | "modelReasoningEffort" | "modelServiceTier" | "modelServiceTierOverride" | "modelSelection" | "runtimeDefaultModel">>,
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

export function mergeAgentDetailIntoSession(
  state: RuntimeStoreState,
  agentId: string,
  detail: AgentDetail,
  resourceSequence = Number.POSITIVE_INFINITY,
): Partial<RuntimeStoreState> {
  const conversation = detail.conversation;
  const current = sessionForEventLogEpoch(
    state.sessionsByAgentId[agentId] ?? emptyAgentSession(),
    conversation?.event_log_epoch,
  );
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
  const liveDetailIsNewer = (current.newestSeq ?? 0) > (conversation?.event_head_seq ?? 0);
  const freshAgent = liveDetailIsNewer && current.detail ? mergeNewerLiveAgentState(detail.agent, current.detail.agent) : detail.agent;
  // Discovery owns the brief preview; detail no longer hydrates raw brief events.
  const rosterAgent = state.bootstrap.agents.find((entry) => entry.id === agentId);
  const resourceAgent = current.detail && current.detail.agent.incarnation === freshAgent.incarnation
    ? preserveResourceOwnedAgent(freshAgent, current.detail.agent, current.resourceRefresh, resourceSequence)
    : freshAgent;
  const agent = {
    ...resourceAgent,
    lastBrief: rosterAgent?.lastBrief ?? current.detail?.agent.lastBrief ?? freshAgent.lastBrief,
  };
  const detailBase: AgentDetail = {
    ...detail,
    agent,
    timeline: current.detail?.timeline ?? detail.timeline,
    hasOlderEvents: undefined,
  };
  const newestSeq = Math.max(conversation?.event_head_seq ?? 0, current.newestSeq ?? 0);
  const hasContent = conversation && (
    conversation.turns.length > 0 ||
    conversation.active_turns.length > 0 ||
    conversation.pending_inputs.length > 0 ||
    (conversation.coverage?.has_older ?? conversation.has_more) ||
    conversation.coverage?.has_newer
  );
  const confirmedMessages = new Set([
    ...(conversation?.pending_inputs.map((input) => input.message_id) ?? []),
    ...(conversation ? [...conversation.turns, ...conversation.active_turns]
      .flatMap((turn) => turn.inputs.map((input) => input.message_id)) : []),
  ]);

  return {
    bootstrap:
      detail.source === "http" && !detail.error
        ? sortBootstrapAgents(mergeAgentIntoBootstrap(state.bootstrap, agent), state.rosterActivityByAgentId)
        : state.bootstrap,
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...current,
        detail: detailBase,
        loading: false,
        liveStatus: detail.error ? "error" : current.liveStatus,
        contentStatus: conversation
          ? hasContent ? "available" : "confirmed-empty"
          : current.contentStatus,
        syncStatus: detail.error ? "error" : "idle",
        lastValidatedAt: detail.error ? current.lastValidatedAt : Date.now(),
        detailValidatedAt: detail.error ? current.detailValidatedAt : Date.now(),
        newestSeq: newestSeq || undefined,
        oldestSeq: conversation?.oldest_retained_seq ?? current.oldestSeq,
        pendingOperatorPrompts: conversation
          ? retainPendingOperatorPrompts(current.pendingOperatorPrompts, confirmedMessages, Date.now()) ?? current.pendingOperatorPrompts
          : current.pendingOperatorPrompts,
        error: detail.error,
      },
    },
  };
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

function presentationAuthRejected(state: RuntimeStoreState): boolean {
  return Boolean(state.bootstrap.connection.authRequired)
    || state.discovery.freshness === "unauthorized";
}

useRuntimeStore.subscribe((state, previous) => {
  if (presentationAuthRejected(state)) {
    if (!presentationAuthRejected(previous)) {
      // Auth rejection is terminal for both presentation and conversation
      // caches, including async hydration still in flight.
      void clearConversationCaches();
      useRuntimeStore.setState({
        agentCreations: {},
        agentCreationContext: state.agentCreationContext + 1,
      });
    }
    if (state.currentUser || !state.currentUserLoaded || state.bootstrap.agents.length > 0 || state.agentRosterSource !== undefined) {
      useRuntimeStore.setState({
        currentUser: undefined, currentUserLoaded: true, agentRosterSource: undefined,
        bootstrap: { ...state.bootstrap, agents: [], attentionCount: 0, metrics: [] },
      });
    }
  } else if (state.agentRosterSource === "http"
    && (state.bootstrap.agents !== previous.bootstrap.agents
      || state.currentUser !== previous.currentUser || state.agentRosterSource !== previous.agentRosterSource)) {
    const key = agentPresentationCacheKey(currentRemoteKey(runtimeConnectionConfig), state.currentUser);
    if (key && key === confirmedPresentationSubjectKey) void writeCachedAgents(key, state.bootstrap.agents);
  }
  if (state.rightPanelView !== previous.rightPanelView) rememberPanelView(state.rightPanelView);
  if (state.rightPanelOpen !== previous.rightPanelOpen || state.rightPanelMode !== previous.rightPanelMode) {
    writePanelPreferences({ ...readPanelPreferences(), open: state.rightPanelOpen, mode: state.rightPanelMode });
  }
});
