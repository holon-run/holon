import { useMemo, useState } from "react";

import { AgentPage } from "../features/agent/AgentPage";
import { DashboardPage } from "../features/dashboard/DashboardPage";
import { InspectorPanel } from "../features/inspector/InspectorPanel";
import { SearchPage } from "../features/search/SearchPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { runtimeFixture } from "../runtime/fixtures";
import type { DisplayLevel, RouteKey } from "../runtime/types";

const globalRoutes: Array<{ key: RouteKey; label: string; icon: string }> = [
  { key: "dashboard", label: "Dashboard", icon: "◎" },
  { key: "search", label: "Search", icon: "⌕" },
  { key: "settings", label: "Settings", icon: "⚙" },
];

export function App() {
  const [route, setRoute] = useState<RouteKey>("dashboard");
  const [selectedAgentId, setSelectedAgentId] = useState(runtimeFixture.agents[0]?.id ?? "");
  const [displayLevel, setDisplayLevel] = useState<DisplayLevel>("info");
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [navCollapsed, setNavCollapsed] = useState(false);

  const selectedAgent = useMemo(
    () => runtimeFixture.agents.find((agent) => agent.id === selectedAgentId) ?? runtimeFixture.agents[0],
    [selectedAgentId],
  );

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
          {runtimeFixture.agents.map((agent) => (
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
              <strong>{runtimeFixture.connection.mode}</strong>
              <small>{runtimeFixture.connection.summary}</small>
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
                <strong>{route === "agent" ? selectedAgent.id : pageTitle(route)}</strong>
                <span>{route === "agent" ? selectedAgent.subtitle : pageSubtitle(route)}</span>
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
                <strong>{selectedAgent.currentWork?.objective ?? "No current work item"}</strong>
                <em>{selectedAgent.currentWork?.state ?? selectedAgent.lifecycle}</em>
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
          <DashboardPage agents={runtimeFixture.agents} onOpenAgent={openAgent} />
        ) : null}
        {route === "agent" ? (
          <AgentPage agent={selectedAgent} displayLevel={displayLevel} onOpenInspector={() => setInspectorOpen(true)} />
        ) : null}
        {route === "search" ? <SearchPage /> : null}
        {route === "settings" ? <SettingsPage connection={runtimeFixture.connection} /> : null}
      </main>

      <InspectorPanel agent={selectedAgent} open={inspectorOpen} onClose={() => setInspectorOpen(false)} />
    </div>
  );
}

function pageTitle(route: RouteKey): string {
  if (route === "search") return "Search";
  if (route === "settings") return "Settings";
  return "Dashboard";
}

function pageSubtitle(route: RouteKey): string {
  if (route === "search") return "cross-agent lookup · messages · briefs · work evidence";
  if (route === "settings") return "local connection · providers · model defaults";
  return `${runtimeFixture.agents.length} agents · ${runtimeFixture.attentionCount} waiting signal`;
}

function levelLabel(level: DisplayLevel): string {
  if (level === "info") return "Info";
  if (level === "verbose") return "Verbose";
  return "Debug";
}
