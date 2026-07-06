import type { TFunction } from "i18next";
import type { ComponentType, HTMLAttributes, ReactNode } from "react";

import { useTranslation } from "react-i18next";
import {
  Bell,
  Bot,
  Circle,
  CircleAlert,
  CircleCheck,
  CirclePause,
  CircleX,
  Clock,
  Eye,
  FileEdit,
  Folder,
  Globe,
  Info,
  LoaderCircle,
  Lock,
  Plug,
  Radio,
  RefreshCw,
  Square,
  Terminal,
  TriangleAlert,
  User,
  WifiOff,
  type LucideProps,
} from "lucide-react";

import { Badge } from "./Badge";

type IconComponent = ComponentType<LucideProps>;

type StatusKind =
  | "agent"
  | "work"
  | "runtime"
  | "stream"
  | "attention"
  | "connection";

interface IconResult {
  Icon: IconComponent;
  spin: boolean;
}

/** Icon for a binary StatusChip (SettingsPage tone-based badges). */
function toneIcon(tone: string): IconResult {
  if (tone === "success" || tone === "ready" || tone === "live") return { Icon: CircleCheck, spin: false };
  if (tone === "error" || tone === "danger") return { Icon: CircleX, spin: false };
  if (tone === "running" || tone === "accent") return { Icon: LoaderCircle, spin: true };
  if (tone === "waiting" || tone === "warning") return { Icon: Clock, spin: false };
  return { Icon: Info, spin: false };
}

/** Icon for a typed StatusBadge based on kind + normalized value. */
function statusIcon(kind: StatusKind, value: string): IconResult {
  if (kind === "agent") {
    if (value === "asleep" || value === "idle") return { Icon: CircleCheck, spin: false };
    if (value === "running" || value === "in-progress") return { Icon: LoaderCircle, spin: true };
    if (value === "waiting") return { Icon: Clock, spin: false };
    if (value === "needs-input" || value === "input") return { Icon: CircleAlert, spin: false };
    if (value === "stopped") return { Icon: Square, spin: false };
    if (value === "disconnected") return { Icon: WifiOff, spin: false };
    if (value === "stale") return { Icon: TriangleAlert, spin: false };
    if (value === "completed") return { Icon: CircleCheck, spin: false };
  }
  if (kind === "work") {
    if (value === "open") return { Icon: Circle, spin: false };
    if (value === "completed") return { Icon: CircleCheck, spin: false };
    if (value === "cancelled") return { Icon: CircleX, spin: false };
    if (value === "draft") return { Icon: FileEdit, spin: false };
    if (value === "blocked") return { Icon: Lock, spin: false };
    if (value === "yielded") return { Icon: CirclePause, spin: false };
    if (value === "pending") return { Icon: Clock, spin: false };
    if (value === "in-progress") return { Icon: LoaderCircle, spin: true };
  }
  if (kind === "stream") {
    if (value === "streaming" || value === "live") return { Icon: Radio, spin: false };
    if (value === "error" || value === "stale") return { Icon: TriangleAlert, spin: false };
    if (value === "connecting" || value === "reconnecting" || value === "recovering") return { Icon: RefreshCw, spin: true };
  }
  if (kind === "runtime") {
    if (value === "loading" || value === "running" || value === "in-progress") return { Icon: LoaderCircle, spin: true };
    if (value === "waiting") return { Icon: Clock, spin: false };
    if (value === "live") return { Icon: Radio, spin: false };
    if (value === "stopped") return { Icon: Square, spin: false };
    if (value === "error" || value === "disconnected") return { Icon: TriangleAlert, spin: false };
    if (value === "asleep" || value === "idle" || value === "completed") return { Icon: CircleCheck, spin: false };
    if (value === "preview" || value === "fixture") return { Icon: Eye, spin: false };
    if (value === "empty") return { Icon: Circle, spin: false };
  }
  if (kind === "attention") {
    if (value === "none" || value === "0") return { Icon: CircleCheck, spin: false };
    return { Icon: Bell, spin: false };
  }
  if (kind === "connection") {
    if (value === "workspace") return { Icon: Folder, spin: false };
    if (value === "agent") return { Icon: Bot, spin: false };
    if (value === "user-global") return { Icon: Globe, spin: false };
    if (value === "user") return { Icon: User, spin: false };
    if (value === "stdio") return { Icon: Terminal, spin: false };
    if (value === "http") return { Icon: Globe, spin: false };
    if (value === "mcp") return { Icon: Plug, spin: false };
    if (value === "loading" || value === "connecting") return { Icon: LoaderCircle, spin: true };
    if (value === "syncing" || value === "reconnecting" || value === "recovering") return { Icon: RefreshCw, spin: true };
    if (value === "live" || value === "streaming") return { Icon: Radio, spin: false };
    if (value === "preview" || value === "fixture") return { Icon: Eye, spin: false };
    if (value === "enabled") return { Icon: CircleCheck, spin: false };
    if (value === "disabled") return { Icon: CircleX, spin: false };
    if (value === "error") return { Icon: CircleAlert, spin: false };
  }
  return { Icon: Info, spin: false };
}

interface StatusChipProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: string;
  iconOnly?: boolean;
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

function StatusBadgeIcon({ kind, value }: { kind: StatusKind; value: string }) {
  const { Icon, spin } = statusIcon(kind, value);
  return <Icon size={13} className={spin ? "animate-spin" : undefined} />;
}

export function StatusChip({ tone = "idle", iconOnly, children, title, ...props }: StatusChipProps) {
  if (iconOnly) {
    const { Icon, spin } = toneIcon(tone);
    const resolvedTitle = title ?? (typeof children === "string" ? children : undefined) ?? undefined;
    return (
      <Badge tone={toneToBadge(tone)} title={resolvedTitle} data-tooltip={resolvedTitle} {...props}>
        <Icon size={13} className={spin ? "animate-spin" : undefined} />
      </Badge>
    );
  }
  return <Badge tone={toneToBadge(tone)} {...props} title={title} data-tooltip={title}>{children}</Badge>;
}

export function StatusBadge({ kind = "runtime", value, children, title, ...props }: StatusBadgeProps) {
  const { t } = useTranslation();
  const status = describeStatus(kind, value, t);
  const normalizedValue = normalizeStatus(value);
  return (
    <StatusChip tone={status.tone} title={title ?? status.title} {...props}>
      {children ?? <StatusBadgeIcon kind={kind} value={normalizedValue} />}
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
  const badgeKey = `badge.${value}`;
  const translated = t(badgeKey);
  if (translated !== badgeKey) return translated;
  return prettify(value);
}

function statusTone(kind: StatusKind, value: string): string {
  if (kind === "attention") return value === "none" || value === "0" ? "muted" : "needs-input";
  if (value === "streaming" || value === "live" || value === "http" || value === "asleep" || value === "idle" || value === "completed") return "success";
  if (value === "connecting" || value === "syncing" || value === "reconnecting" || value === "recovering" || value === "running" || value === "in-progress") return "running";
  if (value === "waiting" || value === "pending" || value === "needs-input" || value === "preview" || value === "fixture") return "waiting";
  if (value === "error" || value === "stale" || value === "stopped" || value === "disconnected") return "error";
  return "muted";
}
