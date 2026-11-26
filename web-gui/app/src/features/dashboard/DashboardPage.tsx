import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { ArrowRight, Bell, ChevronDown, ChevronRight, FolderGit2, LayoutGrid, List, Search, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { PageHeading } from "../../components/ui/PageHeading";
import { AgentAvatar } from "../../components/ui/AgentAvatar";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { Skeleton } from "../../components/ui/Skeleton";
import { AgentStatusIndicator } from "../../components/ui/StatusChip";
import { StatusBadge } from "../../components/ui/StatusChip";
import { ROSTER_STALE_EXTENDED_RETRY_ATTEMPTS } from "../../runtime/global-sync-coordinator";
import { deriveAgentAvatarExpression, deriveAgentDisplayStatus } from "../../runtime/agent-status";
import type { NotificationKind, RuntimeNotification } from "../../runtime/notification-sync";
import { useRuntimeNotifications } from "../../runtime/runtime-notifications";
import type { RosterDiscoveryState } from "../../runtime/types";
import {
  ALL_WORKSPACES_FILTER,
  STANDALONE_WORKSPACE_KEY,
  agentMatchesWorkspaceFilter,
  buildWorkspaceFilterOptions,
  resolvePrimaryWorkspace,
  type PrimaryWorkspaceAssignment,
} from "../../lib/agent-workspaces";
import { compactModelRouteDisplay } from "../../lib/model-route-ref";
import type { AgentSummary, DashboardMetric, RuntimeConnection } from "../../runtime/types";

type DashboardFilter = "all" | "attention" | "active";
type DashboardSort = "activity" | "name";
type DashboardViewMode = "cards" | "compact";
type DashboardInboxTab = "pending" | "recent";

const VIEW_MODE_STORAGE_KEY = "holon.webGui.dashboardViewMode.v1";
const GROUP_WORKSPACE_STORAGE_KEY = "holon.webGui.dashboardGroupByWorkspace.v1";

const IDLE_BOILERPLATE_SUMMARIES = new Set([
  "no queued input, active turn, runnable work, or active waits",
  "posture unavailable",
  "idle",
]);

function readStoredViewMode(): DashboardViewMode {
  if (typeof window === "undefined") return "cards";
  try {
    return window.localStorage.getItem(VIEW_MODE_STORAGE_KEY) === "compact" ? "compact" : "cards";
  } catch {
    return "cards";
  }
}

function writeStoredViewMode(mode: DashboardViewMode): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(VIEW_MODE_STORAGE_KEY, mode);
  } catch {
    // Ignore storage write errors.
  }
}

function readStoredGroupByWorkspace(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(GROUP_WORKSPACE_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

function writeStoredGroupByWorkspace(enabled: boolean): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(GROUP_WORKSPACE_STORAGE_KEY, enabled ? "true" : "false");
  } catch {
    // Ignore storage write errors.
  }
}

export function visibleAgentFocusSummary(agent: AgentSummary): string | undefined {
  const summary = agent.focusSummary?.trim();
  if (!summary) return undefined;
  if (IDLE_BOILERPLATE_SUMMARIES.has(summary.toLowerCase())) return undefined;
  if (agent.currentWork?.objective && summary === agent.currentWork.objective.trim()) return undefined;
  return summary;
}

export function agentHasActiveWork(agent: AgentSummary, statusKind: string): boolean {
  return Boolean(agent.currentWork) || agent.activeTaskCount > 0 || statusKind === "running";
}

export function agentNeedsDashboardAttention(pendingReminderCount: number): boolean {
  return pendingReminderCount > 0;
}

interface DashboardInboxItem {
  key: string;
  agentId: string;
  agent?: AgentSummary;
  agentLabel: string;
  workItemId?: string | null;
  title: string;
  summary?: string;
  createdAt?: string;
  kind: NotificationKind;
  kindLabel: string;
  pillToneClass: string;
  notificationId?: string;
}

interface DashboardPageProps {
  agents: AgentSummary[];
  metrics: DashboardMetric[];
  pendingNotifications?: RuntimeNotification[];
  recentNotifications?: RuntimeNotification[];
  unreadByAgentId?: Record<string, number>;
  onIgnoreNotification?: (id: string) => void;
  onIgnoreAllNotifications?: () => void;
  onOpenNotificationTarget?: (target: { agent_id: string; work_item_id?: string | null }) => void;
  connection: RuntimeConnection;
  discovery?: RosterDiscoveryState;
  loading: boolean;
  onRefresh: () => void;
  onOpenAgent: (agentId: string) => void;
}

export function DashboardPage({
  agents,
  metrics,
  pendingNotifications = [],
  recentNotifications,
  unreadByAgentId = {},
  onIgnoreNotification,
  onIgnoreAllNotifications,
  onOpenNotificationTarget,
  connection,
  discovery,
  loading,
  onRefresh,
  onOpenAgent,
}: DashboardPageProps) {
  const { t, i18n } = useTranslation();
  const storeRecentNotifications = useRuntimeNotifications((state) => state.recent);
  const effectiveRecentNotifications = recentNotifications ?? storeRecentNotifications;

  const [filter, setFilter] = useState<DashboardFilter>("all");
  const [workspaceFilter, setWorkspaceFilter] = useState<string>(ALL_WORKSPACES_FILTER);
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<DashboardSort>("activity");
  const [viewMode, setViewMode] = useState<DashboardViewMode>(readStoredViewMode);
  const [groupByWorkspace, setGroupByWorkspace] = useState<boolean>(readStoredGroupByWorkspace);
  const [collapsedGroups, setCollapsedGroups] = useState<Record<string, boolean>>({});
  const [inboxTab, setInboxTab] = useState<DashboardInboxTab>(() =>
    pendingNotifications.length > 0 ? "pending" : "recent",
  );
  const [idleInboxExpanded, setIdleInboxExpanded] = useState(false);
  const prevPendingCountRef = useRef(pendingNotifications.length);

  useEffect(() => {
    if (pendingNotifications.length > prevPendingCountRef.current) {
      setInboxTab("pending");
    }
    prevPendingCountRef.current = pendingNotifications.length;
  }, [pendingNotifications.length]);

  const hasAgents = agents.length > 0;
  const hasConnectionError = Boolean(connection.error);
  const isBootstrapping = loading && !hasAgents;
  const isRefreshing = loading && hasAgents;
  const isPreview = connection.source === "fixture";
  const stateIsCurrent = !connection.error && discovery?.freshness === "fresh";
  const dashboardState = getDashboardState({ isBootstrapping, hasAgents, hasConnectionError, isPreview });

  const standaloneLabel = t("dashboard.standaloneWorkspace");
  const workspaceOptions = useMemo(
    () => buildWorkspaceFilterOptions(agents, standaloneLabel),
    [agents, standaloneLabel],
  );

  useEffect(() => {
    if (workspaceFilter === ALL_WORKSPACES_FILTER) return;
    if (!workspaceOptions.some((opt) => opt.key === workspaceFilter)) {
      setWorkspaceFilter(ALL_WORKSPACES_FILTER);
    }
  }, [workspaceFilter, workspaceOptions]);

  const pendingByAgentId = useMemo(() => {
    const map = new Map<string, RuntimeNotification[]>();
    for (const item of pendingNotifications) {
      const list = map.get(item.target.agent_id);
      if (list) list.push(item);
      else map.set(item.target.agent_id, [item]);
    }
    return map;
  }, [pendingNotifications]);

  const agentsById = useMemo(
    () => new Map(agents.map((agent) => [agent.id, agent])),
    [agents],
  );

  // Runtime status and card heights remain decoupled from async `/notifications/pending`:
  // notifications are surfaced in the dedicated Inbox Panel above (and used only when
  // the operator explicitly filters by `Needs attention`).
  const enrichedAgents = useMemo(
    () =>
      agents.map((agent) => {
        const reminders = pendingByAgentId.get(agent.id) ?? [];
        const status = deriveAgentDisplayStatus(agent, t, stateIsCurrent);
        const unreadCount = unreadByAgentId[agent.id] ?? 0;
        const primaryWorkspace: PrimaryWorkspaceAssignment = resolvePrimaryWorkspace(agent, standaloneLabel);
        const rawWorkspaceLabel = agent.workspaceSummary?.name || agent.workspace;
        const workspaceLabel = !primaryWorkspace.isStandalone
          ? primaryWorkspace.label
          : rawWorkspaceLabel;
        const modelLabel = compactModelRouteDisplay(agent.model);
        const needsAttention = agentNeedsDashboardAttention(reminders.length);
        const activeWork = agentHasActiveWork(agent, status.kind);
        return {
          agent,
          status,
          reminders,
          unreadCount,
          workspaceLabel,
          primaryWorkspace,
          modelLabel,
          needsAttention,
          activeWork,
        };
      }),
    [agents, pendingByAgentId, standaloneLabel, stateIsCurrent, t, unreadByAgentId],
  );

  const triageItems = useMemo<DashboardInboxItem[]>(
    () =>
      pendingNotifications.map((notification) =>
        projectInboxItem(notification, agentsById, t, true),
      ),
    [agentsById, pendingNotifications, t],
  );

  const recentItems = useMemo<DashboardInboxItem[]>(
    () =>
      effectiveRecentNotifications.map((notification) =>
        projectInboxItem(notification, agentsById, t, false),
      ),
    [agentsById, effectiveRecentNotifications, t],
  );

  const hasPendingItems = triageItems.length > 0;
  const hasRecentItems = recentItems.length > 0;
  const showInboxPanel = hasAgents && (hasPendingItems || hasRecentItems);
  const resolvedInboxTab: DashboardInboxTab =
    inboxTab === "recent" && hasRecentItems
      ? "recent"
      : hasPendingItems
        ? "pending"
        : "recent";
  const isInboxExpanded = hasPendingItems || idleInboxExpanded;
  const activeInboxItems = resolvedInboxTab === "pending" ? triageItems : recentItems;

  const attentionRosterCount = useMemo(
    () => enrichedAgents.filter((entry) => entry.needsAttention).length,
    [enrichedAgents],
  );
  const activeWorkRosterCount = useMemo(
    () => enrichedAgents.filter((entry) => entry.activeWork).length,
    [enrichedAgents],
  );

  // `agents` arrives pre-sorted by `sortAgentsByRosterActivity` (stopped last ->
  // recent operator/brief activity -> ID) using synchronously stored `rosterActivityByAgentId`.
  // Keeping that native order in `"activity"` prevents async notification/unread hydration from
  // shuffling cards under the user's cursor.
  const filteredAgents = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    const filtered = enrichedAgents.filter((entry) => {
      if (filter === "attention" && !entry.needsAttention) return false;
      if (filter === "active" && !entry.activeWork) return false;
      if (!agentMatchesWorkspaceFilter(entry.agent, workspaceFilter)) return false;
      if (!normalizedQuery) return true;
      const haystack = [
        entry.agent.name,
        entry.agent.id,
        entry.agent.currentWork?.objective,
        entry.workspaceLabel,
        ...entry.primaryWorkspace.allExternal.map((w) => w.label),
        entry.modelLabel,
        entry.status.label,
      ]
        .filter(Boolean)
        .join(" ")
        .toLowerCase();
      return haystack.includes(normalizedQuery);
    });

    if (sort === "activity") {
      return filtered;
    }

    return [...filtered].sort((left, right) => {
      const leftLabel = (left.agent.name ?? left.agent.id).toLowerCase();
      const rightLabel = (right.agent.name ?? right.agent.id).toLowerCase();
      return leftLabel.localeCompare(rightLabel);
    });
  }, [enrichedAgents, filter, query, sort, workspaceFilter]);

  const workspaceGroups = useMemo(() => {
    if (!groupByWorkspace || agents.length <= 1) return null;
    const map = new Map<
      string,
      {
        key: string;
        label: string;
        anchor?: string;
        isStandalone: boolean;
        entries: typeof filteredAgents;
      }
    >();
    for (const entry of filteredAgents) {
      const { key, label, anchor, isStandalone } = entry.primaryWorkspace;
      const existing = map.get(key);
      if (existing) {
        existing.entries.push(entry);
      } else {
        map.set(key, {
          key,
          label,
          anchor,
          isStandalone,
          entries: [entry],
        });
      }
    }
    return [...map.values()].sort((left, right) => {
      if (left.isStandalone !== right.isStandalone) return left.isStandalone ? 1 : -1;
      if (left.entries.length !== right.entries.length) return right.entries.length - left.entries.length;
      return left.label.localeCompare(right.label);
    });
  }, [agents.length, filteredAgents, groupByWorkspace]);

  function handleCardClick(event: MouseEvent<HTMLElement>, agentId: string) {
    const target = event.target as HTMLElement | null;
    if (target?.closest("button, a, input, select")) return;
    onOpenAgent(agentId);
  }

  function handleCardKeyDown(event: KeyboardEvent<HTMLElement>, agentId: string) {
    if (event.target !== event.currentTarget) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onOpenAgent(agentId);
    }
  }

  function formatFooterMeta(entry: (typeof enrichedAgents)[number]): string {
    const parts: string[] = [];
    if (entry.agent.activeTaskCount > 0) {
      parts.push(t("dashboard.activeTaskCount", { count: entry.agent.activeTaskCount }));
    }
    if ((entry.agent.activeTimerCount ?? 0) > 0) {
      parts.push(t("dashboard.timerCount", { count: entry.agent.activeTimerCount }));
    }
    if (entry.agent.lastTurnTime && entry.agent.lastTurnTime !== "—") {
      parts.push(entry.agent.lastTurnTime);
    }
    if (parts.length === 0) {
      parts.push(entry.status.label);
    }
    return parts.join(" · ");
  }

  function formatWorkspaceTooltip(entry: (typeof enrichedAgents)[number], displayWorkspace: string): string {
    if (entry.primaryWorkspace.allExternal.length > 1) {
      return t("dashboard.mountedWorkspaces", {
        list: entry.primaryWorkspace.allExternal.map((w) => w.label).join(", "),
      });
    }
    return entry.primaryWorkspace.anchor ?? entry.agent.workspaceSummary?.anchor ?? displayWorkspace;
  }

  function renderEntries(entries: typeof filteredAgents) {
    if (viewMode === "compact" && agents.length > 1) {
      return (
        <div className="agent-compact-list" role="list">
          {entries.map((entry) => {
            const { agent, status, unreadCount, workspaceLabel, primaryWorkspace, modelLabel } = entry;
            const displayWorkspace =
              !workspaceLabel || workspaceLabel === "not bound" ? t("dashboard.notBound") : workspaceLabel;
            const workText = agent.currentWork?.objective ?? visibleAgentFocusSummary(agent);
            return (
              <div
                role="listitem"
                tabIndex={0}
                key={agent.id}
                className="agent-compact-row"
                onClick={(event) => handleCardClick(event, agent.id)}
                onKeyDown={(event) => handleCardKeyDown(event, agent.id)}
              >
                <AgentAvatar
                  agentId={agent.id}
                  expression={deriveAgentAvatarExpression(agent, stateIsCurrent)}
                />
                <div className="agent-compact-identity">
                  <strong>{agent.name ?? agent.id}</strong>
                  {agent.name && agent.name !== agent.id ? <small>{agent.id}</small> : null}
                </div>
                <div className="agent-compact-work" title={workText ?? status.label}>
                  <span>{workText ?? status.label}</span>
                </div>
                <div className="agent-compact-meta" title={formatWorkspaceTooltip(entry, displayWorkspace)}>
                  <span>{displayWorkspace}</span>
                  {primaryWorkspace.extraCount > 0 ? (
                    <span className="workspace-extra-pill">
                      {t("dashboard.extraWorkspaces", { count: primaryWorkspace.extraCount })}
                    </span>
                  ) : null}
                </div>
                <div className="agent-compact-meta" title={modelLabel}>
                  {modelLabel}
                </div>
                <div className="agent-compact-actions">
                  {unreadCount > 0 ? <span className="agent-row-unread">{unreadCount > 99 ? "99+" : unreadCount}</span> : null}
                  <AgentStatusIndicator className="state-chip" status={status} />
                  <Button
                    size="icon"
                    variant="ghost"
                    aria-label={t("dashboard.openAgent", { id: agent.id })}
                    onClick={() => onOpenAgent(agent.id)}
                  >
                    <ArrowRight size={15} />
                  </Button>
                </div>
              </div>
            );
          })}
        </div>
      );
    }

    return (
      <div className="agent-roster">
        {entries.map((entry) => {
          const { agent, status, unreadCount, workspaceLabel, primaryWorkspace, modelLabel } = entry;
          const focusSummary = visibleAgentFocusSummary(agent);
          const subtitle = agent.name && agent.name !== agent.id ? agent.id : undefined;
          const displayWorkspace =
            !workspaceLabel || workspaceLabel === "not bound" ? t("dashboard.notBound") : workspaceLabel;
          return (
            <Card
              className="agent-card agent-card-interactive"
              key={agent.id}
              tabIndex={0}
              onClick={(event) => handleCardClick(event, agent.id)}
              onKeyDown={(event) => handleCardKeyDown(event, agent.id)}
            >
              <div className="agent-card-head">
                <AgentAvatar
                  agentId={agent.id}
                  expression={deriveAgentAvatarExpression(agent, stateIsCurrent)}
                />
                <div className="agent-card-title" title={subtitle ? `${agent.name} (${subtitle})` : agent.id}>
                  <strong>{agent.name ?? agent.id}</strong>
                  {subtitle ? <small>{subtitle}</small> : null}
                </div>
                <div className="agent-card-status">
                  {unreadCount > 0 ? (
                    <span className="agent-row-unread" title={t("app.unreadUpdates", { count: unreadCount })}>
                      {unreadCount > 99 ? "99+" : unreadCount}
                    </span>
                  ) : null}
                  <AgentStatusIndicator className="state-chip" status={status} showLabel={status.kind !== "ready"} />
                </div>
              </div>
              {focusSummary ? <p className="agent-card-summary">{focusSummary}</p> : null}
              {agent.currentWork ? (
                <section className="agent-card-work" title={agent.currentWork.objective}>
                  <span>{t("dashboard.currentWork")}</span>
                  <strong>{agent.currentWork.objective}</strong>
                </section>
              ) : null}
              <dl className="agent-detail-list agent-detail-list-two">
                <div title={formatWorkspaceTooltip(entry, displayWorkspace)}>
                  <dt>{t("dashboard.workspace")}</dt>
                  <dd className="agent-workspace-dd">
                    <span>{displayWorkspace}</span>
                    {primaryWorkspace.extraCount > 0 ? (
                      <span className="workspace-extra-pill">
                        {t("dashboard.extraWorkspaces", { count: primaryWorkspace.extraCount })}
                      </span>
                    ) : null}
                  </dd>
                </div>
                <div title={modelLabel}>
                  <dt>{t("dashboard.model")}</dt>
                  <dd>{modelLabel}</dd>
                </div>
              </dl>
              <footer>
                <span>{formatFooterMeta(entry)}</span>
                <Button size="icon" variant="secondary" aria-label={t("dashboard.openAgent", { id: agent.id })} onClick={() => onOpenAgent(agent.id)}>
                  <ArrowRight size={16} />
                </Button>
              </footer>
            </Card>
          );
        })}
      </div>
    );
  }

  return (
    <section className="page dashboard-page workspace-page" aria-label={t("dashboard.dashboardAria")}>
      <div className="page-inner dashboard-inner workspace-page-content">
        <section className="dashboard-section dashboard-roster-section">
          <PageHeading
            title={t("dashboard.agents")}
            description={t("dashboard.available", { count: agents.length })}
            actions={<>
              <StatusBadge className={`connection-pill ${connection.source}`} kind="connection" value={connection.source} showLabel />
              <Button type="button" variant="outline" disabled={loading} onClick={onRefresh}>
                {loading ? t("common.refreshing") : t("common.refresh")}
              </Button>
            </>}
          />

          {discovery?.freshness === "stale" ? (
            <div className="roster-discovery-banner stale" role="status">
              <strong>{t("dashboard.rosterStaleTitle")}</strong>
              <span>
                {discovery.retryAttempt >= ROSTER_STALE_EXTENDED_RETRY_ATTEMPTS
                  ? t("dashboard.rosterStaleExtendedBody", { attempt: discovery.retryAttempt })
                  : t("dashboard.rosterStaleBody")}
              </span>
            </div>
          ) : null}
          {discovery?.freshness === "unauthorized" ? (
            <div className="roster-discovery-banner unauthorized" role="alert">
              <strong>{t("dashboard.rosterUnauthorizedTitle")}</strong>
              <span>{t("dashboard.rosterUnauthorizedBody")}</span>
            </div>
          ) : null}
          {dashboardState ? <DashboardStateCard state={dashboardState} detail={connection.error ?? connection.summary} /> : null}

          {metrics.length > 0 ? (
            <div className="metric-strip" aria-label={t("dashboard.runtimeMetrics")}>
              {metrics.map((metric) => {
                const clickableFilter: DashboardFilter | undefined =
                  metric.label === "dashboard.agents"
                    ? "all"
                    : metric.label === "dashboard.needsAttention"
                      ? "attention"
                      : metric.label === "dashboard.currentWork" || metric.label === "dashboard.activeTasks"
                        ? "active"
                        : undefined;
                const isSelectedMetric = clickableFilter !== undefined && filter === clickableFilter && clickableFilter !== "all";
                return clickableFilter && agents.length > 1 ? (
                  <button
                    type="button"
                    className={`metric-card metric-card-button ${metric.tone ?? "default"}${isSelectedMetric ? " is-active" : ""}`}
                    key={metric.label}
                    aria-pressed={isSelectedMetric}
                    onClick={() => setFilter((current) => (current === clickableFilter ? "all" : clickableFilter))}
                  >
                    <span>{t(metric.label)}</span>
                    <strong>{metric.value}</strong>
                  </button>
                ) : (
                  <div className={`metric-card ${metric.tone ?? "default"}`} key={metric.label}>
                    <span>{t(metric.label)}</span>
                    <strong>{metric.value}</strong>
                  </div>
                );
              })}
            </div>
          ) : null}

          {showInboxPanel ? (
            <section
              className={`dashboard-triage-panel${hasPendingItems ? "" : " is-calm"}${isInboxExpanded ? "" : " is-collapsed"}`}
              aria-label={hasPendingItems ? t("dashboard.triageTitle") : t("notifications.title")}
            >
              <header className="dashboard-triage-head">
                <div className="dashboard-triage-heading">
                  <span className="dashboard-triage-icon" aria-hidden="true">
                    <Bell size={15} />
                  </span>
                  <div className="dashboard-triage-tabs" role="tablist" aria-label={t("notifications.title")}>
                    {hasPendingItems || !hasRecentItems ? (
                      <button
                        type="button"
                        role="tab"
                        aria-selected={isInboxExpanded && resolvedInboxTab === "pending"}
                        className={`dashboard-triage-tab${isInboxExpanded && resolvedInboxTab === "pending" ? " is-active" : ""}`}
                        onClick={() => {
                          setInboxTab("pending");
                          setIdleInboxExpanded(true);
                        }}
                      >
                        <span>{t("dashboard.triageTitle")}</span>
                        <span className={`dashboard-triage-tab-count${hasPendingItems ? " is-attention" : ""}`}>
                          {triageItems.length}
                        </span>
                      </button>
                    ) : null}
                    {hasRecentItems ? (
                      <button
                        type="button"
                        role="tab"
                        aria-selected={isInboxExpanded && resolvedInboxTab === "recent"}
                        className={`dashboard-triage-tab${isInboxExpanded && resolvedInboxTab === "recent" ? " is-active" : ""}`}
                        onClick={() => {
                          if (!hasPendingItems && resolvedInboxTab === "recent") {
                            setIdleInboxExpanded((open) => !open);
                          } else {
                            setInboxTab("recent");
                            setIdleInboxExpanded(true);
                          }
                        }}
                      >
                        <span>{t("notifications.recent")}</span>
                        <span className="dashboard-triage-tab-count">{recentItems.length}</span>
                      </button>
                    ) : null}
                  </div>
                </div>
                <div className="dashboard-triage-head-actions">
                  {isInboxExpanded && resolvedInboxTab === "pending" && pendingNotifications.length > 1 && onIgnoreAllNotifications ? (
                    <Button type="button" variant="ghost" size="sm" onClick={onIgnoreAllNotifications}>
                      {t("notifications.ignoreAll")}
                    </Button>
                  ) : null}
                  {!hasPendingItems && hasRecentItems ? (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => setIdleInboxExpanded((open) => !open)}
                    >
                      {isInboxExpanded ? t("notifications.collapse") : t("notifications.expand")}
                    </Button>
                  ) : null}
                </div>
              </header>
              {isInboxExpanded ? (
                activeInboxItems.length > 0 ? (
                  <div className="dashboard-triage-list" role="list">
                    {activeInboxItems.map((item) => {
                      const openTarget = () => {
                        if (onOpenNotificationTarget && item.workItemId) {
                          onOpenNotificationTarget({ agent_id: item.agentId, work_item_id: item.workItemId });
                        } else {
                          onOpenAgent(item.agentId);
                        }
                      };
                      const createdDate = item.createdAt ? new Date(item.createdAt) : null;
                      return (
                        <div className="dashboard-triage-row" role="listitem" key={item.key}>
                          <button type="button" className="dashboard-triage-main" onClick={openTarget}>
                            <AgentAvatar
                              agentId={item.agentId}
                              expression={item.agent ? deriveAgentAvatarExpression(item.agent, stateIsCurrent) : "neutral"}
                            />
                            <div className="dashboard-triage-copy">
                              <div className="dashboard-triage-meta">
                                <strong>{item.agentLabel}</strong>
                                {item.agent?.name && item.agent.name !== item.agentId ? (
                                  <small>{item.agentId}</small>
                                ) : null}
                                <span className={`dashboard-triage-pill${item.pillToneClass}`}>{item.kindLabel}</span>
                                {createdDate && Number.isFinite(createdDate.getTime()) ? (
                                  <time dateTime={item.createdAt}>
                                    {createdDate.toLocaleString(i18n.resolvedLanguage, {
                                      month: "short",
                                      day: "numeric",
                                      hour: "2-digit",
                                      minute: "2-digit",
                                    })}
                                  </time>
                                ) : null}
                              </div>
                              <div className="dashboard-triage-title" title={item.title}>
                                {item.title}
                              </div>
                              {item.summary ? (
                                <p className="dashboard-triage-summary" title={item.summary}>
                                  {item.summary}
                                </p>
                              ) : null}
                            </div>
                          </button>
                          <div className="dashboard-triage-actions">
                            <Button type="button" variant="secondary" size="sm" onClick={openTarget}>
                              {item.workItemId ? t("dashboard.triageOpenWorkItem") : t("dashboard.triageOpen")}
                            </Button>
                            {item.notificationId && onIgnoreNotification ? (
                              <button
                                type="button"
                                className="agent-card-reminder-ignore"
                                aria-label={t("notifications.ignoreItem")}
                                title={t("notifications.ignore")}
                                onClick={() => onIgnoreNotification(item.notificationId!)}
                              >
                                <X size={14} aria-hidden="true" />
                              </button>
                            ) : null}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                ) : (
                  <p className="dashboard-triage-empty">{t("notifications.emptyPending")}</p>
                )
              ) : null}
            </section>
          ) : null}

          {hasAgents && agents.length > 1 ? (
            <div className="dashboard-toolbar" role="region" aria-label={t("dashboard.roster")}>
              <div className="dashboard-filter-pills" role="group" aria-label={t("common.status")}>
                <button
                  type="button"
                  className={`dashboard-filter-pill${filter === "all" ? " is-active" : ""}`}
                  aria-pressed={filter === "all"}
                  onClick={() => setFilter("all")}
                >
                  <span>{t("dashboard.filterAll")}</span>
                  <strong>{agents.length}</strong>
                </button>
                <button
                  type="button"
                  className={`dashboard-filter-pill${filter === "attention" ? " is-active" : ""}${attentionRosterCount > 0 ? " has-attention" : ""}`}
                  aria-pressed={filter === "attention"}
                  onClick={() => setFilter((current) => (current === "attention" ? "all" : "attention"))}
                >
                  <span>{t("dashboard.filterAttention")}</span>
                  <strong>{attentionRosterCount}</strong>
                </button>
                <button
                  type="button"
                  className={`dashboard-filter-pill${filter === "active" ? " is-active" : ""}`}
                  aria-pressed={filter === "active"}
                  onClick={() => setFilter((current) => (current === "active" ? "all" : "active"))}
                >
                  <span>{t("dashboard.filterActiveWork")}</span>
                  <strong>{activeWorkRosterCount}</strong>
                </button>
              </div>
              <div className="dashboard-toolbar-controls">
                <div className="dashboard-search-box">
                  <Search size={14} aria-hidden="true" />
                  <input
                    type="search"
                    value={query}
                    aria-label={t("dashboard.searchPlaceholder")}
                    placeholder={t("dashboard.searchPlaceholder")}
                    onChange={(event) => setQuery(event.target.value)}
                  />
                </div>
                {workspaceOptions.length > 1 ? (
                  <select
                    className="dashboard-sort-select dashboard-workspace-select"
                    aria-label={t("dashboard.filterByWorkspace")}
                    value={workspaceFilter}
                    onChange={(event) => setWorkspaceFilter(event.target.value)}
                  >
                    <option value={ALL_WORKSPACES_FILTER}>{t("dashboard.allWorkspaces")}</option>
                    {workspaceOptions.map((opt) => (
                      <option key={opt.key} value={opt.key}>
                        {opt.label} ({opt.count})
                      </option>
                    ))}
                  </select>
                ) : null}
                <select
                  className="dashboard-sort-select"
                  aria-label={t("dashboard.sortPriority")}
                  value={sort}
                  onChange={(event) => setSort(event.target.value as DashboardSort)}
                >
                  <option value="activity">{t("dashboard.sortPriority")}</option>
                  <option value="name">{t("dashboard.sortName")}</option>
                </select>
                {workspaceOptions.length > 1 ? (
                  <button
                    type="button"
                    className={`dashboard-group-toggle${groupByWorkspace ? " is-active" : ""}`}
                    aria-label={t("dashboard.groupByWorkspace")}
                    aria-pressed={groupByWorkspace}
                    title={t("dashboard.groupByWorkspace")}
                    onClick={() => {
                      const next = !groupByWorkspace;
                      setGroupByWorkspace(next);
                      writeStoredGroupByWorkspace(next);
                    }}
                  >
                    <FolderGit2 size={15} aria-hidden="true" />
                  </button>
                ) : null}
                <div className="dashboard-view-toggle" role="group" aria-label={t("dashboard.viewCards")}>
                  <button
                    type="button"
                    className={viewMode === "cards" ? "is-active" : ""}
                    aria-label={t("dashboard.viewCards")}
                    aria-pressed={viewMode === "cards"}
                    title={t("dashboard.viewCards")}
                    onClick={() => {
                      setViewMode("cards");
                      writeStoredViewMode("cards");
                    }}
                  >
                    <LayoutGrid size={15} aria-hidden="true" />
                  </button>
                  <button
                    type="button"
                    className={viewMode === "compact" ? "is-active" : ""}
                    aria-label={t("dashboard.viewCompact")}
                    aria-pressed={viewMode === "compact"}
                    title={t("dashboard.viewCompact")}
                    onClick={() => {
                      setViewMode("compact");
                      writeStoredViewMode("compact");
                    }}
                  >
                    <List size={15} aria-hidden="true" />
                  </button>
                </div>
              </div>
            </div>
          ) : null}

          {isRefreshing ? (
            <div className="dashboard-refreshing" role="status">
              {t("dashboard.refreshingState")}
            </div>
          ) : null}

          {isBootstrapping ? (
            <div className="agent-roster" aria-label={t("dashboard.loadingAgents")}>
              {Array.from({ length: 4 }, (_, index) => (
                <Card className="agent-card skeleton-card" aria-hidden="true" key={index}>
                  <Skeleton className="skeleton-line short" />
                  <Skeleton className="skeleton-line" />
                  <div className="skeleton-grid">
                    <Skeleton as="span" />
                    <Skeleton as="span" />
                    <Skeleton as="span" />
                    <Skeleton as="span" />
                  </div>
                </Card>
              ))}
            </div>
          ) : !hasAgents ? (
            <EmptyState
              className="dashboard-empty"
              icon={hasConnectionError ? "!" : "◎"}
              title={hasConnectionError ? t("dashboard.runtimeUnavailable") : t("dashboard.noAgentsTitle")}
              description={
                hasConnectionError
                  ? t("dashboard.checkConnection")
                  : t("dashboard.startAgent")
              }
            />
          ) : filteredAgents.length === 0 ? (
            <div className="dashboard-filter-empty" role="status">
              <p>{t("dashboard.noMatchingAgents")}</p>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => {
                  setFilter("all");
                  setWorkspaceFilter(ALL_WORKSPACES_FILTER);
                  setQuery("");
                }}
              >
                {t("dashboard.clearFilters")}
              </Button>
            </div>
          ) : workspaceGroups ? (
            <div className="dashboard-workspace-groups">
              {workspaceGroups.map((group) => {
                const collapsed = Boolean(collapsedGroups[group.key]);
                return (
                  <section className="dashboard-workspace-group" key={group.key} aria-label={group.label}>
                    <button
                      type="button"
                      className="dashboard-workspace-group-head"
                      aria-expanded={!collapsed}
                      onClick={() =>
                        setCollapsedGroups((current) => ({ ...current, [group.key]: !current[group.key] }))
                      }
                    >
                      {collapsed ? <ChevronRight size={15} aria-hidden="true" /> : <ChevronDown size={15} aria-hidden="true" />}
                      <FolderGit2 size={15} aria-hidden="true" className="dashboard-workspace-group-icon" />
                      <strong>{group.label}</strong>
                      {group.anchor && group.key !== STANDALONE_WORKSPACE_KEY ? (
                        <small title={group.anchor}>{group.anchor}</small>
                      ) : null}
                      <span className="dashboard-workspace-group-badges">
                        <span className="dashboard-workspace-group-count">{group.entries.length}</span>
                      </span>
                    </button>
                    {!collapsed ? renderEntries(group.entries) : null}
                  </section>
                );
              })}
            </div>
          ) : (
            renderEntries(filteredAgents)
          )}
        </section>
      </div>
    </section>
  );
}

function projectInboxItem(
  notification: RuntimeNotification,
  agentsById: ReadonlyMap<string, AgentSummary>,
  t: (key: string) => string,
  dismissible: boolean,
): DashboardInboxItem {
  const agent = agentsById.get(notification.target.agent_id);
  const kindLabel = t(`notifications.kinds.${notification.kind}`);
  const primaryTitle =
    notification.title
    ?? (dismissible ? agent?.currentWork?.objective : undefined)
    ?? notification.summary
    ?? notification.target.work_item_id
    ?? kindLabel;
  const secondarySummary =
    notification.summary && notification.summary !== primaryTitle
      ? notification.summary
      : undefined;
  const pillToneClass =
    notification.kind === "work_item_completed"
      ? " is-success"
      : notification.kind === "operator_first_brief"
        ? " is-accent"
        : "";
  return {
    key: `${dismissible ? "pending" : "recent"}:${notification.id}`,
    agentId: notification.target.agent_id,
    agent,
    agentLabel: agent?.name ?? notification.target.agent_id,
    workItemId: notification.target.work_item_id,
    title: primaryTitle,
    summary: secondarySummary,
    createdAt: notification.created_at,
    kind: notification.kind,
    kindLabel,
    pillToneClass,
    notificationId: dismissible ? notification.id : undefined,
  };
}

type DashboardState = "loading" | "empty" | "disconnected" | "preview";

function getDashboardState({
  isBootstrapping,
  hasAgents,
  hasConnectionError,
  isPreview,
}: {
  isBootstrapping: boolean;
  hasAgents: boolean;
  hasConnectionError: boolean;
  isPreview: boolean;
}): DashboardState | undefined {
  if (isBootstrapping) return "loading";
  if (hasConnectionError) return "disconnected";
  if (!hasAgents) return "empty";
  if (isPreview) return "preview";
  return undefined;
}

function DashboardStateCard({ state, detail }: { state: DashboardState; detail: string }) {
  const { t } = useTranslation();
  const labelKey = state === "loading" ? "dashboard.stateLoading" : state === "empty" ? "dashboard.stateEmpty" : state === "disconnected" ? "dashboard.stateDisconnected" : "dashboard.statePreview";
  const titleKey = `${labelKey}Title`;
  const bodyKey = `${labelKey}Body`;

  return (
    <aside className={`dashboard-state ${state}`} role="status">
      <StatusBadge kind="runtime" value={state} />
      <div>
        <strong>{t(titleKey)}</strong>
        <p>{t(bodyKey)}</p>
        <small>{detail}</small>
      </div>
    </aside>
  );
}
