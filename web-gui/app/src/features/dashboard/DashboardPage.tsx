import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";
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
  const { t } = useTranslation();
  const hasAgents = agents.length > 0;
  const hasConnectionError = Boolean(connection.error);
  const isBootstrapping = loading && !hasAgents;
  const isRefreshing = loading && hasAgents;
  const isPreview = connection.source === "fixture";
  const dashboardState = getDashboardState({ isBootstrapping, hasAgents, hasConnectionError, isPreview });
  const connectionLabel = connection.source === "http" ? t("dashboard.liveRuntime") : hasConnectionError ? t("dashboard.runtimeUnavailable") : t("dashboard.previewData");
  const agentCountLabel = t("dashboard.agentsCount", { count: agents.length });

  return (
    <section className="page dashboard-page" aria-label={t("dashboard.dashboardAria")}>
      <div className="page-inner dashboard-inner">
        <section className="dashboard-section dashboard-roster-section">
          <div className="section-head dashboard-head">
            <div>
              <span className="eyebrow">{t("dashboard.roster")}</span>
              <h2>{t("dashboard.agents")}</h2>
              <p>{t("dashboard.available", { count: agents.length })}</p>
            </div>
            <div className="dashboard-actions">
              <StatusBadge className={`connection-pill ${connection.source}`} kind="connection" value={connection.source}>
                <span className="runtime-dot" />
                {connectionLabel}
              </StatusBadge>
              <Button type="button" variant="secondary" disabled={loading} onClick={onRefresh}>
                {loading ? t("common.refreshing") : t("common.refresh")}
              </Button>
            </div>
          </div>

          {dashboardState ? <DashboardStateCard state={dashboardState} detail={connection.error ?? connection.summary} /> : null}

          {metrics.length > 0 ? (
            <div className="metric-strip" aria-label={t("dashboard.runtimeMetrics")}>
              {metrics.map((metric) => (
                <div className={`metric-card ${metric.tone ?? "default"}`} key={metric.label}>
                  <span>{t(metric.label)}</span>
                  <strong>{metric.value}</strong>
                </div>
              ))}
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
          ) : (
            <div className="agent-roster">
              {agents.map((agent) => (
                <Card className="agent-card" key={agent.id}>
                  <div className="agent-card-head">
                    <span className={`agent-badge ${agent.badgeTone ?? ""}`} style={agent.badgeHue != null && !agent.badgeTone ? ({ "--badge-hue": `${agent.badgeHue}` } as CSSProperties) : undefined}>{agent.badge}</span>
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
                  <section className="agent-card-work" title={agent.currentWork?.objective ?? t("dashboard.noActiveWork")}>
                    <span>{t("dashboard.currentWork")}</span>
                    <strong>{agent.currentWork?.objective ?? t("dashboard.readyForInput")}</strong>
                  </section>
                  <dl className="agent-detail-list">
                    <div title={agent.workspace}>
                      <dt>{t("dashboard.workspace")}</dt>
                      <dd>{agent.workspace}</dd>
                    </div>
                    <div>
                      <dt>{t("dashboard.posture")}</dt>
                      <dd>{agent.posture}</dd>
                    </div>
                    <div title={agent.model}>
                      <dt>{t("dashboard.model")}</dt>
                      <dd>{agent.model}</dd>
                    </div>
                  </dl>
                  <footer>
                    <span>{agent.footer}</span>
                    <Button size="icon" variant="secondary" aria-label={t("dashboard.openAgent", { id: agent.id })} onClick={() => onOpenAgent(agent.id)}>
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
  const { t } = useTranslation();
  const labelKey = state === "loading" ? "dashboard.stateLoading" : state === "empty" ? "dashboard.stateEmpty" : state === "disconnected" ? "dashboard.stateDisconnected" : "dashboard.statePreview";
  const titleKey = `${labelKey}Title`;
  const bodyKey = `${labelKey}Body`;

  return (
    <aside className={`dashboard-state ${state}`} role="status">
      <StatusBadge kind="runtime" value={state}>
        {t(labelKey)}
      </StatusBadge>
      <div>
        <strong>{t(titleKey)}</strong>
        <p>{t(bodyKey)}</p>
        <small>{detail}</small>
      </div>
    </aside>
  );
}
