export type DisplayLevel = "info" | "verbose" | "debug";

export type RouteKey = "dashboard" | "agent" | "search" | "settings";

export interface RuntimeConnection {
  mode: "local" | "remote";
  summary: string;
  baseUrl?: string;
  hasToken?: boolean;
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

export interface WorkItemSummary {
  id: string;
  objective: string;
  state: string;
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

export interface TaskSummary {
  id: string;
  kind: string;
  status: string;
  summary: string;
  command?: string;
  workdir?: string;
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
  tasks?: TaskSummary[];
  workItems?: WorkItemSummary[];
}

export interface RuntimeModelOption {
  model: string;
  provider: string;
  displayName: string;
  available: boolean;
  unavailableReason?: string;
  supportsImageInput: boolean;
  supportsReasoningEffort: boolean;
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
  modelCatalog: string[];
  unknownModelFallbackConfigured: boolean;
  runtimeMaxOutputTokens: number;
  defaultToolOutputTokens: number;
  maxToolOutputTokens: number;
  disableProviderFallback: boolean;
  providers: RuntimeProviderSummary[];
  webSearch?: RuntimeWebSearchSummary;
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

export interface SearchResultLocator {
  evidenceId?: string;
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

export interface AgentTimelineActivity {
  id: string;
  kind: AgentTimelineItemKind;
  label: string;
  body: string;
  timestamp: string;
  meta: string;
  minDisplayLevel: DisplayLevel;
  sourceIds: string[];
  detail?: AgentTimelineItemDetail;
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
  detail?: AgentTimelineItemDetail;
  activities?: AgentTimelineActivity[];
  rawEvent?: unknown;
  debug?: string;
}

export interface AgentDetail {
  agent: AgentSummary;
  timeline: AgentTimelineItem[];
  source: "http" | "fixture";
  error?: string;
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
}

export interface RuntimeBootstrap {
  attentionCount: number;
  connection: RuntimeConnection;
  metrics: DashboardMetric[];
  agents: AgentSummary[];
}
