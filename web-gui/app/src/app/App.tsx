import { useEffect, useLayoutEffect } from "react";

import { AgentPage } from "../features/agent/AgentPage";
import { Button } from "../components/ui/Button";
import { EmptyState } from "../components/ui/EmptyState";
import { SegmentedControl, SegmentedControlButton } from "../components/ui/SegmentedControl";
import { StatusBadge } from "../components/ui/StatusChip";
import { DashboardPage } from "../features/dashboard/DashboardPage";
import { InspectorPanel } from "../features/inspector/InspectorPanel";
import { SearchPage } from "../features/search/SearchPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { selectSelectedAgent } from "../runtime/runtime-selectors";
import { useRuntimeStore } from "../runtime/runtime-store";
import { useAgentDetail } from "../runtime/useAgentDetail";
import { useRuntimeDashboard } from "../runtime/useRuntimeDashboard";
import type { AgentSummary, DisplayLevel, RouteKey } from "../runtime/types";
import { pushBrowserRoute, routeFromLocation } from "./routes";

const globalRoutes: Array<{ key: RouteKey; label: string; icon: string }> = [
  { key: "dashboard", label: "Dashboard", icon: "◎" },
  { key: "search", label: "Search", icon: "⌕" },
  { key: "settings", label: "Settings", icon: "⚙" },
];

export function App() {
  const { bootstrap, loading, refresh } = useRuntimeDashboard();
  const route = useRuntimeStore((state) => state.route);
  const selectedAgentId = useRuntimeStore((state) => state.selectedAgentId);
  const displayLevel = useRuntimeStore((state) => state.displayLevel);
  const inspectorOpen = useRuntimeStore((state) => state.inspectorOpen);
  const inspectorSelection = useRuntimeStore((state) => state.inspectorSelection);
  const navCollapsed = useRuntimeStore((state) => state.navCollapsed);
  const setRoute = useRuntimeStore((state) => state.setRoute);
  const openAgent = useRuntimeStore((state) => state.openAgent);
  const setDisplayLevel = useRuntimeStore((state) => state.setDisplayLevel);
  const setInspectorOpen = useRuntimeStore((state) => state.setInspectorOpen);
  const inspectActivity = useRuntimeStore((state) => state.inspectActivity);
  const clearInspectorSelection = useRuntimeStore((state) => state.clearInspectorSelection);
  const toggleInspector = useRuntimeStore((state) => state.toggleInspector);
  const toggleNavCollapsed = useRuntimeStore((state) => state.toggleNavCollapsed);
  const selectedAgent = useRuntimeStore(selectSelectedAgent);
  const activeAgentId = route === "agent" ? selectedAgent?.id ?? selectedAgentId : undefined;
  const selectedAgentSession = useRuntimeStore((state) =>
    activeAgentId ? state.sessionsByAgentId[activeAgentId] : undefined,
  );
  const modelCatalog = useRuntimeStore((state) => state.modelCatalog);
  const modelCatalogLoading = useRuntimeStore((state) => state.modelCatalogLoading);
  const modelCatalogError = useRuntimeStore((state) => state.modelCatalogError);
  const runtimeConfig = useRuntimeStore((state) => state.runtimeConfig);
  const runtimeConfigLoading = useRuntimeStore((state) => state.runtimeConfigLoading);
  const runtimeConfigSaving = useRuntimeStore((state) => state.runtimeConfigSaving);
  const runtimeConfigError = useRuntimeStore((state) => state.runtimeConfigError);
  const search = useRuntimeStore((state) => state.search);
  const searchLoading = useRuntimeStore((state) => state.searchLoading);
  const searchError = useRuntimeStore((state) => state.searchError);
  const runSearch = useRuntimeStore((state) => state.runSearch);
  const refreshModelCatalog = useRuntimeStore((state) => state.refreshModelCatalog);
  const refreshRuntimeConfig = useRuntimeStore((state) => state.refreshRuntimeConfig);
  const updateRuntimeConfig = useRuntimeStore((state) => state.updateRuntimeConfig);
  const sendOperatorPrompt = useRuntimeStore((state) => state.sendOperatorPrompt);
  const setAgentModel = useRuntimeStore((state) => state.setAgentModel);
  const clearAgentModel = useRuntimeStore((state) => state.clearAgentModel);
  const loadOlderAgentEvents = useRuntimeStore((state) => state.loadOlderAgentEvents);
  const {
    detail: selectedAgentDetail,
    loading: agentDetailLoading,
    refresh: refreshAgentDetail,
  } = useAgentDetail(activeAgentId, displayLevel);
  const activeAgent = selectedAgent ?? selectedAgentDetail?.agent;
  const selectedAgentLiveStatus = selectedAgentSession?.liveStatus ?? "idle";
  const selectedAgentLiveTitle = liveStatusTitle(selectedAgentLiveStatus, selectedAgentSession?.lastStreamActivityAt, selectedAgentSession?.error);
  const selectedAgentCurrentWork = activeAgent?.currentWork;
  const selectedAgentContext =
    route === "agent" && activeAgent
      ? [activeAgent.lifecycle, activeAgent.posture].filter(Boolean).join(" · ")
      : "loading agent";
  const selectedAgentSourceStatus =
    agentDetailLoading && !selectedAgentDetail
      ? "syncing"
      : selectedAgentDetail?.source === "http" && !selectedAgentDetail.error
        ? "live"
        : "preview";
  const isInitialBootstrapping = loading && bootstrap.connection.summary === "Connecting to local runtime…" && !bootstrap.connection.error;

  useLayoutEffect(() => {
    const applyBrowserRoute = () => {
      const nextRoute = routeFromLocation(window.location);
      if (nextRoute.route === "agent" && nextRoute.agentId) {
        openAgent(nextRoute.agentId);
        return;
      }
      setRoute(nextRoute.route);
    };

    applyBrowserRoute();
    window.addEventListener("popstate", applyBrowserRoute);
    return () => window.removeEventListener("popstate", applyBrowserRoute);
  }, [openAgent, setRoute]);

  useEffect(() => {
    if ((route !== "agent" && route !== "settings") || modelCatalogLoading || modelCatalog.options.length > 0) return;
    void refreshModelCatalog();
  }, [modelCatalog.options.length, modelCatalogLoading, refreshModelCatalog, route]);

  useEffect(() => {
    if (route !== "settings" || runtimeConfigLoading || runtimeConfig.surface) return;
    void refreshRuntimeConfig();
  }, [refreshRuntimeConfig, route, runtimeConfig.surface, runtimeConfigLoading]);

  function navigateRoute(nextRoute: RouteKey) {
    setRoute(nextRoute);
    pushBrowserRoute(nextRoute, selectedAgentId);
  }

  function navigateAgent(agentId: string, eventSeq?: number) {
    openAgent(agentId);
    pushBrowserRoute("agent", agentId, eventSeq == null ? undefined : { event_seq: eventSeq });
  }

  if (isInitialBootstrapping) {
    return <BootstrappingPage />;
  }

  return (
    <div
      className="app-shell"
      data-panel={inspectorOpen ? "open" : "closed"}
      data-nav-collapsed={navCollapsed}
    >
      <aside className="sidebar" aria-label="Holon navigation">
        <div className="window-controls" aria-hidden="true">
          <span />
          <span />
          <span />
        </div>
        <button
          className="nav-collapse"
          type="button"
          aria-label={navCollapsed ? "Expand navigation" : "Collapse navigation"}
          title={navCollapsed ? "Expand navigation" : "Collapse navigation"}
          onClick={toggleNavCollapsed}
        >
          ‹
        </button>

        <nav className="global-nav" aria-label="Global navigation">
          {globalRoutes.map((item) => (
            <button
              className={route === item.key ? "is-active" : ""}
              key={item.key}
              type="button"
              aria-label={item.label}
              title={item.label}
              onClick={() => navigateRoute(item.key)}
            >
              <span>{item.icon}</span>
              <strong>{item.label}</strong>
            </button>
          ))}
        </nav>

        <section className="side-section agent-switcher" aria-label="Active agents">
          <div className="side-heading">
            <span>Active agents</span>
            <strong>{bootstrap.agents.length}</strong>
          </div>
          {bootstrap.agents.length === 0 ? (
            <div className="agent-list-state" role="status">
              <strong>{loading ? "Syncing agents…" : "No agents"}</strong>
              <span>{loading ? "Waiting for the local runtime." : "The runtime has no visible agents yet."}</span>
            </div>
          ) : (
            bootstrap.agents.map((agent) => {
              const status = agentDisplayStatus(agent);
              const secondary = agentSecondaryStatus(agent);

              return (
                <button
                  className={`agent-row ${selectedAgentId === agent.id ? "is-selected" : ""} ${agent.lifecycle}`}
                  key={agent.id}
                  title={`${agent.id} · ${agent.focusSummary} · ${status.title}`}
                  type="button"
                  onClick={() => navigateAgent(agent.id)}
                >
                  <span className={`agent-badge ${agent.badgeTone ?? ""}`}>{agent.badge}</span>
                  <span className="agent-row-main">
                    <span className="agent-row-title">
                      <strong>{agent.id}</strong>
                      <StatusBadge className="agent-row-status" kind="agent" value={status.tone} aria-label={status.title} title={status.title}>
                        {status.label}
                      </StatusBadge>
                    </span>
                    <span className="agent-row-meta">
                      {secondary.map((item, index) => (
                        <span key={`${item}-${index}`}>{item}</span>
                      ))}
                    </span>
                  </span>
                </button>
              );
            })
          )}
        </section>

        <div className="sidebar-bottom">
          <button className="connection-status" type="button">
            <span className="runtime-dot" />
            <span>
              <strong>{bootstrap.connection.mode}</strong>
              <small>{bootstrap.connection.summary}</small>
            </span>
          </button>
        </div>
      </aside>

      <main className="main-shell">
        <header className="topbar">
          <div className="topbar-primary">
            <div className="top-title">
              {route === "agent" ? (
                <Button
                  className="back-button"
                  type="button"
                  size="icon"
                  variant="ghost"
                  aria-label="Back to dashboard"
                  onClick={() => navigateRoute("dashboard")}
                >
                  ←
                </Button>
              ) : null}
              <div>
                <strong>{route === "agent" ? (selectedAgent?.id ?? selectedAgentId) || "Agent" : pageTitle(route)}</strong>
                <span>
                  {route === "agent"
                    ? selectedAgentContext
                    : pageSubtitle(route, bootstrap.attentionCount, bootstrap.agents.length)}
                </span>
              </div>
            </div>
            <div className="top-actions">
              <Button
                type="button"
                size="icon"
                variant="ghost"
                aria-label="Toggle object inspector"
                title="Toggle object inspector"
                onClick={toggleInspector}
              >
                ▭
              </Button>
            </div>
          </div>

          {route === "agent" ? (
            <div className="agent-top-context" aria-label="Agent conversation context">
              <div className="agent-context-main">
                <button
                  className={`work-summary ${selectedAgentCurrentWork ? "has-work" : "is-empty"}`}
                  type="button"
                  onClick={() => setInspectorOpen(true)}
                >
                  <span className="work-summary-label">Current work</span>
                  <strong>{selectedAgentCurrentWork?.objective ?? "No active work item"}</strong>
                  <span className="work-summary-meta">
                    {selectedAgentCurrentWork
                      ? `${selectedAgentCurrentWork.state} · ${selectedAgentCurrentWork.id}`
                      : "Ready for operator input"}
                  </span>
                </button>
              </div>
              <div className="agent-top-controls">
                <div className="agent-stream-controls" aria-label="Agent stream status">
                  <StatusBadge
                    kind="connection"
                    value={selectedAgentSourceStatus}
                    className={`source-chip ${selectedAgentSourceStatus}`}
                  >
                    {selectedAgentSourceStatus}
                  </StatusBadge>
                  <StatusBadge
                    kind="stream"
                    value={selectedAgentLiveStatus}
                    className={`source-chip live-status ${selectedAgentLiveStatus}`}
                    title={selectedAgentLiveTitle}
                  >
                    {liveStatusLabel(selectedAgentLiveStatus)}
                  </StatusBadge>
                  <Button
                    type="button"
                    size="icon"
                    variant="ghost"
                    aria-label={agentDetailLoading ? "Refreshing agent detail" : "Refresh agent detail"}
                    title={agentDetailLoading ? "Refreshing…" : "Refresh agent detail"}
                    disabled={agentDetailLoading}
                    onClick={() => void refreshAgentDetail()}
                  >
                    ↻
                  </Button>
                </div>
                <SegmentedControl className="display-level" label="Display level">
                  {(["info", "verbose", "debug"] as const).map((level) => (
                    <SegmentedControlButton
                      active={displayLevel === level}
                      className={displayLevel === level ? "is-active" : ""}
                      key={level}
                      type="button"
                      onClick={() => setDisplayLevel(level, activeAgentId)}
                    >
                      {levelLabel(level)}
                    </SegmentedControlButton>
                  ))}
                </SegmentedControl>
              </div>
            </div>
          ) : null}
        </header>

        {route === "dashboard" ? (
          <DashboardPage
            agents={bootstrap.agents}
            metrics={bootstrap.metrics}
            connection={bootstrap.connection}
            loading={loading}
            onRefresh={() => {
              void refresh();
            }}
            onOpenAgent={navigateAgent}
          />
        ) : null}
        {route === "agent" && activeAgent ? (
          <AgentPage
            agent={activeAgent}
            detail={selectedAgentDetail}
            displayLevel={displayLevel}
            sendingPrompt={selectedAgentSession?.sendingPrompt ?? false}
            promptError={selectedAgentSession?.promptError}
            modelCatalog={modelCatalog}
            modelCatalogLoading={modelCatalogLoading}
            modelCatalogError={selectedAgentSession?.modelError ?? modelCatalogError}
            hasOlderEvents={selectedAgentSession?.hasOlder ?? selectedAgentDetail?.hasOlderEvents ?? false}
            loadingOlderEvents={selectedAgentSession?.loadingOlder ?? false}
            historyError={selectedAgentSession?.historyError}
            onRefreshModels={refreshModelCatalog}
            onSetModel={(model, reasoningEffort) => setAgentModel(activeAgent.id, model, displayLevel, reasoningEffort)}
            onClearModel={() => clearAgentModel(activeAgent.id, displayLevel)}
            onLoadOlderEvents={() => loadOlderAgentEvents(activeAgent.id, displayLevel)}
            onSendPrompt={(text) => sendOperatorPrompt(activeAgent.id, text, displayLevel)}
            onOpenInspector={() => setInspectorOpen(true)}
            onInspectActivity={(activity) => inspectActivity(activeAgent.id, activity)}
            selectedActivityId={
              inspectorSelection?.kind === "activity" && inspectorSelection.agentId === activeAgent.id
                ? inspectorSelection.activity.id
                : undefined
            }
          />
        ) : null}
        {route === "agent" && !activeAgent ? <MissingAgentPage agentId={selectedAgentId} loading={loading} /> : null}
        {route === "search" ? (
          <SearchPage
            agents={bootstrap.agents}
            search={search}
            loading={searchLoading}
            error={searchError}
            onSearch={runSearch}
            onOpenAgent={navigateAgent}
          />
        ) : null}
        {route === "settings" ? (
          <SettingsPage
            connection={bootstrap.connection}
            modelCatalog={modelCatalog}
            modelCatalogLoading={modelCatalogLoading}
            modelCatalogError={modelCatalogError}
            runtimeConfig={runtimeConfig}
            runtimeConfigLoading={runtimeConfigLoading}
            runtimeConfigSaving={runtimeConfigSaving}
            runtimeConfigError={runtimeConfigError}
            onRefreshModels={refreshModelCatalog}
            onRefreshRuntimeConfig={refreshRuntimeConfig}
            onUpdateRuntimeConfig={updateRuntimeConfig}
          />
        ) : null}
      </main>

      {selectedAgent ? (
        <InspectorPanel
          agent={selectedAgent}
          selection={inspectorSelection?.agentId === selectedAgent.id ? inspectorSelection : undefined}
          open={inspectorOpen}
          onClearSelection={clearInspectorSelection}
          onClose={() => setInspectorOpen(false)}
        />
      ) : null}
    </div>
  );
}

function BootstrappingPage() {
  return (
    <main className="boot-page" aria-label="Holon is loading">
      <section className="boot-card" role="status" aria-live="polite">
        <span className="boot-mark">◎</span>
        <div>
          <p>Starting Holon Web GUI</p>
          <h1>Preparing runtime data…</h1>
          <span>Loading the runtime handshake and agent roster before rendering the workspace.</span>
        </div>
      </section>
    </main>
  );
}

function MissingAgentPage({ agentId, loading }: { agentId: string; loading: boolean }) {
  return (
    <section className="page agent-page" aria-label="Agent conversation">
      <div className="agent-workbench">
        <section className="conversation-pane">
          <EmptyState
            className="conversation-empty"
            title={loading ? "Loading agent…" : "Agent not found"}
            description={agentId ? agentId : "No agent id was provided in the current route."}
          />
        </section>
      </div>
    </section>
  );
}

function pageTitle(route: RouteKey): string {
  if (route === "search") return "Search";
  if (route === "settings") return "Settings";
  return "Dashboard";
}

function pageSubtitle(route: RouteKey, attentionCount: number, agentCount: number): string {
  if (route === "search") return "cross-agent lookup · messages · briefs · work evidence";
  if (route === "settings") return "local connection · providers · model defaults";
  return attentionCount > 0 ? `${agentCount} agents · ${attentionCount} need attention` : `${agentCount} agents · all clear`;
}

function levelLabel(level: DisplayLevel): string {
  if (level === "info") return "Info";
  if (level === "verbose") return "Verbose";
  return "Debug";
}

function agentDisplayStatus(agent: AgentSummary): { label: string; title: string; tone: string } {
  const details = [
    agent.posture ? `posture: ${agent.posture}` : undefined,
    agent.lifecycle ? `lifecycle: ${agent.lifecycle}` : undefined,
    agent.pending > 0 ? `${agent.pending} pending input${agent.pending === 1 ? "" : "s"}` : undefined,
    agent.activeTaskCount > 0
      ? `${agent.activeTaskCount} active task${agent.activeTaskCount === 1 ? "" : "s"}`
      : undefined,
    agent.waitingCount > 0 ? `${agent.waitingCount} waiting condition${agent.waitingCount === 1 ? "" : "s"}` : undefined,
  ].filter(Boolean);
  const title = details.join(" · ") || "No status details";

  if (isStoppedOrArchived(agent.lifecycle) || isStoppedOrArchived(agent.posture)) {
    return { label: "Stopped", title, tone: "stopped" };
  }

  if (agent.pending > 0) {
    return { label: "Needs input", title, tone: "needs-input" };
  }

  if (agent.activeTaskCount > 0 || agent.currentRunId || isActivePosture(agent.posture)) {
    return { label: "Running", title, tone: "running" };
  }

  if (isRunnablePosture(agent.posture)) {
    return { label: "Runnable", title, tone: "running" };
  }

  if (agent.waitingCount > 0 || isWaitingPosture(agent.posture)) {
    return { label: "Waiting", title, tone: "waiting" };
  }

  if (isBlockedPosture(agent.posture)) {
    return { label: "Blocked", title, tone: "stopped" };
  }

  if (isIdlePosture(agent.posture) || isIdlePosture(agent.lifecycle)) {
    return { label: "Ready", title, tone: "ready" };
  }

  return { label: "Unknown", title, tone: "muted" };
}

function agentSecondaryStatus(agent: AgentSummary): string[] {
  const items = [agent.lifecycle, agent.currentWork?.state ?? agent.posture].filter(Boolean);
  return items.length > 0 ? items : ["unknown"];
}

function normalizeAgentStatus(value?: string | null): string {
  return (value ?? "").trim().toLowerCase().replace(/[_\s]+/g, "-");
}

function isStoppedOrArchived(value?: string | null): boolean {
  const status = normalizeAgentStatus(value);
  return status === "stopped" || status === "archived";
}

function isActivePosture(value?: string | null): boolean {
  const status = normalizeAgentStatus(value);
  return status === "active-turn" || status === "awake-running" || status === "running";
}

function isRunnablePosture(value?: string | null): boolean {
  const status = normalizeAgentStatus(value);
  return status === "has-queued-input" || status === "has-runnable-work";
}

function isWaitingPosture(value?: string | null): boolean {
  return normalizeAgentStatus(value).startsWith("waiting");
}

function isBlockedPosture(value?: string | null): boolean {
  return normalizeAgentStatus(value) === "blocked";
}

function isIdlePosture(value?: string | null): boolean {
  const status = normalizeAgentStatus(value);
  return status === "idle" || status === "asleep" || status === "awake-idle" || status === "ready";
}

function liveStatusLabel(status: string): string {
  if (status === "connecting") return "connecting";
  if (status === "streaming") return "streaming";
  if (status === "reconnecting") return "reconnecting";
  if (status === "recovering") return "recovering";
  if (status === "stale") return "stale";
  if (status === "error") return "stream error";
  return "idle";
}

function liveStatusTitle(status: string, lastActivityAt?: string, error?: string): string {
  const parts = [liveStatusLabel(status)];
  if (lastActivityAt) parts.push(`last activity ${formatRelativeTime(lastActivityAt)}`);
  if (error) parts.push(error);
  return parts.join(" · ");
}

function formatRelativeTime(value: string): string {
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return value;
  const seconds = Math.max(0, Math.round((Date.now() - timestamp) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  return new Date(timestamp).toLocaleString();
}
