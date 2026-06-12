import { useLayoutEffect, useRef, useState, type FormEvent } from "react";

import { filterTimelineByDisplayLevel } from "../../runtime/session-reducer";
import type { AgentDetail, AgentSummary, AgentTimelineItem, DisplayLevel } from "../../runtime/types";

interface AgentPageProps {
  agent: AgentSummary;
  detail: AgentDetail | null;
  displayLevel: DisplayLevel;
  loading: boolean;
  sendingPrompt: boolean;
  hasOlderEvents: boolean;
  loadingOlderEvents: boolean;
  promptError?: string;
  historyError?: string;
  onRefresh: () => void;
  onLoadOlderEvents: () => Promise<void>;
  onSendPrompt: (text: string) => Promise<void>;
  onOpenInspector: () => void;
}

export function AgentPage({
  agent,
  detail,
  displayLevel,
  loading,
  sendingPrompt,
  hasOlderEvents,
  loadingOlderEvents,
  promptError,
  historyError,
  onRefresh,
  onLoadOlderEvents,
  onSendPrompt,
  onOpenInspector,
}: AgentPageProps) {
  const [prompt, setPrompt] = useState("");
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const preserveScrollRef = useRef<{ height: number; top: number } | null>(null);
  const stickToBottomRef = useRef(true);
  const activeAgent = detail?.agent ?? agent;
  const timeline = filterTimelineByDisplayLevel(detail?.timeline ?? fallbackTimeline(activeAgent), displayLevel);
  const trimmedPrompt = prompt.trim();
  const canSendPrompt = trimmedPrompt.length > 0 && !sendingPrompt;
  const newestTimelineItem = timeline[timeline.length - 1];
  const timelineVersion = `${timeline.length}:${newestTimelineItem?.id ?? ""}:${timeline[0]?.id ?? ""}:${detail?.events?.length ?? 0}:${hasOlderEvents}`;

  useLayoutEffect(() => {
    const list = messageListRef.current;
    if (!list) return;

    const preserved = preserveScrollRef.current;
    if (preserved) {
      list.scrollTop = list.scrollHeight - preserved.height + preserved.top;
      preserveScrollRef.current = null;
      return;
    }

    if (stickToBottomRef.current) {
      list.scrollTop = list.scrollHeight;
    }
  }, [timelineVersion]);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!canSendPrompt) return;
    try {
      await onSendPrompt(trimmedPrompt);
      setPrompt("");
    } catch {
      // Keep the draft in place; runtime-store exposes the user-facing error.
    }
  }

  function handleMessageListScroll() {
    const list = messageListRef.current;
    if (!list) return;
    stickToBottomRef.current = list.scrollHeight - list.scrollTop - list.clientHeight < 96;
  }

  async function handleLoadOlderEvents() {
    const list = messageListRef.current;
    if (list) {
      preserveScrollRef.current = { height: list.scrollHeight, top: list.scrollTop };
      stickToBottomRef.current = false;
    }
    try {
      await onLoadOlderEvents();
    } catch {
      preserveScrollRef.current = null;
    }
  }

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

          <div className="message-list" ref={messageListRef} onScroll={handleMessageListScroll}>
            {hasOlderEvents ? (
              <div className="history-loader">
                <button type="button" disabled={loadingOlderEvents} onClick={handleLoadOlderEvents}>
                  {loadingOlderEvents ? "Loading earlier…" : "Load earlier"}
                </button>
              </div>
            ) : null}
            {historyError ? (
              <div className="history-status" role="alert">
                {historyError}
              </div>
            ) : null}
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

          <form className="composer" aria-label={`Send operator input to ${activeAgent.id}`} onSubmit={handleSubmit}>
            <textarea
              rows={2}
              placeholder={`Send operator input to ${activeAgent.id}...`}
              value={prompt}
              disabled={sendingPrompt}
              onChange={(event) => setPrompt(event.target.value)}
            />
            {promptError ? (
              <div className="composer-status" role="alert">
                {promptError}
              </div>
            ) : null}
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
                <button className="send-button" type="submit" aria-label="Send" disabled={!canSendPrompt}>
                  {sendingPrompt ? "…" : "↑"}
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
      minDisplayLevel: "info",
      sourceIds: [`${agent.id}-fallback-brief`],
      debug: JSON.stringify(agent, null, 2),
    },
  ];
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
