export type DisplayLevel = "info" | "verbose" | "debug";

export type RouteKey = "dashboard" | "agent" | "search" | "settings";

export interface RuntimeConnection {
  mode: "local" | "remote";
  summary: string;
}

export interface WorkItemSummary {
  id: string;
  objective: string;
  state: string;
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
  currentWork?: WorkItemSummary;
}

export interface RuntimeBootstrap {
  attentionCount: number;
  connection: RuntimeConnection;
  agents: AgentSummary[];
}
