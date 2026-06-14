import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { Skeleton } from "../../components/ui/Skeleton";
import { AgentStateBadge, StatusBadge } from "../../components/ui/StatusChip";
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
  const hasAgents = agents.length > 0;
  const hasConnectionError = Boolean(connection.error);
  const isBootstrapping = loading && !hasAgents;
  const isRefreshing = loading && hasAgents;
  const isPreview = connection.source === "fixture";
  const dashboardState = getDashboardState({ isBootstrapping, hasAgents, hasConnectionError, isPreview });
  const connectionLabel = connection.source === "http" ? "Live runtime" : hasConnectionError ? "Runtime unavailable" : "Preview data";
  const agentCountLabel = `${agents.length} ${agents.length === 1 ? "agent" : "agents"}`;

  return (
    <section className="page dashboard-page" aria-label="Dashboard">
      <div className="page-inner dashboard-inner">
        <section className="dashboard-section dashboard-roster-section">
          <div className="section-head dashboard-head">
            <div>
              <span className="eyebrow">Roster</span>
              <h2>Agents</h2>
              <p>{agentCountLabel} available in the local runtime</p>
            </div>
            <div className="dashboard-actions">
              <StatusBadge className={`connection-pill ${connection.source}`} kind="connection" value={connection.source}>
                <span className="runtime-dot" />
                {connectionLabel}
              </StatusBadge>
              <Button type="button" variant="secondary" disabled={loading} onClick={onRefresh}>
                {loading ? "Refreshing…" : "Refresh"}
              </Button>
            </div>
          </div>

          {dashboardState ? <DashboardStateCard state={dashboardState} detail={connection.error ?? connection.summary} /> : null}

          {metrics.length > 0 ? (
            <div className="metric-strip" aria-label="Runtime metrics">
              {metrics.map((metric) => (
                <div className={`metric-card ${metric.tone ?? "default"}`} key={metric.label}>
                  <span>{metric.label}</span>
                  <strong>{metric.value}</strong>
                </div>
              ))}
            </div>
          ) : null}

          {isRefreshing ? (
            <div className="dashboard-refreshing" role="status">
              Refreshing agent state while keeping the current roster visible…
            </div>
          ) : null}

          {isBootstrapping ? (
            <div className="agent-roster" aria-label="Loading agents">
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
              title={hasConnectionError ? "Runtime data is unavailable" : "No agents are currently visible"}
              description={
                hasConnectionError
                  ? "Check the local Holon API connection, then refresh this dashboard."
                  : "Start or wake an agent and refresh to populate the roster."
              }
            />
          ) : (
            <div className="agent-roster">
              {agents.map((agent) => (
                <Card className="agent-card" key={agent.id}>
                  <div className="agent-card-head">
                    <span className={`agent-badge ${agent.badgeTone ?? ""}`}>{agent.badge}</span>
                    <div className="agent-card-title">
                      <strong>{agent.id}</strong>
                      <small>{agent.profile}</small>
                    </div>
                    <div className="agent-card-status">
                      <AgentStateBadge className={`state-chip ${agent.lifecycle}`} lifecycle={agent.lifecycle} posture={agent.posture} />
                      {agent.attention !== "none" ? (
                        <StatusBadge className="state-chip" kind="attention" value={agent.attention}>
                          {agent.attention}
                        </StatusBadge>
                      ) : null}
                    </div>
                  </div>
                  <p className="agent-card-summary">{agent.focusSummary}</p>
                  <section className="agent-card-work" title={agent.currentWork?.objective ?? "No active work item"}>
                    <span>Current work</span>
                    <strong>{agent.currentWork?.objective ?? "Ready for new operator input"}</strong>
                  </section>
                  <dl className="agent-detail-list">
                    <div title={agent.workspace}>
                      <dt>Workspace</dt>
                      <dd>{agent.workspace}</dd>
                    </div>
                    <div>
                      <dt>Posture</dt>
                      <dd>{agent.posture}</dd>
                    </div>
                    <div title={agent.model}>
                      <dt>Model</dt>
                      <dd>{agent.model}</dd>
                    </div>
                  </dl>
                  <footer>
                    <span>{agent.footer}</span>
                    <Button size="icon" variant="secondary" aria-label={`Open ${agent.id}`} onClick={() => onOpenAgent(agent.id)}>
                      →
                    </Button>
                  </footer>
                </Card>
              ))}
            </div>
          )}
        </section>
      </div>
    </section>
  );
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
  const copy: Record<DashboardState, { label: string; title: string; body: string }> = {
    loading: {
      label: "Connecting",
      title: "Loading runtime snapshot",
      body: "Waiting for the local runtime before showing agent data.",
    },
    empty: {
      label: "Empty",
      title: "Runtime is reachable, but no agents are visible",
      body: "This is an empty runtime state, not a loading state.",
    },
    disconnected: {
      label: "Disconnected",
      title: "Runtime API is not reachable",
      body: "No bundled preview data is shown while disconnected. Check the API base URL or restart the Holon daemon.",
    },
    preview: {
      label: "Preview",
      title: "Showing bundled preview data",
      body: "Configure or reach the local Holon API to switch this dashboard to live data.",
    },
  };
  const selected = copy[state];

  return (
    <aside className={`dashboard-state ${state}`} role="status">
      <StatusBadge kind="runtime" value={state}>
        {selected.label}
      </StatusBadge>
      <div>
        <strong>{selected.title}</strong>
        <p>{selected.body}</p>
        <small>{detail}</small>
      </div>
    </aside>
  );
}
