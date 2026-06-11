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
  return (
    <section className="page dashboard-page" aria-label="Dashboard">
      <div className="page-inner dashboard-inner">
        <section className="runtime-overview" aria-label="Runtime overview">
          <div className="overview-copy">
            <span className="eyebrow">Runtime dashboard</span>
            <h1>Local Holon workbench</h1>
            <p>
              Reads existing Holon routes: <code>/handshake</code>, <code>/agents/list</code>,{" "}
              <code>/agents/:id/state</code>, and <code>/agents/:id/briefs</code>. No GUI-specific
              backend API is required for this slice.
            </p>
          </div>
          <div className="metric-grid">
            {metrics.map((metric) => (
              <div className={`metric-card ${metric.tone ?? "default"}`} key={metric.label}>
                <span>{metric.label}</span>
                <strong>{metric.value}</strong>
              </div>
            ))}
          </div>
        </section>

        {connection.source === "fixture" ? (
          <aside className="dashboard-notice" role="status">
            <strong>Fixture fallback</strong>
            <span>{connection.error ?? connection.summary}</span>
          </aside>
        ) : null}

        <section className="dashboard-section">
          <div className="section-head">
            <div>
              <span className="eyebrow">Agents</span>
              <h2>Agent roster</h2>
            </div>
            <button type="button" disabled={loading} onClick={onRefresh}>
              {loading ? "Refreshing…" : "Refresh"}
            </button>
          </div>

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
                  <div>
                    <span>Current work</span>
                    <strong>{agent.currentWork?.objective ?? "none"}</strong>
                  </div>
                  <div>
                    <span>Workspace</span>
                    <strong>{agent.workspace}</strong>
                  </div>
                  <div>
                    <span>Attention</span>
                    <strong>{agent.attention}</strong>
                  </div>
                  <div>
                    <span>Model</span>
                    <strong>{agent.model}</strong>
                  </div>
                  <div>
                    <span>Posture</span>
                    <strong>{agent.posture}</strong>
                  </div>
                  <div>
                    <span>Tasks</span>
                    <strong>{agent.activeTaskCount}</strong>
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
        </section>
      </div>
    </section>
  );
}
