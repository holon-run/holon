import { agentDetailFixtures } from "./fixtures";
import { briefIdForPayload, reduceAgentSessionTimeline, transcriptEntryIdForPayload } from "./session-reducer";
import type {
  AddSkillInput,
  AgentDetail,
  AgentSummary,
  AgentTemplateCatalogDiagnostic,
  AgentTemplateCatalogEntry,
  AgentTemplateCatalogState,
  AgentTemplateDetail,
  AgentTemplateDetailState,
  AgentTemplateRemoteSource,
  CredentialProfileStatus,
  CredentialStoreState,
  CodexDeviceLoginResponse,
  DashboardMetric,
  MemorySourceContent,
  RuntimeBootstrap,
  RuntimeConfigState,
  RuntimeConfigSurface,
  RuntimeConnection,
  RuntimeBriefRecord,
  RuntimeMessageEnvelope,
  RuntimeModelCatalog,
  RuntimeModelOption,
  RuntimeSearchOptions,
  SkillCatalogState,
  SkillDetailState,
  SkillScope,
  SkillInstallMode,
  RuntimeTaskOutputResult,
  RuntimeTranscriptEntry,
  RuntimeToolExecutionRecord,
  SearchResponse,
  TaskSummary,
  WorkItemSummary,
  WorkspaceSummary,
  DisplayLevel,
  WorkspaceDirectoryListing,
  WorkspaceFileContent,
  WorkspaceFileEntry,
} from "./types";

export interface RuntimeClientOptions {
  mode?: "local" | "remote";
  baseUrl?: string;
  token?: string;
  fetchImpl?: typeof fetch;
}

const DEFAULT_DEV_API_BASE = "/api";
const DEFAULT_REQUEST_TIMEOUT_MS = 8000;
const OPTIONAL_DETAIL_TIMEOUT_MS = 4000;

function fixtureAgentDetail(agentId: string): AgentDetail {
  return agentDetailFixtures[agentId] ?? agentDetailFixtures[Object.keys(agentDetailFixtures)[0]];
}

function disconnectedAgentDetail(agentId: string, error: string): AgentDetail {
  return {
    agent: {
      id: agentId,
      badge: "!",
      badgeTone: "muted",
      profile: "unavailable",
      lifecycle: "unknown",
      focusSummary: "Runtime API unavailable",
      workspace: "unavailable",
      attention: "API disconnected",
      model: "unavailable",
      modelReasoningEffort: undefined,
      footer: "disconnected",
      subtitle: "Runtime API unavailable",
      lastBrief: "",
      lastTurnTime: "",
      pending: 0,
      activeTaskCount: 0,
      waitingCount: 0,
      posture: "disconnected",
      postureReason: error,
      tasks: [],
      workItems: [],
    },
    source: "fixture",
    timeline: [],
    events: [],
    error,
  };
}

function disconnectedAgentSummary(agentId: string, error: string): AgentSummary {
  return {
    id: agentId,
    badge: "!",
    badgeTone: "muted",
    profile: "unavailable",
    lifecycle: "unknown",
    focusSummary: "Runtime API unavailable",
    workspace: "unavailable",
    attention: "API disconnected",
    model: "unavailable",
    modelReasoningEffort: undefined,
    footer: "disconnected",
    subtitle: "Runtime API unavailable",
    lastBrief: "",
    lastTurnTime: "",
    pending: 0,
    activeTaskCount: 0,
    waitingCount: 0,
    posture: "disconnected",
    postureReason: error,
    tasks: [],
    workItems: [],
  };
}

async function fetchAgentDetail(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
  displayLevel: DisplayLevel,
): Promise<AgentDetail> {
  const encodedAgentId = encodeURIComponent(agentId);
  const eventDisplayLevel = displayLevel;
  const [entry, state, events, workItems] = await Promise.all([
    getJson<AgentListEntryDto[]>(fetchImpl, baseUrl, "/agents/list", { timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS, headers })
      .then((agents) => agents.find((agent) => agent.identity?.agent_id === agentId))
      .catch(() => undefined),
    getJson<AgentStateDto>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/state`, { headers }),
    fetchAgentEvents(baseUrl, fetchImpl, headers, agentId, { limit: 80, order: "desc", displayLevel: eventDisplayLevel }).catch((): EventPageResponseDto => ({
      events: [],
      has_older: false,
    })),
    fetchAgentWorkItems(baseUrl, fetchImpl, headers, agentId, { limit: 50 }).catch((): WorkItemDto[] => []),
  ]);
  const fallbackEntry: AgentListEntryDto = entry ?? { identity: { agent_id: agentId } };
  const transcriptEntriesById = await fetchTranscriptEntriesForEvents(baseUrl, fetchImpl, headers, agentId, events.events ?? []);
  const briefRecordsById = await fetchBriefRecordsForEvents(baseUrl, fetchImpl, headers, agentId, events.events ?? [], transcriptEntriesById);
  const agent = projectAgent(
    fallbackEntry,
    state,
    newestBriefFromEvents(events.events ?? [], transcriptEntriesById, briefRecordsById),
    workItems,
  );
  const timeline = reduceAgentSessionTimeline({ events, eventDisplayLevel, transcriptEntriesById, briefRecordsById });

  return {
    agent,
    source: "http",
    timeline,
    events: events.events ?? [],
    briefRecordsById,
    eventCursorSeq: events.cursor_seq,
    newestEventSeq: events.newest_seq,
    oldestEventSeq: events.oldest_seq,
    hasOlderEvents: events.has_older,
  };
}

async function fetchAgentState(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
): Promise<AgentSummary> {
  const encodedAgentId = encodeURIComponent(agentId);
  const [entry, state] = await Promise.all([
    getJson<AgentListEntryDto[]>(fetchImpl, baseUrl, "/agents/list", { timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS, headers })
      .then((agents) => agents.find((agent) => agent.identity?.agent_id === agentId))
      .catch(() => undefined),
    getJson<AgentStateDto>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/state`, { headers }),
  ]);
  const fallbackEntry: AgentListEntryDto = entry ?? { identity: { agent_id: agentId } };
  return projectAgent(fallbackEntry, state);
}

interface AgentListEntryDto {
  identity?: {
    agent_id?: string;
    visibility?: string;
    ownership?: string;
    profile_preset?: string;
  };
  status?: string;
  scheduling_posture?: {
    posture?: string;
    reason?: string;
  };
  lifecycle?: string | Record<string, unknown>;
  pending?: number;
  current_run_id?: string | null;
  waiting_reason?: unknown;
  model?: {
    source?: "runtime_default" | "agent_override";
    effective_model?: string;
    active_model?: string | null;
    override_reasoning_effort?: string | null;
  };
  active_workspace_entry?: {
    workspace_id?: string;
    workspace_alias?: string | null;
    execution_root_id?: string;
    workspace_anchor?: string;
    execution_root?: string;
    cwd?: string;
    projection_kind?: string;
    access_mode?: string;
    projection_metadata?: {
      worktree_branch?: string;
      worktree_path?: string;
    };
  } | null;
}

interface AgentStateDto {
  agent?: {
    identity?: AgentListEntryDto["identity"];
    agent?: {
      id?: string;
      status?: string;
      pending?: number;
      last_brief_at?: string | null;
      current_work_item_id?: string | null;
    };
    scheduling_posture?: AgentListEntryDto["scheduling_posture"];
    active_task_count?: number;
    lifecycle?: string | Record<string, unknown>;
    model?: AgentListEntryDto["model"];
    active_waiting_intents?: unknown[];
  };
  session?: {
    pending_count?: number;
    current_run_id?: string | null;
  };
  tasks?: Array<{
    id?: string;
    kind?: string;
    status?: string;
    summary?: string;
    detail?: unknown;
  }>;
  work_items?: Array<{
    id?: string;
    objective?: string;
    state?: string;
    plan_status?: string;
  }>;
  waiting_intents?: unknown[];
  workspace?: AgentWorkspaceDto;
}

interface WorkItemDto {
  id?: string;
  agent_id?: string;
  revision?: number;
  objective?: string;
  state?: string;
  plan_status?: string;
  plan_artifact?: {
    path?: string;
    relative_path?: string;
    workspace_id?: string;
    workspace_alias?: string;
    preview?: string;
    preview_complete?: boolean;
    updated_at?: string;
  };
  todo_list?: Array<{
    text?: string;
    state?: string;
  }>;
  work_refs?: Array<{
    kind?: string;
    ref?: string;
    title?: string;
    reason?: string;
    status?: string;
    last_seen_at?: string;
  }>;
  blocked_by?: string;
  recheck_at?: string;
  result_brief_id?: string;
  result_summary?: string;
  created_at?: string;
  updated_at?: string;
}

interface AgentWorkspaceDto {
    workspaces?: Array<{
      workspace_id: string;
      workspace_alias?: string | null;
      workspace_anchor?: string | null;
      repo_name?: string | null;
      is_active?: boolean;
      execution_root_id?: string | null;
      execution_root?: string | null;
      cwd?: string | null;
      projection_kind?: string | null;
      access_mode?: string | null;
      worktree?: {
        branch?: string | null;
        path?: string | null;
        original_branch?: string | null;
        original_cwd?: string | null;
      } | null;
    }>;
}

type BriefRecordDto = RuntimeBriefRecord;

export interface EventPageResponseDto {
  events?: EventEnvelopeDto[];
  oldest_seq?: number;
  newest_seq?: number;
  cursor_seq?: number;
  has_older?: boolean;
}

interface EventEnvelopeDto {
  id?: string;
  event_seq?: number;
  ts?: string;
  type?: string;
  payload?: unknown;
}

export interface StreamEventEnvelopeDto {
  id?: string;
  event_seq?: number;
  ts?: string;
  agent_id?: string;
  type?: string;
  provenance?: unknown;
  payload?: unknown;
}

export interface AgentEventStreamSubscription {
  close: () => void;
}

export interface AgentEventStreamOptions {
  afterSeq?: number;
  limit?: number;
  onOpen?: () => void;
  onActivity?: () => void;
  onEvent: (event: StreamEventEnvelopeDto) => void;
  onClose?: () => void;
  onError?: (error: Error) => void;
}

export interface AgentEventPageOptions {
  beforeSeq?: number;
  afterSeq?: number;
  limit?: number;
  order?: "asc" | "desc";
  displayLevel?: DisplayLevel;
}

interface RuntimeModelsDto {
  available_models?: Array<string | RuntimeAvailableModelDto>;
  model_availability?: ModelAvailabilityDto[];
}

interface RuntimeAvailableModelDto {
  model?: string;
  provider?: string;
  display_name?: string;
  capabilities?: {
    image_input?: boolean;
    reasoning_summaries?: boolean;
    supports_reasoning?: boolean;
  };
  policy?: {
    supported_parameters?: string[];
    capabilities?: {
      image_input?: boolean;
      reasoning_summaries?: boolean;
      supports_reasoning?: boolean;
    };
  };
  supported_parameters?: string[];
}

interface ModelAvailabilityDto {
  model?: string;
  provider?: string;
  display_name?: string;
  available?: boolean;
  unavailable_reason?: string;
  policy?: {
    supported_parameters?: string[];
    capabilities?: {
      image_input?: boolean;
      reasoning_summaries?: boolean;
      supports_reasoning?: boolean;
    };
  };
}

interface AgentModelStateDto {
  source?: "runtime_default" | "agent_override";
  effective_model?: string;
  active_model?: string | null;
  override_reasoning_effort?: string | null;
}

interface AgentModelResponseDto {
  model?: AgentModelStateDto;
}

interface RuntimeConfigResponseDto {
  ok?: boolean;
  changed?: boolean;
  config_file_path?: string;
  runtime_surface?: RuntimeConfigSurfaceDto;
  results?: RuntimeConfigUpdateResultDto[];
}

interface RuntimeConfigSurfaceDto {
  model_default?: string;
  model_fallbacks?: string[];
  vision_default?: string | null;
  model_catalog?: string[];
  unknown_model_fallback_configured?: boolean;
  runtime_max_output_tokens?: number;
  default_tool_output_tokens?: number;
  max_tool_output_tokens?: number;
  disable_provider_fallback?: boolean;
  providers?: RuntimeProviderSummaryDto[];
  web_search?: RuntimeWebSearchSummaryDto;
  web_search_providers?: RuntimeWebSearchProviderSummaryDto[];
}

interface RuntimeProviderSummaryDto {
  id?: string;
  transport?: string;
  base_url?: string;
  credential_source?: string;
  credential_kind?: string;
  credential_env?: string;
  credential_profile?: string;
  credential_external?: string;
  credential_configured?: boolean;
  configured_in_config?: boolean;
}

interface RuntimeConfigUpdateResultDto {
  key?: string;
  effect?: "accepted_requires_restart" | "accepted_reloaded" | "rejected";
  reason?: string;
}


interface CredentialProfileStatusDto {
  profile?: string;
  kind?: string;
  configured?: boolean;
}

interface CredentialListResponseDto {
  ok?: boolean;
  profiles?: CredentialProfileStatusDto[];
}

interface SetCredentialResponseDto {
  ok?: boolean;
  profile?: CredentialProfileStatusDto;
}

interface CodexDeviceStartDto {
  ok?: boolean;
  login_id?: string;
  verification_url?: string;
  user_code?: string;
  interval?: number;
  expires_at?: string;
  job?: JobDto;
}

interface RuntimeWebSearchSummaryDto {
  enabled?: boolean;
  builtin_provider_enabled?: boolean;
  provider?: string;
  mode?: "single" | "fallback" | "aggregate";
  providers?: string[];
  max_results?: number;
  max_provider_attempts?: number;
}

interface RuntimeWebSearchProviderSummaryDto {
  id?: string;
  kind?: string;
  base_url?: string;
  credential_profile?: string;
  credential_configured?: boolean;
}

interface SearchResponseDto {
  query?: string;
  limit?: number;
  results?: SearchResultItemDto[];
}

interface SearchResultItemDto {
  type?: "message";
  result_type?: "message";
  agent_id?: string;
  source_ref?: string;
  title?: string;
  snippet?: string;
  updated_at?: string;
  metadata?: {
    message_id?: string;
    turn_id?: string;
    task_id?: string;
    work_item_id?: string;
    message_seq?: number;
  };
  locator?: {
    evidence_id?: string;
    message_id?: string;
    turn_id?: string;
    task_id?: string;
    work_item_id?: string;
    event_seq?: number;
  };
  created_at?: string;
  kind?: string;
  preview?: string;
}

interface MemorySourceContentDto {
  kind?: string;
  source_ref?: string;
  title?: string;
  content?: string;
  truncated?: boolean;
  updated_at?: string;
}

interface AgentMessagesBatchGetResponseDto {
  messages?: RuntimeMessageEnvelope[];
  missing_message_ids?: string[];
}

interface AgentTranscriptEntriesBatchGetResponseDto {
  entries?: RuntimeTranscriptEntry[];
  missing_entry_ids?: string[];
}

interface SkillCatalogEntryDto {
  skill_id?: string;
  root_id?: string;
  skill_dir?: string;
  name?: string;
  description?: string;
  path?: string;
  scope?: "user" | "user_global" | "agent" | "workspace";
}

interface SkillCatalogResponseDto {
  catalog?: SkillCatalogEntryDto[];
}

interface SkillDetailResponseDto {
  skill?: SkillCatalogEntryDto;
  content?: string;
}

interface AgentTemplateCatalogEntryDto {
  catalog_id?: string;
  template?: string;
  template_id?: string;
  source?: AgentTemplateCatalogEntry["source"];
  path?: string;
  name?: string;
  schema_version?: string;
  description?: string;
  included_skills?: string[];
  source_id?: string;
  resolved_ref?: string;
  resolved_revision?: string;
  source_url?: string;
}

interface AgentTemplateRemoteSourceDto {
  source_id?: string;
  kind?: string;
  enabled?: boolean;
  status?: string;
  url?: string;
  resolved_ref?: string;
  resolved_revision?: string | null;
  last_synced_at?: string;
}

interface AgentTemplateCatalogDiagnosticDto {
  level?: string;
  message?: string;
  source_id?: string;
}

interface AgentTemplateCatalogResponseDto {
  catalog?: AgentTemplateCatalogEntryDto[];
  sources?: AgentTemplateRemoteSourceDto[];
  diagnostics?: AgentTemplateCatalogDiagnosticDto[];
}

interface AgentTemplateDetailDto {
  catalog_id?: string;
  template?: string;
  template_id?: string;
  source?: AgentTemplateCatalogEntry["source"];
  source_location?: string;
  name?: string;
  summary?: string;
  schema_version?: string;
  agents_md?: string;
  skills?: Array<{ kind?: string; reference?: string }>;
}

interface AgentTemplateDetailResponseDto {
  detail?: AgentTemplateDetailDto;
}

interface AgentSkillsResponseDto {
  skills?: SkillCatalogEntryDto[];
}

interface JobResponseDto {
  job?: JobDto;
}

interface JobDto {
  id?: string;
  kind?: string;
  status?: "queued" | "running" | "completed" | "failed";
  phase?: string;
  summary?: string;
  error?: string | null;
}

interface WorkspaceDirectoryEntryDto {
  name: string;
  type: string;
  size: number;
  mime_type?: string;
}

interface WorkspaceDirectoryListingDto {
  type: string;
  path: string;
  workspace_id: string;
  entries: WorkspaceDirectoryEntryDto[];
}

interface WorkspaceFileContentDto {
  type: string;
  path: string;
  workspace_id: string;
  size: number;
  mime_type: string;
  truncated: boolean;
  total_size?: number;
  content?: string;
}

export interface RuntimeConfigUpdateEntry {
  key: string;
  value?: unknown;
  unset?: boolean;
}

export function createRuntimeClient(options: RuntimeClientOptions = {}) {
  const connectionMode = options.mode ?? (options.baseUrl ? "remote" : "local");
  const defaultBaseUrl = connectionMode === "local" ? DEFAULT_DEV_API_BASE : undefined;
  const baseUrl = normalizeBaseUrl(options.baseUrl ?? import.meta.env.VITE_HOLON_API_BASE ?? defaultBaseUrl);
  const fetchImpl = options.fetchImpl ?? fetch;
  const requestHeaders = authorizationHeaders(options.token);
  const hasToken = Boolean(options.token?.trim());

  return {
    async getBootstrap(): Promise<RuntimeBootstrap> {
      if (!baseUrl) {
        return buildDisconnectedBootstrap(undefined, "Holon API base URL is not configured.", connectionMode, hasToken);
      }

      try {
        return await fetchRuntimeBootstrap(baseUrl, fetchImpl, requestHeaders, connectionMode, hasToken);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return buildDisconnectedBootstrap(baseUrl, message, connectionMode, hasToken, isAuthRequiredError(error));
      }
    },
    async getAgentDetail(agentId: string, displayLevel: DisplayLevel = "info"): Promise<AgentDetail> {
      if (!baseUrl) {
        return disconnectedAgentDetail(agentId, "Holon API base URL is not configured.");
      }

      try {
        return await fetchAgentDetail(baseUrl, fetchImpl, requestHeaders, agentId, displayLevel);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return disconnectedAgentDetail(agentId, message);
      }
    },
    async getAgentState(agentId: string): Promise<AgentSummary> {
      if (!baseUrl) {
        return disconnectedAgentSummary(agentId, "Holon API base URL is not configured.");
      }
      try {
        return await fetchAgentState(baseUrl, fetchImpl, requestHeaders, agentId);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return disconnectedAgentSummary(agentId, message);
      }
    },
    async getAgentWorkItems(agentId: string, options: { limit?: number } = {}): Promise<WorkItemSummary[]> {
      if (!baseUrl) {
        return [];
      }
      const workItems = await fetchAgentWorkItems(baseUrl, fetchImpl, requestHeaders, agentId, options);
      return projectWorkItems(workItems);
    },
    async getAgentWorkItem(agentId: string, workItemId: string): Promise<WorkItemSummary | undefined> {
      if (!baseUrl || !workItemId) {
        return undefined;
      }
      const workItem = await fetchAgentWorkItem(baseUrl, fetchImpl, requestHeaders, agentId, workItemId);
      return projectWorkItem(workItem);
    },
    async getAgentEvents(agentId: string, options: AgentEventPageOptions = {}): Promise<EventPageResponseDto> {
      if (!baseUrl) {
        return { events: [], has_older: false };
      }
      return fetchAgentEvents(baseUrl, fetchImpl, requestHeaders, agentId, options);
    },
    async getAgentMessagesBatch(agentId: string, messageIds: string[]): Promise<AgentMessagesBatchGetResponseDto> {
      if (!baseUrl || !messageIds.length) {
        return { messages: [], missing_message_ids: [] };
      }
      return postJson<AgentMessagesBatchGetResponseDto>(
        fetchImpl,
        baseUrl,
        `/agents/${encodeURIComponent(agentId)}/messages:batchGet`,
        { message_ids: messageIds },
        requestHeaders,
      );
    },
    async getAgentTranscriptEntriesBatch(agentId: string, entryIds: string[]): Promise<AgentTranscriptEntriesBatchGetResponseDto> {
      if (!baseUrl || !entryIds.length) {
        return { entries: [], missing_entry_ids: [] };
      }
      return postJson<AgentTranscriptEntriesBatchGetResponseDto>(
        fetchImpl,
        baseUrl,
        `/agents/${encodeURIComponent(agentId)}/transcript:batchGet`,
        { entry_ids: entryIds },
        requestHeaders,
      );
    },
    async getAgentBriefsById(agentId: string, briefIds: string[]): Promise<Record<string, RuntimeBriefRecord>> {
      if (!baseUrl || !briefIds.length) {
        return {};
      }
      return fetchBriefRecordsById(baseUrl, fetchImpl, requestHeaders, agentId, briefIds);
    },
    async getToolExecution(agentId: string, toolExecutionId: string): Promise<RuntimeToolExecutionRecord> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      return getJson<RuntimeToolExecutionRecord>(
        fetchImpl,
        baseUrl,
        `/agents/${encodeURIComponent(agentId)}/tool-executions/${encodeURIComponent(toolExecutionId)}`,
        { headers: requestHeaders },
      );
    },
    async getTaskOutput(agentId: string, taskId: string): Promise<RuntimeTaskOutputResult> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      return getJson<RuntimeTaskOutputResult>(
        fetchImpl,
        baseUrl,
        `/agents/${encodeURIComponent(agentId)}/tasks/${encodeURIComponent(taskId)}/output?block=false`,
        { timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS, headers: requestHeaders },
      );
    },
    async getModels(): Promise<RuntimeModelCatalog> {
      if (!baseUrl) {
        return { source: "fixture", options: [] };
      }
      const response = await getJson<RuntimeModelsDto>(fetchImpl, baseUrl, "/models", { headers: requestHeaders });
      return {
        source: "http",
        options: projectModelOptions(response),
      };
    },
    async getRuntimeConfig(): Promise<RuntimeConfigState> {
      if (!baseUrl) {
        return { source: "fixture" };
      }
      const response = await getJson<RuntimeConfigResponseDto>(fetchImpl, baseUrl, "/control/runtime/config", { headers: requestHeaders });
      return projectRuntimeConfigState(response);
    },
    async getSkillCatalog(agentId?: string): Promise<SkillCatalogState> {
      if (!baseUrl) {
        return { source: "fixture", agentId, catalog: [] };
      }
      if (agentId) {
        const response = await getJson<AgentSkillsResponseDto>(
          fetchImpl,
          baseUrl,
          `/agents/${encodeURIComponent(agentId)}/skills`,
          { headers: requestHeaders },
        );
        return projectSkillCatalog({ catalog: response.skills }, agentId);
      }
      const response = await getJson<SkillCatalogResponseDto>(fetchImpl, baseUrl, "/skills/catalog", { headers: requestHeaders });
      return projectSkillCatalog(response, agentId);
    },
    async getSkillDetail(skillId: string): Promise<SkillDetailState> {
      if (!baseUrl) {
        return { source: "fixture", error: "Holon API base URL is not configured." };
      }
      const response = await getJson<SkillDetailResponseDto>(
        fetchImpl,
        baseUrl,
        `/skills/catalog/${encodeURIComponent(skillId)}`,
        { headers: requestHeaders },
      );
      return {
        source: "http",
        skill: response.skill ? projectSkillCatalogEntry(response.skill) : undefined,
        content: response.content ?? "",
      };
    },
    async getTemplateCatalog(): Promise<AgentTemplateCatalogState> {
      if (!baseUrl) {
        return { source: "fixture", catalog: [], sources: [], diagnostics: [] };
      }
      const response = await getJson<AgentTemplateCatalogResponseDto>(fetchImpl, baseUrl, "/templates/catalog", { headers: requestHeaders });
      return projectTemplateCatalog(response);
    },
    async getTemplateDetail(catalogId: string): Promise<AgentTemplateDetailState> {
      if (!baseUrl) {
        return { source: "fixture", error: "Holon API base URL is not configured." };
      }
      const response = await getJson<AgentTemplateDetailResponseDto>(
        fetchImpl,
        baseUrl,
        `/templates/catalog/${encodeURIComponent(catalogId)}`,
        { headers: requestHeaders },
      );
      return {
        source: "http",
        detail: response.detail ? projectTemplateDetail(response.detail) : undefined,
      };
    },
    async installTemplate(githubUrl: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(fetchImpl, baseUrl, "/control/templates/install", { github_url: githubUrl }, requestHeaders);
    },
    async removeTemplate(templateId: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(fetchImpl, baseUrl, "/control/templates/remove", { template_id: templateId }, requestHeaders);
    },
    async syncTemplateRemoteSources(): Promise<string> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await postJson<JobResponseDto>(fetchImpl, baseUrl, "/templates/remote-sources/sync", {}, requestHeaders);
      const jobId = response.job?.id;
      if (!jobId) {
        throw new Error("Template sync response did not include a job id.");
      }
      return jobId;
    },
    async createAgentFromTemplate(agentId: string, template: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(
        fetchImpl,
        baseUrl,
        `/control/agents/${encodeURIComponent(agentId)}/create`,
        { template },
        requestHeaders,
      );
    },
    async addSkillToCatalog(input: AddSkillInput): Promise<string> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await postJson<JobResponseDto>(
        fetchImpl,
        baseUrl,
        "/jobs",
        { kind: "skill.install", params: { kind: input } },
        requestHeaders,
      );
      const jobId = response.job?.id;
      if (!jobId) {
        throw new Error("Skill install job response did not include a job id.");
      }
      return jobId;
    },
    async getJob(jobId: string): Promise<JobDto> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await getJson<JobResponseDto>(
        fetchImpl,
        baseUrl,
        `/jobs/${encodeURIComponent(jobId)}`,
        { headers: requestHeaders },
      );
      if (!response.job) {
        throw new Error(`Job ${jobId} not found.`);
      }
      return response.job;
    },
    async removeSkillFromCatalog(name: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(fetchImpl, baseUrl, "/skills/catalog/remove", { name }, requestHeaders);
    },
    async updateSkillCatalog(name?: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(fetchImpl, baseUrl, "/skills/catalog/update", { name }, requestHeaders);
    },
    async checkSkillCatalog(name?: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(fetchImpl, baseUrl, "/skills/catalog/check", { name }, requestHeaders);
    },
    async enableAgentSkill(agentId: string, name: string, mode: SkillInstallMode = "linked"): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(
        fetchImpl,
        baseUrl,
        `/control/agents/${encodeURIComponent(agentId)}/skills/enable`,
        { name, mode },
        requestHeaders,
      );
    },
    async disableAgentSkill(agentId: string, name: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(
        fetchImpl,
        baseUrl,
        `/control/agents/${encodeURIComponent(agentId)}/skills/disable`,
        { name },
        requestHeaders,
      );
    },
    async updateRuntimeConfig(updates: RuntimeConfigUpdateEntry[]): Promise<RuntimeConfigState> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await patchJson<RuntimeConfigResponseDto>(fetchImpl, baseUrl, "/control/runtime/config", { updates }, requestHeaders);
      return projectRuntimeConfigState(response);
    },
    async listCredentials(): Promise<CredentialStoreState> {
      if (!baseUrl) {
        return { profiles: [] };
      }
      try {
        const response = await getJson<CredentialListResponseDto>(fetchImpl, baseUrl, "/control/runtime/credentials", { headers: requestHeaders });
        return {
          profiles: (response.profiles ?? []).map((p) => ({
            profile: p.profile ?? "unknown",
            kind: p.kind ?? "unknown",
            configured: p.configured ?? false,
          })),
        };
      } catch (error) {
        return { profiles: [], error: error instanceof Error ? error.message : String(error) };
      }
    },
    async setCredential(profile: string, kind: string, material: string): Promise<CredentialProfileStatus> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await putJson<SetCredentialResponseDto>(fetchImpl, baseUrl, `/control/runtime/credentials/${encodeURIComponent(profile)}`, { kind, material }, requestHeaders);
      return {
        profile: response.profile?.profile ?? profile,
        kind: response.profile?.kind ?? kind,
        configured: response.profile?.configured ?? true,
      };
    },
    async deleteCredential(profile: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await deleteJson<unknown>(fetchImpl, baseUrl, `/control/runtime/credentials/${encodeURIComponent(profile)}`, requestHeaders);
    },
    async startCodexDeviceLogin(): Promise<CodexDeviceLoginResponse> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await postJson<CodexDeviceStartDto>(
        fetchImpl,
        baseUrl,
        "/auth/codex/device/start",
        {},
        requestHeaders,
      );
      return {
        ok: response.ok ?? false,
        loginId: response.login_id ?? "",
        verificationUrl: response.verification_url ?? "",
        userCode: response.user_code ?? "",
        interval: response.interval ?? 5,
        expiresAt: response.expires_at ?? "",
        jobId: response.job?.id ?? "",
      };
    },
    async search(query: string, options: RuntimeSearchOptions = {}): Promise<SearchResponse> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await postJson<SearchResponseDto>(fetchImpl, baseUrl, "/search", {
        query,
        agent_ids: options.agentIds,
        include_all_workspaces: options.includeAllWorkspaces,
        limit: options.limit,
        types: ["message"],
      }, requestHeaders);
      return projectSearchResponse(response);
    },
    async getMemorySource(sourceRef: string, maxChars?: number): Promise<MemorySourceContent> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await postJson<MemorySourceContentDto>(fetchImpl, baseUrl, "/memory/get", {
        source_ref: sourceRef,
        max_chars: maxChars,
      }, requestHeaders);
      return projectMemorySourceContent(response);
    },
    streamAgentEvents(agentId: string, options: AgentEventStreamOptions): AgentEventStreamSubscription | undefined {
      if (!baseUrl) return undefined;
      return streamAgentEvents(baseUrl, fetchImpl, requestHeaders, agentId, options);
    },
    streamGlobalEvents(options: AgentEventStreamOptions): AgentEventStreamSubscription | undefined {
      if (!baseUrl) return undefined;
      return streamGlobalEvents(baseUrl, fetchImpl, requestHeaders, options);
    },
    async sendOperatorPrompt(agentId: string, text: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(fetchImpl, baseUrl, `/control/agents/${encodeURIComponent(agentId)}/prompt`, { text }, requestHeaders);
    },
    async setAgentModel(agentId: string, model: string, reasoningEffort?: string): Promise<AgentModelStateDto | undefined> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await postJson<AgentModelResponseDto>(
        fetchImpl,
        baseUrl,
        `/control/agents/${encodeURIComponent(agentId)}/model`,
        { model, reasoning_effort: reasoningEffort, authority_class: "operator_instruction" },
        requestHeaders,
      );
      return response.model;
    },
    async clearAgentModel(agentId: string): Promise<AgentModelStateDto | undefined> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const response = await postJson<AgentModelResponseDto>(
        fetchImpl,
        baseUrl,
        `/control/agents/${encodeURIComponent(agentId)}/model/clear`,
        { authority_class: "operator_instruction" },
        requestHeaders,
      );
      return response.model;
    },
    async browseWorkspaceDir(workspaceId: string, path?: string, executionRootId?: string): Promise<WorkspaceDirectoryListing> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const encodedPath = path ? path.split("/").map(encodeURIComponent).join("/") : "";
      const urlPath = encodedPath
        ? `/workspaces/${encodeURIComponent(workspaceId)}/files/${encodedPath}`
        : `/workspaces/${encodeURIComponent(workspaceId)}/files`;
      const query = executionRootId ? `?execution_root_id=${encodeURIComponent(executionRootId)}` : "";
      const response = await getJson<WorkspaceDirectoryListingDto>(fetchImpl, baseUrl, `${urlPath}${query}`, { headers: requestHeaders });
      return {
        type: "directory",
        path: response.path,
        workspaceId: response.workspace_id,
        entries: (response.entries ?? []).map((e) => ({
          name: e.name,
          type: e.type as WorkspaceFileEntry["type"],
          size: e.size,
          mimeType: e.mime_type,
        })),
      };
    },
    async readWorkspaceFile(workspaceId: string, path: string, executionRootId?: string): Promise<WorkspaceFileContent> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      const encodedPath = path.split("/").map(encodeURIComponent).join("/");
      const query = executionRootId ? `?execution_root_id=${encodeURIComponent(executionRootId)}` : "";
      const response = await getJson<WorkspaceFileContentDto>(
        fetchImpl,
        baseUrl,
        `/workspaces/${encodeURIComponent(workspaceId)}/files/${encodedPath}${query}`,
        { headers: requestHeaders },
      );
      return {
        type: "file",
        path: response.path,
        workspaceId: response.workspace_id,
        size: response.size,
        mimeType: response.mime_type,
        truncated: response.truncated,
        totalSize: response.total_size,
        content: response.content,
      };
    },
    workspaceFileUrl(workspaceId: string, path: string, download?: boolean, executionRootId?: string): string {
      const encodedPath = path.split("/").map(encodeURIComponent).join("/");
      const params = new URLSearchParams();
      if (download) params.set("download", "true");
      if (executionRootId) params.set("execution_root_id", executionRootId);
      const query = params.toString();
      return `${baseUrl}/workspaces/${encodeURIComponent(workspaceId)}/files/${encodedPath}${query ? `?${query}` : ""}`;
    },
  };
}

async function fetchAgentEvents(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
  options: AgentEventPageOptions,
): Promise<EventPageResponseDto> {
  const query = new URLSearchParams();
  if (options.beforeSeq != null) query.set("before_seq", String(options.beforeSeq));
  if (options.afterSeq != null) query.set("after_seq", String(options.afterSeq));
  if (options.limit != null) query.set("limit", String(options.limit));
  if (options.order) query.set("order", options.order);
  if (options.displayLevel) query.set("max_level", options.displayLevel);
  const queryString = query.toString();
  const path = `/agents/${encodeURIComponent(agentId)}/events${queryString ? `?${queryString}` : ""}`;
  return getJson<EventPageResponseDto>(fetchImpl, baseUrl, path, { headers });
}

async function fetchAgentWorkItems(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
  options: { limit?: number } = {},
): Promise<WorkItemDto[]> {
  const query = new URLSearchParams();
  if (options.limit != null) query.set("limit", String(options.limit));
  const queryString = query.toString();
  return getJson<WorkItemDto[]>(
    fetchImpl,
    baseUrl,
    `/agents/${encodeURIComponent(agentId)}/work-items${queryString ? `?${queryString}` : ""}`,
    { timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS, headers },
  );
}

async function fetchAgentWorkItem(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
  workItemId: string,
): Promise<WorkItemDto> {
  return getJson<WorkItemDto>(
    fetchImpl,
    baseUrl,
    `/agents/${encodeURIComponent(agentId)}/work-items/${encodeURIComponent(workItemId)}`,
    { timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS, headers },
  );
}

async function fetchTranscriptEntriesForEvents(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
  events: EventEnvelopeDto[],
): Promise<Record<string, RuntimeTranscriptEntry>> {
  const entryIds = Array.from(
    new Set(
      events
        .map((event) => transcriptEntryIdForPayload(asRecord(event.payload)))
        .filter((entryId): entryId is string => Boolean(entryId)),
    ),
  );
  if (!entryIds.length) return {};
  const response = await postJson<AgentTranscriptEntriesBatchGetResponseDto>(
    fetchImpl,
    baseUrl,
    `/agents/${encodeURIComponent(agentId)}/transcript:batchGet`,
    { entry_ids: entryIds },
    headers,
  ).catch((): AgentTranscriptEntriesBatchGetResponseDto => ({ entries: [], missing_entry_ids: entryIds }));
  return Object.fromEntries((response.entries ?? []).flatMap((entry) => (entry.id ? [[entry.id, entry]] : [])));
}

async function fetchBriefRecordsForEvents(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
  events: EventEnvelopeDto[],
  transcriptEntriesById: Record<string, RuntimeTranscriptEntry>,
): Promise<Record<string, RuntimeBriefRecord>> {
  const briefIds = Array.from(
    new Set(
      events
        .filter((event) => event.type === "brief_created")
        .filter((event) => {
          const payload = asRecord(event.payload);
          const entryId = transcriptEntryIdForPayload(payload);
          return !((entryId ? transcriptEntryText(transcriptEntriesById[entryId]) : undefined) ?? stringValue(payload?.text));
        })
        .map((event) => briefIdForPayload(asRecord(event.payload)))
        .filter((briefId): briefId is string => Boolean(briefId)),
    ),
  );
  if (!briefIds.length) return {};
  return fetchBriefRecordsById(baseUrl, fetchImpl, headers, agentId, briefIds);
}

async function fetchBriefRecordsById(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
  briefIds: string[],
): Promise<Record<string, RuntimeBriefRecord>> {
  const records = await Promise.all(
    briefIds.map(async (briefId): Promise<RuntimeBriefRecord | undefined> => {
      const path = `/agents/${encodeURIComponent(agentId)}/briefs/${encodeURIComponent(briefId)}`;
      return getJson<RuntimeBriefRecord>(fetchImpl, baseUrl, path, { timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS, headers }).catch(() => undefined);
    }),
  );
  return Object.fromEntries(records.flatMap((record) => (record?.id ? [[record.id, record]] : [])));
}

function streamAgentEvents(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  agentId: string,
  options: AgentEventStreamOptions,
): AgentEventStreamSubscription {
  const controller = new AbortController();
  const encodedAgentId = encodeURIComponent(agentId);
  const query = new URLSearchParams();
  if (options.limit != null) query.set("limit", String(options.limit));
  if (options.afterSeq != null) query.set("after_seq", String(options.afterSeq));
  const queryString = query.toString();
  const path = `/agents/${encodedAgentId}/events/stream${queryString ? `?${queryString}` : ""}`;

  void readEventStream(fetchImpl, `${baseUrl}${path}`, headers, controller.signal, options);

  return {
    close: () => controller.abort(),
  };
}
function streamGlobalEvents(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  options: AgentEventStreamOptions,
): AgentEventStreamSubscription {
  const controller = new AbortController();
  const path = "/events/stream";
  void readEventStream(fetchImpl, `${baseUrl}${path}`, headers, controller.signal, options);
  return {
    close: () => controller.abort(),
  };
}

async function readEventStream(
  fetchImpl: typeof fetch,
  url: string,
  headers: Record<string, string>,
  signal: AbortSignal,
  options: AgentEventStreamOptions,
): Promise<void> {
  try {
    const response = await fetchImpl(url, {
      headers: { Accept: "text/event-stream", ...headers },
      signal,
    });
    if (!response.ok) {
      throw new Error(`GET ${new URL(url).pathname} failed with ${response.status}`);
    }
    if (!response.body) {
      throw new Error("event stream response body is not readable");
    }

    options.onOpen?.();
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    while (!signal.aborted) {
      const { done, value } = await reader.read();
      if (done) break;
      options.onActivity?.();
      buffer += decoder.decode(value, { stream: true });
      const frames = takeSseFrames(buffer);
      buffer = frames.remaining;
      for (const frame of frames.frames) {
        options.onActivity?.();
        const event = parseSseEventFrame(frame);
        if (event) options.onEvent(event);
      }
    }
    if (!signal.aborted) {
      options.onClose?.();
    }
  } catch (error) {
    if (!signal.aborted) {
      options.onError?.(error instanceof Error ? error : new Error(String(error)));
    }
  }
}

function takeSseFrames(buffer: string): { frames: string[]; remaining: string } {
  const frames: string[] = [];
  let remaining = buffer;
  while (true) {
    const lfIndex = remaining.indexOf("\n\n");
    const crlfIndex = remaining.indexOf("\r\n\r\n");
    const candidates = [lfIndex, crlfIndex].filter((index) => index >= 0);
    if (candidates.length === 0) break;
    const index = Math.min(...candidates);
    const delimiterLength = remaining.startsWith("\r\n\r\n", index) ? 4 : 2;
    frames.push(remaining.slice(0, index));
    remaining = remaining.slice(index + delimiterLength);
  }
  return { frames, remaining };
}

function parseSseEventFrame(frame: string): StreamEventEnvelopeDto | undefined {
  const dataLines: string[] = [];
  for (const line of frame.split(/\r?\n/)) {
    if (line.startsWith(":")) continue;
    if (line.startsWith("data:")) {
      dataLines.push(line.slice("data:".length).trimStart());
    }
  }
  if (dataLines.length === 0) return undefined;
  return JSON.parse(dataLines.join("\n")) as StreamEventEnvelopeDto;
}

async function fetchRuntimeBootstrap(
  baseUrl: string,
  fetchImpl: typeof fetch,
  headers: Record<string, string>,
  connectionMode: "local" | "remote",
  hasToken: boolean,
): Promise<RuntimeBootstrap> {
  await getJson<{ auth?: { mode?: string } }>(fetchImpl, baseUrl, "/handshake", { headers });
  const agentEntries = await getJson<AgentListEntryDto[]>(fetchImpl, baseUrl, "/agents/list", { headers });

  const agents = agentEntries.map((entry) => projectAgent(entry));
  const attentionCount = agents.filter((agent) => agent.pending > 0 || agent.waitingCount > 0).length;
  const activeTaskCount = agents.reduce((sum, agent) => sum + agent.activeTaskCount, 0);
  const currentWorkCount = agents.filter((agent) => agent.currentWork).length;
  const connection: RuntimeConnection = {
    mode: connectionMode,
    source: "http",
    baseUrl,
    hasToken,
    summary: `${connectionBaseLabel(baseUrl)} · ${connectionMode}${hasToken ? " · Token" : ""}`,
  };

  return {
    attentionCount,
    connection,
    metrics: buildMetrics(agents.length, attentionCount, activeTaskCount, currentWorkCount),
    agents,
  };
}

function connectionBaseLabel(baseUrl: string): string {
  try {
    return new URL(baseUrl).host;
  } catch {
    return baseUrl;
  }
}

function projectSkillCatalog(response: SkillCatalogResponseDto, agentId?: string): SkillCatalogState {
  return {
    source: "http",
    agentId,
    catalog: (response.catalog ?? [])
      .filter((entry) => Boolean(entry.name || entry.skill_id))
      .map(projectSkillCatalogEntry),
  };
}

function projectSkillCatalogEntry(entry: SkillCatalogEntryDto) {
  return {
    skillId: entry.skill_id ?? entry.name ?? "unknown",
    rootId: entry.root_id ?? "",
    skillDir: entry.skill_dir ?? entry.name ?? "",
    name: entry.name ?? entry.skill_id ?? "unknown",
    description: entry.description ?? "",
    path: entry.path ?? "",
    scope: entry.scope ?? "user",
  };
}

function projectTemplateCatalog(response: AgentTemplateCatalogResponseDto): AgentTemplateCatalogState {
  return {
    source: "http",
    catalog: (response.catalog ?? [])
      .filter((entry) => Boolean(entry.catalog_id || entry.template))
      .map(projectTemplateCatalogEntry),
    sources: (response.sources ?? []).map(projectTemplateRemoteSource),
    diagnostics: (response.diagnostics ?? []).map(projectTemplateDiagnostic),
  };
}

function projectTemplateCatalogEntry(entry: AgentTemplateCatalogEntryDto): AgentTemplateCatalogEntry {
  const catalogId = entry.catalog_id ?? entry.template ?? entry.template_id ?? "unknown";
  return {
    catalogId,
    template: entry.template ?? catalogId,
    templateId: entry.template_id ?? entry.template ?? catalogId,
    source: entry.source ?? "user_global",
    path: entry.path,
    name: entry.name ?? entry.template_id ?? catalogId,
    schemaVersion: entry.schema_version,
    description: entry.description ?? "",
    includedSkills: entry.included_skills ?? [],
    sourceId: entry.source_id,
    resolvedRef: entry.resolved_ref,
    resolvedRevision: entry.resolved_revision,
    sourceUrl: entry.source_url,
  };
}

function projectTemplateRemoteSource(source: AgentTemplateRemoteSourceDto): AgentTemplateRemoteSource {
  return {
    sourceId: source.source_id ?? "unknown",
    kind: source.kind ?? "unknown",
    enabled: source.enabled ?? false,
    status: source.status,
    url: source.url,
    resolvedRef: source.resolved_ref,
    resolvedRevision: source.resolved_revision ?? undefined,
    lastSyncedAt: source.last_synced_at,
  };
}

function projectTemplateDiagnostic(diagnostic: AgentTemplateCatalogDiagnosticDto): AgentTemplateCatalogDiagnostic {
  return {
    level: diagnostic.level,
    message: diagnostic.message ?? "Template catalog diagnostic",
    sourceId: diagnostic.source_id,
  };
}

function projectTemplateDetail(detail: AgentTemplateDetailDto): AgentTemplateDetail {
  const catalogId = detail.catalog_id ?? detail.template ?? detail.template_id ?? "unknown";
  return {
    catalogId,
    template: detail.template ?? catalogId,
    templateId: detail.template_id ?? detail.template ?? catalogId,
    source: detail.source ?? "user_global",
    sourceLocation: detail.source_location,
    name: detail.name ?? detail.template_id ?? catalogId,
    summary: detail.summary ?? "",
    schemaVersion: detail.schema_version,
    agentsMd: detail.agents_md ?? "",
    skills: (detail.skills ?? []).map((skill) => ({
      kind: skill.kind ?? "unknown",
      reference: skill.reference ?? "",
    })),
  };
}

async function getJson<T>(
  fetchImpl: typeof fetch,
  baseUrl: string,
  path: string,
  options: { timeoutMs?: number; headers?: Record<string, string> } = {},
): Promise<T> {
  const controller = new AbortController();
  const timeout = globalThis.setTimeout(() => controller.abort(), options.timeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS);
  const response = await fetchImpl(`${baseUrl}${path}`, {
    headers: { Accept: "application/json", ...options.headers },
    signal: controller.signal,
  }).finally(() => globalThis.clearTimeout(timeout));
  if (!response.ok) {
    throw await httpRequestError("GET", path, response);
  }
  return (await response.json()) as T;
}

async function postJson<T>(
  fetchImpl: typeof fetch,
  baseUrl: string,
  path: string,
  body: unknown,
  headers: Record<string, string> = {},
  options: { timeoutMs?: number } = {},
): Promise<T> {
  const controller = new AbortController();
  const timeout = globalThis.setTimeout(() => controller.abort(), options.timeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS);
  const response = await fetchImpl(`${baseUrl}${path}`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      ...headers,
    },
    body: JSON.stringify(body),
    signal: controller.signal,
  }).finally(() => globalThis.clearTimeout(timeout));
  if (!response.ok) {
    throw await httpRequestError("POST", path, response);
  }

  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

async function patchJson<T>(
  fetchImpl: typeof fetch,
  baseUrl: string,
  path: string,
  body: unknown,
  headers: Record<string, string> = {},
): Promise<T> {
  const controller = new AbortController();
  const timeout = globalThis.setTimeout(() => controller.abort(), DEFAULT_REQUEST_TIMEOUT_MS);
  const response = await fetchImpl(`${baseUrl}${path}`, {
    method: "PATCH",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      ...headers,
    },
    body: JSON.stringify(body),
    signal: controller.signal,
  }).finally(() => globalThis.clearTimeout(timeout));
  if (!response.ok) {
    throw await httpRequestError("PATCH", path, response);
  }

  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}


async function putJson<T>(
  fetchImpl: typeof fetch,
  baseUrl: string,
  path: string,
  body: unknown,
  headers: Record<string, string> = {},
): Promise<T> {
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), DEFAULT_REQUEST_TIMEOUT_MS);
  const response = await fetchImpl(`${baseUrl}${path}`, {
    method: "PUT",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      ...headers,
    },
    body: JSON.stringify(body),
    signal: controller.signal,
  }).finally(() => window.clearTimeout(timeout));
  if (!response.ok) {
    throw await httpRequestError("PUT", path, response);
  }

  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

async function deleteJson<T>(
  fetchImpl: typeof fetch,
  baseUrl: string,
  path: string,
  headers: Record<string, string> = {},
): Promise<T> {
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), DEFAULT_REQUEST_TIMEOUT_MS);
  const response = await fetchImpl(`${baseUrl}${path}`, {
    method: "DELETE",
    headers: {
      Accept: "application/json",
      ...headers,
    },
    signal: controller.signal,
  }).finally(() => window.clearTimeout(timeout));
  if (!response.ok) {
    throw await httpRequestError("DELETE", path, response);
  }

  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

function projectRuntimeConfigState(response: RuntimeConfigResponseDto): RuntimeConfigState {
  return {
    source: "http",
    configFilePath: response.config_file_path,
    changed: response.changed,
    surface: response.runtime_surface ? projectRuntimeConfigSurface(response.runtime_surface) : undefined,
    results: response.results?.map((result) => ({
      key: result.key ?? "unknown",
      effect: result.effect ?? "rejected",
      reason: result.reason ?? "",
    })),
  };
}

function projectRuntimeConfigSurface(surface: RuntimeConfigSurfaceDto): RuntimeConfigSurface {
  return {
    modelDefault: surface.model_default ?? "",
    modelFallbacks: surface.model_fallbacks ?? [],
    visionDefault: surface.vision_default ?? undefined,
    modelCatalog: surface.model_catalog ?? [],
    unknownModelFallbackConfigured: surface.unknown_model_fallback_configured ?? false,
    runtimeMaxOutputTokens: surface.runtime_max_output_tokens ?? 0,
    defaultToolOutputTokens: surface.default_tool_output_tokens ?? 0,
    maxToolOutputTokens: surface.max_tool_output_tokens ?? 0,
    disableProviderFallback: surface.disable_provider_fallback ?? false,
    providers: (surface.providers ?? []).map((provider) => ({
      id: provider.id ?? "unknown",
      transport: provider.transport ?? "unknown",
      baseUrl: provider.base_url ?? "",
      credentialSource: provider.credential_source ?? "unknown",
      credentialKind: provider.credential_kind ?? "unknown",
      credentialEnv: provider.credential_env,
      credentialProfile: provider.credential_profile,
      credentialExternal: provider.credential_external,
      credentialConfigured: provider.credential_configured ?? false,
      configuredInConfig: provider.configured_in_config ?? false,
    })),
    webSearch: surface.web_search
      ? {
          enabled: surface.web_search.enabled ?? true,
          builtinProviderEnabled: surface.web_search.builtin_provider_enabled ?? true,
          provider: surface.web_search.provider ?? "auto",
          mode: surface.web_search.mode ?? "fallback",
          providers: surface.web_search.providers ?? [],
          maxResults: surface.web_search.max_results ?? 5,
          maxProviderAttempts: surface.web_search.max_provider_attempts ?? 3,
        }
      : undefined,
    webSearchProviders: (surface.web_search_providers ?? []).map((provider) => ({
      id: provider.id ?? "unknown",
      kind: provider.kind ?? "unknown",
      baseUrl: provider.base_url,
      credentialProfile: provider.credential_profile,
      credentialConfigured: provider.credential_configured ?? false,
    })),
  };
}

function projectSearchResponse(response: SearchResponseDto): SearchResponse {
  return {
    query: response.query ?? "",
    limit: response.limit ?? 0,
    results: (response.results ?? []).map((result) => ({
      resultType: result.result_type ?? result.type ?? "message",
      agentId: result.agent_id ?? "unknown-agent",
      locator: {
        evidenceId: result.locator?.evidence_id ?? result.source_ref,
        sourceRef: result.source_ref ?? result.locator?.evidence_id,
        messageId: result.locator?.message_id ?? result.metadata?.message_id,
        turnId: result.locator?.turn_id ?? result.metadata?.turn_id,
        taskId: result.locator?.task_id ?? result.metadata?.task_id,
        workItemId: result.locator?.work_item_id ?? result.metadata?.work_item_id,
        eventSeq: result.locator?.event_seq ?? result.metadata?.message_seq,
      },
      createdAt: result.created_at ?? result.updated_at,
      kind: result.kind ?? "message",
      preview: result.preview ?? result.snippet ?? result.title ?? result.source_ref ?? "",
    })),
  };
}

function projectMemorySourceContent(response: MemorySourceContentDto): MemorySourceContent {
  return {
    kind: response.kind ?? "unknown",
    sourceRef: response.source_ref ?? "",
    title: response.title ?? response.source_ref ?? "Memory source",
    content: response.content ?? "",
    truncated: response.truncated ?? false,
    updatedAt: response.updated_at,
  };
}

function projectAgent(entry: AgentListEntryDto, state?: AgentStateDto, brief?: BriefRecordDto, workItemRecords?: WorkItemDto[]): AgentSummary {
  const id = entry.identity?.agent_id ?? state?.agent?.agent?.id ?? "unknown-agent";
  const status = state?.agent?.agent?.status ?? entry.status ?? "unknown";
  const profile = compactJoin([entry.identity?.visibility ?? "public", entry.identity?.ownership, entry.identity?.profile_preset]);
  const wsList = state?.workspace?.workspaces ?? [];
  const activeWs = wsList.find((w) => w.is_active);
  // Fallback to list entry's active_workspace_entry when state hasn't loaded yet.
  const listEntry = activeWs ? undefined : entry.active_workspace_entry;
  const workspace = activeWs
    ? activeWs.workspace_alias ?? activeWs.workspace_id ?? activeWs.worktree?.branch ?? "not bound"
    : listEntry?.workspace_alias ?? listEntry?.workspace_id ?? "not bound";
  const workspaceSummary = activeWs
    ? projectWorkspaceFromInfo(activeWs)
    : listEntry
      ? projectWorkspaceFromListEntry(listEntry)
      : undefined;
  const attachedWorkspaces = wsList.map((w) => ({
    workspaceId: w.workspace_id,
    name: w.workspace_alias ?? basename(w.workspace_anchor ?? w.workspace_id) ?? w.workspace_id,
    anchor: w.workspace_anchor ?? w.workspace_id,
    executionRootId: w.execution_root_id ?? undefined,
    repoName: w.repo_name ?? undefined,
  }));
  const currentWork = selectCurrentWork(workItemRecords ?? state?.work_items ?? [], state?.agent?.agent?.current_work_item_id);
  const workItems = selectWorkItems(workItemRecords ?? state?.work_items ?? [], state?.agent?.agent?.current_work_item_id);
  const tasks = projectTasks(state?.tasks ?? []);
  const pending = state?.session?.pending_count ?? entry.pending ?? 0;
  const activeTaskCount = state?.tasks?.length ?? state?.agent?.active_task_count ?? 0;
  const waitingCount = state?.waiting_intents?.length ?? state?.agent?.active_waiting_intents?.length ?? (entry.waiting_reason ? 1 : 0);
  const posture = state?.agent?.scheduling_posture?.posture ?? entry.scheduling_posture?.posture ?? "unknown";
  const postureReason = state?.agent?.scheduling_posture?.reason ?? entry.scheduling_posture?.reason ?? "posture unavailable";
  const focusSummary = currentWork?.objective ?? postureReason;
  const model = state?.agent?.model?.active_model ?? state?.agent?.model?.effective_model ?? entry.model?.active_model ?? entry.model?.effective_model ?? "runtime default";
  const modelSource = state?.agent?.model?.source ?? entry.model?.source;
  const modelReasoningEffort = state?.agent?.model?.override_reasoning_effort ?? entry.model?.override_reasoning_effort ?? undefined;
  const lifecycle = stringifyLifecycle(state?.agent?.lifecycle ?? entry.lifecycle ?? status);
  const currentRunId = state?.session?.current_run_id ?? entry.current_run_id ?? null;

  return {
    id,
    badge: badgeFor(id),
    badgeHue: hueFor(id),
    profile,
    lifecycle: normalizeKebab(status),
    focusSummary,
    workspace,
    attention: attentionLabel(pending, waitingCount),
    model,
    modelSource,
    modelReasoningEffort: modelReasoningEffort ?? undefined,
    footer: `${lifecycle} · ${posture}`,
    subtitle: `${status} · ${workspace}`,
    lastBrief: brief?.text ?? "",
    lastTurnTime: formatTime(brief?.created_at ?? state?.agent?.agent?.last_brief_at),
    pending,
    activeTaskCount,
    waitingCount,
    posture,
    postureReason,
    currentRunId,
    currentWork,
    workspaceSummary,
    attachedWorkspaces,
    tasks,
    workItems,
  };
}

function projectWorkspaceFromInfo(ws: NonNullable<AgentWorkspaceDto["workspaces"]>[number]): WorkspaceSummary {
  const anchor = ws.workspace_anchor ?? ws.execution_root ?? ws.cwd ?? "—";
  const name = ws.workspace_alias ?? basename(anchor) ?? ws.workspace_id ?? "not bound";
  const wt = ws.worktree;
  return {
    id: ws.workspace_id,
    name,
    anchor,
    executionRootId: ws.execution_root_id ?? undefined,
    projectionKind: ws.projection_kind ?? undefined,
    accessMode: ws.access_mode ?? undefined,
    executionRoot: ws.execution_root ?? undefined,
    cwd: ws.cwd ?? undefined,
    worktree: wt
      ? {
          branch: wt.branch ?? undefined,
          path: wt.path ?? undefined,
          originalBranch: wt.original_branch ?? undefined,
          originalCwd: wt.original_cwd ?? undefined,
        }
      : undefined,
  };
}

function projectWorkspaceFromListEntry(entry: NonNullable<AgentListEntryDto["active_workspace_entry"]>): WorkspaceSummary {
  const metadata = entry.projection_metadata;
  const anchor = entry.workspace_anchor ?? entry.execution_root ?? entry.cwd ?? "—";
  const name = entry.workspace_alias ?? basename(anchor) ?? entry.workspace_id ?? "not bound";
  return {
    id: entry.workspace_id ?? "not bound",
    name,
    anchor,
    executionRootId: entry.execution_root_id,
    projectionKind: entry.projection_kind,
    accessMode: entry.access_mode,
    executionRoot: entry.execution_root,
    cwd: entry.cwd,
    worktree: metadata?.worktree_branch || metadata?.worktree_path
      ? {
          branch: metadata?.worktree_branch,
          path: metadata?.worktree_path,
          originalBranch: undefined,
          originalCwd: undefined,
        }
      : undefined,
  };
}

function basename(path: string): string | undefined {
  const normalized = path.replace(/\/+$/, "");
  if (!normalized || normalized === "—") return undefined;
  return normalized.split("/").pop();
}

function projectTasks(tasks: NonNullable<AgentStateDto["tasks"]>): TaskSummary[] {
  return tasks
    .filter((task) => task.id)
    .map((task) => {
      const detail = asRecord(task.detail);
      const cmd = stringValue(detail?.cmd);
      const workdir = stringValue(detail?.workdir);
      return {
        id: task.id ?? "unknown-task",
        kind: task.kind ?? "task",
        status: task.status ?? "unknown",
        summary: task.summary ?? cmd ?? task.id ?? "Task",
        command: cmd,
        workdir,
      };
    });
}

function newestBriefFromEvents(
  events: EventEnvelopeDto[],
  transcriptEntriesById: Record<string, RuntimeTranscriptEntry> = {},
  briefRecordsById: Record<string, RuntimeBriefRecord> = {},
): BriefRecordDto | undefined {
  return events
    .filter((event) => event.type === "brief_created")
    .map((event) => {
      const payload = event.payload && typeof event.payload === "object" ? (event.payload as Record<string, unknown>) : {};
      const entryId = transcriptEntryIdForPayload(payload);
      const briefId = briefIdForPayload(payload);
      const text = (entryId ? transcriptEntryText(transcriptEntriesById[entryId]) : undefined) ??
        (briefId ? briefRecordsById[briefId]?.text : undefined) ??
        (typeof payload.text === "string" ? payload.text : undefined);
      const createdAt = typeof payload.created_at === "string" ? payload.created_at : event.ts;
      const kind = typeof payload.kind === "string" ? payload.kind : undefined;
      return { id: event.id, created_at: createdAt, text, kind };
    })
    .filter((brief) => brief.text)
    .sort((left, right) => sortableTime(right.created_at) - sortableTime(left.created_at))[0];
}

function transcriptEntryText(entry: RuntimeTranscriptEntry | undefined): string | undefined {
  const data = asRecord(entry?.data);
  const text = stringValue(data?.text);
  if (text) return text;
  const blocks = Array.isArray(data?.blocks) ? data.blocks : [];
  const parts = blocks.flatMap((block) => {
    const record = asRecord(block);
    return stringValue(record?.text) ?? stringValue(record?.content) ?? [];
  });
  return parts.filter(Boolean).join("\n\n") || undefined;
}

function sortableTime(value: string | undefined): number {
  if (!value) return 0;
  const time = new Date(value).getTime();
  return Number.isNaN(time) ? 0 : time;
}

export function projectModelOptions(response: RuntimeModelsDto): RuntimeModelOption[] {
  if (response.model_availability?.length) {
    return response.model_availability
      .filter((entry): entry is ModelAvailabilityDto & { model: string } => Boolean(entry.model))
      .map((entry) => ({
        model: entry.model,
        provider: entry.provider ?? entry.model.split("/")[0] ?? "unknown",
        displayName: entry.display_name ?? entry.model,
        available: entry.available ?? false,
        unavailableReason: entry.unavailable_reason,
        supportsImageInput: entry.policy?.capabilities?.image_input ?? false,
        supportsReasoningEffort: supportsReasoningEffort(entry),
      }))
      .sort(compareModelOptions);
  }

  return (response.available_models ?? [])
    .map((entry) => {
      const model = typeof entry === "string" ? entry : entry.model;
      if (!model) return undefined;
      return {
        model,
        provider: typeof entry === "string" ? (model.split("/")[0] ?? "unknown") : (entry.provider ?? model.split("/")[0] ?? "unknown"),
        displayName: typeof entry === "string" ? model : (entry.display_name ?? model),
        available: true,
        supportsImageInput: typeof entry === "string" ? false : (entry.capabilities?.image_input ?? false),
        supportsReasoningEffort: typeof entry === "string" ? false : supportsReasoningEffort(entry),
      };
    })
    .filter((entry): entry is RuntimeModelOption => Boolean(entry))
    .sort(compareModelOptions);
}

function supportsReasoningEffort(entry: ModelAvailabilityDto | RuntimeAvailableModelDto): boolean {
  return (
    entry.policy?.supported_parameters?.includes("reasoning_effort") ||
    ("supported_parameters" in entry && entry.supported_parameters?.includes("reasoning_effort")) ||
    entry.policy?.capabilities?.supports_reasoning ||
    entry.policy?.capabilities?.reasoning_summaries ||
    ("capabilities" in entry && entry.capabilities?.supports_reasoning) ||
    ("capabilities" in entry && entry.capabilities?.reasoning_summaries) ||
    false
  );
}

function compareModelOptions(left: RuntimeModelOption, right: RuntimeModelOption): number {
  if (left.available !== right.available) return left.available ? -1 : 1;
  const provider = left.provider.localeCompare(right.provider);
  if (provider !== 0) return provider;
  return left.displayName.localeCompare(right.displayName);
}

function selectCurrentWork(
  workItems: Array<{ id?: string; objective?: string; state?: string; plan_status?: string }>,
  currentWorkItemId?: string | null,
): WorkItemSummary | undefined {
  if (!currentWorkItemId) return undefined;
  const selected = workItems.find((item) => item.id === currentWorkItemId);
  if (!selected?.id) return undefined;
  return {
    id: selected.id,
    objective: selected.objective ?? selected.id,
    state: selected.state ?? "unknown",
    planStatus: selected.plan_status,
    current: true,
  };
}

function projectWorkItems(
  workItems: Array<{ id?: string; objective?: string; state?: string; plan_status?: string }>,
): WorkItemSummary[] {
  return selectWorkItems(workItems);
}

function projectWorkItem(workItem: WorkItemDto | undefined, currentWorkItemId?: string | null): WorkItemSummary | undefined {
  if (!workItem?.id) return undefined;
  const planArtifact = workItem.plan_artifact;
  return {
    id: workItem.id,
    objective: workItem.objective ?? workItem.id,
    state: workItem.state ?? "unknown",
    planStatus: workItem.plan_status,
    current: workItem.id === currentWorkItemId,
    revision: workItem.revision,
    createdAt: workItem.created_at,
    updatedAt: workItem.updated_at,
    blockedBy: workItem.blocked_by,
    recheckAt: workItem.recheck_at,
    resultBriefId: workItem.result_brief_id,
    resultSummary: workItem.result_summary,
    planArtifact: planArtifact
      ? {
          path: planArtifact.path,
          relativePath: planArtifact.relative_path,
          workspaceAlias: planArtifact.workspace_alias,
          workspaceId: planArtifact.workspace_id,
          preview: planArtifact.preview,
          previewComplete: planArtifact.preview_complete,
          updatedAt: planArtifact.updated_at,
        }
      : undefined,
    todoList: (workItem.todo_list ?? [])
      .filter((item) => item.text)
      .map((item) => ({
        text: item.text ?? "",
        state: item.state ?? "unknown",
      })),
    workRefs: (workItem.work_refs ?? [])
      .filter((item) => item.ref)
      .map((item) => ({
        kind: item.kind ?? "other",
        ref: item.ref ?? "",
        title: item.title,
        reason: item.reason,
        status: item.status,
        lastSeenAt: item.last_seen_at,
      })),
  };
}

function selectWorkItems(
  workItems: Array<{ id?: string; objective?: string; state?: string; plan_status?: string }>,
  currentWorkItemId?: string | null,
): WorkItemSummary[] {
  return workItems
    .filter((item) => item.id)
    .map((item) => projectWorkItem(item as WorkItemDto, currentWorkItemId))
    .filter((item): item is WorkItemSummary => Boolean(item))
    .sort((left, right) => {
      if (left.current !== right.current) return left.current ? -1 : 1;
      return left.objective.localeCompare(right.objective);
    });
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

class RuntimeHttpError extends Error {
  readonly status: number;
  readonly code?: string;

  constructor(method: string, path: string, status: number, reason?: string, code?: string) {
    super(reason ? `${method} ${path} failed with ${status}: ${reason}` : `${method} ${path} failed with ${status}`);
    this.name = "RuntimeHttpError";
    this.status = status;
    this.code = code;
  }
}

async function httpRequestError(method: string, path: string, response: Response): Promise<RuntimeHttpError> {
  const envelope = await readErrorEnvelope(response);
  return new RuntimeHttpError(method, path, response.status, envelope?.error, envelope?.code);
}

async function readErrorEnvelope(response: Response): Promise<{ error?: string; code?: string } | undefined> {
  const contentType = response.headers.get("content-type") ?? "";
  if (!contentType.includes("application/json")) return undefined;
  try {
    const value = await response.json();
    const record = asRecord(value);
    return record ? { error: stringValue(record.error), code: stringValue(record.code) } : undefined;
  } catch {
    return undefined;
  }
}

function isAuthRequiredError(error: unknown): boolean {
  return error instanceof RuntimeHttpError && error.code === "auth_required";
}

function buildDisconnectedBootstrap(
  baseUrl: string | undefined,
  error: string,
  mode: "local" | "remote" = "local",
  hasToken = false,
  authRequired = false,
): RuntimeBootstrap {
  return {
    attentionCount: 0,
    connection: {
      source: "fixture",
      mode,
      baseUrl,
      hasToken,
      authRequired,
      error,
      summary: baseUrl ? `${baseUrl} unavailable` : "Holon API unavailable",
    },
    metrics: buildMetrics(0, 0, 0, 0),
    agents: [],
  };
}

function buildMetrics(agentCount: number, attentionCount: number, activeTaskCount: number, currentWorkCount: number): DashboardMetric[] {
  return [
    { label: "dashboard.agents", value: String(agentCount) },
    { label: "dashboard.needsAttention", value: String(attentionCount), tone: attentionCount > 0 ? "attention" : "muted" },
    { label: "dashboard.activeTasks", value: String(activeTaskCount), tone: activeTaskCount > 0 ? "attention" : "muted" },
    { label: "dashboard.currentWork", value: String(currentWorkCount) },
  ];
}

function normalizeBaseUrl(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  if (!trimmed) return undefined;
  const base = trimmed.replace(/\/+$/, "");
  return base.endsWith("/api") ? base : `${base}/api`;
}

function authorizationHeaders(token: string | undefined): Record<string, string> {
  const trimmed = token?.trim();
  return trimmed ? { Authorization: `Bearer ${trimmed}` } : {};
}

function compactJoin(parts: Array<string | undefined | null>): string {
  return parts.filter(Boolean).join(" · ") || "public";
}

function badgeFor(id: string): string {
  const words = id.split(/[-_]/).filter(Boolean);
  return (words.length > 1 ? words.map((word) => word[0]).join("") : id.slice(0, 3)).slice(0, 4).toUpperCase();
}

/**
 * Deterministic hue (0-360) from agent id for avatar color.
 * Uses a curated palette of 20 evenly-spaced hues with controlled
 * saturation/lightness so white text stays readable (WCAG AA).
 */
function hueFor(id: string): number {
  let hash = 0;
  for (let i = 0; i < id.length; i++) {
    hash = ((hash << 5) - hash + id.charCodeAt(i)) | 0;
  }
  const palette = [0, 18, 35, 52, 90, 130, 160, 175, 190, 205, 220, 240, 260, 275, 290, 310, 325, 340, 355, 8];
  return palette[Math.abs(hash) % palette.length];
}

function attentionLabel(pending: number, waiting: number): string {
  if (pending > 0 && waiting > 0) return `${pending} pending · ${waiting} waiting`;
  if (pending > 0) return `${pending} pending`;
  if (waiting > 0) return `${waiting} waiting`;
  return "none";
}

function stringifyLifecycle(value: string | Record<string, unknown>): string {
  if (typeof value === "string") return value;
  return Object.keys(value)[0] ?? "unknown";
}

function normalizeKebab(value: string): string {
  return value.replace(/_/g, "-");
}

function formatTime(value: string | null | undefined): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  return new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" }).format(date);
}
