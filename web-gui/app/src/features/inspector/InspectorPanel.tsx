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
        <section className="context-card current-work">
          <div className="context-head">
            <span className="eyebrow">Current work item</span>
            <span className="state-chip completed">{agent.currentWork?.state ?? "none"}</span>
          </div>
          <h2>{agent.currentWork?.objective ?? "No current work item"}</h2>
          <code>{agent.currentWork?.id ?? "ready"}</code>
        </section>
      </div>
    </aside>
  );
}
