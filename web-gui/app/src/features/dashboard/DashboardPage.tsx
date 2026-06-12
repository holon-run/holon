import type { AgentSummary, DashboardMetric, RuntimeConnection } from "../../runtime/types";

interface DashboardPageProps {
  agents: AgentSummary[];
  metrics: DashboardMetric[];
  connection: RuntimeConnection;
  loading: boolean;
  onRefresh: () => void;
  onOpenAgent: (agentId: string) => void;
}

export function DashboardPage({ agents, metrics, connection, loading, onRefresh, onOpenAgent }: DashboardPageProps) {
  const isBootstrapping = loading && agents.length === 0;
  const connectionLabel = connection.source === "http" ? "Live runtime" : isBootstrapping ? "Connecting" : "Preview data";
  const agentCountLabel = `${agents.length} ${agents.length === 1 ? "agent" : "agents"}`;

  return (
    <section className="page dashboard-page" aria-label="Dashboard">
      <div className="page-inner dashboard-inner">
        <section className="dashboard-section dashboard-roster-section">
          <div className="section-head dashboard-head">
            <div>
              <span className="eyebrow">Agents</span>
              <h1>Agent roster</h1>
              <p>{agentCountLabel} · local Holon runtime overview</p>
            </div>
            <div className="dashboard-actions">
              <span className={`connection-pill ${connection.source}`}>
                <span className="runtime-dot" />
                {connectionLabel}
              </span>
              <button type="button" disabled={loading} onClick={onRefresh}>
                {loading ? "Refreshing…" : "Refresh"}
              </button>
            </div>
          </div>

          <div className="metric-strip" aria-label="Runtime metrics">
            {metrics.map((metric) => (
              <div className={`metric-card ${metric.tone ?? "default"}`} key={metric.label}>
                <span>{metric.label}</span>
                <strong>{metric.value}</strong>
              </div>
            ))}
          </div>

          {connection.source === "fixture" && !isBootstrapping ? (
            <aside className="dashboard-notice" role="status">
              <strong>Preview data</strong>
              <span>{connection.error ?? connection.summary}</span>
            </aside>
          ) : null}

          {loading && agents.length > 0 ? (
            <div className="dashboard-refreshing" role="status">
              Refreshing agent state…
            </div>
          ) : null}

          {isBootstrapping ? (
            <div className="agent-roster" aria-label="Loading agents">
              {Array.from({ length: 4 }, (_, index) => (
                <article className="agent-card skeleton-card" aria-hidden="true" key={index}>
                  <div className="skeleton-line short" />
                  <div className="skeleton-line" />
                  <div className="skeleton-grid">
                    <span />
                    <span />
                    <span />
                    <span />
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <div className="agent-roster">
              {agents.map((agent) => (
                <article className="agent-card" key={agent.id}>
                  <div className="agent-card-head">
                    <span className={`agent-badge ${agent.badgeTone ?? ""}`}>{agent.badge}</span>
                    <div>
                      <strong>{agent.id}</strong>
                      <small>{agent.profile}</small>
                    </div>
                    <span className={`state-chip ${agent.lifecycle}`}>{agent.lifecycle}</span>
                  </div>
                  <p>{agent.focusSummary}</p>
                  <div className="agent-detail-grid">
                    <div title={agent.currentWork?.objective ?? "none"}>
                      <span>Current work</span>
                      <strong>{agent.currentWork?.objective ?? "none"}</strong>
                    </div>
                    <div title={agent.workspace}>
                      <span>Workspace</span>
                      <strong>{agent.workspace}</strong>
                    </div>
                    <div>
                      <span>Attention</span>
                      <strong>{agent.attention}</strong>
                    </div>
                    <div title={agent.model}>
                      <span>Model</span>
                      <strong>{agent.model}</strong>
                    </div>
                    <div>
                      <span>Posture</span>
                      <strong>{agent.posture}</strong>
                    </div>
                    <div>
                      <span>Last brief</span>
                      <strong title={agent.lastBrief}>{agent.lastBrief}</strong>
                    </div>
                  </div>
                  <footer>
                    <span>{agent.footer}</span>
                    <button type="button" aria-label={`Open ${agent.id}`} onClick={() => onOpenAgent(agent.id)}>
                      →
                    </button>
                  </footer>
                </article>
              ))}
            </div>
          )}
        </section>
      </div>
    </section>
  );
}
