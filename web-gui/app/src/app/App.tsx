import { AgentPage } from "../features/agent/AgentPage";
import { DashboardPage } from "../features/dashboard/DashboardPage";
import { InspectorPanel } from "../features/inspector/InspectorPanel";
import { SearchPage } from "../features/search/SearchPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { selectSelectedAgent } from "../runtime/runtime-selectors";
import { useRuntimeStore } from "../runtime/runtime-store";
import { useAgentDetail } from "../runtime/useAgentDetail";
import { useRuntimeDashboard } from "../runtime/useRuntimeDashboard";
import type { AgentSummary, DisplayLevel, RouteKey } from "../runtime/types";

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
  const navCollapsed = useRuntimeStore((state) => state.navCollapsed);
  const setRoute = useRuntimeStore((state) => state.setRoute);
  const openAgent = useRuntimeStore((state) => state.openAgent);
  const setDisplayLevel = useRuntimeStore((state) => state.setDisplayLevel);
  const setInspectorOpen = useRuntimeStore((state) => state.setInspectorOpen);
  const toggleInspector = useRuntimeStore((state) => state.toggleInspector);
  const toggleNavCollapsed = useRuntimeStore((state) => state.toggleNavCollapsed);
  const selectedAgent = useRuntimeStore(selectSelectedAgent);
  const selectedAgentSession = useRuntimeStore((state) =>
    selectedAgent?.id ? state.sessionsByAgentId[selectedAgent.id] : undefined,
  );
  const sendOperatorPrompt = useRuntimeStore((state) => state.sendOperatorPrompt);
  const loadOlderAgentEvents = useRuntimeStore((state) => state.loadOlderAgentEvents);
  const {
    detail: selectedAgentDetail,
    loading: agentDetailLoading,
    refresh: refreshAgentDetail,
  } = useAgentDetail(selectedAgent?.id, displayLevel);

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
              onClick={() => setRoute(item.key)}
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
          {bootstrap.agents.map((agent) => (
            <button
              className={`agent-row ${selectedAgentId === agent.id ? "is-selected" : ""} ${agent.lifecycle}`}
              key={agent.id}
              title={`${agent.id} · ${agent.focusSummary}`}
              type="button"
              onClick={() => openAgent(agent.id)}
            >
              <span className={`agent-badge ${agent.badgeTone ?? ""}`}>{agent.badge}</span>
              <span className="agent-row-main">
                <span className="agent-row-title">
                  <strong>{agent.id}</strong>
                  <em className={`agent-state-dot ${agent.lifecycle}`} aria-label={agent.lifecycle} />
                </span>
                <small>{agent.focusSummary}</small>
                <span className="agent-row-meta">
                  <span>{agent.lifecycle}</span>
                  <span>{agent.currentWork?.state ?? agent.posture}</span>
                </span>
              </span>
              {agentSignalCount(agent) > 0 ? <span className="agent-row-count">{agentSignalCount(agent)}</span> : null}
            </button>
          ))}
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
                <button
                  className="back-button"
                  type="button"
                  aria-label="Back to dashboard"
                  onClick={() => setRoute("dashboard")}
                >
                  ←
                </button>
              ) : null}
              <div>
                <strong>{route === "agent" ? selectedAgent?.id ?? "Agent" : pageTitle(route)}</strong>
                <span>
                  {route === "agent"
                    ? selectedAgent?.subtitle ?? "loading agent"
                    : pageSubtitle(route, bootstrap.attentionCount, bootstrap.agents.length)}
                </span>
              </div>
            </div>
            <div className="top-actions">
              <button
                type="button"
                aria-label="Toggle object inspector"
                title="Toggle object inspector"
                onClick={toggleInspector}
              >
                ▭
              </button>
            </div>
          </div>

          {route === "agent" ? (
            <div className="agent-top-context" aria-label="Agent conversation context">
              <button className="work-summary" type="button" onClick={() => setInspectorOpen(true)}>
                <span>Current work item</span>
                <strong>{selectedAgent?.currentWork?.objective ?? "No current work item"}</strong>
                <em>{selectedAgent?.currentWork?.state ?? selectedAgent?.lifecycle ?? "unknown"}</em>
              </button>
              <div className="display-level" aria-label="Display level">
                {(["info", "verbose", "debug"] as const).map((level) => (
                  <button
                    className={displayLevel === level ? "is-active" : ""}
                    key={level}
                    type="button"
                    onClick={() => setDisplayLevel(level)}
                  >
                    {levelLabel(level)}
                  </button>
                ))}
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
            onOpenAgent={openAgent}
          />
        ) : null}
        {route === "agent" && selectedAgent ? (
          <AgentPage
            agent={selectedAgent}
            detail={selectedAgentDetail}
            displayLevel={displayLevel}
            loading={agentDetailLoading}
            sendingPrompt={selectedAgentSession?.sendingPrompt ?? false}
            promptError={selectedAgentSession?.promptError}
            hasOlderEvents={selectedAgentSession?.hasOlder ?? selectedAgentDetail?.hasOlderEvents ?? false}
            loadingOlderEvents={selectedAgentSession?.loadingOlder ?? false}
            liveStatus={selectedAgentSession?.liveStatus ?? "idle"}
            historyError={selectedAgentSession?.historyError}
            onRefresh={() => {
              void refreshAgentDetail();
            }}
            onLoadOlderEvents={() => loadOlderAgentEvents(selectedAgent.id, displayLevel)}
            onSendPrompt={(text) => sendOperatorPrompt(selectedAgent.id, text, displayLevel)}
            onOpenInspector={() => setInspectorOpen(true)}
          />
        ) : null}
        {route === "search" ? <SearchPage /> : null}
        {route === "settings" ? <SettingsPage connection={bootstrap.connection} /> : null}
      </main>

      {selectedAgent ? <InspectorPanel agent={selectedAgent} open={inspectorOpen} onClose={() => setInspectorOpen(false)} /> : null}
    </div>
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
  return `${agentCount} agents · ${attentionCount} need attention`;
}

function levelLabel(level: DisplayLevel): string {
  if (level === "info") return "Info";
  if (level === "verbose") return "Verbose";
  return "Debug";
}

function agentSignalCount(agent: AgentSummary): number {
  return agent.pending + agent.activeTaskCount + agent.waitingCount;
}
