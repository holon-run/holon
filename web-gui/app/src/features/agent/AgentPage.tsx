import { useEffect, useLayoutEffect, useRef, useState, type FormEvent } from "react";

import { filterTimelineByDisplayLevel } from "../../runtime/session-reducer";
import type { AgentDetail, AgentSummary, AgentTimelineActivity, AgentTimelineItem, DisplayLevel } from "../../runtime/types";

interface AgentPageProps {
  agent: AgentSummary;
  detail: AgentDetail | null;
  displayLevel: DisplayLevel;
  sendingPrompt: boolean;
  hasOlderEvents: boolean;
  loadingOlderEvents: boolean;
  promptError?: string;
  historyError?: string;
  onLoadOlderEvents: () => Promise<void>;
  onSendPrompt: (text: string) => Promise<void>;
  onOpenInspector: () => void;
}

const DEFAULT_INFO_TIMELINE_ITEM_LIMIT = 12;
const HISTORY_PAGE_VISIBLE_INCREMENT = 80;
const TOP_SCROLL_THRESHOLD = 16;

export function AgentPage({
  agent,
  detail,
  displayLevel,
  sendingPrompt,
  hasOlderEvents,
  loadingOlderEvents,
  promptError,
  historyError,
  onLoadOlderEvents,
  onSendPrompt,
  onOpenInspector,
}: AgentPageProps) {
  const [prompt, setPrompt] = useState("");
  const [visibleInfoItemLimit, setVisibleInfoItemLimit] = useState(DEFAULT_INFO_TIMELINE_ITEM_LIMIT);
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const preserveScrollRef = useRef<{ height: number; top: number } | null>(null);
  const stickToBottomRef = useRef(true);
  const activeAgent = detail?.agent ?? agent;
  const timeline = filterTimelineByDisplayLevel(detail?.timeline ?? fallbackTimeline(activeAgent), displayLevel, {
    infoItemLimit: visibleInfoItemLimit,
  });
  const trimmedPrompt = prompt.trim();
  const canSendPrompt = trimmedPrompt.length > 0 && !sendingPrompt;
  const newestTimelineItem = timeline[timeline.length - 1];
  const timelineVersion = `${timeline.length}:${newestTimelineItem?.id ?? ""}:${timeline[0]?.id ?? ""}:${detail?.events?.length ?? 0}:${hasOlderEvents}`;

  useEffect(() => {
    setVisibleInfoItemLimit(DEFAULT_INFO_TIMELINE_ITEM_LIMIT);
  }, [activeAgent.id, displayLevel]);

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
      preserveScrollRef.current =
        list.scrollTop > TOP_SCROLL_THRESHOLD ? { height: list.scrollHeight, top: list.scrollTop } : null;
      stickToBottomRef.current = false;
    }
    if (displayLevel === "info") {
      setVisibleInfoItemLimit((limit) => limit + HISTORY_PAGE_VISIBLE_INCREMENT);
    }
    try {
      await onLoadOlderEvents();
    } catch {
      if (displayLevel === "info") {
        setVisibleInfoItemLimit((limit) => Math.max(DEFAULT_INFO_TIMELINE_ITEM_LIMIT, limit - HISTORY_PAGE_VISIBLE_INCREMENT));
      }
      preserveScrollRef.current = null;
    }
  }

  return (
    <section className="page agent-page" aria-label="Agent conversation">
      <div className="agent-workbench">
        <section className="conversation-pane">
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
            {timeline.map((item, index) => {
              const compactAssistant = item.kind === "assistant" && timeline[index - 1]?.kind === "assistant";
              return (
                <article className={`message ${item.kind}${compactAssistant ? " is-compact" : ""}`} key={item.id}>
                  <div className="bubble">
                    {!compactAssistant ? (
                      <div className="message-heading">
                        <span className="message-label">{item.label}</span>
                        {item.kind === "tool" || item.kind === "event" || item.kind === "system" ? (
                          <span className="message-inline-meta">
                            {formatTimelineMeta(item.meta, displayLevel)}
                          </span>
                        ) : null}
                      </div>
                    ) : null}
                    <p>{item.body}</p>
                    {item.detail ? (
                      <div className={`message-detail ${item.detail.tone ?? "data"}`}>
                        <span>{item.detail.label}</span>
                        <pre>{item.detail.text}</pre>
                      </div>
                    ) : null}
                  </div>
                  {displayLevel !== "info" && item.activities?.length ? (
                    <ActivityTrail activities={item.activities} displayLevel={displayLevel} onOpenInspector={onOpenInspector} />
                  ) : null}
                  {!compactAssistant ? (
                    <div className="message-meta">
                      <time>{formatDisplayTime(item.timestamp)}</time>
                      {item.kind === "tool" || item.kind === "event" || item.kind === "system" ? null : (
                        <span>{formatTimelineMeta(item.meta, displayLevel)}</span>
                      )}
                      {displayLevel !== "info" ? (
                        <button className="copy-action" type="button" onClick={onOpenInspector}>
                          inspect
                        </button>
                      ) : null}
                    </div>
                  ) : null}
                </article>
              );
            })}
            {timeline.length === 0 ? (
              <div className="conversation-empty">
                <strong>No visible messages</strong>
                <span>Switch to Verbose or Debug to inspect lower-level runtime events.</span>
              </div>
            ) : null}
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

function ActivityTrail({
  activities,
  displayLevel,
  onOpenInspector,
}: {
  activities: AgentTimelineActivity[];
  displayLevel: DisplayLevel;
  onOpenInspector: () => void;
}) {
  const visibleActivities = displayLevel === "debug" ? activities : activities.slice(-4);
  const hiddenCount = activities.length - visibleActivities.length;

  return (
    <div className="activity-trail" aria-label="Agent activity">
      {visibleActivities.map((activity) => (
        <div className={`activity-item ${activity.kind}`} key={activity.id}>
          <div className="activity-row">
            <span className="activity-label">{activity.label}</span>
            <span className="activity-body">{activity.body}</span>
            <time>{formatDisplayTime(activity.timestamp)}</time>
          </div>
          {displayLevel === "debug" ? (
            <div className="activity-meta">
              <span>{activity.meta}</span>
              <button className="copy-action" type="button" onClick={onOpenInspector}>
                inspect
              </button>
            </div>
          ) : null}
          {displayLevel === "debug" && activity.detail ? (
            <div className={`message-detail activity-detail ${activity.detail.tone ?? "data"}`}>
              <span>{activity.detail.label}</span>
              <pre>{activity.detail.text}</pre>
            </div>
          ) : null}
        </div>
      ))}
      {hiddenCount > 0 ? <div className="activity-more">+{hiddenCount} earlier activities</div> : null}
    </div>
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

function formatTimelineMeta(meta: string, displayLevel: DisplayLevel): string {
  if (displayLevel === "debug") return `${meta} · debug`;
  const parts = meta
    .split(" · ")
    .map((part) => part.trim())
    .filter((part) => part && !/^event #\d+$/i.test(part));
  if (displayLevel === "verbose") return parts.join(" · ") || meta.split(" · ")[0] || meta;
  return parts[0] || meta;
}
