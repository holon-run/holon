import { agentDetailFixtures, runtimeFixture } from "./fixtures";
import { reduceAgentSessionTimeline } from "./session-reducer";
import type {
  AgentDetail,
  AgentSummary,
  DashboardMetric,
  RuntimeBootstrap,
  RuntimeConnection,
  WorkItemSummary,
} from "./types";

export interface RuntimeClientOptions {
  baseUrl?: string;
  fetchImpl?: typeof fetch;
}

function fixtureAgentDetail(agentId: string): AgentDetail {
  return agentDetailFixtures[agentId] ?? agentDetailFixtures[Object.keys(agentDetailFixtures)[0]];
}

async function fetchAgentDetail(baseUrl: string, fetchImpl: typeof fetch, agentId: string): Promise<AgentDetail> {
  const encodedAgentId = encodeURIComponent(agentId);
  const [entry, state, briefs, transcript, events] = await Promise.all([
    getJson<AgentListEntryDto[]>(fetchImpl, baseUrl, "/agents/list").then((agents) => agents.find((agent) => agent.identity?.agent_id === agentId)),
    getJson<AgentStateDto>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/state`),
    getJson<BriefRecordDto[]>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/briefs?limit=5`),
    getJson<TranscriptEntryDto[]>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/transcript?limit=40`),
    getJson<EventPageResponseDto>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/events?limit=20&order=desc&max_level=verbose`),
  ]);
  const fallbackEntry: AgentListEntryDto = entry ?? { identity: { agent_id: agentId } };
  const agent = projectAgent(fallbackEntry, state, briefs[0]);
  const timeline = reduceAgentSessionTimeline({ transcript, briefs, events });

  return {
    agent,
    source: "http",
    timeline,
  };
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
  waiting_reason?: unknown;
  model?: {
    effective_model?: string;
    active_model?: string | null;
  };
  active_workspace_entry?: {
    workspace_id?: string;
    workspace_alias?: string | null;
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
  };
  tasks?: unknown[];
  work_items?: Array<{
    id?: string;
    objective?: string;
    state?: string;
  }>;
  waiting_intents?: unknown[];
  workspace?: {
    active_workspace_entry?: AgentListEntryDto["active_workspace_entry"];
    worktree_session?: {
      worktree_branch?: string;
    } | null;
  };
}

interface BriefRecordDto {
  id?: string;
  created_at?: string;
  text?: string;
  kind?: string;
}

interface TranscriptEntryDto {
  id?: string;
  created_at?: string;
  kind?: string;
  round?: number | null;
  stop_reason?: string | null;
  input_tokens?: number | null;
  output_tokens?: number | null;
  data?: unknown;
}

interface EventPageResponseDto {
  events?: Array<{
    id?: string;
    event_seq?: number;
    ts?: string;
    type?: string;
    payload?: unknown;
  }>;
}

export function createRuntimeClient(options: RuntimeClientOptions = {}) {
  const baseUrl = normalizeBaseUrl(options.baseUrl ?? import.meta.env.VITE_HOLON_API_BASE);
  const fetchImpl = options.fetchImpl ?? fetch;

  return {
    async getBootstrap(): Promise<RuntimeBootstrap> {
      if (!baseUrl) {
        return runtimeFixture;
      }

      try {
        return await fetchRuntimeBootstrap(baseUrl, fetchImpl);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return withFixtureFallback(baseUrl, message);
      }
    },
    async getAgentDetail(agentId: string): Promise<AgentDetail> {
      if (!baseUrl) {
        return fixtureAgentDetail(agentId);
      }

      try {
        return await fetchAgentDetail(baseUrl, fetchImpl, agentId);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return {
          ...fixtureAgentDetail(agentId),
          error: message,
        };
      }
    },
  };
}

async function fetchRuntimeBootstrap(baseUrl: string, fetchImpl: typeof fetch): Promise<RuntimeBootstrap> {
  const [handshake, agentEntries] = await Promise.all([
    getJson<{ auth?: { mode?: string } }>(fetchImpl, baseUrl, "/handshake"),
    getJson<AgentListEntryDto[]>(fetchImpl, baseUrl, "/agents/list"),
  ]);

  const statePairs = await Promise.all(
    agentEntries.map(async (entry) => {
      const id = entry.identity?.agent_id;
      if (!id) return [entry, undefined, undefined] as const;

      try {
        const [state, briefs] = await Promise.all([
          getJson<AgentStateDto>(fetchImpl, baseUrl, `/agents/${encodeURIComponent(id)}/state`),
          getJson<BriefRecordDto[]>(fetchImpl, baseUrl, `/agents/${encodeURIComponent(id)}/briefs?limit=1`),
        ]);
        return [entry, state, briefs[0]] as const;
      } catch {
        return [entry, undefined, undefined] as const;
      }
    }),
  );

  const agents = statePairs.map(([entry, state, brief]) => projectAgent(entry, state, brief));
  const attentionCount = agents.filter((agent) => agent.pending > 0 || agent.waitingCount > 0).length;
  const activeTaskCount = agents.reduce((sum, agent) => sum + agent.activeTaskCount, 0);
  const openWorkCount = agents.filter((agent) => agent.currentWork).length;
  const connection: RuntimeConnection = {
    mode: handshake.auth?.mode === "bearer" ? "remote" : "local",
    source: "http",
    baseUrl,
    summary: `${baseUrl} · ${handshake.auth?.mode ?? "local"} · existing /agents routes`,
  };

  return {
    attentionCount,
    connection,
    metrics: buildMetrics(agents.length, attentionCount, activeTaskCount, openWorkCount),
    agents,
  };
}

async function getJson<T>(fetchImpl: typeof fetch, baseUrl: string, path: string): Promise<T> {
  const response = await fetchImpl(`${baseUrl}${path}`, {
    headers: { Accept: "application/json" },
  });
  if (!response.ok) {
    throw new Error(`GET ${path} failed with ${response.status}`);
  }
  return (await response.json()) as T;
}

function projectAgent(entry: AgentListEntryDto, state?: AgentStateDto, brief?: BriefRecordDto): AgentSummary {
  const id = entry.identity?.agent_id ?? state?.agent?.agent?.id ?? "unknown-agent";
  const status = state?.agent?.agent?.status ?? entry.status ?? "unknown";
  const profile = compactJoin([entry.identity?.visibility ?? "public", entry.identity?.ownership, entry.identity?.profile_preset]);
  const workspaceEntry = state?.workspace?.active_workspace_entry ?? entry.active_workspace_entry;
  const workspace = workspaceEntry?.workspace_alias ?? workspaceEntry?.workspace_id ?? state?.workspace?.worktree_session?.worktree_branch ?? "not bound";
  const currentWork = selectCurrentWork(state?.work_items ?? [], state?.agent?.agent?.current_work_item_id);
  const pending = state?.session?.pending_count ?? entry.pending ?? 0;
  const activeTaskCount = state?.tasks?.length ?? state?.agent?.active_task_count ?? 0;
  const waitingCount = state?.waiting_intents?.length ?? state?.agent?.active_waiting_intents?.length ?? (entry.waiting_reason ? 1 : 0);
  const posture = state?.agent?.scheduling_posture?.posture ?? entry.scheduling_posture?.posture ?? "unknown";
  const postureReason = state?.agent?.scheduling_posture?.reason ?? entry.scheduling_posture?.reason ?? "posture unavailable";
  const model = state?.agent?.model?.active_model ?? state?.agent?.model?.effective_model ?? entry.model?.active_model ?? entry.model?.effective_model ?? "runtime default";
  const lifecycle = stringifyLifecycle(state?.agent?.lifecycle ?? entry.lifecycle ?? status);

  return {
    id,
    badge: badgeFor(id),
    profile,
    lifecycle: normalizeKebab(status),
    focusSummary: postureReason,
    workspace,
    attention: attentionLabel(pending, waitingCount),
    model,
    footer: `${lifecycle} · ${posture}`,
    subtitle: `${status} · ${workspace}`,
    lastBrief: brief?.text ?? "No recent brief returned by /agents/:id/briefs.",
    lastTurnTime: formatTime(brief?.created_at ?? state?.agent?.agent?.last_brief_at),
    pending,
    activeTaskCount,
    waitingCount,
    posture,
    postureReason,
    currentWork,
  };
}

function selectCurrentWork(
  workItems: Array<{ id?: string; objective?: string; state?: string }>,
  currentWorkItemId?: string | null,
): WorkItemSummary | undefined {
  const selected = workItems.find((item) => item.id === currentWorkItemId) ?? workItems.find((item) => item.state === "open") ?? workItems[0];
  if (!selected?.id) return undefined;
  return {
    id: selected.id,
    objective: selected.objective ?? selected.id,
    state: selected.state ?? "unknown",
  };
}

function withFixtureFallback(baseUrl: string, error: string): RuntimeBootstrap {
  return {
    ...runtimeFixture,
    connection: {
      ...runtimeFixture.connection,
      source: "fixture",
      baseUrl,
      error,
      summary: `${baseUrl} unavailable · showing fixture fallback`,
    },
  };
}

function buildMetrics(agentCount: number, attentionCount: number, activeTaskCount: number, openWorkCount: number): DashboardMetric[] {
  return [
    { label: "Agents", value: String(agentCount) },
    { label: "Needs attention", value: String(attentionCount), tone: attentionCount > 0 ? "attention" : "muted" },
    { label: "Active tasks", value: String(activeTaskCount), tone: activeTaskCount > 0 ? "attention" : "muted" },
    { label: "Open work", value: String(openWorkCount) },
  ];
}

function normalizeBaseUrl(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  if (!trimmed) return undefined;
  return trimmed.replace(/\/+$/, "");
}

function compactJoin(parts: Array<string | undefined | null>): string {
  return parts.filter(Boolean).join(" · ") || "public";
}

function badgeFor(id: string): string {
  const words = id.split(/[-_]/).filter(Boolean);
  return (words.length > 1 ? words.map((word) => word[0]).join("") : id.slice(0, 3)).slice(0, 4).toUpperCase();
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

