import type { TFunction } from "i18next";
import type { AgentSummary } from "./types";

export interface AgentDisplayStatus {
  label: string;
  title: string;
  tone: "stopped" | "running" | "needs-input" | "waiting" | "ready" | "muted";
}

export function deriveAgentDisplayStatus(agent: AgentSummary, t: TFunction): AgentDisplayStatus {
  const lifecycle = normalizeAgentStatus(agent.lifecycle);
  const posture = normalizeAgentStatus(agent.posture);
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

  if (isStoppedOrArchived(lifecycle) || isStoppedOrArchived(posture)) {
    return { label: t("badge.stopped"), title, tone: "stopped" };
  }

  if (posture === "active-turn" || lifecycle === "awake-running" || lifecycle === "running" || Boolean(agent.currentRunId)) {
    return { label: t("badge.running"), title, tone: "running" };
  }

  if (posture === "waiting-for-operator") {
    return { label: t("badge.needsInput"), title, tone: "needs-input" };
  }

  if (posture.startsWith("waiting") || posture === "blocked" || agent.waitingCount > 0) {
    return { label: t("badge.waiting"), title, tone: "waiting" };
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
    return { label: t("badge.ready"), title, tone: "ready" };
  }

  return { label: t("badge.unknown"), title, tone: "muted" };
}

function normalizeAgentStatus(value?: string | null): string {
  return (value ?? "").trim().toLowerCase().replace(/[_\s]+/g, "-");
}

function isStoppedOrArchived(status: string): boolean {
  return status === "stopped" || status === "archived";
}
