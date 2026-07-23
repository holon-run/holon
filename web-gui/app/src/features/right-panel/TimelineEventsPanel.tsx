import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import type { SessionEventEnvelope } from "../../runtime/session-events";
import { deriveSessionTimeline, type SessionProjectionState } from "../../runtime/session-projection";
import type { TimelineEventsState } from "../../runtime/runtime-store";
import { compactAgentTimelineItems } from "../../runtime/session-reducer";
import {
  buildTimelineEventsBundle,
  diagnoseTimelineEvent,
  filterTimelineEvents,
  redactTimelineEventPayload,
  timelineEventGapCount,
  timelineEventsFilename,
  type TimelineEventDisposition,
  type TimelineEventFamily,
} from "../../runtime/timeline-event-diagnostics";
import { triggerBlobDownload } from "./download";

interface TimelineEventsPanelProps {
  agentId: string;
  timelineEvents?: TimelineEventsState;
  projection?: SessionProjectionState;
  onRefresh: () => void;
  onLoadOlder: () => void;
}

export function TimelineEventsPanel({
  agentId,
  timelineEvents,
  projection,
  onRefresh,
  onLoadOlder,
}: TimelineEventsPanelProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [family, setFamily] = useState<TimelineEventFamily | "all">("all");
  const [status, setStatus] = useState<TimelineEventDisposition | "all">("all");
  const [pausedSnapshot, setPausedSnapshot] = useState<{
    events: SessionEventEnvelope[];
    projection?: SessionProjectionState;
    timeline: ReturnType<typeof deriveSessionTimeline>;
  }>();
  const [selectedSeq, setSelectedSeq] = useState<number>();
  const [includeRawPayload, setIncludeRawPayload] = useState(false);
  const listRef = useRef<HTMLDivElement>(null);
  const liveEvents = useMemo(
    () => (timelineEvents?.eventSeqs ?? []).map((seq) => timelineEvents?.eventsBySeq[seq]).filter(isTimelineEvent),
    [timelineEvents],
  );
  const liveSemanticTimeline = useMemo(
    () => projection ? compactAgentTimelineItems(deriveSessionTimeline(projection, "debug", true)) : [],
    [projection],
  );
  const events = pausedSnapshot?.events ?? liveEvents;
  const diagnosticProjection = pausedSnapshot?.projection ?? projection;
  const semanticTimeline = pausedSnapshot?.timeline ?? liveSemanticTimeline;
  const diagnostics = useMemo(
    () => new Map(events.map((event) => [event.event_seq, diagnoseTimelineEvent(event, diagnosticProjection, semanticTimeline)])),
    [diagnosticProjection, events, semanticTimeline],
  );
  const filteredEvents = useMemo(
    () => filterTimelineEvents(events, diagnostics, { query, family, status }).slice().reverse(),
    [diagnostics, events, family, query, status],
  );
  const selectedEvent = events.find((event) => event.event_seq === selectedSeq);
  const selectedDiagnostic = selectedEvent ? diagnostics.get(selectedEvent.event_seq) : undefined;
  const virtualizer = useVirtualizer({
    count: filteredEvents.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 44,
    overscan: 10,
  });
  const gapCount = timelineEventGapCount(events);
  const rejectedCount = Array.from(diagnostics.values()).filter((diagnostic) => diagnostic.disposition === "rejected").length;
  const unprojectedCount = Array.from(diagnostics.values()).filter((diagnostic) =>
    diagnostic.disposition === "hidden" || diagnostic.disposition === "unhandled").length;

  useEffect(() => {
    if (selectedSeq != null && !events.some((event) => event.event_seq === selectedSeq)) {
      setSelectedSeq(undefined);
    }
  }, [events, selectedSeq]);

  function downloadBundle() {
    const exportedAt = new Date();
    const bundle = buildTimelineEventsBundle({
      agentId,
      eventLogEpoch: timelineEvents?.eventLogEpoch,
      oldestSeq: timelineEvents?.oldestSeq,
      newestSeq: timelineEvents?.newestSeq,
      hasOlder: timelineEvents?.hasOlder ?? false,
      events,
      diagnostics,
      filters: { query, family, status },
      includeRawPayload,
      exportedAt: exportedAt.toISOString(),
      guiVersion: __HOLON_GUI_VERSION__,
    });
    triggerBlobDownload(
      new Blob([JSON.stringify(bundle, null, 2)], { type: "application/json" }),
      timelineEventsFilename(agentId, exportedAt),
    );
  }

  return (
    <section className="timeline-events-panel" aria-label={t("timelineEvents.title")}>
      <div className="timeline-events-summary">
        <div>
          <span className="eyebrow">{t("timelineEvents.diagnostics")}</span>
          <h2>{t("timelineEvents.title")}</h2>
          <p>{t("timelineEvents.description")}</p>
        </div>
        <span className={`timeline-events-live ${pausedSnapshot ? "paused" : "live"}`}>
          {t(pausedSnapshot ? "timelineEvents.paused" : "timelineEvents.live")}
        </span>
      </div>

      <dl className="timeline-events-facts">
        <div><dt>{t("timelineEvents.epoch")}</dt><dd><code>{timelineEvents?.eventLogEpoch || "—"}</code></dd></div>
        <div><dt>{t("timelineEvents.range")}</dt><dd>{formatSeqRange(timelineEvents?.oldestSeq, timelineEvents?.newestSeq)}</dd></div>
        <div><dt>{t("timelineEvents.loaded")}</dt><dd>{events.length}</dd></div>
        <div><dt>{t("timelineEvents.issues")}</dt><dd>{t("timelineEvents.issueCounts", { gaps: gapCount, rejected: rejectedCount, unprojected: unprojectedCount })}</dd></div>
      </dl>

      <div className="timeline-events-toolbar">
        <input
          aria-label={t("timelineEvents.search")}
          placeholder={t("timelineEvents.searchPlaceholder")}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
        <select aria-label={t("timelineEvents.family")} value={family} onChange={(event) => setFamily(event.target.value as TimelineEventFamily | "all")}>
          <option value="all">{t("timelineEvents.allFamilies")}</option>
          {(["message", "assistant", "tool", "task", "work_item", "scheduler", "provider", "other"] as const)
            .map((value) => <option value={value} key={value}>{t(`timelineEvents.family_${value}`)}</option>)}
        </select>
        <select aria-label={t("timelineEvents.status")} value={status} onChange={(event) => setStatus(event.target.value as TimelineEventDisposition | "all")}>
          <option value="all">{t("timelineEvents.allStatuses")}</option>
          {(["shown", "merged", "hidden", "rejected", "unhandled"] as const)
            .map((value) => <option value={value} key={value}>{t(`timelineEvents.disposition_${value}`)}</option>)}
        </select>
      </div>

      <div className="timeline-events-actions">
        <Button
          type="button"
          size="sm"
          variant="secondary"
          onClick={() => setPausedSnapshot(pausedSnapshot
            ? undefined
            : { events: [...liveEvents], projection, timeline: liveSemanticTimeline })}
        >
          {t(pausedSnapshot ? "timelineEvents.resume" : "timelineEvents.pause")}
        </Button>
        <Button type="button" size="sm" variant="secondary" disabled={timelineEvents?.loading} onClick={onRefresh}>
          {t("common.refresh")}
        </Button>
        <Button type="button" size="sm" variant="secondary" disabled={!timelineEvents?.hasOlder || timelineEvents.loadingOlder} onClick={onLoadOlder}>
          {t(timelineEvents?.loadingOlder ? "timelineEvents.loadingOlder" : "timelineEvents.loadOlder")}
        </Button>
        <Button type="button" size="sm" onClick={downloadBundle}>{t("timelineEvents.export")}</Button>
      </div>
      <label className="timeline-events-raw-option">
        <input type="checkbox" checked={includeRawPayload} onChange={(event) => setIncludeRawPayload(event.target.checked)} />
        <span>{t("timelineEvents.includeRawPayload")}</span>
      </label>
      <p className="inspector-muted">{t("timelineEvents.sourceIdsLimit")}</p>

      {timelineEvents?.error ? <p className="inspector-error" role="alert">{timelineEvents.error}</p> : null}
      <p className="timeline-events-count">{t("timelineEvents.showing", { shown: filteredEvents.length, total: events.length })}</p>

      <div className="timeline-events-workspace">
        <div className="timeline-events-list" ref={listRef}>
          {timelineEvents?.loading && events.length === 0 ? (
            <p className="inspector-muted">{t("common.loading")}</p>
          ) : filteredEvents.length === 0 ? (
            <p className="inspector-muted">{t("timelineEvents.empty")}</p>
          ) : (
            <div className="timeline-events-virtual" style={{ height: virtualizer.getTotalSize() }}>
              {virtualizer.getVirtualItems().map((virtualRow) => {
                const event = filteredEvents[virtualRow.index];
                const diagnostic = diagnostics.get(event.event_seq);
                return (
                  <button
                    type="button"
                    className={`timeline-event-row ${selectedSeq === event.event_seq ? "selected" : ""}`}
                    key={event.event_seq}
                    onClick={() => setSelectedSeq(event.event_seq)}
                    style={{ transform: `translateY(${virtualRow.start}px)` }}
                  >
                    <code>#{event.event_seq}</code>
                    <span className="timeline-event-row-main">
                      <strong>{event.type}</strong>
                      <small>{summarizeTimelineEvent(event)}</small>
                    </span>
                    <span className={`timeline-event-disposition ${diagnostic?.disposition ?? "unhandled"}`}>
                      {t(`timelineEvents.disposition_${diagnostic?.disposition ?? "unhandled"}`)}
                    </span>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        <div className="timeline-event-detail">
          {selectedEvent ? (
            <>
              <div className="timeline-event-detail-head">
                <div><span className="eyebrow">#{selectedEvent.event_seq}</span><h3>{selectedEvent.type}</h3></div>
                <Button type="button" size="sm" variant="secondary" onClick={() => void copyJson(selectedEvent)}>{t("timelineEvents.copyEvent")}</Button>
              </div>
              <dl className="inspector-facts">
                <div><dt>{t("timelineEvents.timestamp")}</dt><dd>{formatTimestamp(selectedEvent.ts)}</dd></div>
                <div><dt>{t("timelineEvents.schema")}</dt><dd><code>{selectedEvent.payload_schema ?? "—"} v{selectedEvent.payload_schema_version ?? "—"}</code></dd></div>
                <div><dt>{t("timelineEvents.disposition")}</dt><dd><span className={`timeline-event-disposition ${selectedDiagnostic?.disposition ?? "unhandled"}`}>{t(`timelineEvents.disposition_${selectedDiagnostic?.disposition ?? "unhandled"}`)}</span></dd></div>
                <div><dt>{t("timelineEvents.reason")}</dt><dd>{selectedDiagnostic?.reason ?? "—"}</dd></div>
                <div><dt>{t("timelineEvents.timelineItems")}</dt><dd>{selectedDiagnostic?.timelineItemIds.length ? selectedDiagnostic.timelineItemIds.join(", ") : "—"}</dd></div>
              </dl>
              <details open>
                <summary>{t("timelineEvents.envelope")}</summary>
                <pre>{JSON.stringify(redactTimelineEventPayload(selectedEvent), null, 2)}</pre>
              </details>
              <details>
                <summary>{t("timelineEvents.rawPayload")}</summary>
                <pre>{JSON.stringify(selectedEvent.payload, null, 2)}</pre>
              </details>
            </>
          ) : <p className="inspector-muted">{t("timelineEvents.selectEvent")}</p>}
        </div>
      </div>
    </section>
  );
}

function summarizeTimelineEvent(event: SessionEventEnvelope): string {
  const payload = event.payload && typeof event.payload === "object" && !Array.isArray(event.payload)
    ? event.payload as Record<string, unknown>
    : undefined;
  const summary = ["summary", "message", "status", "objective", "tool_name", "task_id", "work_item_id"]
    .map((key) => payload?.[key])
    .find((value) => typeof value === "string" && value.trim());
  return typeof summary === "string" ? summary : formatTimestamp(event.ts);
}

function formatSeqRange(oldestSeq: number | undefined, newestSeq: number | undefined): string {
  if (oldestSeq == null || newestSeq == null) return "—";
  return `#${oldestSeq}–#${newestSeq}`;
}

function formatTimestamp(value: string | undefined): string {
  if (!value) return "—";
  const timestamp = new Date(value);
  return Number.isNaN(timestamp.valueOf()) ? value : timestamp.toLocaleString();
}

function isTimelineEvent(event: SessionEventEnvelope | undefined): event is SessionEventEnvelope {
  return event?.event_seq != null;
}

async function copyJson(value: unknown): Promise<void> {
  await navigator.clipboard?.writeText(JSON.stringify(value, null, 2));
}
