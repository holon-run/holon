import type { AgentSummary, RightPanelView } from "../../runtime/types";
import { ActivityInspectorPanel, activityInspectorTitle } from "../inspector/ActivityInspectorPanel";
import { AgentOverviewPanel } from "./AgentOverviewPanel";

interface RightSidePanelProps {
  agent: AgentSummary;
  view?: RightPanelView;
  open: boolean;
  onShowAgentOverview: () => void;
  onClose: () => void;
}

export function RightSidePanel({ agent, view, open, onShowAgentOverview, onClose }: RightSidePanelProps) {
  const activeView = view?.agentId === agent.id ? view : { kind: "agent_overview" as const, agentId: agent.id };
  const title = activeView.kind === "activity_inspector" ? activityInspectorTitle(activeView.activity) : "Agent overview";

  return (
    <aside className="side-panel" aria-label="Context side panel" hidden={!open}>
      <div className="panel-header">
        <div>
          <span className="eyebrow">Context panel</span>
          <strong>{title}</strong>
        </div>
        <div className="panel-actions">
          {activeView.kind !== "agent_overview" ? (
            <button type="button" aria-label="Show agent overview" onClick={onShowAgentOverview}>
              Agent Overview
            </button>
          ) : null}
          <button type="button" aria-label="Close side panel" onClick={onClose}>
            ×
          </button>
        </div>
      </div>
      <div className="panel-body">
        {activeView.kind === "activity_inspector" ? (
          <ActivityInspectorPanel activity={activeView.activity} detailState={activeView.detailState} />
        ) : (
          <AgentOverviewPanel agent={agent} />
        )}
      </div>
    </aside>
  );
}
