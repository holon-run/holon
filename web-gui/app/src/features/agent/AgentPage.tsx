import type { AgentSummary, DisplayLevel } from "../../runtime/types";

interface AgentPageProps {
  agent: AgentSummary;
  displayLevel: DisplayLevel;
  onOpenInspector: () => void;
}

export function AgentPage({ agent, displayLevel, onOpenInspector }: AgentPageProps) {
  return (
    <section className="page agent-page" aria-label="Agent conversation">
      <div className="agent-workbench">
        <section className="conversation-pane">
          <div className="message-list">
            <article className="message assistant">
              <div className="bubble">
                <p>{agent.lastBrief}</p>
              </div>
              <div className="message-meta">
                <time>{agent.lastTurnTime}</time>
                <span>{displayLevel === "info" ? "brief" : `brief · ${displayLevel}`}</span>
                {displayLevel !== "info" ? (
                  <button className="copy-action" type="button" onClick={onOpenInspector}>
                    inspect
                  </button>
                ) : null}
              </div>
            </article>
          </div>

          <form className="composer" aria-label={`Send operator input to ${agent.id}`}>
            <textarea rows={2} placeholder={`Send operator input to ${agent.id}...`} />
            <div className="composer-toolbar">
              <div className="composer-left">
                <button type="button" aria-label="Attach">
                  ＋
                </button>
              </div>
              <div className="composer-right">
                <button className="model-button" type="button" onClick={onOpenInspector}>
                  {agent.model}⌄
                </button>
                <button className="send-button" type="submit" aria-label="Send">
                  ↑
                </button>
              </div>
            </div>
          </form>
        </section>
      </div>
    </section>
  );
}
