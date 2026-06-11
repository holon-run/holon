import type { AgentDetail, AgentSummary, AgentTimelineItem, DisplayLevel } from "../../runtime/types";

interface AgentPageProps {
  agent: AgentSummary;
  detail: AgentDetail | null;
  displayLevel: DisplayLevel;
  loading: boolean;
  onRefresh: () => void;
  onOpenInspector: () => void;
}

export function AgentPage({ agent, detail, displayLevel, loading, onRefresh, onOpenInspector }: AgentPageProps) {
  const activeAgent = detail?.agent ?? agent;
  const timeline = filterTimeline(detail?.timeline ?? fallbackTimeline(activeAgent), displayLevel);

  return (
    <section className="page agent-page" aria-label="Agent conversation">
      <div className="agent-workbench">
        <section className="conversation-pane">
          <div className="conversation-head">
            <div>
              <span className="eyebrow">Agent conversation</span>
              <h1>{activeAgent.id}</h1>
              <p>{activeAgent.postureReason}</p>
            </div>
            <div className="conversation-actions">
              {detail?.source === "http" && !detail.error ? (
                <span className="source-chip live">live</span>
              ) : (
                <span className="source-chip">fixture fallback</span>
              )}
              <button type="button" disabled={loading} onClick={onRefresh}>
                {loading ? "Refreshing…" : "Refresh"}
              </button>
            </div>
          </div>

          <div className="message-list">
            {timeline.map((item) => (
              <article className={`message ${item.kind}`} key={item.id}>
                <div className="bubble">
                  <span className="message-label">{item.label}</span>
                  <p>{item.body}</p>
                  {displayLevel === "debug" && item.debug ? <pre>{item.debug}</pre> : null}
                </div>
                <div className="message-meta">
                  <time>{formatDisplayTime(item.timestamp)}</time>
                  <span>{displayLevel === "info" ? item.meta.split(" · ")[0] : `${item.meta} · ${displayLevel}`}</span>
                  {displayLevel !== "info" ? (
                    <button className="copy-action" type="button" onClick={onOpenInspector}>
                      inspect
                    </button>
                  ) : null}
                </div>
              </article>
            ))}
          </div>

          <form className="composer" aria-label={`Send operator input to ${activeAgent.id}`}>
            <textarea rows={2} placeholder={`Send operator input to ${activeAgent.id}...`} />
            <div className="composer-toolbar">
              <div className="composer-left">
                <button type="button" aria-label="Attach">
                  ＋
                </button>
              </div>
              <div className="composer-right">
                <button className="model-button" type="button" onClick={onOpenInspector}>
                  {activeAgent.model}⌄
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

function fallbackTimeline(agent: AgentSummary): AgentTimelineItem[] {
  return [
    {
      id: `${agent.id}-fallback-brief`,
      kind: "assistant",
      label: "Latest brief",
      body: agent.lastBrief,
      timestamp: agent.lastTurnTime,
      meta: "brief",
      debug: JSON.stringify(agent, null, 2),
    },
  ];
}

function filterTimeline(items: AgentTimelineItem[], displayLevel: DisplayLevel): AgentTimelineItem[] {
  if (displayLevel === "debug") return items;
  if (displayLevel === "verbose") return items.filter((item) => item.kind !== "event" || item.label !== "runtime event");
  return items.filter((item) => item.kind === "operator" || item.kind === "assistant").slice(-12);
}

function formatDisplayTime(value: string): string {
  if (!value) return "—";
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value || "—";
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  }).format(parsed);
}
