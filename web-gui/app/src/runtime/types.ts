export type DisplayLevel = "info" | "verbose" | "debug";

export type RouteKey = "dashboard" | "agent" | "search" | "skills" | "skillDetail" | "templates" | "templateDetail" | "settings";

export interface RuntimeConnection {
  mode: "local" | "remote";
  summary: string;
  baseUrl?: string;
  hasToken?: boolean;
  authRequired?: boolean;
  source: "http" | "fixture";
  error?: string;
}

export interface RuntimeConnectionConfig {
  mode: "local" | "remote";
  baseUrl?: string;
  token?: string;
}

export interface RuntimeConnectionProfile {
  baseUrl: string;
  hasToken: boolean;
}

export type SkillScope = "user" | "user_global" | "agent" | "workspace";

export interface SkillCatalogEntry {
  skillId: string;
  rootId: string;
  skillDir: string;
  name: string;
  description: string;
  path: string;
  scope: SkillScope;
}

export interface SkillDetailState {
  source: "http" | "fixture";
  skill?: SkillCatalogEntry;
  content?: string;
  error?: string;
}

export interface SkillCatalogState {
  source: "http" | "fixture";
  agentId?: string;
  catalog: SkillCatalogEntry[];
  error?: string;
}

export type AgentTemplateSourceKind = "builtin" | "user_global" | "agent_home" | "remote";

export interface AgentTemplateCatalogEntry {
  catalogId: string;
  template: string;
  templateId: string;
  source: AgentTemplateSourceKind;
  path?: string;
  name: string;
  schemaVersion?: string;
  description: string;
  includedSkills: string[];
  sourceId?: string;
  resolvedRef?: string;
  resolvedRevision?: string;
  sourceUrl?: string;
}

export interface AgentTemplateRemoteSource {
  sourceId: string;
  kind: string;
  enabled: boolean;
  status?: string;
  url?: string;
  resolvedRef?: string;
  resolvedRevision?: string;
  lastSyncedAt?: string;
}

export interface AgentTemplateCatalogDiagnostic {
  level?: string;
  message: string;
  sourceId?: string;
}

export interface AgentTemplateCatalogState {
  source: "http" | "fixture";
  catalog: AgentTemplateCatalogEntry[];
  sources: AgentTemplateRemoteSource[];
  diagnostics: AgentTemplateCatalogDiagnostic[];
  error?: string;
}

export interface AgentTemplateSkillDependency {
  kind: string;
  reference: string;
}

export interface AgentTemplateDetail {
  catalogId: string;
  template: string;
  templateId: string;
  source: AgentTemplateSourceKind;
  sourceLocation?: string;
  name: string;
  summary: string;
  schemaVersion?: string;
  agentsMd: string;
  skills: AgentTemplateSkillDependency[];
}

export interface AgentTemplateDetailState {
  source: "http" | "fixture";
  detail?: AgentTemplateDetail;
  error?: string;
}

export type SkillInstallMode = "linked" | "copied";

export type AddSkillInput =
  | { kind: "builtin"; name: string }
  | { kind: "named"; name: string; mode?: SkillInstallMode }
  | { kind: "local"; path: string; mode?: SkillInstallMode }
  | { kind: "remote"; package: string; skill?: string; mode?: SkillInstallMode };

export interface WorkItemSummary {
  id: string;
  objective: string;
  state: string;
  readiness?: string;
  schedulingState?: string;
  focus?: string;
  planStatus?: string;
  current?: boolean;
  revision?: number;
  createdAt?: string;
  updatedAt?: string;
  blockedBy?: string;
  recheckAt?: string;
  resultBriefId?: string;
  resultSummary?: string;
  planArtifact?: WorkItemPlanArtifactSummary;
  todoList?: WorkItemTodoItem[];
  workRefs?: WorkItemRefSummary[];
}

export interface WorkItemPlanArtifactSummary {
  path?: string;
  relativePath?: string;
  workspaceAlias?: string;
  workspaceId?: string;
  preview?: string;
  previewComplete?: boolean;
  updatedAt?: string;
}

export interface WorkItemTodoItem {
  text: string;
  state: string;
}

export interface WorkItemRefSummary {
  kind: string;
  ref: string;
  title?: string;
  reason?: string;
  status?: string;
  lastSeenAt?: string;
}

export interface WorkspaceSummary {
  id: string;
  name: string;
  anchor: string;
  executionRootId?: string;
  projectionKind?: string;
  accessMode?: string;
  executionRoot?: string;
  cwd?: string;
  worktree?: {
    branch?: string;
    path?: string;
    originalBranch?: string;
    originalCwd?: string;
  };
}

export interface AttachedWorkspaceEntry {
  workspaceId: string;
  name: string;
  anchor: string;
  executionRootId?: string;
  repoName?: string;
}

export interface TaskSummary {
  id: string;
  kind: string;
  status: string;
  summary: string;
  command?: string;
  workdir?: string;
}

export interface TaskTokenUsage {
  total: { input_tokens: number; output_tokens: number; total_tokens: number };
  last_turn?: { input_tokens: number; output_tokens: number; total_tokens: number } | null;
  total_model_rounds: number;
}

export interface SpawnAgentModelRequest {
  provider: string;
  model: string;
  reasoning_effort?: string | null;
  temperature?: number | null;
  max_output_tokens?: number | null;
  allow_fallback?: boolean | null;
}

export interface SpawnAgentModelResolution {
  requested?: SpawnAgentModelRequest | null;
  resolved_provider: string;
  resolved_model: string;
  resolution_status: "inherited" | "accepted" | "normalized" | "fallback_used" | "rejected";
  policy_notes?: string[];
}

export interface TaskChildObservability {
  phase: "running" | "blocked" | "waiting" | "terminal";
  blocked_reason?: string | null;
  waiting_reason?: string | null;
  current_work_item_id?: string | null;
  work_summary?: string | null;
  last_progress_brief?: string | null;
  last_result_brief?: string | null;
}

export interface TaskChildSupervision {
  child_agent_id: string;
  child_work_item_id?: string | null;
  cleanup_owner: string;
  cleanup_status?: string | null;
  delegation_id?: string | null;
  followup_target: string;
  parent_agent_id: string;
  parent_work_item_id?: string | null;
  supervision_task_id: string;
  workspace_mode?: "inherit" | "worktree" | null;
  worktree?: {
    worktree_path?: string | null;
    worktree_branch?: string | null;
    actual_branch?: string | null;
    original_branch?: string | null;
    original_cwd?: string | null;
    projection_kind?: string | null;
    changed_files?: string[];
    cleanup_status?: string | null;
    cleanup_reason?: string | null;
    cleanup_error?: string | null;
    branch_cleanup_status?: string | null;
    branch_cleanup_error?: string | null;
    auto_cleaned_up?: boolean | null;
    retained_for_review?: boolean | null;
  } | null;
}

export interface TaskCommandInfo {
  cmd?: string | null;
  workdir?: string | null;
  shell?: string | null;
  login?: boolean | null;
  tty?: boolean | null;
  exit_status?: number | null;
  output_path?: string | null;
  accepts_input?: boolean | null;
  input_target?: string | null;
  terminal_reentry?: boolean | null;
  promoted_from_exec_command?: boolean | null;
  result_summary?: string | null;
  cmd_digest?: string | null;
}

export interface TaskFailureArtifact {
  category: string;
  kind: string;
  summary: string;
  domain?: string | null;
  retryable?: boolean | null;
  recovery_hint?: string | null;
  status?: number | null;
  exit_status?: number | null;
  provider?: string | null;
  model_ref?: string | null;
  task_id?: string | null;
  source_chain?: string[];
  context?: Record<string, unknown>;
  metadata?: Record<string, string>;
}

export interface TaskStatusSnapshot {
  task_id: string;
  kind: string;
  status: string;
  summary?: string | null;
  created_at: string;
  updated_at: string;
  wait_policy: string;
  parent_message_id?: string | null;
  command?: TaskCommandInfo | null;
  child_agent_id?: string | null;
  child_observability?: TaskChildObservability | null;
  child_supervision?: TaskChildSupervision | null;
  token_usage?: TaskTokenUsage | null;
  model_resolution?: SpawnAgentModelResolution | null;
}

export interface DashboardMetric {
  label: string;
  value: string;
  tone?: "default" | "attention" | "muted";
}

export interface AgentSummary {
  id: string;
  badge: string;
  badgeTone?: "muted";
  badgeHue?: number;
  profile: string;
  lifecycle: string;
  focusSummary: string;
  workspace: string;
  attention: string;
  model: string;
  modelSource?: "runtime_default" | "agent_override";
  modelReasoningEffort?: string;
  footer: string;
  subtitle: string;
  lastBrief: string;
  lastTurnTime: string;
  pending: number;
  activeTaskCount: number;
  waitingCount: number;
  posture: string;
  postureReason: string;
  currentRunId?: string | null;
  currentWork?: WorkItemSummary;
  workspaceSummary?: WorkspaceSummary;
  attachedWorkspaces?: AttachedWorkspaceEntry[];
  tasks?: TaskSummary[];
  workItems?: WorkItemSummary[];
}

export interface RuntimeModelOption {
  model: string;
  routeRef: string;
  provider: string;
  providerFamily: string;
  endpoint: string;
  routeProvider: string;
  displayName: string;
  available: boolean;
  unavailableReason?: string;
  supportsImageInput: boolean;
  supportsImageGeneration: boolean;
  supportsReasoningEffort: boolean;
  reasoningEffortOptions: string[];
}

export interface RuntimeModelCatalog {
  source: "http" | "fixture";
  options: RuntimeModelOption[];
  error?: string;
}

export interface RuntimeBriefRecord {
  id?: string;
  created_at?: string;
  text?: string;
  kind?: string;
}

export interface RuntimeProviderSummary {
  id: string;
  transport: string;
  baseUrl: string;
  oauthSupported: boolean;
  apiKeySupported: boolean;
  credentialSource: string;
  credentialKind: string;
  credentialEnv?: string;
  credentialProfile?: string;
  credentialExternal?: string;
  credentialConfigured: boolean;
  configuredInConfig: boolean;
}

export interface RuntimeConfigSurface {
  modelDefault: string;
  modelFallbacks: string[];
  visionDefault?: string;
  imageGenerationDefault?: string;
  modelCatalog: string[];
  unknownModelFallbackConfigured: boolean;
  defaultToolOutputTokens: number;
  maxToolOutputTokens: number;
  disableProviderFallback: boolean;
  providers: RuntimeProviderSummary[];
  webSearch?: RuntimeWebSearchSummary;
  availableSearchProviderKinds: RuntimeWebSearchProviderKindSummary[];
  webSearchProviders: RuntimeWebSearchProviderSummary[];
}

export interface RuntimeWebSearchSummary {
  enabled: boolean;
  builtinProviderEnabled: boolean;
  provider: string;
  mode: "single" | "fallback" | "aggregate";
  providers: string[];
  maxResults: number;
  maxProviderAttempts: number;
}

export interface RuntimeWebSearchProviderSummary {
  id: string;
  kind: string;
  baseUrl?: string;
  credentialProfile?: string;
  credentialConfigured: boolean;
}

export interface RuntimeWebSearchProviderKindSummary {
  kind: string;
  capabilities: RuntimeWebSearchProviderCapabilities;
}

export interface RuntimeWebSearchProviderCapabilities {
  auth: "none" | "api_key" | "native_provider" | "self_hosted";
  costClass: "free" | "self_hosted" | "paid" | "provider_metered";
  qualityHint: "html_fallback" | "keyword" | "semantic" | "research" | "native";
  supportsDomainFilter: boolean;
  supportsFreshness: boolean;
  supportsRegionOrLanguage: boolean;
  supportsFullContent: boolean;
  supportsNativeCitations: boolean;
  defaultPriority: number;
  status: "supported" | "unsupported" | "native_only";
}

export interface RuntimeConfigUpdateResult {
  key: string;
  effect: "accepted_requires_restart" | "accepted_reloaded" | "rejected";
  reason: string;
}

export interface RuntimeConfigState {
  source: "http" | "fixture";
  configFilePath?: string;
  surface?: RuntimeConfigSurface;
  changed?: boolean;
  results?: RuntimeConfigUpdateResult[];
  error?: string;
}


export interface CredentialProfileStatus {
  profile: string;
  kind: string;
  configured: boolean;
}

export interface CredentialStoreState {
  profiles: CredentialProfileStatus[];
  error?: string;
}

export interface CodexDeviceLoginResponse {
  ok: boolean;
  loginId: string;
  verificationUrl: string;
  userCode: string;
  interval: number;
  expiresAt: string;
  jobId: string;
}

export type CodexDeviceLoginStatus =
  | "idle"
  | "starting"
  | "waiting"
  | "completed"
  | "failed"
  | "cancelled";

export interface CodexDeviceLoginState {
  status: CodexDeviceLoginStatus;
  verificationUrl?: string;
  userCode?: string;
  jobId?: string;
  expiresAt?: string;
  error?: string;
}

export interface SearchResultLocator {
  evidenceId?: string;
  sourceRef?: string;
  messageId?: string;
  turnId?: string;
  taskId?: string;
  workItemId?: string;
  eventSeq?: number;
}

export interface SearchResultItem {
  resultType: "message";
  agentId: string;
  locator: SearchResultLocator;
  createdAt?: string;
  kind: string;
  preview: string;
}

export interface SearchResponse {
  query: string;
  limit: number;
  results: SearchResultItem[];
}

export interface MemorySourceContent {
  kind: string;
  sourceRef: string;
  title: string;
  content: string;
  truncated: boolean;
  updatedAt?: string;
}

export interface RuntimeSearchOptions {
  agentIds?: string[];
  includeAllWorkspaces?: boolean;
  limit?: number;
}

export interface RuntimeMessageEnvelope {
  id?: string;
  agent_id?: string;
  origin?: unknown;
  body?: unknown;
  [key: string]: unknown;
}

export interface RuntimeTranscriptEntry {
  id?: string;
  agent_id?: string;
  data?: unknown;
  [key: string]: unknown;
}

export type AgentTimelineItemKind = "operator" | "assistant" | "tool" | "event" | "system";

export interface AgentTimelineItemDetail {
  label: string;
  text: string;
  tone?: "command" | "output" | "data" | "diff";
}

export type TimelineExecutionOutcome =
  | "queued"
  | "running"
  | "cancelling"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "promoted";

export interface TimelineExecutionMeta {
  outcome?: TimelineExecutionOutcome;
  exitStatus?: number;
  durationMs?: number;
  outputTruncated?: boolean;
  taskId?: string;
}

export interface TimelineStatusStep {
  status: TimelineExecutionOutcome;
  timestamp?: string;
}

export type TimelineStateObjectRef =
  | {
      kind: "work_item";
      id: string;
      objective?: string;
      state?: string;
    }
  | {
      kind: "tool_execution";
      id: string;
      toolName: string;
      status: string;
    }
  | {
      kind: "task";
      id: string;
      status: string;
      summary?: string;
    };

export interface AgentTimelineActivity {
  id: string;
  kind: AgentTimelineItemKind;
  label: string;
  body: string;
  timestamp: string;
  meta: string;
  minDisplayLevel: DisplayLevel;
  sourceIds: string[];
  stateObjectRef?: TimelineStateObjectRef;
  relatedStateObjectRef?: TimelineStateObjectRef;
  detail?: AgentTimelineItemDetail;
  executionMeta?: TimelineExecutionMeta;
  statusTrail?: TimelineStatusStep[];
  rawEvent?: unknown;
  debug?: string;
}

export interface RuntimeToolExecutionRecord {
  id?: string;
  agent_id?: string;
  tool_call_id?: string;
  tool_name?: string;
  status?: string;
  summary?: string;
  input?: unknown;
  output?: unknown;
  result?: unknown;
  error?: unknown;
  duration_ms?: number;
  created_at?: string;
  completed_at?: string;
  [key: string]: unknown;
}

export interface ToolExecutionArtifactContent {
  artifactIndex: number;
  size: number;
  content: string;
}

export interface RuntimeTaskOutputResult {
  retrieval_status?: string;
  task?: {
    task_id?: string;
    kind?: string;
    status?: string;
    summary?: string;
    output_preview?: string;
    output_truncated?: boolean;
    result_summary?: string;
    exit_status?: number;
    [key: string]: unknown;
  };
  status?: string;
  stdout?: string;
  stderr?: string;
  output?: string;
  summary?: string;
  truncated?: boolean;
  [key: string]: unknown;
}

export interface InspectorActivityDetailState {
  loading?: boolean;
  error?: string;
  toolExecution?: RuntimeToolExecutionRecord;
  taskOutput?: RuntimeTaskOutputResult;
}

export interface WorkItemDetailState {
  loading?: boolean;
  error?: string;
  workItem?: WorkItemSummary;
}

export interface TaskDetailState {
  loading?: boolean;
  error?: string;
  output?: RuntimeTaskOutputResult;
  status?: TaskStatusSnapshot;
}

export interface ToolExecutionDetailState {
  loading?: boolean;
  error?: string;
  toolExecution?: RuntimeToolExecutionRecord;
}

export interface WorkspaceFileEntry {
  name: string;
  type: "directory" | "file" | "symlink";
  size: number;
  mimeType?: string;
}

export interface WorkspaceDirectoryListing {
  type: "directory";
  path: string;
  workspaceId: string;
  entries: WorkspaceFileEntry[];
}

export interface WorkspaceFileContent {
  type: "file";
  path: string;
  workspaceId: string;
  size: number;
  mimeType: string;
  truncated: boolean;
  totalSize?: number;
  content?: string;
}

export type RightPanelView =
  | {
      kind: "agent_overview";
      agentId: string;
    }
  | {
      kind: "work_item_detail";
      agentId: string;
      workItem: WorkItemSummary;
    }
  | {
      kind: "activity_inspector";
      agentId: string;
      activity: AgentTimelineActivity;
      detailState?: InspectorActivityDetailState;
    }
  | {
      kind: "task_detail";
      agentId: string;
      task: TaskSummary;
      detailState?: TaskDetailState;
    }
  | {
      kind: "tool_execution_detail";
      agentId: string;
      toolExecutionId: string;
      toolName?: string;
      relatedStateObjectRef?: TimelineStateObjectRef;
      detailState?: ToolExecutionDetailState;
    }
  | {
      kind: "file_browser";
      agentId: string;
      workspaceId: string;
      executionRootId?: string;
      initialFilePath?: string;
      initialPath?: string;
    };

export interface AgentTimelineItem {
  id: string;
  kind: AgentTimelineItemKind;
  label: string;
  body: string;
  timestamp: string;
  meta: string;
  minDisplayLevel: DisplayLevel;
  sourceIds: string[];
  stateObjectRef?: TimelineStateObjectRef;
  relatedStateObjectRef?: TimelineStateObjectRef;
  detail?: AgentTimelineItemDetail;
  executionMeta?: TimelineExecutionMeta;
  statusTrail?: TimelineStatusStep[];
  activities?: AgentTimelineActivity[];
  rawEvent?: unknown;
  debug?: string;
  briefHydration?: BriefHydrationViewState;
}

export type BriefHydrationStatus = "pending" | "loading" | "resolved" | "not_found" | "failed";

export interface BriefHydrationViewState {
  briefId: string;
  status: BriefHydrationStatus;
  attempt: number;
  errorKind?: string;
}

export interface AgentDetail {
  agent: AgentSummary;
  timeline: AgentTimelineItem[];
  source: "http" | "fixture";
  error?: string;
  eventLogEpoch?: string;
  events?: Array<{
    id?: string;
    event_seq?: number;
    ts?: string;
    type?: string;
    payload?: unknown;
  }>;
  eventCursorSeq?: number;
  newestEventSeq?: number;
  oldestEventSeq?: number;
  hasOlderEvents?: boolean;
  briefRecordsById?: Record<string, RuntimeBriefRecord>;
  transcriptEntriesById?: Record<string, RuntimeTranscriptEntry>;
}

export interface RuntimeBootstrap {
  attentionCount: number;
  connection: RuntimeConnection;
  metrics: DashboardMetric[];
  agents: AgentSummary[];
}
