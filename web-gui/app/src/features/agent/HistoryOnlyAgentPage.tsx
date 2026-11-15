import { type SyntheticEvent, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { UseConversationSessionResult } from "../../runtime/useConversationSession";
import { ConversationSyncStatus, ConversationTimeline } from "./ConversationTimeline";
import { positionConversationAnchor, revealConversationAnchor } from "./conversation-location";

/** No AgentSummary, State projection, or business mutation callbacks enter this shell. */
export function HistoryOnlyAgentPage({ agentId, session, onRetryAccess, onReturnToLatest }: {
  agentId: string;
  session: UseConversationSessionResult;
  onRetryAccess: () => void;
  onReturnToLatest: () => void;
}) {
  const { t } = useTranslation();
  const { location } = session;
  const timelineRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const [positionedRequest, setPositionedRequest] = useState<number | null>(null);
  useLayoutEffect(() => {
    setPositionedRequest(null);
    for (const element of timelineRef.current?.querySelectorAll<HTMLElement>("[data-location-target]") ?? []) {
      delete element.dataset.locationTarget;
    }
  }, [location?.requestId]);
  useLayoutEffect(() => {
    const timeline = timelineRef.current;
    const content = contentRef.current;
    if (!timeline || !content || location?.kind !== "ready" || !location.anchor) return;
    const position = () => {
      const element = revealConversationAnchor(timeline, location.anchor!);
      if (!element || element.dataset.locationTarget === "true"
        || !positionConversationAnchor(timeline, element)) return;
      element.dataset.locationTarget = "true";
      setPositionedRequest(location.requestId);
    };
    position();
    const observer = new ResizeObserver(position);
    observer.observe(content);
    const frame = window.requestAnimationFrame(position);
    return () => { observer.disconnect(); window.cancelAnimationFrame(frame); };
  }, [location, session.model, positionedRequest]);
  const rejectControl = (event: SyntheticEvent) => {
    event.preventDefault();
    event.stopPropagation();
  };
  return (
    <div className="agent-workbench" data-testid="history-only-agent">
      <section className="conversation-pane">
        <div className="conversation-header">
          <strong>{agentId}</strong>
          <div className="conversation-status-banner" role="status">{t("agentAccess.historyOnly")}</div>
          <button type="button" onClick={onRetryAccess}>{t("common.retry")}</button>
          <ConversationSyncStatus model={session.model} onRetry={session.retry} />
          {session.browsing || location ? (
            <button type="button" onClick={onReturnToLatest}>{t("agentPage.backToLatest")}</button>
          ) : null}
        </div>
        <div className="timeline" ref={timelineRef}>
          <div ref={contentRef}>
          {location ? (
            <div className="conversation-status-banner" role={location.kind === "error" ? "alert" : "status"} aria-live="polite">
              <span>{t(location.kind === "source_only" ? "agentPage.location.sourceOnly"
                : location.kind === "error" ? location.reason === "legacy" ? "agentPage.location.legacyUnavailable"
                  : location.reason === "unsupported" ? "agentPage.location.unsupported"
                    : location.reason === "runtime" ? "agentPage.location.runtimeUnavailable" : "agentPage.location.failed"
                : location.kind === "ready" && positionedRequest === location.requestId ? "agentPage.location.located"
                : "agentPage.location.loading")}</span>
              {location.error ? <span>{location.error}</span> : null}
              {location.kind === "error" && !location.reason ? (
                <button type="button" onClick={session.locateSource}>{t("agentPage.location.retry")}</button>
              ) : null}
            </div>
          ) : null}
          {session.model.hasMoreHistory || session.model.historyState.kind === "loading" || session.model.historyState.kind === "error" ? (
            <div className="history-loader">
              {session.model.historyState.kind === "error" ? <span role="alert">{t("agentPage.location.olderFailed")}</span> : null}
              <button type="button" disabled={session.model.historyState.kind === "loading"} onClick={session.loadOlderHistory}>
                {t(session.model.historyState.kind === "loading" ? "agentPage.loadingEarlier" : "agentPage.loadEarlier")}
              </button>
            </div>
          ) : null}
          <ConversationTimeline
            historyOnly
            model={session.model}
            locatedAnchor={location?.kind === "ready" ? location.anchor : undefined}
            revealTarget={location?.kind === "ready" && location.anchor && positionedRequest !== location.requestId
              ? { anchor: location.anchor, turnId: location.turnId } : undefined}
            onLoadBrief={session.loadBrief}
            onLoadDetail={session.loadDetail}
            onLoadOlderActivities={session.loadOlderActivities}
            onRetry={session.retry}
            briefRecord={session.briefRecord}
            briefLoadState={session.briefState}
            detailLoadState={session.detailState}
          />
          {session.hasNewerHistory || session.newerHistoryState.kind === "loading" || session.newerHistoryState.kind === "error" ? (
            <div className="history-loader">
              {session.newerHistoryState.kind === "error" ? <span role="alert">{t("agentPage.location.newerFailed")}</span> : null}
              <button type="button" disabled={session.newerHistoryState.kind === "loading"} onClick={session.loadNewerHistory}>
                {t(session.newerHistoryState.kind === "loading" ? "agentPage.location.loadingNewer" : "agentPage.location.loadNewer")}
              </button>
            </div>
          ) : null}
          </div>
        </div>
        <form className="composer" onSubmit={rejectControl} onDrop={rejectControl} onDragOver={rejectControl}>
          <textarea disabled aria-label={t("agentPage.send")} />
          <input type="file" disabled hidden />
          <div className="composer-toolbar">
            <button type="button" disabled onClick={rejectControl}>{t("agentAccess.attachFiles")}</button>
            <button type="button" disabled onClick={rejectControl}>{t("agent.switchModel")}</button>
            <button type="button" disabled onClick={rejectControl}>{t("agent.stopCurrentTurn")}</button>
            <button type="submit" disabled onClick={rejectControl}>{t("agentPage.send")}</button>
          </div>
        </form>
      </section>
    </div>
  );
}
