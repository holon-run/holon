export type DisplayLevel = "info" | "verbose" | "debug";

export type RouteKey = "dashboard" | "agent" | "search" | "settings";

export interface RuntimeConnection {
  mode: "local" | "remote";
  summary: string;
  baseUrl?: string;
  source: "http" | "fixture";
  error?: string;
}

export interface WorkItemSummary {
  id: string;
  objective: string;
  state: string;
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
  profile: string;
  lifecycle: string;
  focusSummary: string;
  workspace: string;
  attention: string;
  model: string;
  footer: string;
  subtitle: string;
  lastBrief: string;
  lastTurnTime: string;
  pending: number;
  activeTaskCount: number;
  waitingCount: number;
  posture: string;
  postureReason: string;
  currentWork?: WorkItemSummary;
}

export type AgentTimelineItemKind = "operator" | "assistant" | "tool" | "event" | "system";

export interface AgentTimelineItem {
  id: string;
  kind: AgentTimelineItemKind;
  label: string;
  body: string;
  timestamp: string;
  meta: string;
  minDisplayLevel: DisplayLevel;
  sourceIds: string[];
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
}

export interface RuntimeBootstrap {
  attentionCount: number;
  connection: RuntimeConnection;
  metrics: DashboardMetric[];
  agents: AgentSummary[];
}
