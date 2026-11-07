import type { TFunction } from "i18next";
import type { AgentAvatarExpression } from "../lib/agent-avatar";
import type { AgentSummary } from "./types";

export interface AgentDisplayStatus {
  label: string;
  title: string;
  tone: "stopped" | "running" | "needs-input" | "waiting" | "ready" | "muted" | "deleting";
}

export function deriveAgentDisplayStatus(agent: AgentSummary, t: TFunction): AgentDisplayStatus {
  const details = [
    agent.posture ? `${t("statusDetail.posture")}: ${agent.posture}` : undefined,
    agent.postureReason ? `${t("statusDetail.reason")}: ${agent.postureReason}` : undefined,
    agent.lifecycle ? `${t("statusDetail.lifecycle")}: ${agent.lifecycle}` : undefined,
    agent.currentRunId ? `${t("statusDetail.run")}: ${agent.currentRunId}` : undefined,
    agent.pending > 0 ? t("statusDetail.queuedInputs", { count: agent.pending }) : undefined,
    agent.activeTaskCount > 0
      ? t("statusDetail.activeTasks", { count: agent.activeTaskCount })
      : undefined,
    agent.waitingCount > 0 ? t("statusDetail.waitingConditions", { count: agent.waitingCount }) : undefined,
  ].filter(Boolean);
  const title = details.join(" · ") || t("statusDetail.noDetails");
  const tone = deriveAgentStatusTone(agent);
  const labels = {
    deleting: "badge.deleting",
    stopped: "badge.stopped",
    running: "badge.running",
    "needs-input": "badge.needsInput",
    waiting: "badge.waiting",
    ready: "badge.ready",
    muted: "badge.unknown",
  } as const;
  return { label: t(labels[tone]), title, tone };
}

/** Expressions supplement status badges, and never imply activity from stale data. */
export function deriveAgentAvatarExpression(agent: AgentSummary, stateIsCurrent = true): AgentAvatarExpression {
  if (!stateIsCurrent) return "neutral";
  switch (deriveAgentStatusTone(agent)) {
    case "running": return "focused";
    case "needs-input": return "questioning";
    case "waiting": return "waiting";
    case "ready":
      // Background tasks alone do not mean the Agent is waiting for them.
      if (agent.activeTaskCount > 0) return "neutral";
      if (
        normalizeAgentStatus(agent.lifecycle) === "asleep"
        && normalizeAgentStatus(agent.posture) === "idle"
        && agent.pending === 0
      ) return "sleeping";
      return "neutral";
    default: return "neutral";
  }
}

function deriveAgentStatusTone(agent: AgentSummary): AgentDisplayStatus["tone"] {
  const lifecycle = normalizeAgentStatus(agent.lifecycle);
  const posture = normalizeAgentStatus(agent.posture);

  if (lifecycle === "deleting") {
    return "deleting";
  }

  if (isStoppedOrArchived(lifecycle) || isStoppedOrArchived(posture)) {
    return "stopped";
  }

  if (posture === "active-turn" || lifecycle === "awake-running" || lifecycle === "running" || Boolean(agent.currentRunId)) {
    return "running";
  }

  if (posture === "waiting-for-operator") {
    return "needs-input";
  }

  if (posture.startsWith("waiting") || posture === "blocked" || agent.waitingCount > 0) {
    return "waiting";
  }

  if (
    posture === "has-queued-input" ||
    posture === "has-runnable-work" ||
    posture === "idle" ||
    lifecycle === "asleep" ||
    lifecycle === "awake-idle" ||
    lifecycle === "idle" ||
    lifecycle === "ready" ||
    agent.pending > 0
  ) {
    return "ready";
  }

  return "muted";
}

function normalizeAgentStatus(value?: string | null): string {
  return (value ?? "").trim().toLowerCase().replace(/[_\s]+/g, "-");
}

function isStoppedOrArchived(status: string): boolean {
  return status === "stopped" || status === "archived";
}
