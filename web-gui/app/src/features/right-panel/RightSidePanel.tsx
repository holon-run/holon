import { useEffect, useState } from "react";

import type { AgentSummary, RightPanelView, SkillCatalogState, WorkItemDetailState, WorkItemSummary } from "../../runtime/types";
import { ActivityInspectorPanel, activityInspectorTitle } from "../inspector/ActivityInspectorPanel";
import { AgentOverviewPanel, AgentSkillManagerPanel, WorkItemDetailPanel } from "./AgentOverviewPanel";

interface RightSidePanelProps {
  agent: AgentSummary;
  skillCatalog?: SkillCatalogState;
  availableSkillCatalog?: SkillCatalogState;
  skillCatalogLoading?: boolean;
  availableSkillCatalogLoading?: boolean;
  skillCatalogError?: string;
  workItemDetailsById?: Record<string, WorkItemDetailState>;
  view?: RightPanelView;
  open: boolean;
  onLoadWorkItemDetail: (workItemId: string) => void;
  onOpenWorkItemDetail: (workItem: WorkItemSummary) => void;
  onRefreshAgentSkills: () => void;
  onRefreshAvailableSkills: () => void;
  onEnableAgentSkill: (name: string) => void;
  onDisableAgentSkill: (name: string) => void;
  onShowAgentOverview: () => void;
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
  view,
  open,
  onLoadWorkItemDetail,
  onOpenWorkItemDetail,
  onRefreshAgentSkills,
  onRefreshAvailableSkills,
  onEnableAgentSkill,
  onDisableAgentSkill,
  onShowAgentOverview,
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
        : "Agent overview";
  const detailState = activeView.kind === "work_item_detail" ? workItemDetailsById[activeView.workItem.id] : undefined;
  const detailWorkItem = activeView.kind === "work_item_detail" ? detailState?.workItem ?? activeView.workItem : undefined;

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
        ) : (
          <AgentOverviewPanel
            agent={agent}
            skillCatalog={skillCatalog}
            skillCatalogLoading={skillCatalogLoading}
            skillCatalogError={skillCatalogError}
            onLoadWorkItemDetail={onLoadWorkItemDetail}
            onOpenWorkItemDetail={onOpenWorkItemDetail}
            onRefreshAgentSkills={onRefreshAgentSkills}
            onDisableAgentSkill={onDisableAgentSkill}
            onOpenSkillManager={openSkillManager}
          />
        )}
      </div>
    </aside>
  );
}
