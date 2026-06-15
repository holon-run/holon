import type { AgentSummary } from "./types";

export interface AgentDisplayStatus {
  label: "Stopped" | "Running" | "Waiting for input" | "Waiting" | "Ready" | "Unknown";
  title: string;
  tone: "stopped" | "running" | "needs-input" | "waiting" | "ready" | "muted";
}

export function deriveAgentDisplayStatus(agent: AgentSummary): AgentDisplayStatus {
  const lifecycle = normalizeAgentStatus(agent.lifecycle);
  const posture = normalizeAgentStatus(agent.posture);
  const details = [
    agent.posture ? `posture: ${agent.posture}` : undefined,
    agent.postureReason ? `reason: ${agent.postureReason}` : undefined,
    agent.lifecycle ? `lifecycle: ${agent.lifecycle}` : undefined,
    agent.currentRunId ? `run: ${agent.currentRunId}` : undefined,
    agent.pending > 0 ? `${agent.pending} queued input${agent.pending === 1 ? "" : "s"}` : undefined,
    agent.activeTaskCount > 0
      ? `${agent.activeTaskCount} active task${agent.activeTaskCount === 1 ? "" : "s"}`
      : undefined,
    agent.waitingCount > 0 ? `${agent.waitingCount} waiting condition${agent.waitingCount === 1 ? "" : "s"}` : undefined,
  ].filter(Boolean);
  const title = details.join(" · ") || "No status details";

  if (isStoppedOrArchived(lifecycle) || isStoppedOrArchived(posture)) {
    return { label: "Stopped", title, tone: "stopped" };
  }

  if (posture === "active-turn" || lifecycle === "awake-running" || lifecycle === "running" || Boolean(agent.currentRunId)) {
    return { label: "Running", title, tone: "running" };
  }

  if (posture === "waiting-for-operator") {
    return { label: "Waiting for input", title, tone: "needs-input" };
  }

  if (posture.startsWith("waiting") || posture === "blocked" || agent.waitingCount > 0) {
    return { label: "Waiting", title, tone: "waiting" };
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
    return { label: "Ready", title, tone: "ready" };
  }

  return { label: "Unknown", title, tone: "muted" };
}

function normalizeAgentStatus(value?: string | null): string {
  return (value ?? "").trim().toLowerCase().replace(/[_\s]+/g, "-");
}

function isStoppedOrArchived(status: string): boolean {
  return status === "stopped" || status === "archived";
}
