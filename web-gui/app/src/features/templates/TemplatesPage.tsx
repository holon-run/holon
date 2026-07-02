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
  error?: string;
  onRefresh: () => void;
  onSyncSources: () => Promise<boolean>;
  onInstallTemplate: (githubUrl: string) => Promise<boolean>;
  onRemoveTemplate: (templateId: string) => Promise<boolean>;
  onOpenTemplate: (catalogId: string) => void;
  onAddRemoteSource: (sourceId: string, url: string, gitRef?: string) => Promise<boolean>;
  onRemoveRemoteSource: (sourceId: string) => Promise<boolean>;
}

interface TemplateDetailPageProps {
  catalogId: string;
  detail?: AgentTemplateDetailState;
  loading: boolean;
  error?: string;
  onBack: () => void;
  onRefresh: () => void;
  onRemoveTemplate: (templateId: string) => Promise<boolean>;
}

export function TemplatesPage({
  catalog,
  loading,
  error,
  onRefresh,
  onSyncSources,
  onInstallTemplate,
  onRemoveTemplate,
  onOpenTemplate,
  onAddRemoteSource,
  onRemoveRemoteSource,
}: TemplatesPageProps) {
  const templates = catalog.catalog;
  const [query, setQuery] = useState("");
  const [sourceFilter, setSourceFilter] = useState<"all" | AgentTemplateSourceKind>("all");
  const [githubUrl, setGithubUrl] = useState("");
  const [newSourceId, setNewSourceId] = useState("");
  const [newSourceUrl, setNewSourceUrl] = useState("");
  const [newSourceRef, setNewSourceRef] = useState("");
  const [sourceFormError, setSourceFormError] = useState<string | undefined>();
  const [sourceFormBusy, setSourceFormBusy] = useState(false);
  const stats = useMemo(() => templateStats(templates), [templates]);
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
    const id = newSourceId.trim();
    const url = newSourceUrl.trim();
    if (!id || !url) return;
    setSourceFormBusy(true);
    setSourceFormError(undefined);
    try {
      const ok = await onAddRemoteSource(id, url, newSourceRef.trim() || undefined);
      if (ok) {
        setNewSourceId("");
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
            Browse read-only AgentTemplate definitions from builtin, global, and remote sources. Install/remove user templates
            through daemon APIs; source configuration can follow in a later settings phase.
          </p>
        </div>
        <div className="skills-actions" aria-label="Template library actions">
          <Button type="button" variant="outline" disabled={loading} onClick={() => void onSyncSources()}>
            Sync sources
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
        </div>
      ) : null}

      <Card className="skills-library-card">
        <CardHeader className="skills-library-head">
          <div>
            <p>
              Showing {visibleTemplates.length} of {templates.length} templates
            </p>
          </div>
          <StatusBadge className="state-chip" kind="connection" value={catalog.source} />
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
            Template content is read-only. Use the + button in Active Agents to create an agent from a template. Removal targets user/global templates; builtin and remote entries remain owned by their source.
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
                <option value="builtin">Builtin</option>
                <option value="user_global">Global</option>
                <option value="user">User</option>
                <option value="remote">Remote</option>
                <option value="agent">Agent</option>
              </select>
            </label>
          </div>

          {visibleTemplates.length ? (
            <ul className="skills-list templates-list">
              {visibleTemplates.map((template) => (
                <TemplateRow
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

      <Card className="skills-library-card">
        <CardHeader className="skills-library-head">
          <div>
            <p>Remote sources</p>
          </div>
          <StatusBadge className="state-chip" kind="connection" value={catalog.source} />
        </CardHeader>
        <CardContent>
          <form className="skills-add-form" onSubmit={(event) => void handleAddSource(event)}>
            <label>
              <span>Source ID</span>
              <input
                value={newSourceId}
                placeholder="my-templates"
                onChange={(event) => setNewSourceId(event.target.value)}
                disabled={sourceFormBusy}
              />
            </label>
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
            <Button type="submit" variant="accent" disabled={sourceFormBusy || !newSourceId.trim() || !newSourceUrl.trim()}>
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
          <span className="eyebrow">AgentTemplate</span>
          <h1>{template?.name ?? catalogId}</h1>
          <p>{template?.summary || "Read-only template detail from the Holon daemon catalog."}</p>
        </div>
        <div className="skills-actions">
          <Button type="button" variant="outline" disabled={loading} onClick={onRefresh}>
            {loading ? "Refreshing…" : "Refresh"}
          </Button>
          {template?.source === "user_global" || template?.source === "user" ? (
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
                <p>{template.template}</p>
              </div>
              <StatusBadge className="state-chip" kind="connection" value={template.source} />
            </CardHeader>
            <CardContent>
              <dl className="skills-detail-meta">
                <div>
                  <dt>Catalog id</dt>
                  <dd>{template.catalogId}</dd>
                </div>
                <div>
                  <dt>Template id</dt>
                  <dd>{template.templateId}</dd>
                </div>
                {template.schemaVersion ? (
                  <div>
                    <dt>Schema</dt>
                    <dd>{template.schemaVersion}</dd>
                  </div>
                ) : null}
                {template.sourceLocation ? (
                  <div>
                    <dt>Source</dt>
                    <dd>{template.sourceLocation}</dd>
                  </div>
                ) : null}
              </dl>
            </CardContent>
          </Card>

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

function TemplateRow({
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
  const canRemove = template.source === "user_global" || template.source === "user";
  return (
    <li className="skills-row">
      <button className="skills-row-open" type="button" onClick={() => onOpen(template.catalogId)}>
        <span className="template-row-title">
          <strong>{template.name}</strong>
          <StatusBadge className="state-chip" kind="connection" value={template.source} />
        </span>
        <span className="template-row-description">{template.description || template.template}</span>
        <span className="template-row-meta">
          <span>{template.catalogId}</span>
          {template.includedSkills.length ? <span>{template.includedSkills.length} skills</span> : null}
          {template.sourceId ? <span>{template.sourceId}</span> : null}
        </span>
      </button>
      <div className="skills-row-actions">

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
    { label: "builtin", value: String(templates.filter((template) => template.source === "builtin").length) },
    { label: "global", value: String(templates.filter((template) => template.source === "user_global" || template.source === "user").length) },
    { label: "remote", value: String(templates.filter((template) => template.source === "remote").length) },
  ];
}
