import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import holonMarkUrl from "../assets/holon-mark.png";
import { AgentPage } from "../features/agent/AgentPage";
import { Button } from "../components/ui/Button";
import { EmptyState } from "../components/ui/EmptyState";
import { SegmentedControl, SegmentedControlButton } from "../components/ui/SegmentedControl";
import { StatusBadge } from "../components/ui/StatusChip";
import { DashboardPage } from "../features/dashboard/DashboardPage";
import { RightSidePanel } from "../features/right-panel/RightSidePanel";
import { SearchPage } from "../features/search/SearchPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { SkillDetailPage, SkillsPage } from "../features/skills/SkillsPage";
import { TemplateDetailPage, TemplatesPage } from "../features/templates/TemplatesPage";
import { deriveAgentDisplayStatus } from "../runtime/agent-status";
import { selectSelectedAgent } from "../runtime/runtime-selectors";
import { canUseRemoteRuntimeConnections, readStoredRemoteConnectionProfiles, useRuntimeStore } from "../runtime/runtime-store";
import { useAgentDetail } from "../runtime/useAgentDetail";
import { useRuntimeDashboard } from "../runtime/useRuntimeDashboard";
import type { AgentSummary, DisplayLevel, RouteKey, RuntimeConnection, RuntimeConnectionConfig, RuntimeConnectionProfile } from "../runtime/types";
import { pushBrowserRoute, routeFromLocation } from "./routes";

const globalRoutes: Array<{ key: RouteKey; labelKey: string; icon: string }> = [
  { key: "dashboard", labelKey: "nav.dashboard", icon: "◎" },
  { key: "search", labelKey: "nav.search", icon: "⌕" },
  { key: "skills", labelKey: "nav.skills", icon: "◇" },
  { key: "templates", labelKey: "nav.templates", icon: "▣" },
  { key: "settings", labelKey: "nav.settings", icon: "⚙" },
];

const APP_WINDOW_TITLE = "Holon";

export function App() {
  const { bootstrap, loading, refresh } = useRuntimeDashboard();
  const { t } = useTranslation();
  const [showCreateAgentModal, setShowCreateAgentModal] = useState(false);
  const [createAgentId, setCreateAgentId] = useState("");
  const [createAgentTemplate, setCreateAgentTemplate] = useState("");
  const [createAgentError, setCreateAgentError] = useState<string | undefined>();
  const [createAgentBusy, setCreateAgentBusy] = useState(false);
  const route = useRuntimeStore((state) => state.route);
  const selectedAgentId = useRuntimeStore((state) => state.selectedAgentId);
  const selectedSkillId = useRuntimeStore((state) => state.selectedSkillId);
  const selectedTemplateId = useRuntimeStore((state) => state.selectedTemplateId);
  const displayLevel = useRuntimeStore((state) =>
    state.displayLevelsByAgentId[selectedAgentId] ?? "info",
  );
  const rightPanelOpen = useRuntimeStore((state) => state.rightPanelOpen);
  const rightPanelView = useRuntimeStore((state) => state.rightPanelView);
  const navCollapsed = useRuntimeStore((state) => state.navCollapsed);
  const setRoute = useRuntimeStore((state) => state.setRoute);
  const openAgent = useRuntimeStore((state) => state.openAgent);
  const openSkill = useRuntimeStore((state) => state.openSkill);
  const openTemplate = useRuntimeStore((state) => state.openTemplate);
  const setDisplayLevel = useRuntimeStore((state) => state.setDisplayLevel);
  const setRightPanelOpen = useRuntimeStore((state) => state.setRightPanelOpen);
  const inspectActivity = useRuntimeStore((state) => state.inspectActivity);
  const showAgentOverview = useRuntimeStore((state) => state.showAgentOverview);
  const showWorkItemDetail = useRuntimeStore((state) => state.showWorkItemDetail);
  const showTaskDetail = useRuntimeStore((state) => state.showTaskDetail);
  const showFileBrowser = useRuntimeStore((state) => state.showFileBrowser);
  const navigateBack = useRuntimeStore((state) => state.navigateBack);
  const toggleRightPanel = useRuntimeStore((state) => state.toggleRightPanel);
  const toggleNavCollapsed = useRuntimeStore((state) => state.toggleNavCollapsed);
  const setRuntimeConnection = useRuntimeStore((state) => state.setRuntimeConnection);
  const selectedAgent = useRuntimeStore(selectSelectedAgent);
  const rosterActivityByAgentId = useRuntimeStore((state) => state.rosterActivityByAgentId);
  const activeAgentId = route === "agent" ? selectedAgent?.id ?? selectedAgentId : undefined;
  const sidePanelAgentId = selectedAgent?.id ?? selectedAgentId;
  const selectedAgentSession = useRuntimeStore((state) =>
    sidePanelAgentId ? state.sessionsByAgentId[sidePanelAgentId] : undefined,
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
  const searchResultContentBySourceRef = useRuntimeStore((state) => state.searchResultContentBySourceRef);
  const searchResultContentLoadingBySourceRef = useRuntimeStore((state) => state.searchResultContentLoadingBySourceRef);
  const searchResultContentErrorBySourceRef = useRuntimeStore((state) => state.searchResultContentErrorBySourceRef);
  const skillCatalog = useRuntimeStore((state) => state.skillCatalog);
  const skillCatalogLoading = useRuntimeStore((state) => state.skillCatalogLoading);
  const skillCatalogError = useRuntimeStore((state) => state.skillCatalogError);
  const skillDetail = useRuntimeStore((state) =>
    selectedSkillId ? state.skillDetailById[selectedSkillId] : undefined,
  );
  const skillDetailLoading = useRuntimeStore((state) =>
    selectedSkillId ? state.skillDetailLoadingById[selectedSkillId] ?? false : false,
  );
  const skillDetailError = useRuntimeStore((state) =>
    selectedSkillId ? state.skillDetailErrorById[selectedSkillId] : undefined,
  );
  const templateCatalog = useRuntimeStore((state) => state.templateCatalog);
  const templateCatalogLoading = useRuntimeStore((state) => state.templateCatalogLoading);
  const templateCatalogError = useRuntimeStore((state) => state.templateCatalogError);
 const templateSyncInProgress = useRuntimeStore((state) => state.templateSyncInProgress);
 const templateSyncMessage = useRuntimeStore((state) => state.templateSyncMessage);
  const templateDetail = useRuntimeStore((state) =>
    selectedTemplateId ? state.templateDetailById[selectedTemplateId] : undefined,
  );
  const templateDetailLoading = useRuntimeStore((state) =>
    selectedTemplateId ? state.templateDetailLoadingById[selectedTemplateId] ?? false : false,
  );
  const templateDetailError = useRuntimeStore((state) =>
    selectedTemplateId ? state.templateDetailErrorById[selectedTemplateId] : undefined,
  );
  const addSkillToCatalog = useRuntimeStore((state) => state.addSkillToCatalog);
  const removeSkillFromCatalog = useRuntimeStore((state) => state.removeSkillFromCatalog);
  const skillInstallJobs = useRuntimeStore((state) => state.skillInstallJobs);
  const agentSkillCatalog = useRuntimeStore((state) =>
    sidePanelAgentId ? state.agentSkillCatalogByAgentId[sidePanelAgentId] : undefined,
  );
  const agentSkillCatalogLoading = useRuntimeStore((state) =>
    sidePanelAgentId ? state.agentSkillCatalogLoadingByAgentId[sidePanelAgentId] ?? false : false,
  );
  const agentSkillCatalogError = useRuntimeStore((state) =>
    sidePanelAgentId ? state.agentSkillCatalogErrorByAgentId[sidePanelAgentId] : undefined,
  );
  const runSearch = useRuntimeStore((state) => state.runSearch);
  const loadSearchResultContent = useRuntimeStore((state) => state.loadSearchResultContent);
  const refreshSkillCatalog = useRuntimeStore((state) => state.refreshSkillCatalog);
  const refreshSkillDetail = useRuntimeStore((state) => state.refreshSkillDetail);
  const refreshTemplateCatalog = useRuntimeStore((state) => state.refreshTemplateCatalog);
  const refreshTemplateDetail = useRuntimeStore((state) => state.refreshTemplateDetail);
  const installTemplate = useRuntimeStore((state) => state.installTemplate);
  const removeTemplate = useRuntimeStore((state) => state.removeTemplate);
  const syncTemplateRemoteSources = useRuntimeStore((state) => state.syncTemplateRemoteSources);
 const dismissTemplateDiagnostics = useRuntimeStore((state) => state.dismissTemplateDiagnostics);
 const dismissTemplateError = useRuntimeStore((state) => state.dismissTemplateError);
  const createAgentFromTemplate = useRuntimeStore((state) => state.createAgentFromTemplate);
  const refreshAgentSkillCatalog = useRuntimeStore((state) => state.refreshAgentSkillCatalog);
  const enableAgentSkill = useRuntimeStore((state) => state.enableAgentSkill);
  const disableAgentSkill = useRuntimeStore((state) => state.disableAgentSkill);
  const refreshModelCatalog = useRuntimeStore((state) => state.refreshModelCatalog);
  const refreshRuntimeConfig = useRuntimeStore((state) => state.refreshRuntimeConfig);
  const updateRuntimeConfig = useRuntimeStore((state) => state.updateRuntimeConfig);
  const credentialStore = useRuntimeStore((state) => state.credentialStore);
  const credentialStoreLoading = useRuntimeStore((state) => state.credentialStoreLoading);
  const refreshCredentialStore = useRuntimeStore((state) => state.refreshCredentialStore);
  const setCredential = useRuntimeStore((state) => state.setCredential);
  const deleteCredential = useRuntimeStore((state) => state.deleteCredential);
  const codexDeviceLogin = useRuntimeStore((state) => state.codexDeviceLogin);
  const startCodexDeviceLogin = useRuntimeStore((state) => state.startCodexDeviceLogin);
  const clearCodexDeviceLogin = useRuntimeStore((state) => state.clearCodexDeviceLogin);
  const sendOperatorPrompt = useRuntimeStore((state) => state.sendOperatorPrompt);
  const setAgentModel = useRuntimeStore((state) => state.setAgentModel);
  const clearAgentModel = useRuntimeStore((state) => state.clearAgentModel);
  const loadOlderAgentEvents = useRuntimeStore((state) => state.loadOlderAgentEvents);
  const loadAgentWorkItemDetail = useRuntimeStore((state) => state.loadAgentWorkItemDetail);
  const loadAgentTaskDetail = useRuntimeStore((state) => state.loadAgentTaskDetail);
  const {
    detail: selectedAgentDetail,
    loading: agentDetailLoading,
    refresh: refreshAgentDetail,
  } = useAgentDetail(activeAgentId, displayLevel);
  const activeAgent = selectedAgent ?? selectedAgentDetail?.agent;
  const selectedAgentLiveStatus = selectedAgentSession?.liveStatus ?? "idle";
  const selectedAgentLiveTitle = liveStatusTitle(selectedAgentLiveStatus, t, selectedAgentSession?.lastStreamActivityAt, selectedAgentSession?.error);
  const selectedAgentStatus = route === "agent" && activeAgent ? deriveAgentDisplayStatus(activeAgent, t) : undefined;
  const selectedAgentContext = selectedAgentStatus?.label ?? "loading agent";
  const selectedAgentSourceStatus =
    agentDetailLoading && !selectedAgentDetail
      ? "syncing"
      : selectedAgentDetail?.source === "http" && !selectedAgentDetail.error
        ? "live"
        : "preview";
  const agentTopControls =
    route === "agent" ? (
      <div className="agent-top-controls" aria-label={t("app.conversationControls")}>
        <div className="agent-stream-controls" aria-label={t("app.streamStatus")}>
          <StatusBadge
            kind="connection"
            value={selectedAgentSourceStatus}
            className={`source-chip ${selectedAgentSourceStatus}`}
            data-tooltip-pos="bottom"
          />
          <StatusBadge
            kind="stream"
            value={selectedAgentLiveStatus}
            className={`source-chip live-status ${selectedAgentLiveStatus}`}
            title={selectedAgentLiveTitle}
            data-tooltip-pos="bottom"
          />
          <Button
            type="button"
            size="icon"
            variant="ghost"
            aria-label={agentDetailLoading ? t("app.refreshingAgent") : t("app.refreshAgent")}
            title={agentDetailLoading ? t("common.refreshing") : t("app.refreshAgent")}
            disabled={agentDetailLoading}
            onClick={() => void refreshAgentDetail()}
          >
            ↻
          </Button>
        </div>
        <SegmentedControl className="display-level" label={t("app.displayLevel")}>
          {(["info", "verbose", "debug"] as const).map((level) => (
            <SegmentedControlButton
              active={displayLevel === level}
              className={displayLevel === level ? "is-active" : ""}
              key={level}
              type="button"
              onClick={() => setDisplayLevel(level, activeAgentId)}
            >
              {level === "info" ? t("app.info") : level === "verbose" ? t("app.verbose") : t("app.debug")}
            </SegmentedControlButton>
          ))}
        </SegmentedControl>
      </div>
    ) : null;
  const isInitialBootstrapping = loading && bootstrap.agents.length === 0 && !bootstrap.connection.error;

  useLayoutEffect(() => {
    const applyBrowserRoute = () => {
      const nextRoute = routeFromLocation(window.location);
      if (nextRoute.route === "agent" && nextRoute.agentId) {
        openAgent(nextRoute.agentId, nextRoute.eventSeq);
        return;
      }
      if (nextRoute.route === "skillDetail" && nextRoute.skillId) {
        openSkill(nextRoute.skillId);
        return;
      }
      if (nextRoute.route === "templateDetail" && nextRoute.templateId) {
        openTemplate(nextRoute.templateId);
        return;
      }
      setRoute(nextRoute.route);
    };

    applyBrowserRoute();
    window.addEventListener("popstate", applyBrowserRoute);
    return () => window.removeEventListener("popstate", applyBrowserRoute);
  }, [openAgent, openSkill, openTemplate, setRoute]);

  useEffect(() => {
    if ((route !== "agent" && route !== "settings") || modelCatalogLoading || modelCatalog.options.length > 0) return;
    void refreshModelCatalog();
  }, [modelCatalog.options.length, modelCatalogLoading, refreshModelCatalog, route]);

  useEffect(() => {
    if (route !== "settings" || runtimeConfigLoading || runtimeConfig.surface) return;
    void refreshRuntimeConfig();
  }, [refreshRuntimeConfig, route, runtimeConfig.surface, runtimeConfigLoading]);

  useEffect(() => {
    if (route !== "skills" || skillCatalogLoading || skillCatalog.source !== "fixture") return;
    void refreshSkillCatalog();
  }, [refreshSkillCatalog, route, skillCatalog.source, skillCatalogLoading]);

  useEffect(() => {
    if (route !== "skillDetail" || !selectedSkillId || skillDetailLoading || skillDetail) return;
    void refreshSkillDetail(selectedSkillId);
  }, [refreshSkillDetail, route, selectedSkillId, skillDetail, skillDetailLoading]);

  useEffect(() => {
    if ((route !== "templates" && route !== "templateDetail") || templateCatalogLoading || templateCatalog.source !== "fixture") return;
    void refreshTemplateCatalog();
  }, [refreshTemplateCatalog, route, templateCatalog.source, templateCatalogLoading]);

  useEffect(() => {
    if (route !== "templateDetail" || !selectedTemplateId || templateDetailLoading || templateDetail) return;
    void refreshTemplateDetail(selectedTemplateId);
  }, [refreshTemplateDetail, route, selectedTemplateId, templateDetail, templateDetailLoading]);

  useEffect(() => {
    if (!sidePanelAgentId || agentSkillCatalogLoading || agentSkillCatalog) return;
    void refreshAgentSkillCatalog(sidePanelAgentId);
  }, [agentSkillCatalog, agentSkillCatalogLoading, refreshAgentSkillCatalog, sidePanelAgentId]);

  useEffect(() => {
    document.title = browserWindowTitle(bootstrap.connection);
  }, [bootstrap.connection.baseUrl, bootstrap.connection.mode]);

  function navigateRoute(nextRoute: RouteKey) {
    setRoute(nextRoute);
    pushBrowserRoute(nextRoute, selectedAgentId);
  }

  function navigateSkill(skillId: string) {
    openSkill(skillId);
    pushBrowserRoute("skillDetail", skillId);
  }

  function navigateTemplate(catalogId: string) {
    openTemplate(catalogId);
    pushBrowserRoute("templateDetail", undefined, catalogId);
  }

  function navigateAgent(agentId: string, eventSeq?: number) {
    openAgent(agentId, eventSeq);
    pushBrowserRoute("agent", agentId, undefined, eventSeq == null ? undefined : { event_seq: eventSeq });
  }

  async function handleCreateAgentSubmit(): Promise<void> {
    const id = createAgentId.trim();
    const tmpl = createAgentTemplate.trim();
    if (!id || !tmpl) return;
    setCreateAgentBusy(true);
    setCreateAgentError(undefined);
    try {
      const ok = await createTemplateAgent(id, tmpl);
      if (ok) {
        setCreateAgentId("");
        setCreateAgentTemplate("");
        setShowCreateAgentModal(false);
      } else {
        setCreateAgentError("Failed to create agent.");
      }
    } catch (error) {
      setCreateAgentError(error instanceof Error ? error.message : String(error));
    } finally {
      setCreateAgentBusy(false);
    }
  }

  async function addTemplateRemoteSource(sourceId: string, url: string, gitRef?: string): Promise<boolean> {
    try {
      const configValue = JSON.stringify({ url, ref: gitRef ?? null });
      const result = await updateRuntimeConfig([{ key: `agent_templates.remote_sources.${sourceId}`, value: configValue }]);
      if (result) {
        await refreshRuntimeConfig();
        await syncTemplateRemoteSources();
        await refreshTemplateCatalog();
        return true;
      }
      return false;
    } catch {
      return false;
    }
  }

  async function removeTemplateRemoteSource(sourceId: string): Promise<boolean> {
    try {
      const result = await updateRuntimeConfig([{ key: `agent_templates.remote_sources.${sourceId}`, unset: true }]);
      if (result) {
        await refreshRuntimeConfig();
        await refreshTemplateCatalog();
        return true;
      }
      return false;
    } catch {
      return false;
    }
  }

  async function createTemplateAgent(agentId: string, template: string): Promise<boolean> {
    const ok = await createAgentFromTemplate(agentId, template);
    if (ok) navigateAgent(agentId);
    return ok;
  }

  if (isInitialBootstrapping) {
    return <BootstrappingPage connection={bootstrap.connection} onSetConnection={setRuntimeConnection} />;
  }

  return (
    <div
      className="app-shell"
      data-panel={rightPanelOpen ? "open" : "closed"}
      data-nav-collapsed={navCollapsed}
    >
      <aside className="sidebar" aria-label="Holon navigation">
        <div className="sidebar-brand">
          <a
            className="brand-link"
            href="https://holon.run"
            target="_blank"
            rel="noreferrer"
            aria-label={t("common.openWebsite")}
            title={t("common.openWebsite")}
          >
            <span className="brand-mark" aria-hidden="true">
              <img src={holonMarkUrl} alt="" />
            </span>
            <span className="brand-name">Holon</span>
          </a>
          <a
            className="brand-icon-link"
            href="https://github.com/holon-run/holon"
            target="_blank"
            rel="noreferrer"
            aria-label={t("common.openGithub")}
            title={t("common.openGithub")}
          >
            <svg aria-hidden="true" viewBox="0 0 16 16" width="16" height="16">
              <path
                fill="currentColor"
                fillRule="evenodd"
                d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82a7.6 7.6 0 0 1 4 0c1.53-1.03 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.28.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z"
                clipRule="evenodd"
              />
            </svg>
          </a>
          <button
            className="nav-collapse"
            type="button"
            aria-label={navCollapsed ? t("app.expandNav") : t("app.collapseNav")}
            title={navCollapsed ? t("app.expandNav") : t("app.collapseNav")}
            onClick={toggleNavCollapsed}
          >
            ‹
          </button>
        </div>

        <nav className="global-nav" aria-label="Global navigation">
          {globalRoutes.map((item) => (
            <button
              className={route === item.key ? "is-active" : ""}
              key={item.key}
              type="button"
              aria-label={t(item.labelKey)}
              title={t(item.labelKey)}
              onClick={() => navigateRoute(item.key)}
            >
              <span>{item.icon}</span>
              <strong>{t(item.labelKey)}</strong>
            </button>
          ))}
        </nav>

        <section className="side-section agent-switcher" aria-label={t("app.agents")}>
          <div className="side-heading">
            <span>{t("app.agents")}</span>
            <strong>{bootstrap.agents.length}</strong>
            <button
              className="side-heading-add"
              type="button"
              aria-label={t("app.createAgentTitle")}
              title={t("app.createAgentTitle")}
              onClick={async () => {
                if (templateCatalog.source === "fixture" && !templateCatalogLoading) {
                  await refreshTemplateCatalog();
                }
                setShowCreateAgentModal(true);
              }}
            >
              +
            </button>
          </div>
          {bootstrap.agents.length === 0 ? (
            <div className="agent-list-state" role="status">
              <strong>{loading ? t("status.syncing") : t("dashboard.noAgentsTitle")}</strong>
              <span>{loading ? t("boot.body") : t("dashboard.startAgent")}</span>
            </div>
          ) : (
            bootstrap.agents.map((agent) => {
              const status = deriveAgentDisplayStatus(agent, t);
              const workSummary = agent.currentWork?.objective;
              const unreadCount = rosterActivityByAgentId[agent.id]?.unreadCount ?? 0;

              return (
                <button
                  className={`agent-row ${selectedAgentId === agent.id ? "is-selected" : ""} ${agent.lifecycle}`}
                  key={agent.id}
                  title={`${agent.id} · ${agent.focusSummary} · ${status.title}`}
                  type="button"
                  onClick={() => navigateAgent(agent.id)}
                >
                  <span className={`agent-badge ${agent.badgeTone ?? ""}`} style={agent.badgeHue != null && !agent.badgeTone ? ({ "--badge-hue": `${agent.badgeHue}` } as CSSProperties) : undefined}>{agent.badge}</span>
                  <span className="agent-row-main">
                    <span className="agent-row-title">
                      <strong>{agent.id}</strong>
                      {unreadCount > 0 ? (
                        <span className="agent-row-unread" aria-label={t("app.unreadUpdates", { count: unreadCount })} title={t("app.unreadUpdates", { count: unreadCount })}>
                          {formatUnreadCount(unreadCount)}
                        </span>
                      ) : null}
                      <span className={`agent-row-status-dot ${status.tone}`} aria-label={status.title} title={status.title}>
                        {agentStatusIcon(status.tone)}
                      </span>
                    </span>
                    {workSummary ? (
                      <span className="agent-row-meta">
                        <span>{workSummary}</span>
                      </span>
                    ) : null}
                  </span>
                </button>
              );
            })
          )}
        </section>

        <div className="sidebar-bottom">
          <ConnectionSwitcher connection={bootstrap.connection} onSetConnection={setRuntimeConnection} />
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
                  aria-label={t("nav.dashboard")}
                  onClick={() => navigateRoute("dashboard")}
                >
                  ←
                </Button>
              ) : null}
              <div>
                <strong>{route === "agent" ? (selectedAgent?.id ?? selectedAgentId) || t("rightPanel.agent") : route === "search" ? t("nav.search") : route === "skills" || route === "skillDetail" ? t("nav.skills") : route === "templates" || route === "templateDetail" ? t("nav.templates") : route === "settings" ? t("nav.settings") : t("nav.dashboard")}</strong>
                <span title={route === "agent" ? selectedAgentStatus?.title : undefined}>
                  {route === "agent"
                    ? selectedAgentContext
                    : route === "search"
                      ? t("app.subtitleSearch")
                      : route === "skills" || route === "skillDetail"
                        ? t("app.subtitleSkills")
                        : route === "templates" || route === "templateDetail"
                          ? t("app.subtitleTemplates")
                          : route === "settings"
                            ? t("app.subtitleSettings")
                            : bootstrap.attentionCount > 0
                              ? t("app.subtitleDashboard", { count: bootstrap.agents.length, attention: bootstrap.attentionCount })
                              : t("app.subtitleDashboardAllClear", { count: bootstrap.agents.length })}
                </span>
              </div>
            </div>
            <div className="top-actions">
              {agentTopControls}
              <Button
                type="button"
                size="icon"
                variant="ghost"
                aria-label={t("rightPanel.contextPanel")}
                title={t("rightPanel.contextPanel")}
                onClick={toggleRightPanel}
              >
                ▭
              </Button>
            </div>
          </div>
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
            targetEventSeq={selectedAgentSession?.targetEventSeq}
            onRefreshModels={refreshModelCatalog}
            onSetModel={(model, reasoningEffort) => setAgentModel(activeAgent.id, model, displayLevel, reasoningEffort)}
            onClearModel={() => clearAgentModel(activeAgent.id, displayLevel)}
            onLoadOlderEvents={() => loadOlderAgentEvents(activeAgent.id, displayLevel)}
            onSendPrompt={(text) => sendOperatorPrompt(activeAgent.id, text, displayLevel)}
            onOpenInspector={() => {
              showAgentOverview(activeAgent.id);
            }}
            onInspectActivity={(activity) => inspectActivity(activeAgent.id, activity)}
            selectedActivityId={
              rightPanelView?.kind === "activity_inspector" && rightPanelView.agentId === activeAgent.id
                ? rightPanelView.activity.id
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
            resultContentBySourceRef={searchResultContentBySourceRef}
            resultContentLoadingBySourceRef={searchResultContentLoadingBySourceRef}
            resultContentErrorBySourceRef={searchResultContentErrorBySourceRef}
            onSearch={runSearch}
            onLoadResultContent={loadSearchResultContent}
            onOpenAgent={navigateAgent}
          />
        ) : null}
        {route === "skills" ? (
          <SkillsPage
            catalog={skillCatalog}
            loading={skillCatalogLoading}
            error={skillCatalogError}
            installJobs={skillInstallJobs}
            onRefresh={refreshSkillCatalog}
            onAddSkill={addSkillToCatalog}
            onRemoveSkill={removeSkillFromCatalog}
            onOpenSkill={navigateSkill}
          />
        ) : null}
        {route === "skillDetail" ? (
          <SkillDetailPage
            skillId={selectedSkillId}
            detail={skillDetail}
            loading={skillDetailLoading}
            error={skillDetailError}
            onBack={() => navigateRoute("skills")}
            onRefresh={() => refreshSkillDetail(selectedSkillId)}
          />
        ) : null}
        {route === "templates" ? (
          <TemplatesPage
            catalog={templateCatalog}
            loading={templateCatalogLoading}
           syncInProgress={templateSyncInProgress}
           syncMessage={templateSyncMessage}
            error={templateCatalogError}
            onRefresh={refreshTemplateCatalog}
            onSyncSources={syncTemplateRemoteSources}
            onInstallTemplate={installTemplate}
            onRemoveTemplate={removeTemplate}
            onOpenTemplate={navigateTemplate}
            onAddRemoteSource={addTemplateRemoteSource}
            onRemoveRemoteSource={removeTemplateRemoteSource}
           onDismissDiagnostics={dismissTemplateDiagnostics}
           onDismissError={dismissTemplateError}
          />
        ) : null}
        {route === "templateDetail" ? (
          <TemplateDetailPage
            catalogId={selectedTemplateId}
            detail={templateDetail}
            loading={templateDetailLoading}
            error={templateDetailError}
            onBack={() => navigateRoute("templates")}
            onRefresh={() => refreshTemplateDetail(selectedTemplateId)}
            onRemoveTemplate={removeTemplate}
           onCreateAgent={(template) => { setCreateAgentTemplate(template); setShowCreateAgentModal(true); }}
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
            credentialStore={credentialStore}
            credentialStoreLoading={credentialStoreLoading}
            onRefreshCredentialStore={refreshCredentialStore}
            onSetCredential={setCredential}
            onDeleteCredential={deleteCredential}
            codexDeviceLogin={codexDeviceLogin}
            onStartCodexDeviceLogin={startCodexDeviceLogin}
            onClearCodexDeviceLogin={clearCodexDeviceLogin}
          />
        ) : null}
      </main>

      {selectedAgent ? (
        <RightSidePanel
          agent={selectedAgent}
          skillCatalog={agentSkillCatalog}
          availableSkillCatalog={skillCatalog}
          skillCatalogLoading={agentSkillCatalogLoading}
          availableSkillCatalogLoading={skillCatalogLoading}
          skillCatalogError={agentSkillCatalogError}
          workItemDetailsById={selectedAgentSession?.workItemDetailsById ?? {}}
          taskDetailsById={selectedAgentSession?.taskDetailsById ?? {}}
          view={rightPanelView?.agentId === selectedAgent.id ? rightPanelView : undefined}
          open={rightPanelOpen}
          onLoadWorkItemDetail={(workItemId) => loadAgentWorkItemDetail(selectedAgent.id, workItemId)}
          onOpenWorkItemDetail={(workItem) => {
            showWorkItemDetail(selectedAgent.id, workItem);
            loadAgentWorkItemDetail(selectedAgent.id, workItem.id);
          }}
          onOpenTask={(task) => {
            showTaskDetail(selectedAgent.id, task);
            loadAgentTaskDetail(selectedAgent.id, task.id);
          }}
          onRefreshAgentSkills={() => refreshAgentSkillCatalog(selectedAgent.id)}
          onRefreshAvailableSkills={() => {
            void refreshSkillCatalog();
          }}
          onEnableAgentSkill={(name) => {
            void enableAgentSkill(selectedAgent.id, name);
          }}
          onDisableAgentSkill={(name) => {
            void disableAgentSkill(selectedAgent.id, name);
          }}
          onOpenSkill={navigateSkill}
          onShowAgentOverview={showAgentOverview}
          onNavigateBack={navigateBack}
          onBrowseFiles={(workspaceId: string, executionRootId?: string) => {
            showFileBrowser(selectedAgent.id, workspaceId, undefined, executionRootId);
          }}
          onOpenPlanFile={(workspaceId: string, filePath: string) => {
            showFileBrowser(selectedAgent.id, workspaceId, undefined, undefined, filePath);
          }}
          onClose={() => setRightPanelOpen(false)}
        />
      ) : null}

      {showCreateAgentModal ? (
        <div className="modal-overlay" role="dialog" aria-modal="true" aria-label={t("app.createAgentTitle")} onClick={(e) => { if (e.target === e.currentTarget) setShowCreateAgentModal(false); }}>
          <div className="modal-card">
            <div className="modal-head">
              <strong>{t("app.createAgentTitle")}</strong>
              <button type="button" className="modal-close" aria-label={t("common.close")} onClick={() => setShowCreateAgentModal(false)}>×</button>
            </div>
            <form className="modal-body" onSubmit={(e) => { e.preventDefault(); void handleCreateAgentSubmit(); }}>
              <label>
                <span>{t("app.agentId")}</span>
                <input
                  value={createAgentId}
                  onChange={(e) => setCreateAgentId(e.target.value)}
                  placeholder="new-agent-id"
                  autoFocus
                  disabled={createAgentBusy}
                />
              </label>
              <label>
                <span>{t("app.template")}</span>
                <select
                  value={createAgentTemplate}
                  onChange={(e) => setCreateAgentTemplate(e.target.value)}
                  disabled={createAgentBusy}
                >
                  <option value="">{t("app.chooseTemplate")}</option>
                  {templateCatalog.catalog.map((t) => (
                    <option key={t.catalogId} value={t.template}>
                      {t.name} ({t.source})
                    </option>
                  ))}
                </select>
              </label>
              {createAgentError ? <span className="connection-error" role="alert">{createAgentError}</span> : null}
              <div className="modal-actions">
                <Button type="button" variant="outline" disabled={createAgentBusy} onClick={() => setShowCreateAgentModal(false)}>
                  {t("common.cancel")}
                </Button>
                <Button type="submit" variant="accent" disabled={createAgentBusy || !createAgentId.trim() || !createAgentTemplate.trim()}>
                  {createAgentBusy ? t("common.creating") : t("common.create")}
                </Button>
              </div>
            </form>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function BootstrappingPage({
  connection,
  onSetConnection,
}: {
  connection: RuntimeConnection;
  onSetConnection: (config: RuntimeConnectionConfig) => Promise<void>;
}) {
  const { t } = useTranslation();
  return (
    <main className="boot-page" aria-label="Holon is loading">
      <section className="boot-card" role="status" aria-live="polite">
        <span className="boot-mark">◎</span>
        <div>
          <p>{t("boot.title")}</p>
          <h1>{t("boot.preparing")}</h1>
          <span>{t("boot.body")}</span>
          <ConnectionSwitcher connection={connection} onSetConnection={onSetConnection} compact={false} />
        </div>
      </section>
    </main>
  );
}

function ConnectionSwitcher({
  connection,
  onSetConnection,
  compact = true,
}: {
  connection: RuntimeConnection;
  onSetConnection: (config: RuntimeConnectionConfig) => Promise<void>;
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(!compact || Boolean(connection.error));
  const [baseUrl, setBaseUrl] = useState(connection.mode === "remote" ? connection.baseUrl ?? "" : "");
  const [token, setToken] = useState("");
  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | undefined>();
  const remoteConnectionsAllowed = canUseRemoteRuntimeConnections();
  const [savedRemotes, setSavedRemotes] = useState<RuntimeConnectionProfile[]>(() => readStoredRemoteConnectionProfiles());
  const switcherRef = useRef<HTMLDivElement>(null);
  const authRequired = Boolean(connection.authRequired);

  useEffect(() => {
    if (connection.mode === "remote") setBaseUrl(connection.baseUrl ?? "");
  }, [connection.baseUrl, connection.mode]);

  useEffect(() => {
    if (!compact || !open) return;
    const closeOnOutside = (event: MouseEvent) => {
      if (!switcherRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", closeOnOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", closeOnOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [compact, open]);

  function toggleOpen() {
    setOpen((value) => {
      const nextOpen = !value;
      if (nextOpen) {
        setSavedRemotes(readStoredRemoteConnectionProfiles());
        setFormError(undefined);
      }
      return nextOpen;
    });
  }

  async function applyConnection(config: RuntimeConnectionConfig) {
    setSaving(true);
    setFormError(undefined);
    try {
      await onSetConnection(config);
      setSavedRemotes(readStoredRemoteConnectionProfiles());
      if (compact && !connection.authRequired) setOpen(false);
    } catch (error) {
      setFormError(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className={`connection-switcher ${open ? "is-open" : ""} ${compact ? "is-popover" : ""}`} ref={switcherRef}>
      <button
        className="connection-status"
        type="button"
        aria-expanded={open}
        aria-haspopup={compact ? "dialog" : undefined}
        title={connection.baseUrl ?? connection.summary}
        onClick={toggleOpen}
      >
        <span className={`runtime-dot ${connection.error ? "error" : ""}`} />
        <span>
          <strong>{connection.mode}</strong>
          <small>{connection.summary}</small>
        </span>
      </button>
      {open ? (
        <form
          className="connection-panel"
          role={compact ? "dialog" : undefined}
          aria-label={t("connection.title")}
          onSubmit={(event) => {
            event.preventDefault();
            if (authRequired) {
              const trimmedToken = token.trim();
              if (!trimmedToken) {
                setFormError(t("connection.tokenRequired"));
                return;
              }
              const retryConfig: RuntimeConnectionConfig =
                connection.mode === "remote"
                  ? { mode: "remote", baseUrl: baseUrl.trim() || connection.baseUrl, token: trimmedToken }
                  : { mode: "local", token: trimmedToken };
              void applyConnection(retryConfig);
              return;
            }
            const trimmedBaseUrl = baseUrl.trim();
            if (!trimmedBaseUrl) {
              setFormError(t("connection.urlRequired"));
              return;
            }
            void applyConnection({ mode: "remote", baseUrl: trimmedBaseUrl });
          }}
        >
          <div className="connection-panel-head">
            <div>
              <strong>{t("connection.title")}</strong>
              <span>
                {authRequired
                  ? t("connection.authRequired")
                  : remoteConnectionsAllowed
                  ? t("connection.canSwitch")
                  : t("connection.locked")}
              </span>
            </div>
            {compact ? (
              <button type="button" aria-label={t("connection.closePanel")} onClick={() => setOpen(false)}>
                ×
              </button>
            ) : null}
          </div>
          <button
            className={`saved-remote-row ${connection.mode === "local" ? "is-selected" : ""}`}
            type="button"
            disabled={saving}
            onClick={() => void applyConnection({ mode: "local" })}
          >
            <span>
              <strong>{t("connection.localhost")}</strong>
              <small>{remoteConnectionsAllowed ? t("connection.localRuntime") : t("connection.sameOrigin")}</small>
            </span>
            <span>{connection.mode === "local" ? t("status.current") : t("status.use")}</span>
          </button>
          {remoteConnectionsAllowed ? (
            <>
              <div className="saved-remotes" aria-label={t("connection.savedRemotes")}>
                <span className="connection-section-label">{t("connection.savedRemotes")}</span>
                {savedRemotes.length > 0 ? (
                  savedRemotes.map((remote) => {
                    const selected = connection.mode === "remote" && connection.baseUrl === remote.baseUrl;
                    return (
                      <button
                        className={`saved-remote-row ${selected ? "is-selected" : ""}`}
                        type="button"
                        key={remote.baseUrl}
                        title={remote.baseUrl}
                        disabled={saving}
                        onClick={() => void applyConnection({ mode: "remote", baseUrl: remote.baseUrl })}
                      >
                        <span>
                          <strong>{remoteLabel(remote.baseUrl)}</strong>
                          <small>{remote.baseUrl}</small>
                        </span>
                        <span>{selected ? t("status.current") : remote.hasToken ? t("connection.token") : t("status.use")}</span>
                      </button>
                    );
                  })
                ) : (
                  <p className="saved-remotes-empty">{t("connection.noSavedRemotes")}</p>
                )}
              </div>
              <span className="connection-section-label">{t("connection.addRemote")}</span>
              <label>
                {t("connection.remoteUrl")}
                <input
                  value={baseUrl}
                  onChange={(event) => setBaseUrl(event.target.value)}
                  placeholder="http://192.168.1.10:7878"
                  inputMode="url"
                />
              </label>
            </>
          ) : (
            <p className="saved-remotes-empty">{t("connection.remoteOnly")}</p>
          )}
          {authRequired ? (
            <label>
              {t("connection.bearerToken")}
              <input
                value={token}
                onChange={(event) => setToken(event.target.value)}
                placeholder={connection.hasToken ? t("connection.replaceToken") : t("connection.pasteToken")}
                type="password"
                autoComplete="current-password"
                autoFocus
              />
            </label>
          ) : null}
          {formError ? <span className="connection-error">{formError}</span> : null}
          {remoteConnectionsAllowed || authRequired ? (
            <div className="connection-actions">
              <Button type="submit" size="sm" disabled={saving}>
                {saving ? t("connection.connecting") : authRequired ? t("connection.retryWithToken") : t("connection.useRemote")}
              </Button>
            </div>
          ) : null}
        </form>
      ) : null}
    </div>
  );
}

function remoteLabel(baseUrl: string): string {
  try {
    return new URL(baseUrl).host;
  } catch {
    return baseUrl;
  }
}

function browserWindowTitle(connection: RuntimeConnection): string {
  const runtimeLabel = browserRuntimeTitleLabel(connection);
  return runtimeLabel ? `${APP_WINDOW_TITLE} · ${runtimeLabel}` : APP_WINDOW_TITLE;
}

function browserRuntimeTitleLabel(connection: RuntimeConnection): string {
  const baseUrl = connection.baseUrl?.trim();
  const host = baseUrl ? browserHostForBaseUrl(baseUrl) : browserWindowHost();
  if (!host) return connection.mode === "remote" ? "remote" : "";
  return connection.mode === "remote" ? `remote ${host}` : host;
}

function browserHostForBaseUrl(baseUrl: string): string | undefined {
  try {
    const url = typeof window === "undefined" ? new URL(baseUrl) : new URL(baseUrl, window.location.href);
    return url.host || undefined;
  } catch {
    return undefined;
  }
}

function browserWindowHost(): string | undefined {
  if (typeof window === "undefined") return undefined;
  return window.location.host || window.location.hostname || undefined;
}

function MissingAgentPage({ agentId, loading }: { agentId: string; loading: boolean }) {
  const { t } = useTranslation();
  return (
    <section className="page agent-page" aria-label="Agent conversation">
      <div className="agent-workbench">
        <section className="conversation-pane">
          <EmptyState
            className="conversation-empty"
            title={loading ? t("app.loadingAgent") : t("app.agentNotFound")}
            description={agentId ? agentId : t("app.noAgentId")}
          />
        </section>
      </div>
    </section>
  );
}

function formatUnreadCount(count: number): string {
  return count > 99 ? "99+" : String(count);
}

function agentStatusIcon(tone: string): string {
  if (tone === "running") return "●";
  if (tone === "needs-input") return "!";
  if (tone === "waiting") return "◌";
  if (tone === "ready") return "✓";
  if (tone === "stopped") return "×";
  return "·";
}


function liveStatusLabel(status: string, t: TFunction): string {
  if (status === "connecting") return t("status.connecting");
  if (status === "streaming") return t("status.streaming");
  if (status === "reconnecting") return t("status.reconnecting");
  if (status === "recovering") return t("status.recovering");
  if (status === "stale") return t("status.stale");
  if (status === "error") return t("status.streamError");
  return t("status.idle");
}

function liveStatusTitle(status: string, t: TFunction, lastActivityAt?: string, error?: string): string {
  const parts = [liveStatusLabel(status, t)];
  if (lastActivityAt) parts.push(`${t("common.lastActivity")} ${formatRelativeTime(lastActivityAt, t)}`);
  if (error) parts.push(error);
  return parts.join(" · ");
}

function formatRelativeTime(value: string, t: TFunction): string {
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return value;
  const seconds = Math.max(0, Math.round((Date.now() - timestamp) / 1000));
  if (seconds < 60) return t("common.secondsAgo", { seconds });
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return t("common.minutesAgo", { minutes });
  return new Date(timestamp).toLocaleString();
}
