import { rememberPanelFileLocation } from "../../runtime/panel-preferences";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { CONVERSATION_MIN, PANEL_MIN } from "./panel-layout";

import type { AgentSummary, RightPanelView, RuntimeConnection, SkillCatalogState, TaskDetailState, ToolExecutionDetailState, WorkItemDetailState, WorkItemSummary } from "../../runtime/types";
import type { AgentControlAction, AgentDeletionStatus } from "../../runtime/types";
import type { AgentSessionState, TimelineEventsState } from "../../runtime/runtime-store";
import { getRuntimeTraceRevision, isRuntimeTraceEnabled, subscribeRuntimeTrace } from "../../runtime/runtime-trace";
import type { TaskSummary } from "../../runtime/types";
import { ArrowLeft, Maximize2, Minimize2, X } from "lucide-react";
import { ActivityInspectorPanel, activityInspectorTitle } from "../inspector/ActivityInspectorPanel";
import { AgentOverviewPanel, AgentSkillManagerPanel, ToolExecutionDetailPanel, WorkItemDetailPanel } from "./AgentOverviewPanel";
import { TaskDetailPanel } from "./TaskDetailPanel";
import { FileBrowserPanel, type FileBrowserSnapshot } from "./FileBrowserPanel";
import { RuntimeTracePanel } from "./RuntimeTracePanel";
import { TimelineEventsPanel } from "./TimelineEventsPanel";
import { useTranslation } from "react-i18next";

interface RightSidePanelProps {
  agent: AgentSummary;
  deletionStatus?: AgentDeletionStatus | null;
  connection: RuntimeConnection;
  skillCatalog?: SkillCatalogState;
  availableSkillCatalog?: SkillCatalogState;
  skillCatalogLoading?: boolean;
  availableSkillCatalogLoading?: boolean;
  skillCatalogError?: string;
  workItemDetailsById?: Record<string, WorkItemDetailState>;
  taskDetailsById?: Record<string, TaskDetailState>;
  toolExecutionDetailsById?: Record<string, ToolExecutionDetailState>;
  timelineEvents?: TimelineEventsState;
  session?: AgentSessionState;
  view?: RightPanelView;
  open: boolean;
  mode: "normal" | "expanded";
  full: boolean;
  width: number;
  onResize: (width: number, save?: boolean) => void;
  onToggleMode: () => void;
  onLoadWorkItemDetail: (workItemId: string) => void;
  onOpenWorkItemDetail: (workItem: WorkItemSummary) => void;
  onOpenTask: (task: TaskSummary) => void;
  onRefreshAgentSkills: () => void;
  onRefreshAvailableSkills: () => void;
  onEnableAgentSkill: (name: string) => void;
  onDisableAgentSkill: (name: string) => void;
  onOpenSkill: (skillId: string) => void;
  onShowAgentOverview: () => void;
  onRefreshTimelineEvents: () => void;
  onLoadOlderTimelineEvents: () => void;
  onNavigateBack: () => void;
  onSelectView: (view: RightPanelView) => void;
  onBrowseFiles: (location: import("../../runtime/types").WorkspaceFileLocation) => void;
  onOpenPlanFile?: (location: import("../../runtime/types").WorkspaceFileLocation) => void;
  onControlAgent?: (action: AgentControlAction) => Promise<void>;
  onDeleteAgent?: (cascadePrivateChildren: boolean) => Promise<void>;
  onRenameAgent?: (name: string) => Promise<void>;
  onClose: () => void;
}

export function RightSidePanel({
  agent,
  deletionStatus,
  connection,
  skillCatalog,
  availableSkillCatalog,
  skillCatalogLoading,
  availableSkillCatalogLoading,
  skillCatalogError,
  workItemDetailsById = {},
  taskDetailsById = {},
  toolExecutionDetailsById = {},
  timelineEvents,
  session,
  view,
  open,
  mode,
  full,
  width,
  onResize,
  onToggleMode,
  onLoadWorkItemDetail,
  onOpenWorkItemDetail,
  onOpenTask,
  onRefreshAgentSkills,
  onRefreshAvailableSkills,
  onEnableAgentSkill,
  onDisableAgentSkill,
  onOpenSkill,
  onShowAgentOverview,
  onRefreshTimelineEvents,
  onLoadOlderTimelineEvents,
  onNavigateBack,
  onSelectView,
  onBrowseFiles,
  onOpenPlanFile,
  onControlAgent,
  onDeleteAgent,
  onRenameAgent,
  onClose,
}: RightSidePanelProps) {
  const { t } = useTranslation();
  const panelRef = useRef<HTMLElement>(null);
  const [dragging, setDragging] = useState(false);
  const returnFocus = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    if (open) {
      const focused = document.activeElement;
      if (focused instanceof HTMLElement && !panelRef.current?.contains(focused)) returnFocus.current = focused;
      panelRef.current?.focus({ preventScroll: true });
    } else if (returnFocus.current?.isConnected) {
      returnFocus.current.focus({ preventScroll: true });
    } else if (returnFocus.current) {
      document.querySelector<HTMLTextAreaElement>(".composer textarea")?.focus({ preventScroll: true });
    }
  }, [open]);
  useLayoutEffect(() => {
    if (full && !panelRef.current?.contains(document.activeElement)) panelRef.current?.focus({ preventScroll: true });
  }, [full]);

  const startResize = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      setDragging(true);
      const startX = e.clientX;
      const startWidth = width;
      let finalWidth = startWidth;
      const onMove = (ev: MouseEvent) => {
        const delta = startX - ev.clientX;
        finalWidth = Math.max(PANEL_MIN, Math.min(window.innerWidth - 72 - CONVERSATION_MIN, startWidth + delta));
        onResize(finalWidth);
      };
      const onUp = () => {
        setDragging(false);
        onResize(finalWidth, true);
        document.removeEventListener("mousemove", onMove);
        document.removeEventListener("mouseup", onUp);
      };
      document.addEventListener("mousemove", onMove);
      document.addEventListener("mouseup", onUp);
    },
    [onResize, width],
  );

  // Global shortcuts while the panel is open: Cmd/Ctrl+. toggles the expanded
  // overlay; Escape steps down the ladder expanded -> normal -> closed.
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.isComposing) return;
      if ((event.metaKey || event.ctrlKey) && event.key === ".") {
        event.preventDefault();
        onToggleMode();
        return;
      }
      if (event.key !== "Escape") return;
      // Let dialogs (e.g. modals) consume Escape first.
      if (document.querySelector('[role="dialog"]')) return;
      event.preventDefault();
      if (mode === "expanded") {
        onToggleMode();
      } else {
        onClose();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, mode, onToggleMode, onClose]);

  const [showSkillManager, setShowSkillManager] = useState(false);
  const [showRuntimeTrace, setShowRuntimeTrace] = useState(false);
  useSyncExternalStore(subscribeRuntimeTrace, getRuntimeTraceRevision, getRuntimeTraceRevision);
  const runtimeTraceEnabled = isRuntimeTraceEnabled();
  const activeView = view?.agentId === agent.id ? view : { kind: "agent_overview" as const, agentId: agent.id };
  const lastFile = useRef<Extract<RightPanelView, { kind: "file_browser" }>>(undefined);
  const lastDetail = useRef<RightPanelView>(undefined);
  const fileSnapshots = useRef(new Map<string, FileBrowserSnapshot>());
  const [fileTitle, setFileTitle] = useState<{ viewKey: string; path?: string }>();
  const bodyRef = useRef<HTMLDivElement>(null);
  const bodyPositions = useRef(new Map<string, { top: number; expanded: boolean[] }>());
  const viewKey = JSON.stringify(activeView.kind === "file_browser" ? activeView : {
    kind: activeView.kind,
    id: activeView.kind === "tool_execution_detail" ? activeView.toolExecutionId
      : activeView.kind === "task_detail" ? activeView.task.id
      : activeView.kind === "work_item_detail" ? activeView.workItem.id
      : activeView.kind === "activity_inspector" ? activeView.activity.id : agent.id,
  });
  useLayoutEffect(() => {
    if (activeView.kind === "file_browser") lastFile.current = activeView;
    else if (activeView.kind !== "agent_overview") lastDetail.current = activeView;
  }, [activeView]);
  useLayoutEffect(() => {
    const node = bodyRef.current;
    if (!node || !open) return;
    const restore = bodyPositions.current.get(viewKey);
    node.scrollTop = restore?.top ?? 0;
    node.querySelectorAll("details").forEach((item, i) => { if (restore?.expanded[i] !== undefined) item.open = restore.expanded[i]; });
    return () => {
      bodyPositions.current.set(viewKey, { top: node.scrollTop, expanded: Array.from(node.querySelectorAll("details"), (item) => item.open) });
      if (bodyPositions.current.size > 32) bodyPositions.current.delete(bodyPositions.current.keys().next().value!);
    };
  }, [viewKey, open]);
  const workspace = agent.workspaceSummary;
  const workspaces = agent.attachedWorkspaces?.length ? agent.attachedWorkspaces : workspace?.id ? [{ workspaceId: workspace.id, name: workspace.name ?? workspace.id, executionRootId: workspace.executionRootId ?? undefined }] : [];
  const openFiles = () => {
    if (lastFile.current) onSelectView(lastFile.current);
    else if (workspace?.id) onBrowseFiles({ workspaceId: workspace.id, path: "", executionRootId: workspace.executionRootId });
    else if (workspaces[0]) onBrowseFiles({ workspaceId: workspaces[0].workspaceId, path: "", executionRootId: workspaces[0].executionRootId });
  };
  const skillManagerActive = activeView.kind === "agent_overview" && showSkillManager;
  const runtimeTraceActive = activeView.kind === "agent_overview" && showRuntimeTrace && runtimeTraceEnabled;
  const title =
    runtimeTraceActive
      ? t("runtimeTrace.title")
      : skillManagerActive
      ? t("rightPanel.manageSkills")
      : activeView.kind === "timeline_events"
      ? t("timelineEvents.title")
      : activeView.kind === "activity_inspector"
      ? activityInspectorTitle(activeView.activity)
      : activeView.kind === "work_item_detail"
        ? t("rightPanel.workItemDetail")
        : activeView.kind === "task_detail"
          ? t("rightPanel.taskDetail")
          : activeView.kind === "tool_execution_detail"
            ? (toolExecutionDetailsById[activeView.toolExecutionId]?.toolExecution?.tool_name ?? activeView.toolName ?? t("inspector.toolExecution"))
          : activeView.kind === "file_browser"
            ? ((fileTitle?.viewKey === viewKey ? fileTitle.path : activeView.initialFilePath)?.split("/").pop() ?? t("rightPanel.fileBrowser"))
          : t("panel.agentOverview");
  const detailState = activeView.kind === "work_item_detail" ? workItemDetailsById[activeView.workItem.id] : undefined;
  const listedWorkItem = activeView.kind === "work_item_detail"
    ? agent.workItems?.find((wi) => wi.id === activeView.workItem.id)
      ?? activeView.workItem
    : undefined;
  // Keep live list status while filling in fields omitted from slim projections.
  const detailWorkItem = detailState?.workItem && listedWorkItem ? {
    ...listedWorkItem,
    planArtifact: detailState.workItem.planArtifact,
    todoList: detailState.workItem.todoList,
    workRefs: detailState.workItem.workRefs,
    resultSummary: listedWorkItem.resultSummary ?? detailState.workItem.resultSummary,
  } : detailState?.workItem ?? listedWorkItem;
  const taskDetailState = activeView.kind === "task_detail" ? activeView.detailState ?? taskDetailsById[activeView.task.id] : undefined;
  const toolExecutionDetailState = activeView.kind === "tool_execution_detail"
    ? toolExecutionDetailsById[activeView.toolExecutionId] ?? activeView.detailState
    : undefined;

  useEffect(() => {
    setShowSkillManager(false);
    setShowRuntimeTrace(false);
  }, [agent.id, activeView.kind]);

  useEffect(() => {
    if (!runtimeTraceEnabled) setShowRuntimeTrace(false);
  }, [runtimeTraceEnabled]);

  const openSkillManager = () => {
    setShowSkillManager(true);
    if (!availableSkillCatalogLoading && (availableSkillCatalog?.catalog.length ?? 0) === 0) {
      onRefreshAvailableSkills();
    }
  };

  return (
    <aside className="side-panel" data-mode={mode} data-full={full} tabIndex={-1} aria-label={t("rightPanel.contextPanel")} hidden={!open} ref={panelRef}>
      {open && !full ? (
        <div className="panel-resizer" role="separator" aria-orientation="vertical" aria-label={t("rightPanel.resizePanel")} tabIndex={0}
          aria-valuenow={width} aria-valuemin={PANEL_MIN} aria-valuemax={Math.max(PANEL_MIN, window.innerWidth - 72 - CONVERSATION_MIN)}
          onKeyDown={(e) => { if (e.key === "ArrowLeft" || e.key === "ArrowRight") { e.preventDefault(); onResize(Math.max(PANEL_MIN, Math.min(window.innerWidth - 72 - CONVERSATION_MIN, width + (e.key === "ArrowLeft" ? 32 : -32))), true); } }}
          data-dragging={dragging} onMouseDown={startResize} onDoubleClick={onToggleMode} />
      ) : null}
      <div className="panel-header">
        <div>
          <strong>{title}</strong>
        </div>
        <div className="panel-actions">
          {full ? <button type="button" onClick={onClose}>{t("rightPanel.backToConversation")}</button> : null}
          {activeView.kind !== "agent_overview" || skillManagerActive ? (
            <button
              type="button"
              aria-label={t("rightPanel.backToSource")}
              onClick={() => {
                setShowSkillManager(false);
                setShowRuntimeTrace(false);
                onNavigateBack();
              }}
            >
              <ArrowLeft size={14} />
            </button>
          ) : null}
        {activeView.kind === "agent_overview" && runtimeTraceEnabled ? (
            <button
              type="button"
              aria-label={t("runtimeTrace.open")}
              onClick={() => {
                setShowSkillManager(false);
                setShowRuntimeTrace((current) => !current);
              }}
            >
              {runtimeTraceActive ? t("rightPanel.agentOverview") : t("runtimeTrace.shortTitle")}
            </button>
          ) : null}
          <button
            type="button"
            aria-label={mode === "expanded" ? t("rightPanel.restorePanel") : t("rightPanel.expandPanel")}
            title={mode === "expanded" ? t("rightPanel.restorePanel") : t("rightPanel.expandPanel")}
            onClick={onToggleMode}
          >
            {mode === "expanded" ? <Minimize2 size={14} aria-hidden="true" /> : <Maximize2 size={14} aria-hidden="true" />}
          </button>
          <button type="button" aria-label={t("panel.closePanel")} onClick={onClose}>
            <X size={16} aria-hidden="true" />
          </button>
        </div>
      </div>
      <nav className="panel-sections" aria-label={t("rightPanel.contextPanel")}>
        <button type="button" aria-current={activeView.kind === "agent_overview" ? "page" : undefined} onClick={onShowAgentOverview}>{t("rightPanel.overviewTab")}</button>
        <button type="button" aria-current={activeView.kind === "file_browser" ? "page" : undefined} disabled={!lastFile.current && !workspaces.length} onClick={openFiles}>{t("fileBrowser.files")}</button>
        <button type="button" aria-current={activeView.kind !== "file_browser" && activeView.kind !== "agent_overview" ? "page" : undefined} disabled={!lastDetail.current && (activeView.kind === "file_browser" || activeView.kind === "agent_overview")} onClick={() => { if (lastDetail.current) onSelectView(lastDetail.current); }}>{t("rightPanel.detailTab")}</button>
      </nav>
      {activeView.kind === "file_browser" && workspaces.length > 1 ? <select className="panel-workspace-select" aria-label={t("rightPanel.workspaces")} value={JSON.stringify([activeView.workspaceId, activeView.executionRootId ?? null])}
        onChange={(event) => { const selected = workspaces.find((ws) => JSON.stringify([ws.workspaceId, ws.executionRootId ?? null]) === event.target.value); if (selected) onBrowseFiles({ workspaceId: selected.workspaceId, path: "", executionRootId: selected.executionRootId }); }}>
        {workspaces.map((ws) => <option key={JSON.stringify([ws.workspaceId, ws.executionRootId ?? null])} value={JSON.stringify([ws.workspaceId, ws.executionRootId ?? null])}>{ws.name}{workspaces.filter((other) => other.workspaceId === ws.workspaceId).length > 1 ? ` · ${ws.executionRootId ?? "root"}` : ""}</option>)}
      </select> : null}
      <div className="panel-body" ref={bodyRef} key={viewKey}>
        {open && (runtimeTraceActive ? (
          <RuntimeTracePanel agentId={agent.id} connection={connection} />
        ) : skillManagerActive ? (
          <AgentSkillManagerPanel
            skillCatalog={skillCatalog}
            availableSkillCatalog={availableSkillCatalog}
            skillCatalogLoading={skillCatalogLoading}
            availableSkillCatalogLoading={availableSkillCatalogLoading}
            onRefreshAvailableSkills={onRefreshAvailableSkills}
            onEnableAgentSkill={onEnableAgentSkill}
          />
        ) : activeView.kind === "timeline_events" ? (
          <TimelineEventsPanel
            agentId={agent.id}
            timelineEvents={timelineEvents}
            projection={session}
            onRefresh={onRefreshTimelineEvents}
            onLoadOlder={onLoadOlderTimelineEvents}
          />
        ) : activeView.kind === "activity_inspector" ? (
          <ActivityInspectorPanel activity={activeView.activity} detailState={activeView.detailState} />
        ) : activeView.kind === "work_item_detail" && detailWorkItem ? (
          <div className="inspector-stack">
            <WorkItemDetailPanel workItem={detailWorkItem} detailState={detailState} onOpenPlanFile={onOpenPlanFile} />
          </div>
        ) : activeView.kind === "task_detail" ? (
          <div className="inspector-stack">
            <TaskDetailPanel
              task={activeView.task}
              detailState={taskDetailState}
              agentId={agent.id}
              onOpenWorkItem={onOpenWorkItemDetail}
            />
          </div>
        ) : activeView.kind === "tool_execution_detail" ? (
          <div className="inspector-stack">
            <ToolExecutionDetailPanel toolExecutionId={activeView.toolExecutionId} toolName={activeView.toolName} detailState={toolExecutionDetailState} relatedStateObjectRef={activeView.relatedStateObjectRef} onOpenWorkItem={onOpenWorkItemDetail} onOpenTask={onOpenTask} onBrowseFiles={onBrowseFiles} />
          </div>
        ) : activeView.kind === "file_browser" ? (
          <FileBrowserPanel key={viewKey} workspaceId={activeView.workspaceId} executionRootId={activeView.executionRootId} initialPath={activeView.initialPath} initialFilePath={activeView.initialFilePath} initialFragment={activeView.fragment} workspaceLabel={workspaces.find((ws) => ws.workspaceId === activeView.workspaceId)?.name} onClose={onNavigateBack}
            snapshot={fileSnapshots.current.get(viewKey)} onSnapshot={(snapshot) => {
              if (snapshot.selectedFile?.loading || !snapshot.listing) return;
              rememberPanelFileLocation({ ...activeView, initialPath: snapshot.currentPath, initialFilePath: snapshot.selectedFile?.path, fragment: snapshot.fragment });
              if (fileTitle?.viewKey !== viewKey || fileTitle.path !== snapshot.selectedFile?.path) setFileTitle({ viewKey, path: snapshot.selectedFile?.path });
              fileSnapshots.current.delete(viewKey);
              fileSnapshots.current.set(viewKey, snapshot);
              while (fileSnapshots.current.size > 6) fileSnapshots.current.delete(fileSnapshots.current.keys().next().value!);
            }} />
        ) : (
          <AgentOverviewPanel
            agent={agent}
            deletionStatus={deletionStatus}
            skillCatalog={skillCatalog}
            availableSkillCatalog={availableSkillCatalog}
            skillCatalogLoading={skillCatalogLoading}
            skillCatalogError={skillCatalogError}
            onLoadWorkItemDetail={onLoadWorkItemDetail}
            onOpenWorkItemDetail={onOpenWorkItemDetail}
            onOpenTask={onOpenTask}
            onRefreshAgentSkills={onRefreshAgentSkills}
            onDisableAgentSkill={onDisableAgentSkill}
            onOpenSkill={onOpenSkill}
            onOpenSkillManager={openSkillManager}
            onBrowseFiles={onBrowseFiles}
            onControlAgent={onControlAgent}
            onDeleteAgent={onDeleteAgent}
            onRenameAgent={onRenameAgent}
          />
        ))}
      </div>
    </aside>
  );
}
