import type { TFunction } from "i18next";
import type { AgentAvatarExpression } from "../lib/agent-avatar";

export interface AgentStatusInput {
  lifecycle?: string | null;
  posture?: string | null;
  postureReason?: string;
  currentRunId?: string | null;
  pending?: number;
  activeTaskCount?: number;
  waitingCount?: number;
}

export type AgentDisplayKind = "stopped" | "needs-input" | "running" | "runnable" | "waiting" | "blocked" | "ready" | "unknown" | "deleting";
export type AgentStatusIcon = "circle-x" | "circle-alert" | "spinner" | "circle-arrow-right" | "clock" | "lock" | "circle-check" | "circle-help";

interface AgentStatusProjection {
  kind: AgentDisplayKind;
  icon: AgentStatusIcon;
  tone: "muted" | "attention" | "accent" | "danger";
}

const presentation: Record<AgentDisplayKind, Omit<AgentStatusProjection, "kind">> = {
  stopped: { icon: "circle-x", tone: "muted" },
  "needs-input": { icon: "circle-alert", tone: "attention" },
  running: { icon: "spinner", tone: "accent" },
  runnable: { icon: "circle-arrow-right", tone: "accent" },
  waiting: { icon: "clock", tone: "muted" },
  blocked: { icon: "lock", tone: "danger" },
  ready: { icon: "circle-check", tone: "muted" },
  unknown: { icon: "circle-help", tone: "muted" },
  deleting: { icon: "circle-x", tone: "danger" },
};

const postureKinds: Record<string, AgentDisplayKind> = {
  "waiting-for-operator": "needs-input",
  "active-turn": "running",
  "has-queued-input": "runnable",
  "has-runnable-work": "runnable",
  "waiting-for-task": "waiting",
  "waiting-for-external": "waiting",
  blocked: "blocked",
  idle: "ready",
};

export interface AgentDisplayStatus extends AgentStatusProjection {
  label: string;
  title: string;
}

export function deriveAgentStatusProjection(agent: AgentStatusInput, stateIsCurrent = true): AgentStatusProjection {
  const kind = deriveAgentDisplayKind(agent, stateIsCurrent);
  return { kind, ...presentation[kind] };
}

export function deriveAgentDisplayStatus(agent: AgentStatusInput, t: TFunction, stateIsCurrent = true): AgentDisplayStatus {
  const details = [
    agent.posture ? `${t("statusDetail.posture")}: ${agent.posture}` : undefined,
    agent.postureReason ? `${t("statusDetail.reason")}: ${agent.postureReason}` : undefined,
    agent.lifecycle ? `${t("statusDetail.lifecycle")}: ${agent.lifecycle}` : undefined,
    agent.currentRunId ? `${t("statusDetail.run")}: ${agent.currentRunId}` : undefined,
    (agent.pending ?? 0) > 0 ? t("statusDetail.queuedInputs", { count: agent.pending }) : undefined,
    (agent.activeTaskCount ?? 0) > 0
      ? t("statusDetail.activeTasks", { count: agent.activeTaskCount })
      : undefined,
    (agent.waitingCount ?? 0) > 0 ? t("statusDetail.waitingConditions", { count: agent.waitingCount }) : undefined,
  ].filter(Boolean);
  const projection = deriveAgentStatusProjection(agent, stateIsCurrent);
  const labels = {
    deleting: "badge.deleting",
    stopped: "badge.stopped",
    running: "badge.running",
    runnable: "badge.runnable",
    "needs-input": "badge.needsInput",
    waiting: "badge.waiting",
    blocked: "badge.blocked",
    ready: "badge.ready",
    unknown: "badge.unknown",
  } as const;
  const label = t(labels[projection.kind]);
  const detail = details.join(" · ") || t("statusDetail.noDetails");
  const title = `${label} · ${stateIsCurrent ? detail : `${t("statusDetail.cached")}: ${detail}`}`;
  return { ...projection, label, title };
}

/** Expressions supplement status badges, and never imply activity from stale data. */
export function deriveAgentAvatarExpression(agent: AgentStatusInput, stateIsCurrent = true): AgentAvatarExpression {
  switch (deriveAgentStatusProjection(agent, stateIsCurrent).kind) {
    case "running": return "focused";
    case "needs-input": return "questioning";
    case "waiting": return "waiting";
    case "blocked": return "waiting";
    case "ready":
      // Background tasks alone do not mean the Agent is waiting for them.
      if ((agent.activeTaskCount ?? 0) > 0) return "neutral";
      if (
        normalizeAgentStatus(agent.lifecycle) === "asleep"
        && normalizeAgentStatus(agent.posture) === "idle"
        && (agent.pending ?? 0) === 0
      ) return "sleeping";
      return "neutral";
    default: return "neutral";
  }
}

function deriveAgentDisplayKind(agent: AgentStatusInput, stateIsCurrent: boolean): AgentDisplayKind {
  if (!stateIsCurrent) return "unknown";
  const lifecycle = normalizeAgentStatus(agent.lifecycle);
  const posture = normalizeAgentStatus(agent.posture);

  if (lifecycle === "deleting") {
    return "deleting";
  }

  if (lifecycle === "offline" || lifecycle === "unloaded") {
    return "unknown";
  }

  if (isStoppedStatus(lifecycle) || isStoppedStatus(posture)) {
    return "stopped";
  }

  const knownKind = Object.hasOwn(postureKinds, posture) ? postureKinds[posture] : undefined;
  if (knownKind) return knownKind;

  // Compatibility hints apply only when the scheduler posture is unavailable.
  if (lifecycle === "awake-running" || lifecycle === "running" || Boolean(agent.currentRunId)) {
    return "running";
  }

  if ((agent.pending ?? 0) > 0) return "runnable";
  if ((agent.waitingCount ?? 0) > 0) return "waiting";

  if (
    lifecycle === "asleep" ||
    lifecycle === "awake-idle" ||
    lifecycle === "idle" ||
    lifecycle === "ready"
  ) {
    return "ready";
  }

  return "unknown";
}

function normalizeAgentStatus(value?: string | null): string {
  return (value ?? "").trim().toLowerCase().replace(/[_\s]+/g, "-");
}

function isStoppedStatus(status: string): boolean {
  return status === "stopped" || status === "archived" || status === "deleted";
}
