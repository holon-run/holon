import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type { AgentSummary } from "../../runtime/types";

interface InspectorPanelProps {
  agent: AgentSummary;
  open: boolean;
  onClose: () => void;
}

export function InspectorPanel({ agent, open, onClose }: InspectorPanelProps) {
  return (
    <aside className="side-panel" aria-label="Object side panel" hidden={!open}>
      <div className="panel-header">
        <div>
          <span className="eyebrow">Inspector</span>
          <strong>WorkItem detail</strong>
        </div>
        <button type="button" aria-label="Close side panel" onClick={onClose}>
          ×
        </button>
      </div>
      <div className="panel-body">
        {agent.currentWork ? (
          <section className="context-card current-work">
            <div className="context-head">
              <span className="eyebrow">Current work item</span>
              <StatusBadge className="state-chip" kind="work" value={agent.currentWork.state} />
            </div>
            <h2>{agent.currentWork.objective}</h2>
            <code>{agent.currentWork.id}</code>
          </section>
        ) : (
          <EmptyState
            className="inspector-empty"
            icon="◎"
            title="No current work item"
            description="This agent is ready for operator input. WorkItem details will appear here once a task is active."
          />
        )}
      </div>
    </aside>
  );
}
