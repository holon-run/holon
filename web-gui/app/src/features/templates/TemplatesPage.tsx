import { useMemo, useState, type FormEvent } from "react";

import { MarkdownContent } from "../../components/MarkdownContent";
import { Button } from "../../components/ui/Button";
import { Card, CardContent, CardHeader } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type {
  AgentTemplateCatalogEntry,
  AgentTemplateCatalogState,
  AgentTemplateDetailState,
  AgentTemplateSourceKind,
} from "../../runtime/types";

interface TemplatesPageProps {
  catalog: AgentTemplateCatalogState;
  loading: boolean;
  syncInProgress: boolean;
  syncMessage?: string;
  error?: string;
  onRefresh: () => void;
  onSyncSources: () => Promise<boolean>;
  onInstallTemplate: (githubUrl: string) => Promise<boolean>;
  onRemoveTemplate: (templateId: string) => Promise<boolean>;
  onOpenTemplate: (catalogId: string) => void;
  onAddRemoteSource: (sourceId: string, url: string, gitRef?: string) => Promise<boolean>;
  onRemoveRemoteSource: (sourceId: string) => Promise<boolean>;
  onDismissDiagnostics: () => void;
  onDismissError: () => void;
}

interface TemplateDetailPageProps {
  catalogId: string;
  detail?: AgentTemplateDetailState;
  loading: boolean;
  error?: string;
  onBack: () => void;
  onRefresh: () => void;
  onRemoveTemplate: (templateId: string) => Promise<boolean>;
  onCreateAgent: (template: string) => void;
}

export function TemplatesPage({
  catalog,
  loading,
  syncInProgress,
 syncMessage,
  error,
  onRefresh,
  onSyncSources,
  onInstallTemplate,
  onRemoveTemplate,
  onOpenTemplate,
  onAddRemoteSource,
  onRemoveRemoteSource,
  onDismissDiagnostics,
  onDismissError,
}: TemplatesPageProps) {
  const templates = catalog.catalog;
  const [query, setQuery] = useState("");
  const [sourceFilter, setSourceFilter] = useState<"all" | AgentTemplateSourceKind>("all");
  const [githubUrl, setGithubUrl] = useState("");
  const [customSourceId, setCustomSourceId] = useState("");
  const [newSourceUrl, setNewSourceUrl] = useState("");
  const [newSourceRef, setNewSourceRef] = useState("");
  const [sourceFormError, setSourceFormError] = useState<string | undefined>();
  const [sourceFormBusy, setSourceFormBusy] = useState(false);
  const stats = useMemo(() => templateStats(templates), [templates]);
  const existingSourceIds = useMemo(() => catalog.sources.map((source) => source.sourceId), [catalog.sources]);
  const suggestedSourceId = useMemo(() => uniqueSourceId(deriveSourceId(newSourceUrl), existingSourceIds), [existingSourceIds, newSourceUrl]);
  const visibleTemplates = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return templates.filter((template) => {
      if (sourceFilter !== "all" && template.source !== sourceFilter) return false;
      if (!normalizedQuery) return true;
      return [
        template.name,
        template.description,
        template.catalogId,
        template.template,
        template.templateId,
        template.sourceId,
        template.sourceUrl,
        ...template.includedSkills,
      ]
        .filter((value): value is string => Boolean(value))
        .some((value) => value.toLowerCase().includes(normalizedQuery));
    });
  }, [query, sourceFilter, templates]);

  async function handleInstall(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const source = githubUrl.trim();
    if (!source) return;
    const ok = await onInstallTemplate(source);
    if (ok) setGithubUrl("");
  }


  async function handleAddSource(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const url = newSourceUrl.trim();
    if (!url) return;
    const customId = customSourceId.trim();
    const id = customId ? sanitizeSourceId(customId) : suggestedSourceId;
    if (!id) {
      setSourceFormError("Enter a GitHub URL so Holon can generate a source id.");
      return;
    }
    if (customId && id !== customId) {
      setSourceFormError(`Custom source id was normalized to "${id}". Use that value or leave custom id empty.`);
      return;
    }
    if (existingSourceIds.includes(id)) {
      setSourceFormError(`Remote source "${id}" already exists.`);
      return;
    }
    setSourceFormBusy(true);
    setSourceFormError(undefined);
    try {
      const ok = await onAddRemoteSource(id, url, newSourceRef.trim() || undefined);
      if (ok) {
        setCustomSourceId("");
        setNewSourceUrl("");
        setNewSourceRef("");
      } else {
        setSourceFormError("Failed to add remote source.");
      }
    } catch (error) {
      setSourceFormError(error instanceof Error ? error.message : String(error));
    } finally {
      setSourceFormBusy(false);
    }
  }

  return (
    <div className="templates-inner skills-inner scroll-surface">
      <section className="skills-hero context-card">
        <div className="skills-hero-copy">
          <span className="eyebrow">Agent Template Library</span>
          <h1>Agent Templates</h1>
          <p>
            Browse read-only AgentTemplate definitions from global and remote sources. Install/remove user templates
            through daemon APIs; source configuration can follow in a later settings phase.
          </p>
        </div>
        <div className="skills-actions" aria-label="Template library actions">
          <Button type="button" variant="outline" disabled={loading || syncInProgress} onClick={() => void onSyncSources()}>
            {syncInProgress ? "Syncing…" : "Sync sources"}
          </Button>
          <Button type="button" variant="outline" disabled={loading} onClick={onRefresh}>
            {loading ? "Refreshing…" : "Refresh"}
          </Button>
        </div>
      </section>

      <section className="skills-summary" aria-label="Template library summary">
        {stats.map((stat) => (
          <Card className="skills-stat" key={stat.label}>
            <strong>{stat.value}</strong>
            <span>{stat.label}</span>
          </Card>
        ))}
      </section>

      {error ? (
        <div className="skills-error" role="alert">
          <strong>Template operation failed</strong>
          <span>{error}</span>
          <button type="button" className="skills-error-dismiss" aria-label="Dismiss error" onClick={onDismissError}>×</button>
        </div>
      ) : null}

      {syncMessage && !syncInProgress ? (
        <div className="skills-success" role="status">
          <span>{syncMessage}</span>
        </div>
      ) : null}

      {catalog.diagnostics.length > 0 ? (
        <div className="skills-error" role="status">
          <strong>Catalog diagnostics</strong>
          {catalog.diagnostics.map((diagnostic, index) => (
            <span key={`${diagnostic.sourceId ?? "catalog"}-${index}`}>
              {diagnostic.sourceId ? `${diagnostic.sourceId}: ` : ""}
              {diagnostic.message}
            </span>
          ))}
          <button type="button" className="skills-error-dismiss" aria-label="Dismiss diagnostics" onClick={onDismissDiagnostics}>×</button>
        </div>
      ) : null}

      <Card className="skills-library-card">
        <CardHeader className="skills-library-head">
          <div>
            <p>
              Showing {visibleTemplates.length} of {templates.length} templates
            </p>
          </div>
        </CardHeader>
        <CardContent>
          <div className="template-actions-grid">

            <form className="skills-add-form" onSubmit={(event) => void handleInstall(event)}>
              <label className="skills-add-source">
                <span>Install template</span>
                <input
                  value={githubUrl}
                  placeholder="https://github.com/org/repo/tree/main/agent_templates/name"
                  onChange={(event) => setGithubUrl(event.target.value)}
                  disabled={loading}
                />
              </label>
              <Button type="submit" variant="accent" disabled={loading || !githubUrl.trim()}>
                Install
              </Button>
            </form>
          </div>
          <p className="skills-add-help">
            Template content is read-only. Use the + button in Active Agents to create an agent from a template. Removal targets user/global templates; remote entries remain owned by their source.
          </p>

          <div className="skills-toolbar" role="search">
            <label className="skills-search">
              <span>Search templates</span>
              <input
                id="templates-search"
                name="templates-search"
                type="search"
                value={query}
                placeholder="Name, description, template id, or skill"
                onChange={(event) => setQuery(event.target.value)}
              />
            </label>
            <label className="skills-scope-filter">
              <span>Source</span>
              <select value={sourceFilter} onChange={(event) => setSourceFilter(event.target.value as typeof sourceFilter)}>
                <option value="all">All sources</option>
                <option value="user_global">Global</option>
                <option value="remote">Remote</option>
                <option value="agent_home">Agent</option>
              </select>
            </label>
          </div>

          {visibleTemplates.length ? (
            <ul className="template-card-grid">
              {visibleTemplates.map((template) => (
                <TemplateCard
                  key={template.catalogId}
                  template={template}
                  loading={loading}
                  onOpen={onOpenTemplate}
                  onRemove={onRemoveTemplate}
                />
              ))}
            </ul>
          ) : (
            <EmptyState
              icon="▣"
              title={loading ? "Loading templates…" : templates.length ? "No templates match the current filters" : "No templates in the catalog"}
              description={
                templates.length
                  ? "Try a different query or source filter."
                  : "Refresh after installing templates or syncing remote sources."
              }
            />
          )}
        </CardContent>
      </Card>

      <details className="template-remote-sources-collapse" open>
        <summary className="template-remote-sources-toggle">
          Remote sources ({catalog.sources.length})
        </summary>
      <Card className="skills-library-card">
        <CardContent>
          <form className="skills-add-form template-remote-source-form" onSubmit={(event) => void handleAddSource(event)}>
            <label className="skills-add-source">
              <span>GitHub URL</span>
              <input
                value={newSourceUrl}
                placeholder="https://github.com/org/repo/tree/main/agent_templates"
                onChange={(event) => setNewSourceUrl(event.target.value)}
                disabled={sourceFormBusy}
              />
            </label>
            <label>
              <span>Ref (optional)</span>
              <input
                value={newSourceRef}
                placeholder="main"
                onChange={(event) => setNewSourceRef(event.target.value)}
                disabled={sourceFormBusy}
              />
            </label>
            <div className="template-source-id-preview">
              <span>Generated source id</span>
              <strong>{customSourceId.trim() || suggestedSourceId || "generated from GitHub URL"}</strong>
            </div>
            <details className="template-source-advanced">
              <summary>Advanced: custom source id</summary>
              <label>
                <span>Custom source id</span>
                <input
                  value={customSourceId}
                  placeholder="org-repo"
                  onChange={(event) => setCustomSourceId(event.target.value)}
                  disabled={sourceFormBusy}
                />
              </label>
            </details>
            <Button type="submit" variant="accent" disabled={sourceFormBusy || !newSourceUrl.trim()}>
              Add
            </Button>
          </form>
          {sourceFormError ? <span className="connection-error" role="alert">{sourceFormError}</span> : null}

          {catalog.sources.length ? (
            <ul className="skills-list">
              {catalog.sources.map((source) => (
                <li className="skills-row" key={source.sourceId}>
                  <div className="skills-row-open">
                    <span className="template-row-title">
                      <strong>{source.sourceId}</strong>
                      <StatusBadge className="state-chip" kind="connection" value={source.status ?? (source.enabled ? "enabled" : "disabled")} />
                    </span>
                    <span className="template-row-description">{source.url ?? source.kind}</span>
                    <span className="template-row-meta">
                      {source.resolvedRef ? <span>ref {source.resolvedRef}</span> : null}
                      {source.lastSyncedAt ? <span>synced {source.lastSyncedAt}</span> : null}
                    </span>
                  </div>
                  <div className="skills-row-actions">
                    <Button type="button" size="sm" variant="outline" disabled={loading} onClick={() => void onRemoveRemoteSource(source.sourceId)}>
                      Remove
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          ) : (
            <EmptyState icon="⇄" title="No configured remote template sources" description="Add a GitHub remote source above to sync agent templates." />
          )}
        </CardContent>
      </Card>
      </details>
    </div>
  );
}

export function TemplateDetailPage({
  catalogId,
  detail,
  loading,
  error,
  onBack,
  onRefresh,
  onRemoveTemplate,
  onCreateAgent,
}: TemplateDetailPageProps) {
  const template = detail?.detail;
  const [viewMode, setViewMode] = useState<"rendered" | "source">("rendered");

  return (
    <div className="skills-inner scroll-surface">
      <section className="skill-detail-hero context-card">
        <div>
          <button className="text-button" type="button" onClick={onBack}>
            ← Agent Templates
          </button>
          <span className="eyebrow">{template?.schemaVersion ?? "AgentTemplate"}</span>
          <h1>{template?.name ?? catalogId}</h1>
          <p>{template?.summary || "Read-only template detail from the Holon daemon catalog."}</p>
          {template ? (
            <div className="template-detail-meta-bar">
              <StatusBadge className="state-chip" kind="connection" value={template.source} />
              <span className="template-detail-meta-id">{template.catalogId}</span>
              {template.sourceLocation ? (
                <span className="template-detail-meta-path">{template.sourceLocation}</span>
              ) : null}
              {template.skills.length ? (
                <span className="template-detail-meta-skills">{template.skills.length} skills</span>
              ) : null}
            </div>
          ) : null}
        </div>
        <div className="skills-actions">
          {template ? (
            <Button type="button" variant="accent" onClick={() => onCreateAgent(template.template)}>
              Create Agent
            </Button>
          ) : null}
          <Button type="button" variant="outline" disabled={loading} onClick={onRefresh}>
            {loading ? "Refreshing…" : "Refresh"}
          </Button>
          {template?.source === "user_global" ? (
            <Button type="button" variant="outline" disabled={loading} onClick={() => void onRemoveTemplate(template.templateId)}>
              Remove
            </Button>
          ) : null}
        </div>
      </section>

      {error || detail?.error ? (
        <div className="skills-error" role="alert">
          <strong>Template detail failed</strong>
          <span>{error ?? detail?.error}</span>
        </div>
      ) : null}

      {template ? (
        <>
          <Card className="skills-library-card">
            <CardHeader className="skills-library-head">
              <div>
                <p>AGENTS.md</p>
              </div>
              <div className="skills-actions">
                <Button type="button" size="sm" variant={viewMode === "rendered" ? "accent" : "outline"} onClick={() => setViewMode("rendered")}>
                  Rendered
                </Button>
                <Button type="button" size="sm" variant={viewMode === "source" ? "accent" : "outline"} onClick={() => setViewMode("source")}>
                  Source
                </Button>
              </div>
            </CardHeader>
            <CardContent>
              {viewMode === "rendered" ? (
                <MarkdownContent text={template.agentsMd} />
              ) : (
                <pre className="skill-source">
                  <code>{template.agentsMd}</code>
                </pre>
              )}
            </CardContent>
          </Card>

          <Card className="skills-library-card">
            <CardHeader className="skills-library-head">
              <div>
                <p>Included skills</p>
              </div>
            </CardHeader>
            <CardContent>
              {template.skills.length ? (
                <ul className="skills-list">
                  {template.skills.map((skill) => (
                    <li className="skills-row" key={`${skill.kind}:${skill.reference}`}>
                      <div className="skills-row-open">
                        <span className="template-row-title">
                          <strong>{skill.reference}</strong>
                          <StatusBadge className="state-chip" kind="connection" value={skill.kind} />
                        </span>
                      </div>
                    </li>
                  ))}
                </ul>
              ) : (
                <EmptyState icon="◇" title="No declared skill dependencies" description="This template does not declare extra skills." />
              )}
            </CardContent>
          </Card>
        </>
      ) : (
        <EmptyState icon="▣" title={loading ? "Loading template…" : "Template not found"} description={catalogId} />
      )}
    </div>
  );
}

function TemplateCard({
  template,
  loading,
  onOpen,
  onRemove,
}: {
  template: AgentTemplateCatalogEntry;
  loading: boolean;
  onOpen: (catalogId: string) => void;
  onRemove: (templateId: string) => Promise<boolean>;
}) {
  const canRemove = template.source === "user_global";
  return (
    <li className="template-card">
      <button className="template-card-open" type="button" onClick={() => onOpen(template.catalogId)}>
        <span className="template-card-title">
          <strong>{template.name}</strong>
          <StatusBadge className="state-chip" kind="connection" value={template.source} />
        </span>
        <span className="template-card-description">{template.description || template.template}</span>
        <span className="template-card-meta">
          <span>{template.catalogId}</span>
          {template.includedSkills.length ? <span>{template.includedSkills.length} skills</span> : null}
          {template.sourceId ? <span>{template.sourceId}</span> : null}
        </span>
      </button>
      <div className="template-card-actions">
        {canRemove ? (
          <Button type="button" size="sm" variant="outline" disabled={loading} onClick={() => void onRemove(template.templateId)}>
            Remove
          </Button>
        ) : null}
      </div>
    </li>
  );
}

function templateStats(templates: AgentTemplateCatalogEntry[]) {
  return [
    { label: "templates", value: String(templates.length) },
    { label: "global", value: String(templates.filter((template) => template.source === "user_global").length) },
    { label: "remote", value: String(templates.filter((template) => template.source === "remote").length) },
  ];
}

function deriveSourceId(url: string): string {
  const trimmed = url.trim();
  if (!trimmed) return "";
  const githubRepo = githubRepoSlug(trimmed);
  if (githubRepo) return githubRepo;
  return sanitizeSourceId(trimmed.split("/").filter(Boolean).slice(-2).join("-")) || "remote-source";
}

function githubRepoSlug(url: string): string | undefined {
  const parsed = parseGitHubUrl(url);
  if (!parsed) return undefined;
  return sanitizeSourceId(`${parsed.owner}-${parsed.repo}`);
}

function parseGitHubUrl(url: string): { owner: string; repo: string } | undefined {
  try {
    const parsed = new URL(url);
    if (parsed.hostname !== "github.com") return undefined;
    const [owner, repo] = parsed.pathname.split("/").filter(Boolean);
    if (!owner || !repo) return undefined;
    return { owner, repo: repo.replace(/\.git$/, "") };
  } catch {
    const match = url.match(/github\.com[:/](?<owner>[^/:\s]+)\/(?<repo>[^/\s]+?)(?:\.git)?(?:[/?#].*)?$/);
    const owner = match?.groups?.owner;
    const repo = match?.groups?.repo;
    return owner && repo ? { owner, repo } : undefined;
  }
}

function sanitizeSourceId(value: string): string {
  return value
    .trim()
    .toLowerCase()
    .replace(/\.git$/, "")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .replace(/-{2,}/g, "-");
}

function uniqueSourceId(sourceId: string, existingSourceIds: string[]): string {
  if (!sourceId) return "";
  const existing = new Set(existingSourceIds);
  if (!existing.has(sourceId)) return sourceId;
  for (let index = 2; ; index += 1) {
    const candidate = `${sourceId}-${index}`;
    if (!existing.has(candidate)) return candidate;
  }
}
