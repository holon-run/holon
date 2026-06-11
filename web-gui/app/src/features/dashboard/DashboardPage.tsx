import type { AgentSummary } from "../../runtime/types";

interface DashboardPageProps {
  agents: AgentSummary[];
  onOpenAgent: (agentId: string) => void;
}

export function DashboardPage({ agents, onOpenAgent }: DashboardPageProps) {
  return (
    <section className="page dashboard-page" aria-label="Dashboard">
      <div className="page-inner dashboard-inner">
        <section className="dashboard-section">
          <div className="section-head">
            <div>
              <span className="eyebrow">Agents</span>
              <h2>Agent roster</h2>
            </div>
            <button type="button">Refresh</button>
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
