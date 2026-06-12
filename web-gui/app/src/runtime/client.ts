import { agentDetailFixtures, runtimeFixture } from "./fixtures";
import { reduceAgentSessionTimeline } from "./session-reducer";
import type {
  AgentDetail,
  AgentSummary,
  DashboardMetric,
  RuntimeBootstrap,
  RuntimeConnection,
  WorkItemSummary,
  DisplayLevel,
} from "./types";

export interface RuntimeClientOptions {
  baseUrl?: string;
  fetchImpl?: typeof fetch;
}

const DEFAULT_DEV_API_BASE = "/holon-api";
const DEFAULT_REQUEST_TIMEOUT_MS = 8000;
const OPTIONAL_DETAIL_TIMEOUT_MS = 4000;

function fixtureAgentDetail(agentId: string): AgentDetail {
  return agentDetailFixtures[agentId] ?? agentDetailFixtures[Object.keys(agentDetailFixtures)[0]];
}

async function fetchAgentDetail(baseUrl: string, fetchImpl: typeof fetch, agentId: string, displayLevel: DisplayLevel): Promise<AgentDetail> {
  const encodedAgentId = encodeURIComponent(agentId);
  const [entry, state, briefs, transcript, events] = await Promise.all([
    getJson<AgentListEntryDto[]>(fetchImpl, baseUrl, "/agents/list", { timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS })
      .then((agents) => agents.find((agent) => agent.identity?.agent_id === agentId))
      .catch(() => undefined),
    getJson<AgentStateDto>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/state`),
    getJson<BriefRecordDto[]>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/briefs?limit=5`, {
      timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS,
    }).catch(() => []),
    getJson<TranscriptEntryDto[]>(fetchImpl, baseUrl, `/agents/${encodedAgentId}/transcript?limit=40`, {
      timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS,
    }).catch(() => []),
    fetchAgentEvents(baseUrl, fetchImpl, agentId, { limit: 80, order: "desc", displayLevel }).catch((): EventPageResponseDto => ({
      events: [],
      has_older: false,
    })),
  ]);
  const fallbackEntry: AgentListEntryDto = entry ?? { identity: { agent_id: agentId } };
  const agent = projectAgent(fallbackEntry, state, briefs[0]);
  const timeline = reduceAgentSessionTimeline({ transcript, briefs, events, eventDisplayLevel: displayLevel });

  return {
    agent,
    source: "http",
    timeline,
    events: events.events ?? [],
    eventCursorSeq: events.cursor_seq,
    newestEventSeq: events.newest_seq,
    oldestEventSeq: events.oldest_seq,
    hasOlderEvents: events.has_older,
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

export interface EventPageResponseDto {
  events?: Array<{
    id?: string;
    event_seq?: number;
    ts?: string;
    type?: string;
    payload?: unknown;
  }>;
  oldest_seq?: number;
  newest_seq?: number;
  cursor_seq?: number;
  has_older?: boolean;
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
  onEvent: (event: StreamEventEnvelopeDto) => void;
  onError?: (error: Error) => void;
}

export interface AgentEventPageOptions {
  beforeSeq?: number;
  afterSeq?: number;
  limit?: number;
  order?: "asc" | "desc";
  displayLevel?: DisplayLevel;
}

export function createRuntimeClient(options: RuntimeClientOptions = {}) {
  const defaultBaseUrl = import.meta.env.DEV ? DEFAULT_DEV_API_BASE : undefined;
  const baseUrl = normalizeBaseUrl(options.baseUrl ?? import.meta.env.VITE_HOLON_API_BASE ?? defaultBaseUrl);
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
    async getAgentDetail(agentId: string, displayLevel: DisplayLevel = "info"): Promise<AgentDetail> {
      if (!baseUrl) {
        return fixtureAgentDetail(agentId);
      }

      try {
        return await fetchAgentDetail(baseUrl, fetchImpl, agentId, displayLevel);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return {
          ...fixtureAgentDetail(agentId),
          error: message,
        };
      }
    },
    async getAgentEvents(agentId: string, options: AgentEventPageOptions = {}): Promise<EventPageResponseDto> {
      if (!baseUrl) {
        return { events: [], has_older: false };
      }
      return fetchAgentEvents(baseUrl, fetchImpl, agentId, options);
    },
    streamAgentEvents(agentId: string, options: AgentEventStreamOptions): AgentEventStreamSubscription | undefined {
      if (!baseUrl) return undefined;
      return streamAgentEvents(baseUrl, fetchImpl, agentId, options);
    },
    async sendOperatorPrompt(agentId: string, text: string): Promise<void> {
      if (!baseUrl) {
        throw new Error("Holon API base URL is not configured.");
      }
      await postJson<unknown>(fetchImpl, baseUrl, `/control/agents/${encodeURIComponent(agentId)}/prompt`, { text });
    },
  };
}

async function fetchAgentEvents(
  baseUrl: string,
  fetchImpl: typeof fetch,
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
  return getJson<EventPageResponseDto>(fetchImpl, baseUrl, path);
}

function streamAgentEvents(
  baseUrl: string,
  fetchImpl: typeof fetch,
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

  void readEventStream(fetchImpl, `${baseUrl}${path}`, controller.signal, options);

  return {
    close: () => controller.abort(),
  };
}

async function readEventStream(
  fetchImpl: typeof fetch,
  url: string,
  signal: AbortSignal,
  options: AgentEventStreamOptions,
): Promise<void> {
  try {
    const response = await fetchImpl(url, {
      headers: { Accept: "text/event-stream" },
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
      buffer += decoder.decode(value, { stream: true });
      const frames = takeSseFrames(buffer);
      buffer = frames.remaining;
      for (const frame of frames.frames) {
        const event = parseSseEventFrame(frame);
        if (event) options.onEvent(event);
      }
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
          getJson<AgentStateDto>(fetchImpl, baseUrl, `/agents/${encodeURIComponent(id)}/state`, {
            timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS,
          }),
          getJson<BriefRecordDto[]>(fetchImpl, baseUrl, `/agents/${encodeURIComponent(id)}/briefs?limit=1`, {
            timeoutMs: OPTIONAL_DETAIL_TIMEOUT_MS,
          }),
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
  const currentWorkCount = agents.filter((agent) => agent.currentWork).length;
  const connection: RuntimeConnection = {
    mode: handshake.auth?.mode === "bearer" ? "remote" : "local",
    source: "http",
    baseUrl,
    summary: `${baseUrl} · ${handshake.auth?.mode ?? "local"} · existing /agents routes`,
  };

  return {
    attentionCount,
    connection,
    metrics: buildMetrics(agents.length, attentionCount, activeTaskCount, currentWorkCount),
    agents,
  };
}

async function getJson<T>(
  fetchImpl: typeof fetch,
  baseUrl: string,
  path: string,
  options: { timeoutMs?: number } = {},
): Promise<T> {
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), options.timeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS);
  const response = await fetchImpl(`${baseUrl}${path}`, {
    headers: { Accept: "application/json" },
    signal: controller.signal,
  }).finally(() => window.clearTimeout(timeout));
  if (!response.ok) {
    throw new Error(`GET ${path} failed with ${response.status}`);
  }
  return (await response.json()) as T;
}

async function postJson<T>(fetchImpl: typeof fetch, baseUrl: string, path: string, body: unknown): Promise<T> {
  const response = await fetchImpl(`${baseUrl}${path}`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(`POST ${path} failed with ${response.status}`);
  }

  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
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
  if (!currentWorkItemId) return undefined;
  const selected = workItems.find((item) => item.id === currentWorkItemId);
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
      summary: `${baseUrl} unavailable · using preview data`,
    },
  };
}

function buildMetrics(agentCount: number, attentionCount: number, activeTaskCount: number, currentWorkCount: number): DashboardMetric[] {
  return [
    { label: "Agents", value: String(agentCount) },
    { label: "Needs attention", value: String(attentionCount), tone: attentionCount > 0 ? "attention" : "muted" },
    { label: "Active tasks", value: String(activeTaskCount), tone: activeTaskCount > 0 ? "attention" : "muted" },
    { label: "Current work", value: String(currentWorkCount) },
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
