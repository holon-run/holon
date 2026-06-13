import { MarkdownContent } from "../../components/MarkdownContent";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { Skeleton } from "../../components/ui/Skeleton";
import { StatusChip } from "../../components/ui/StatusChip";
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
              <span className="eyebrow">Agents</span>
              <h1>Agent roster</h1>
              <p>{agentCountLabel} · local Holon runtime overview</p>
            </div>
            <div className="dashboard-actions">
              <span className={`connection-pill ${connection.source}`}>
                <span className="runtime-dot" />
                {connectionLabel}
              </span>
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
                    <div>
                      <strong>{agent.id}</strong>
                      <small>{agent.profile}</small>
                    </div>
                    <StatusChip className={`state-chip ${agent.lifecycle}`} tone={agent.lifecycle === "stopped" ? "muted" : "success"}>
                      {agent.lifecycle}
                    </StatusChip>
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
                      <div className="agent-card-brief" title={agent.lastBrief}>
                        <MarkdownContent text={agent.lastBrief} compact />
                      </div>
                    </div>
                  </div>
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
      title: "Using the last available preview surface",
      body: "The local runtime request failed. Live data will resume after the API is reachable.",
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
      <StatusChip tone={state === "disconnected" ? "error" : state === "loading" ? "syncing" : state}>
        {selected.label}
      </StatusChip>
      <div>
        <strong>{selected.title}</strong>
        <p>{selected.body}</p>
        <small>{detail}</small>
      </div>
    </aside>
  );
}
