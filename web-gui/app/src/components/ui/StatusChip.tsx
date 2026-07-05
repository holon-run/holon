import type { TFunction } from "i18next";
import type { HTMLAttributes, ReactNode } from "react";

import { useTranslation } from "react-i18next";
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
  const { t } = useTranslation();
  const status = describeStatus(kind, value, t);
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

function describeStatus(kind: StatusKind, rawValue: string | null | undefined, t: TFunction): { label: string; title: string; tone: string } {
  const value = normalizeStatus(rawValue);
  const label = statusLabel(kind, value, t);
  return {
    label,
    title: label === value || label === prettify(value) ? value : `${label} · ${value}`,
    tone: statusTone(kind, value),
  };
}

function normalizeStatus(value?: string | null): string {
  return (value ?? "unknown").trim().toLowerCase().replace(/[_\s]+/g, "-") || "unknown";
}

function prettify(value: string): string {
  return value.replace(/-/g, " ");
}

function statusLabel(kind: StatusKind, value: string, t: TFunction): string {
  if (kind === "stream" && value === "error") return t("badge.streamError");
  if (value === "asleep" || value === "idle") return t("badge.ready");
  if (value === "in-progress") return t("badge.inProgress");
  if (value === "needs-input" || value === "input") return t("badge.needsInput");
  if (value === "http" || value === "live") return t("badge.live");
  if (value === "fixture" || value === "preview") return t("badge.preview");
  if (value === "syncing") return t("badge.syncing");
  if (value === "running") return t("badge.running");
  if (value === "waiting") return t("badge.waiting");
  if (value === "stale") return t("badge.stale");
  if (value === "stopped") return t("badge.stopped");
  if (value === "disconnected") return t("badge.disconnected");
  // Try the badge namespace for common runtime values (scopes, states, etc.)
  return t(`badge.${value}`, { defaultValue: prettify(value) });
}

function statusTone(kind: StatusKind, value: string): string {
  if (kind === "attention") return value === "none" || value === "0" ? "muted" : "needs-input";
  if (value === "streaming" || value === "live" || value === "http" || value === "asleep" || value === "idle" || value === "completed") return "success";
  if (value === "connecting" || value === "syncing" || value === "reconnecting" || value === "recovering" || value === "running" || value === "in-progress") return "running";
  if (value === "waiting" || value === "pending" || value === "needs-input" || value === "preview" || value === "fixture") return "waiting";
  if (value === "error" || value === "stale" || value === "stopped" || value === "disconnected") return "error";
  return "muted";
}
