import type { HTMLAttributes, ReactNode } from "react";

import { Badge } from "./Badge";

type StatusKind =
  | "agent"
  | "work"
  | "runtime"
  | "stream"
  | "attention"
  | "connection";

interface StatusChipProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: string;
}

interface StatusBadgeProps extends Omit<HTMLAttributes<HTMLSpanElement>, "children"> {
  kind?: StatusKind;
  value?: string | null;
  children?: ReactNode;
}

interface AgentStateBadgeProps extends Omit<StatusBadgeProps, "kind"> {
  lifecycle?: string | null;
  posture?: string | null;
}

function toneToBadge(tone: string) {
  if (tone === "live" || tone === "streaming" || tone === "success" || tone === "ready") return "success";
  if (tone === "connecting" || tone === "syncing" || tone === "running" || tone === "active") return "accent";
  if (tone === "error" || tone === "disconnected" || tone === "stale" || tone === "stopped") return "danger";
  if (tone === "preview" || tone === "waiting" || tone === "needs-input") return "warning";
  if (tone === "muted") return "muted";
  return "neutral";
}

export function StatusChip({ tone = "idle", ...props }: StatusChipProps) {
  return <Badge tone={toneToBadge(tone)} {...props} />;
}

export function StatusBadge({ kind = "runtime", value, children, title, ...props }: StatusBadgeProps) {
  const status = describeStatus(kind, value);
  return (
    <StatusChip tone={status.tone} title={title ?? status.title} {...props}>
      {children ?? status.label}
    </StatusChip>
  );
}

export function AgentStateBadge({ lifecycle, posture, value, ...props }: AgentStateBadgeProps) {
  const state = value ?? lifecycle ?? posture;
  return <StatusBadge kind="agent" value={state} {...props} />;
}

function describeStatus(kind: StatusKind, rawValue?: string | null): { label: string; title: string; tone: string } {
  const value = normalizeStatus(rawValue);
  const label = statusLabel(kind, value);
  return {
    label,
    title: label === value ? value : `${label} · ${value}`,
    tone: statusTone(kind, value),
  };
}

function normalizeStatus(value?: string | null): string {
  return (value ?? "unknown").trim().toLowerCase().replace(/[_\s]+/g, "-") || "unknown";
}

function statusLabel(kind: StatusKind, value: string): string {
  if (kind === "stream" && value === "error") return "Stream error";
  if (value === "asleep" || value === "idle") return "Ready";
  if (value === "in-progress") return "In progress";
  if (value === "needs-input" || value === "input") return "Needs input";
  if (value === "http" || value === "live") return "Live";
  if (value === "fixture" || value === "preview") return "Preview";
  if (value === "syncing") return "Syncing";
  if (value === "running") return "Running";
  if (value === "waiting") return "Waiting";
  if (value === "stale") return "Stale";
  if (value === "stopped") return "Stopped";
  if (value === "disconnected") return "Disconnected";
  return value.replace(/-/g, " ");
}

function statusTone(kind: StatusKind, value: string): string {
  if (kind === "attention") return value === "none" || value === "0" ? "muted" : "needs-input";
  if (value === "streaming" || value === "live" || value === "http" || value === "asleep" || value === "idle" || value === "completed") return "success";
  if (value === "connecting" || value === "syncing" || value === "reconnecting" || value === "recovering" || value === "running" || value === "in-progress") return "running";
  if (value === "waiting" || value === "pending" || value === "needs-input" || value === "preview" || value === "fixture") return "waiting";
  if (value === "error" || value === "stale" || value === "stopped" || value === "disconnected") return "error";
  return "muted";
}
