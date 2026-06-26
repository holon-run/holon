import { useEffect, useState } from "react";

import type { AgentSummary, RightPanelView, SkillCatalogState, TaskDetailState, WorkItemDetailState, WorkItemSummary } from "../../runtime/types";
import type { TaskSummary } from "../../runtime/types";
import { ActivityInspectorPanel, activityInspectorTitle } from "../inspector/ActivityInspectorPanel";
import { AgentOverviewPanel, AgentSkillManagerPanel, TaskDetailPanel, WorkItemDetailPanel } from "./AgentOverviewPanel";
import { FileBrowserPanel } from "./FileBrowserPanel";

interface RightSidePanelProps {
  agent: AgentSummary;
  skillCatalog?: SkillCatalogState;
  availableSkillCatalog?: SkillCatalogState;
  skillCatalogLoading?: boolean;
  availableSkillCatalogLoading?: boolean;
  skillCatalogError?: string;
  workItemDetailsById?: Record<string, WorkItemDetailState>;
  taskDetailsById?: Record<string, TaskDetailState>;
  view?: RightPanelView;
  open: boolean;
  onLoadWorkItemDetail: (workItemId: string) => void;
  onOpenWorkItemDetail: (workItem: WorkItemSummary) => void;
  onOpenTask: (task: TaskSummary) => void;
  onRefreshAgentSkills: () => void;
  onRefreshAvailableSkills: () => void;
  onEnableAgentSkill: (name: string) => void;
  onDisableAgentSkill: (name: string) => void;
  onOpenSkill: (skillId: string) => void;
  onShowAgentOverview: () => void;
  onBrowseFiles: () => void;
  onClose: () => void;
}

export function RightSidePanel({
  agent,
  skillCatalog,
  availableSkillCatalog,
  skillCatalogLoading,
  availableSkillCatalogLoading,
  skillCatalogError,
  workItemDetailsById = {},
  taskDetailsById = {},
  view,
  open,
  onLoadWorkItemDetail,
  onOpenWorkItemDetail,
  onOpenTask,
  onRefreshAgentSkills,
  onRefreshAvailableSkills,
  onEnableAgentSkill,
  onDisableAgentSkill,
  onOpenSkill,
  onShowAgentOverview,
  onBrowseFiles,
  onClose,
}: RightSidePanelProps) {
  const [showSkillManager, setShowSkillManager] = useState(false);
  const activeView = view?.agentId === agent.id ? view : { kind: "agent_overview" as const, agentId: agent.id };
  const skillManagerActive = activeView.kind === "agent_overview" && showSkillManager;
  const title =
    skillManagerActive
      ? "Manage agent skills"
      : activeView.kind === "activity_inspector"
      ? activityInspectorTitle(activeView.activity)
      : activeView.kind === "work_item_detail"
        ? "Work item detail"
        : activeView.kind === "task_detail"
          ? "Task detail"
          : activeView.kind === "file_browser"
            ? "File browser"
          : "Agent overview";
  const detailState = activeView.kind === "work_item_detail" ? workItemDetailsById[activeView.workItem.id] : undefined;
  const detailWorkItem = activeView.kind === "work_item_detail" ? detailState?.workItem ?? activeView.workItem : undefined;
  const taskDetailState = activeView.kind === "task_detail" ? activeView.detailState ?? taskDetailsById[activeView.task.id] : undefined;

  useEffect(() => {
    setShowSkillManager(false);
  }, [agent.id, activeView.kind]);

  const openSkillManager = () => {
    setShowSkillManager(true);
    if (!availableSkillCatalogLoading && (availableSkillCatalog?.catalog.length ?? 0) === 0) {
      onRefreshAvailableSkills();
    }
  };

  return (
    <aside className="side-panel" aria-label="Context side panel" hidden={!open}>
      <div className="panel-header">
        <div>
          <span className="eyebrow">Context panel</span>
          <strong>{title}</strong>
        </div>
        <div className="panel-actions">
          {activeView.kind !== "agent_overview" || skillManagerActive ? (
            <button
              type="button"
              aria-label="Show agent overview"
              onClick={() => {
                setShowSkillManager(false);
                onShowAgentOverview();
              }}
            >
              Agent Overview
            </button>
          ) : null}
          <button type="button" aria-label="Close side panel" onClick={onClose}>
            ×
          </button>
        </div>
      </div>
      <div className="panel-body">
        {skillManagerActive ? (
          <AgentSkillManagerPanel
            skillCatalog={skillCatalog}
            availableSkillCatalog={availableSkillCatalog}
            skillCatalogLoading={skillCatalogLoading}
            availableSkillCatalogLoading={availableSkillCatalogLoading}
            onRefreshAvailableSkills={onRefreshAvailableSkills}
            onEnableAgentSkill={onEnableAgentSkill}
          />
        ) : activeView.kind === "activity_inspector" ? (
          <ActivityInspectorPanel activity={activeView.activity} detailState={activeView.detailState} />
        ) : activeView.kind === "work_item_detail" && detailWorkItem ? (
          <div className="inspector-stack">
            <WorkItemDetailPanel workItem={detailWorkItem} detailState={detailState} />
          </div>
        ) : activeView.kind === "task_detail" ? (
          <div className="inspector-stack">
            <TaskDetailPanel task={activeView.task} detailState={taskDetailState} />
          </div>
        ) : activeView.kind === "file_browser" ? (
          <FileBrowserPanel workspaceId={activeView.workspaceId} initialPath={activeView.initialPath} />
        ) : (
          <AgentOverviewPanel
            agent={agent}
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
          />
        )}
      </div>
    </aside>
  );
}
