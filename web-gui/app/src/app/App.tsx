import { useMemo, useState } from "react";

import { AgentPage } from "../features/agent/AgentPage";
import { DashboardPage } from "../features/dashboard/DashboardPage";
import { InspectorPanel } from "../features/inspector/InspectorPanel";
import { SearchPage } from "../features/search/SearchPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { useAgentDetail } from "../runtime/useAgentDetail";
import { useRuntimeDashboard } from "../runtime/useRuntimeDashboard";
import type { DisplayLevel, RouteKey } from "../runtime/types";

const globalRoutes: Array<{ key: RouteKey; label: string; icon: string }> = [
  { key: "dashboard", label: "Dashboard", icon: "◎" },
  { key: "search", label: "Search", icon: "⌕" },
  { key: "settings", label: "Settings", icon: "⚙" },
];

export function App() {
  const { bootstrap, loading, refresh } = useRuntimeDashboard();
  const [route, setRoute] = useState<RouteKey>("dashboard");
  const [selectedAgentId, setSelectedAgentId] = useState("");
  const [displayLevel, setDisplayLevel] = useState<DisplayLevel>("info");
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [navCollapsed, setNavCollapsed] = useState(false);

  const selectedAgent = useMemo(
    () => bootstrap.agents.find((agent) => agent.id === selectedAgentId) ?? bootstrap.agents[0],
    [bootstrap.agents, selectedAgentId],
  );
  const {
    detail: selectedAgentDetail,
    loading: agentDetailLoading,
    refresh: refreshAgentDetail,
  } = useAgentDetail(selectedAgent?.id, displayLevel);

  function openAgent(agentId: string) {
    setSelectedAgentId(agentId);
    setRoute("agent");
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
          onClick={() => setNavCollapsed((value) => !value)}
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
          <div className="side-heading">Active agents</div>
          {bootstrap.agents.map((agent) => (
            <button
              className={`agent-row ${selectedAgentId === agent.id ? "is-selected" : ""}`}
              key={agent.id}
              type="button"
              onClick={() => openAgent(agent.id)}
            >
              <span className={`agent-badge ${agent.badgeTone ?? ""}`}>{agent.badge}</span>
              <span>
                <strong>{agent.id}</strong>
                <small>
                  {agent.lifecycle} · {agent.focusSummary}
                </small>
              </span>
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
                onClick={() => setInspectorOpen((value) => !value)}
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
            onRefresh={() => {
              void refreshAgentDetail();
            }}
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
  return `${agentCount} agents · ${attentionCount} waiting signal`;
}

function levelLabel(level: DisplayLevel): string {
  if (level === "info") return "Info";
  if (level === "verbose") return "Verbose";
  return "Debug";
}
