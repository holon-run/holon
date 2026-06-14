import { FormEvent, useEffect, useMemo, useState } from "react";

import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import type { AgentSummary, SearchResponse, SearchResultItem } from "../../runtime/types";

interface SearchPageProps {
  agents: AgentSummary[];
  search: SearchResponse | null;
  loading: boolean;
  error?: string;
  onSearch: (query: string, options?: { agentIds?: string[]; limit?: number }) => Promise<void>;
  onOpenAgent: (agentId: string) => void;
}

const DEFAULT_LIMIT = 20;

export function SearchPage({ agents, search, loading, error, onSearch, onOpenAgent }: SearchPageProps) {
  const [query, setQuery] = useState(() => search?.query ?? readInitialQuery());
  const [agentId, setAgentId] = useState("all");
  const [limit, setLimit] = useState(String(search?.limit || DEFAULT_LIMIT));
  const trimmedQuery = query.trim();
  const selectedAgentIds = agentId === "all" ? undefined : [agentId];
  const hasResults = Boolean(search?.results.length);
  const resultCount = search?.results.length ?? 0;
  const agentOptions = useMemo(() => [...agents].sort((left, right) => left.id.localeCompare(right.id)), [agents]);

  useEffect(() => {
    const initialQuery = readInitialQuery();
    if (!initialQuery || search || loading) return;
    void onSearch(initialQuery, { limit: numberFromInput(limit, DEFAULT_LIMIT) });
  }, [limit, loading, onSearch, search]);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    writeQueryParam(trimmedQuery);
    void onSearch(trimmedQuery, { agentIds: selectedAgentIds, limit: numberFromInput(limit, DEFAULT_LIMIT) });
  }

  return (
    <section className="page search-page" aria-label="Search">
      <div className="page-inner search-inner">
        <section className="search-hero">
          <span className="eyebrow">Runtime search</span>
          <h1>Search</h1>
          <p>Find matching runtime messages across agents, turns, tasks, and WorkItems.</p>
        </section>

        <form className="search-form" onSubmit={submit}>
          <label className="search-query">
            <span>Query</span>
            <input
              autoFocus
              placeholder="Search messages, tool output previews, work item context…"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
          </label>
          <label>
            <span>Agent</span>
            <select value={agentId} onChange={(event) => setAgentId(event.target.value)}>
              <option value="all">All agents</option>
              {agentOptions.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.id}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>Limit</span>
            <input value={limit} inputMode="numeric" onChange={(event) => setLimit(event.target.value)} />
          </label>
          <Button type="submit" variant="accent" disabled={!trimmedQuery || loading}>
            {loading ? "Searching…" : "Search"}
          </Button>
        </form>

        {error ? <div className="search-error" role="alert">{error}</div> : null}

        <section className="search-results" aria-live="polite">
          {!search && !loading && !error ? (
            <EmptyState
              icon="⌕"
              title="Search runtime messages"
              description="Enter a query to search indexed operator inputs, assistant replies, tool receipts, and runtime evidence previews."
            />
          ) : null}
          {search && !loading && !hasResults ? (
            <EmptyState
              icon="∅"
              title="No matches"
              description={`No indexed messages matched “${search.query}”. Try a different keyword or search all agents.`}
            />
          ) : null}
          {hasResults ? (
            <>
              <div className="search-results-head">
                <strong>{resultCount} results</strong>
                <span>
                  for “{search?.query}” · limit {search?.limit}
                </span>
              </div>
              <div className="search-result-list">
                {search?.results.map((result) => (
                  <SearchResultCard key={result.locator.evidenceId ?? `${result.agentId}:${result.createdAt}:${result.preview}`} result={result} onOpenAgent={onOpenAgent} />
                ))}
              </div>
            </>
          ) : null}
        </section>
      </div>
    </section>
  );
}

function SearchResultCard({ result, onOpenAgent }: { result: SearchResultItem; onOpenAgent: (agentId: string) => void }) {
  const locator = [
    result.locator.turnId ? `turn ${shortId(result.locator.turnId)}` : undefined,
    result.locator.taskId ? `task ${shortId(result.locator.taskId)}` : undefined,
    result.locator.workItemId ? `work ${shortId(result.locator.workItemId)}` : undefined,
    result.locator.messageId ? `message ${shortId(result.locator.messageId)}` : undefined,
  ].filter(Boolean);

  return (
    <article className="search-result-card">
      <header>
        <button type="button" onClick={() => onOpenAgent(result.agentId)}>
          {result.agentId}
        </button>
        <span>{result.kind}</span>
        <time>{formatTimestamp(result.createdAt)}</time>
      </header>
      <p>{result.preview}</p>
      {locator.length > 0 ? <footer>{locator.join(" · ")}</footer> : null}
    </article>
  );
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
