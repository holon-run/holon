import { FormEvent, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import i18next from "i18next";

import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import type { AgentSummary, MemorySourceContent, RuntimeSearchOptions, SearchResponse, SearchResultItem } from "../../runtime/types";

interface SearchPageProps {
  agents: AgentSummary[];
  search: SearchResponse | null;
  loading: boolean;
  error?: string;
  resultContentBySourceRef: Record<string, MemorySourceContent>;
  resultContentLoadingBySourceRef: Record<string, boolean>;
  resultContentErrorBySourceRef: Record<string, string | undefined>;
  onSearch: (query: string, options?: RuntimeSearchOptions) => Promise<void>;
  onLoadResultContent: (sourceRef: string) => Promise<void>;
  onOpenAgent: (agentId: string, eventSeq?: number) => void;
}

function extractMessageBodyPreview(value: string): string | undefined {
  const bodyMarker = "\nbody:\n";
  const inlineBodyMarker = "\nbody:";
  const compactBodyMarker = " body:";
  const markerIndex = value.indexOf(bodyMarker);
  const inlineMarkerIndex = value.indexOf(inlineBodyMarker);
  const compactMarkerIndex = value.lastIndexOf(compactBodyMarker);
  const body = markerIndex >= 0
    ? value.slice(markerIndex + bodyMarker.length)
    : value.startsWith("body:\n")
      ? value.slice("body:\n".length)
      : inlineMarkerIndex >= 0
        ? value.slice(inlineMarkerIndex + inlineBodyMarker.length)
        : compactMarkerIndex >= 0
          ? value.slice(compactMarkerIndex + compactBodyMarker.length)
        : value.startsWith("body:")
          ? value.slice("body:".length)
      : undefined;
  const summary = body
    ?.replace(/\n\[truncated\]$/, "")
    .trim();
  return summary || undefined;
}

const DEFAULT_LIMIT = 20;

export function SearchPage({
  agents,
  search,
  loading,
  error,
  resultContentBySourceRef,
  resultContentLoadingBySourceRef,
  resultContentErrorBySourceRef,
  onSearch,
  onLoadResultContent,
  onOpenAgent,
}: SearchPageProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState(() => search?.query ?? readInitialQuery());
  const [agentId, setAgentId] = useState("all");
  const [limit, setLimit] = useState(String(search?.limit || DEFAULT_LIMIT));
  const trimmedQuery = query.trim();
  const hasResults = Boolean(search?.results.length);
  const resultCount = search?.results.length ?? 0;
  const agentOptions = useMemo(() => [...agents].sort((left, right) => left.id.localeCompare(right.id)), [agents]);

  useEffect(() => {
    const initialQuery = readInitialQuery();
    if (!initialQuery || search || loading) return;
    if (!canSearchSelection("all", agents)) return;
    void onSearch(initialQuery, searchOptionsForSelection("all", agents, limit));
  }, [agents, limit, loading, onSearch, search]);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    writeQueryParam(trimmedQuery);
    void onSearch(trimmedQuery, searchOptionsForSelection(agentId, agents, limit));
  }

  return (
    <section className="page search-page" aria-label="Search">
      <div className="page-inner search-inner">
        <section className="search-hero">
          <span className="eyebrow">{t("searchPage.runtimeSearch")}</span>
          <h1>{t("searchPage.title")}</h1>
        </section>

        <form className="search-form" onSubmit={submit}>
          <label className="search-query">
            <span>{t("searchPage.query")}</span>
            <input
              autoFocus
              placeholder={t("searchPage.queryPlaceholder")}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
          </label>
          <label>
            <span>{t("searchPage.agent")}</span>
            <select value={agentId} onChange={(event) => setAgentId(event.target.value)}>
              <option value="all">{t("searchPage.allAgents")}</option>
              {agentOptions.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.id}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>{t("searchPage.limit")}</span>
            <input value={limit} inputMode="numeric" onChange={(event) => setLimit(event.target.value)} />
          </label>
          <Button type="submit" variant="accent" disabled={!trimmedQuery || loading || !canSearchSelection(agentId, agents)}>
            {loading ? t("searchPage.searching") : t("searchPage.search")}
          </Button>
        </form>

        {error ? <div className="search-error" role="alert">{error}</div> : null}

        <section className="search-results" aria-live="polite">
          {!search && !loading && !error ? (
            <EmptyState
              icon="⌕"
              title={t("searchPage.searchRuntimeMessages")}
              description={t("searchPage.searchDescription")}
            />
          ) : null}
          {search && !loading && !hasResults ? (
            <EmptyState
              icon="∅"
              title={t("searchPage.noMatches")}
              description={`No indexed messages matched “${search.query}”. Try a different keyword or search all agents.`}
            />
          ) : null}
          {hasResults ? (
            <>
              <div className="search-results-head">
                <strong>{t("searchPage.resultsCount", { count: resultCount })}</strong>
                <span>
                  for “{search?.query}” · limit {search?.limit}
                </span>
              </div>
              <div className="search-result-list">
                {search?.results.map((result) => (
                  <SearchResultCard
                    key={result.locator.evidenceId ?? `${result.agentId}:${result.createdAt}:${result.preview}`}
                    result={result}
                    content={result.locator.sourceRef ? resultContentBySourceRef[result.locator.sourceRef] : undefined}
                    contentLoading={result.locator.sourceRef ? resultContentLoadingBySourceRef[result.locator.sourceRef] : false}
                    contentError={result.locator.sourceRef ? resultContentErrorBySourceRef[result.locator.sourceRef] : undefined}
                    onLoadResultContent={onLoadResultContent}
                    onOpenAgent={onOpenAgent}
                  />
                ))}
              </div>
            </>
          ) : null}
        </section>
      </div>
    </section>
  );
}

function SearchResultCard({
  result,
  content,
  contentLoading,
  contentError,
  onLoadResultContent,
  onOpenAgent,
}: {
  result: SearchResultItem;
  content?: MemorySourceContent;
  contentLoading?: boolean;
  contentError?: string;
  onLoadResultContent: (sourceRef: string) => Promise<void>;
  onOpenAgent: (agentId: string, eventSeq?: number) => void;
}) {
  const { t } = useTranslation();
  const preview = formatSearchPreview(result.preview);
  const sourceRef = result.locator.sourceRef ?? result.locator.evidenceId;
  const locator = [
    result.locator.eventSeq != null ? `event #${result.locator.eventSeq}` : undefined,
    result.locator.turnId ? `turn ${shortId(result.locator.turnId)}` : undefined,
    result.locator.taskId ? `task ${shortId(result.locator.taskId)}` : undefined,
    result.locator.workItemId ? `work ${shortId(result.locator.workItemId)}` : undefined,
    result.locator.messageId ? `message ${shortId(result.locator.messageId)}` : undefined,
  ].filter(Boolean);
  const canLocate = result.locator.eventSeq != null;

  return (
    <article className="search-result-card">
      <header>
        <button type="button" onClick={() => onOpenAgent(result.agentId)}>
          {result.agentId}
        </button>
        <span>{result.kind}</span>
        {canLocate ? (
          <button type="button" onClick={() => onOpenAgent(result.agentId, result.locator.eventSeq)}>
            Locate event
          </button>
        ) : null}
        <time>{formatTimestamp(result.createdAt)}</time>
      </header>
      <div className="search-result-preview">
        {preview.title ? <strong>{preview.title}</strong> : null}
        <p>{preview.summary}</p>
        {preview.meta.length > 0 ? (
          <dl>
            {preview.meta.map(([label, value]) => (
              <div key={label}>
                <dt>{label}</dt>
                <dd>{value}</dd>
              </div>
            ))}
          </dl>
        ) : null}
      </div>
      {locator.length > 0 ? <footer>{locator.join(" · ")}</footer> : null}
      <details
        className="search-result-details"
        onToggle={(event) => {
          if (event.currentTarget.open && sourceRef) {
            void onLoadResultContent(sourceRef);
          }
        }}
      >
        <summary>{t("searchPage.fullSource")}</summary>
        {sourceRef ? (
          <section className="search-result-full-source" aria-live="polite">
            {contentLoading ? <p>{t("searchPage.loadingSource")}</p> : null}
            {contentError ? <p className="search-result-full-source-error">{contentError}</p> : null}
            {content ? (
              <>
                <div className="search-result-full-source-head">
                  <strong>{content.title}</strong>
                  {content.truncated ? <span>{t("searchPage.truncated")}</span> : null}
                </div>
                <pre>{content.content || t("searchPage.noContent")}</pre>
              </>
            ) : null}
          </section>
        ) : (
          <p className="search-result-full-source-empty">{t("searchPage.noSourceRef")}</p>
        )}
        <dl>
          <div>
            <dt>{t("searchPage.resultType")}</dt>
            <dd>{result.resultType}</dd>
          </div>
          <div>
            <dt>{t("searchPage.evidence")}</dt>
            <dd>{result.locator.evidenceId ? shortId(result.locator.evidenceId) : t("searchPage.notProvided")}</dd>
          </div>
          <div>
            <dt>{t("searchPage.locator")}</dt>
            <dd>{locator.length > 0 ? locator.join(" · ") : t("searchPage.noLocatorIds")}</dd>
          </div>
          {preview.isFormatted ? (
            <div>
              <dt>{t("searchPage.rawPreview")}</dt>
              <dd>
                <pre>{result.preview}</pre>
              </dd>
            </div>
          ) : null}
        </dl>
      </details>
    </article>
  );
}

interface FormattedSearchPreview {
  title?: string;
  summary: string;
  meta: Array<[string, string]>;
  isFormatted: boolean;
}

export function formatSearchPreview(value: string): FormattedSearchPreview {
  const text = value.trim();
  if (!text) return { summary: i18next.t("searchPage.noPreview"), meta: [], isFormatted: false };

  const messageBody = extractMessageBodyPreview(text);
  if (messageBody) return { title: i18next.t("searchPage.messageBody"), summary: messageBody, meta: [], isFormatted: true };

  const parsed = parseJsonPreview(text);
  if (!parsed) return { summary: text, meta: [], isFormatted: false };

  const record = asRecord(parsed);
  if (!record) {
    return { summary: summarizeJsonValue(parsed), meta: [], isFormatted: true };
  }

  const title = previewTitle(record);
  const summary = previewSummary(record) ?? summarizeJsonValue(parsed);
  const meta = previewMeta(record);
  return { title, summary, meta, isFormatted: true };
}

function parseJsonPreview(value: string): unknown | undefined {
  if (!value.startsWith("{") && !value.startsWith("[")) return undefined;
  try {
    return JSON.parse(value);
  } catch {
    return undefined;
  }
}

function previewTitle(record: Record<string, unknown>): string | undefined {
  const toolName = stringField(record, "tool_name") ?? stringField(record, "tool") ?? stringField(record, "function_name");
  if (toolName) return `Tool call: ${toolName}`;

  const role = stringField(record, "role") ?? stringField(record, "source") ?? stringField(record, "origin");
  const kind = stringField(record, "kind") ?? stringField(record, "type");
  if (role && kind) return `${formatLabel(role)} ${formatLabel(kind)}`;
  if (kind) return formatLabel(kind);
  return undefined;
}

function previewSummary(record: Record<string, unknown>): string | undefined {
  const direct = firstStringField(record, [
    "summary",
    "summary_text",
    "text_preview",
    "output_preview",
    "stdout_preview",
    "stderr_preview",
    "result_summary",
    "result_summary_preview",
    "message",
    "text",
    "content",
    "reason",
  ]);
  if (direct) return direct;

  const body = asRecord(record.body);
  if (body) return previewSummary(body);

  const output = asRecord(record.output) ?? asRecord(record.result);
  if (output) return previewSummary(output);

  const command = commandText(record);
  if (command) return command;

  const objective = firstStringField(record, ["objective", "objective_preview", "title"]);
  if (objective) return objective;

  return undefined;
}

function previewMeta(record: Record<string, unknown>): Array<[string, string]> {
  const entries: Array<[string, string | undefined]> = [
    ["Status", firstStringField(record, ["status", "state", "effect"])],
    ["Command", commandText(record)],
    ["Path", firstStringField(record, ["path", "file", "config_file_path"])],
    ["Task", firstStringField(record, ["task_id", "taskId"])],
    ["Work item", firstStringField(record, ["work_item_id", "workItemId"])],
    ["Evidence", firstStringField(record, ["evidence_id", "evidenceId", "source_ref"])],
  ];

  const result = asRecord(record.result) ?? asRecord(record.output);
  if (result) {
    entries.push(
      ["Exit", numberField(result, "exit_status")],
      ["Changed", numberField(result, "changed_file_count")],
      ["Summary", firstStringField(result, ["summary", "summary_text", "result_summary", "result_summary_preview"])],
    );
  }

  return entries
    .filter((entry): entry is [string, string] => Boolean(entry[1]))
    .slice(0, 4);
}

function commandText(record: Record<string, unknown>): string | undefined {
  const command = asRecord(record.command);
  return (
    firstStringField(record, ["cmd", "cmd_preview", "command"]) ??
    (command ? firstStringField(command, ["cmd", "cmd_preview", "command"]) : undefined)
  );
}

function summarizeJsonValue(value: unknown): string {
  if (Array.isArray(value)) return `${value.length} item${value.length === 1 ? "" : "s"}`;
  const record = asRecord(value);
  if (!record) return String(value);
  const keys = Object.keys(record).slice(0, 6);
  return keys.length ? i18next.t("searchPage.structuredRecordWithKeys", { keys: keys.join(", ") }) : i18next.t("searchPage.structuredRecord");
}

function firstStringField(record: Record<string, unknown>, keys: string[]): string | undefined {
  for (const key of keys) {
    const value = stringField(record, key);
    if (value) return value;
  }
  return undefined;
}

function stringField(record: Record<string, unknown>, key: string): string | undefined {
  const value = record[key];
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function numberField(record: Record<string, unknown>, key: string): string | undefined {
  const value = record[key];
  return typeof value === "number" && Number.isFinite(value) ? String(value) : undefined;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined;
}

function formatLabel(value: string): string {
  return value
    .replace(/[_-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function numberFromInput(value: string, fallback: number): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) return fallback;
  return Math.floor(parsed);
}

function shortId(value: string): string {
  return value.length > 12 ? value.slice(0, 12) : value;
}

function formatTimestamp(value?: string): string {
  if (!value) return "unknown time";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

function readInitialQuery(): string {
  if (typeof window === "undefined") return "";
  return new URLSearchParams(window.location.search).get("q") ?? "";
}

function writeQueryParam(query: string): void {
  if (typeof window === "undefined") return;
  const url = new URL(window.location.href);
  if (query) {
    url.searchParams.set("q", query);
  } else {
    url.searchParams.delete("q");
  }
  window.history.replaceState(null, "", url);
}

export function searchOptionsForSelection(agentId: string, agents: AgentSummary[], limit: string): RuntimeSearchOptions {
  const selectedAgentIds = agentId === "all" ? agents.map((agent) => agent.id).filter(Boolean) : [agentId];
  return {
    agentIds: selectedAgentIds.length > 0 ? selectedAgentIds : undefined,
    includeAllWorkspaces: agentId === "all",
    limit: numberFromInput(limit, DEFAULT_LIMIT),
  };
}

export function canSearchSelection(agentId: string, agents: AgentSummary[]): boolean {
  if (agentId !== "all") return Boolean(agentId.trim());
  return agents.some((agent) => Boolean(agent.id.trim()));
}
