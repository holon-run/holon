import { useMemo, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import {
  buildRuntimeTraceDiagnosticBundle,
  clearRuntimeTraceRecords,
  getRuntimeTraceRecords,
  getRuntimeTraceRevision,
  isRuntimeTraceEnabled,
  setRuntimeTraceEnabled,
  subscribeRuntimeTrace,
  type RuntimeTraceOutcome,
  type RuntimeTraceRecord,
} from "../../runtime/runtime-trace";
import type { RuntimeConnection } from "../../runtime/types";
import { triggerBlobDownload } from "./download";

const TRACE_OUTCOMES: RuntimeTraceOutcome[] = ["ok", "error", "cancelled", "deduped", "skipped"];

export function filterRuntimeTraceRecords(
  records: readonly RuntimeTraceRecord[],
  filter: { query?: string; outcome?: RuntimeTraceOutcome | "all" },
): readonly RuntimeTraceRecord[] {
  const query = filter.query?.trim().toLocaleLowerCase();
  return records.filter((record) => {
    if (filter.outcome && filter.outcome !== "all" && record.outcome !== filter.outcome) return false;
    if (!query) return true;
    return (
      record.name.toLocaleLowerCase().includes(query)
      || record.trigger?.toLocaleLowerCase().includes(query)
      || Object.entries(record.attributes ?? {}).some(([key, value]) =>
        key.toLocaleLowerCase().includes(query) || String(value).toLocaleLowerCase().includes(query))
    );
  });
}

export function runtimeTraceDiagnosticFilename(agentId: string, exportedAt = new Date()): string {
  const timestamp = exportedAt.toISOString().replaceAll(":", "-");
  const safeAgentId = agentId.replaceAll(/[^a-zA-Z0-9._-]/g, "_");
  return `holon-runtime-trace-${safeAgentId}-${timestamp}.json`;
}

interface RuntimeTracePanelProps {
  agentId: string;
  connection: RuntimeConnection;
}

export function RuntimeTracePanel({ agentId, connection }: RuntimeTracePanelProps) {
  const { t } = useTranslation();
  const revision = useSyncExternalStore(subscribeRuntimeTrace, getRuntimeTraceRevision, getRuntimeTraceRevision);
  const enabled = isRuntimeTraceEnabled();
  const [pausedRecords, setPausedRecords] = useState<readonly RuntimeTraceRecord[]>();
  const [query, setQuery] = useState("");
  const [outcome, setOutcome] = useState<RuntimeTraceOutcome | "all">("all");
  const liveRecords = useMemo(() => getRuntimeTraceRecords({ agentId }), [agentId, revision]);
  const records = pausedRecords ?? liveRecords;
  const filteredRecords = useMemo(
    () => filterRuntimeTraceRecords(records, { query, outcome }),
    [outcome, query, records],
  );

  function downloadDiagnosticBundle() {
    const exportedAt = new Date();
    const bundle = buildRuntimeTraceDiagnosticBundle({
      agentId,
      guiVersion: __HOLON_GUI_VERSION__,
      mode: import.meta.env.MODE,
      connection: {
        mode: connection.mode,
        source: connection.source,
        connected: connection.source === "http" && !connection.error,
      },
      exportedAt: exportedAt.toISOString(),
    });
    triggerBlobDownload(
      new Blob([JSON.stringify(bundle, null, 2)], { type: "application/json" }),
      runtimeTraceDiagnosticFilename(agentId, exportedAt),
    );
  }

  return (
    <section className="runtime-trace-panel" aria-label={t("runtimeTrace.title")}>
      <div className="runtime-trace-summary">
        <div>
          <span className="eyebrow">{t("runtimeTrace.diagnostics")}</span>
          <h2>{t("runtimeTrace.title")}</h2>
          <p>{t(enabled ? "runtimeTrace.recordingDescription" : "runtimeTrace.disabledDescription")}</p>
        </div>
        <span className={`runtime-trace-recording ${enabled ? "enabled" : "disabled"}`}>
          {t(enabled ? "runtimeTrace.recording" : "runtimeTrace.off")}
        </span>
      </div>

      {!enabled ? (
        <div className="settings-callout">
          <strong>{t("runtimeTrace.disabledTitle")}</strong>
          <span>{t("runtimeTrace.enableHint")}</span>
          <Button type="button" onClick={() => setRuntimeTraceEnabled(true)}>
            {t("runtimeTrace.enable")}
          </Button>
        </div>
      ) : (
        <>
          <div className="runtime-trace-toolbar">
            <input
              aria-label={t("runtimeTrace.filter")}
              placeholder={t("runtimeTrace.filterPlaceholder")}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
            <select aria-label={t("runtimeTrace.outcome")} value={outcome} onChange={(event) => setOutcome(event.target.value as RuntimeTraceOutcome | "all")}>
              <option value="all">{t("runtimeTrace.allOutcomes")}</option>
              {TRACE_OUTCOMES.map((value) => <option value={value} key={value}>{value}</option>)}
            </select>
          </div>
          <div className="runtime-trace-actions">
            <Button type="button" variant="secondary" onClick={() => setPausedRecords(pausedRecords ? undefined : [...liveRecords])}>
              {t(pausedRecords ? "runtimeTrace.resume" : "runtimeTrace.pause")}
            </Button>
            <Button type="button" variant="secondary" onClick={() => {
              setPausedRecords(undefined);
              clearRuntimeTraceRecords();
            }}>
              {t("runtimeTrace.clear")}
            </Button>
            <Button type="button" onClick={downloadDiagnosticBundle}>
              {t("runtimeTrace.download")}
            </Button>
          </div>
          <p className="runtime-trace-count">
            {t("runtimeTrace.recordCount", { shown: filteredRecords.length, total: records.length })}
            {pausedRecords ? ` · ${t("runtimeTrace.paused")}` : ""}
          </p>
          <div className="runtime-trace-records">
            {filteredRecords.length === 0 ? (
              <p className="inspector-muted">{t("runtimeTrace.empty")}</p>
            ) : filteredRecords.slice().reverse().map((record) => (
              <article className="runtime-trace-record" key={record.spanId}>
                <header>
                  <strong>{record.name}</strong>
                  <span className={`runtime-trace-outcome ${record.outcome}`}>{record.outcome}</span>
                </header>
                <dl>
                  <div><dt>{t("runtimeTrace.started")}</dt><dd>{new Date(record.startedAt).toLocaleTimeString()}</dd></div>
                  <div><dt>{t("runtimeTrace.duration")}</dt><dd>{record.durationMs.toFixed(1)} ms</dd></div>
                  {record.trigger ? <div><dt>{t("runtimeTrace.trigger")}</dt><dd>{record.trigger}</dd></div> : null}
                  {record.parentSpanId ? <div><dt>{t("runtimeTrace.parent")}</dt><dd><code>{record.parentSpanId}</code></dd></div> : null}
                </dl>
                {record.attributes && Object.keys(record.attributes).length > 0 ? (
                  <details>
                    <summary>{t("runtimeTrace.attributes")}</summary>
                    <pre>{JSON.stringify(record.attributes, null, 2)}</pre>
                  </details>
                ) : null}
              </article>
            ))}
          </div>
        </>
      )}
    </section>
  );
}
