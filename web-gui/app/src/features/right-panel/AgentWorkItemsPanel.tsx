import { useState } from "react";
import { useTranslation } from "react-i18next";

import { StatusBadge } from "../../components/ui/StatusChip";
import type { WorkItemSummary } from "../../runtime/types";
import { useVisibleAgentResourceRefresh, type AgentResourceReadOptions } from "../../runtime/useVisibleAgentResourceRefresh";
import { ObjectDetailRefreshFeedback } from "./ObjectDetailRefreshFeedback";

export const COMPLETED_WORK_ITEM_LIMIT = 50;

interface AgentWorkItemsPanelProps {
  agentId: string;
  incarnation?: number;
  workItems: WorkItemSummary[];
  activeWorkItemCount?: number;
  loadCompletedWorkItems: (agentId: string, options?: AgentResourceReadOptions) => Promise<WorkItemSummary[]>;
  onSelect: (workItem: WorkItemSummary) => void;
}

function WorkItemCard({ workItem, onSelect }: Pick<AgentWorkItemsPanelProps, "onSelect"> & { workItem: WorkItemSummary }) {
  const { t } = useTranslation();
  return (
    <button type="button" className={`inspector-list-item work-item-button${workItem.current ? " featured" : ""}`} onClick={() => onSelect(workItem)}>
      <div className="inspector-list-head">
        <strong>{workItem.objective}</strong>
        <StatusBadge className="state-chip" kind="work" value={workItem.readiness ?? workItem.state} />
      </div>
      <small>{[workItem.current ? t("status.current") : undefined, workItem.planStatus].filter(Boolean).join(" · ")}</small>
      <code>{workItem.id}</code>
    </button>
  );
}

export function AgentWorkItemsPanel(props: AgentWorkItemsPanelProps) {
  return <AgentWorkItemsPanelContent key={`${props.agentId}:${props.incarnation}`} {...props} />;
}

function AgentWorkItemsPanelContent({ agentId, workItems, activeWorkItemCount, loadCompletedWorkItems, onSelect }: AgentWorkItemsPanelProps) {
  const { t } = useTranslation();
  const [sectionOpen, setSectionOpen] = useState(true);
  const [completedOpen, setCompletedOpen] = useState(false);
  const resource = useVisibleAgentResourceRefresh(agentId, "completedWorkItems", sectionOpen && completedOpen, loadCompletedWorkItems);
  const active = workItems.filter((item) => item.state !== "completed");
  const current = active.filter((item) => item.current);
  const other = active.filter((item) => !item.current);
  const completed = resource.state?.data?.slice(0, COMPLETED_WORK_ITEM_LIMIT) ?? [];
  const count = activeWorkItemCount ?? active.length;
  return (
    <details className="context-card inspector-card collapsible-inspector-card current-work" open onToggle={(event) => {
      if (event.target === event.currentTarget) setSectionOpen(event.currentTarget.open);
    }}>
      <summary className="collapsible-inspector-summary">
        <span className="collapsible-inspector-title">
          <span className="eyebrow">{t("rightPanel.workItemsWithCount", { count })}</span>
        </span>
      </summary>
      <div className="collapsible-inspector-body">
        {count > active.length ? <p className="inspector-muted">{t("rightPanel.activeSnapshotTruncated", { shown: active.length, count })}</p> : null}
        {current.map((workItem) => <WorkItemCard key={workItem.id} workItem={workItem} onSelect={onSelect} />)}
        <div className="inspector-nested-stack">
          {other.map((workItem) => <WorkItemCard key={workItem.id} workItem={workItem} onSelect={onSelect} />)}
        </div>
        <details className="inspector-details-list" onToggle={(event) => {
          if (event.target === event.currentTarget) setCompletedOpen(event.currentTarget.open);
        }}>
          <summary>{t("rightPanel.recentCompletedWorkItems", { count: COMPLETED_WORK_ITEM_LIMIT })}</summary>
          {completedOpen ? (
            <div className="inspector-nested-stack">
              <button type="button" className="text-button" disabled={resource.state?.loading || resource.state?.refreshing} onClick={resource.refresh}>
                {t("rightPanel.refreshCompletedWorkItems")}
              </button>
              <ObjectDetailRefreshFeedback state={resource.state} />
              {resource.state?.loading ? <p className="inspector-muted">{t("rightPanel.loadingCompletedWorkItems")}</p> : null}
              {!resource.state?.loading && !resource.state?.error && !completed.length ? <p className="inspector-muted">{t("rightPanel.noCompletedWorkItems")}</p> : null}
              {completed.map((workItem) => <WorkItemCard key={workItem.id} workItem={workItem} onSelect={onSelect} />)}
            </div>
          ) : null}
        </details>
      </div>
    </details>
  );
}
