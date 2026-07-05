import { useMemo, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";

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
  const { t } = useTranslation();
  const templates = catalog.catalog;
  const [query, setQuery] = useState("");
  const [sourceFilter, setSourceFilter] = useState<"all" | AgentTemplateSourceKind>("all");
  const [githubUrl, setGithubUrl] = useState("");
  const [customSourceId, setCustomSourceId] = useState("");
  const [newSourceUrl, setNewSourceUrl] = useState("");
  const [newSourceRef, setNewSourceRef] = useState("");
  const [sourceFormError, setSourceFormError] = useState<string | undefined>();
  const [sourceFormBusy, setSourceFormBusy] = useState(false);
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
      setSourceFormError(t("templatesPage.urlRequired"));
      return;
    }
    if (customId && id !== customId) {
      setSourceFormError(t("templatesPage.idNormalized", { id }));
      return;
    }
    if (existingSourceIds.includes(id)) {
      setSourceFormError(t("templatesPage.sourceExists", { id }));
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
        setSourceFormError(t("templatesPage.addFailed"));
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
          <span className="eyebrow">{t("templatesPage.templateLibrary")}</span>
          <h1>{t("templatesPage.title")}</h1>
          <p>{t("templatesPage.description")}</p>
        </div>
        <div className="skills-actions" aria-label={t("templatesPage.templateLibrary")}>
          <Button type="button" variant="outline" disabled={loading || syncInProgress} onClick={() => void onSyncSources()}>
            {syncInProgress ? t("templatesPage.syncing") : t("templatesPage.syncSources")}
          </Button>
          <Button type="button" variant="outline" disabled={loading} onClick={onRefresh}>
            {loading ? t("common.refreshing") : t("common.refresh")}
          </Button>
        </div>
      </section>

      {error ? (
        <div className="skills-error" role="alert">
          <strong>{t("templatesPage.operationFailed")}</strong>
          <span>{error}</span>
          <button type="button" className="skills-error-dismiss" aria-label={t("common.dismiss")} onClick={onDismissError}>×</button>
        </div>
      ) : null}

      {syncMessage && !syncInProgress ? (
        <div className="skills-success" role="status">
          <span>{syncMessage}</span>
        </div>
      ) : null}

      {catalog.diagnostics.length > 0 ? (
        <div className="skills-error" role="status">
          <strong>{t("templatesPage.catalogDiagnostics")}</strong>
          {catalog.diagnostics.map((diagnostic, index) => (
            <span key={`${diagnostic.sourceId ?? "catalog"}-${index}`}>
              {diagnostic.sourceId ? `${diagnostic.sourceId}: ` : ""}
              {diagnostic.message}
            </span>
          ))}
          <button type="button" className="skills-error-dismiss" aria-label={t("common.dismiss")} onClick={onDismissDiagnostics}>×</button>
        </div>
      ) : null}

      <Card className="skills-library-card">
        <CardHeader className="skills-library-head">
          <div>
            <p>
              {t("templatesPage.showingTemplates", { visible: visibleTemplates.length, total: templates.length })}
            </p>
          </div>
        </CardHeader>
        <CardContent>
          <div className="template-actions-grid">

            <form className="skills-add-form" onSubmit={(event) => void handleInstall(event)}>
              <label className="skills-add-source">
                <span>{t("templatesPage.installTemplate")}</span>
                <input
                  value={githubUrl}
                  placeholder={t("templatesPage.installPlaceholder")}
                  onChange={(event) => setGithubUrl(event.target.value)}
                  disabled={loading}
                />
              </label>
              <Button type="submit" variant="accent" disabled={loading || !githubUrl.trim()}>
                {t("templatesPage.install")}
              </Button>
            </form>
          </div>
          <p className="skills-add-help">
            {t("templatesPage.addHelp")}
          </p>

          <div className="skills-toolbar" role="search">
            <label className="skills-search">
              <span>{t("templatesPage.searchTemplates")}</span>
              <input
                id="templates-search"
                name="templates-search"
                type="search"
                value={query}
                placeholder={t("templatesPage.searchPlaceholder")}
                onChange={(event) => setQuery(event.target.value)}
              />
            </label>
            <label className="skills-scope-filter">
              <span>{t("templatesPage.source")}</span>
              <select value={sourceFilter} onChange={(event) => setSourceFilter(event.target.value as typeof sourceFilter)}>
                <option value="all">{t("templatesPage.allSources")}</option>
                <option value="user_global">{t("templatesPage.global")}</option>
                <option value="remote">{t("templatesPage.remote")}</option>
                <option value="agent_home">{t("templatesPage.agent")}</option>
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
              title={loading ? t("templatesPage.loading") : templates.length ? t("templatesPage.noMatch") : t("templatesPage.noTemplates")}
              description={
                templates.length
                  ? t("templatesPage.tryDifferentFilter")
                  : t("templatesPage.refreshAfterSync")
              }
            />
          )}
        </CardContent>
      </Card>

      <details className="template-remote-sources-collapse" open>
        <summary className="template-remote-sources-toggle">
          {t("templatesPage.remoteSources")} ({catalog.sources.length})
        </summary>
      <Card className="skills-library-card">
        <CardContent>
          <form className="skills-add-form template-remote-source-form" onSubmit={(event) => void handleAddSource(event)}>
            <label className="skills-add-source">
              <span>{t("templatesPage.githubUrl")}</span>
              <input
                value={newSourceUrl}
                placeholder="https://github.com/org/repo/tree/main/agent_templates"
                onChange={(event) => setNewSourceUrl(event.target.value)}
                disabled={sourceFormBusy}
              />
            </label>
            <label>
              <span>{t("templatesPage.refOptional")}</span>
              <input
                value={newSourceRef}
                placeholder="main"
                onChange={(event) => setNewSourceRef(event.target.value)}
                disabled={sourceFormBusy}
              />
            </label>
            <div className="template-source-id-preview">
              <span>{t("templatesPage.generatedSourceId")}</span>
              <strong>{customSourceId.trim() || suggestedSourceId || t("templatesPage.generatedFromUrl")}</strong>
            </div>
            <details className="template-source-advanced">
              <summary>{t("templatesPage.advancedCustomId")}</summary>
              <label>
                <span>{t("templatesPage.customSourceId")}</span>
                <input
                  value={customSourceId}
                  placeholder="org-repo"
                  onChange={(event) => setCustomSourceId(event.target.value)}
                  disabled={sourceFormBusy}
                />
              </label>
            </details>
            <Button type="submit" variant="accent" disabled={sourceFormBusy || !newSourceUrl.trim()}>
              {t("templatesPage.add")}
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
                      {t("templatesPage.remove")}
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          ) : (
            <EmptyState icon="⇄" title={t("templatesPage.noSources")} description={t("templatesPage.noSourcesDesc")} />
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
  const { t } = useTranslation();
  const [viewMode, setViewMode] = useState<"rendered" | "source">("rendered");

  return (
    <div className="skills-inner scroll-surface">
      <section className="skill-detail-hero context-card">
        <div>
          <button className="text-button" type="button" onClick={onBack}>
            {t("templatesPage.back")}
          </button>
          <span className="eyebrow">{template?.schemaVersion ?? t("templatesPage.agentTemplate")}</span>
          <h1>{template?.name ?? catalogId}</h1>
          <p>{template?.summary || t("templatesPage.readOnlyDetail")}</p>
          {template ? (
            <div className="template-detail-meta-bar">
              <StatusBadge className="state-chip" kind="connection" value={template.source} />
              <span className="template-detail-meta-id">{template.catalogId}</span>
              {template.sourceLocation ? (
                <span className="template-detail-meta-path">{template.sourceLocation}</span>
              ) : null}
              {template.skills.length ? (
                <span className="template-detail-meta-skills">{t("templatesPage.skillsCount", { count: template.skills.length })}</span>
              ) : null}
            </div>
          ) : null}
        </div>
        <div className="skills-actions">
          {template ? (
            <Button type="button" variant="accent" onClick={() => onCreateAgent(template.template)}>
              {t("templatesPage.createAgent")}
            </Button>
          ) : null}
          <Button type="button" variant="outline" disabled={loading} onClick={onRefresh}>
            {loading ? t("common.refreshing") : t("common.refresh")}
          </Button>
          {template?.source === "user_global" ? (
            <Button type="button" variant="outline" disabled={loading} onClick={() => void onRemoveTemplate(template.templateId)}>
              {t("templatesPage.remove")}
            </Button>
          ) : null}
        </div>
      </section>

      {error || detail?.error ? (
        <div className="skills-error" role="alert">
          <strong>{t("templatesPage.detailFailed")}</strong>
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
                  {t("templatesPage.rendered")}
                </Button>
                <Button type="button" size="sm" variant={viewMode === "source" ? "accent" : "outline"} onClick={() => setViewMode("source")}>
                  {t("templatesPage.sourceView")}
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
                <p>{t("templatesPage.includedSkills")}</p>
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
                <EmptyState icon="◇" title={t("templatesPage.noSkills")} description={t("templatesPage.noSkillsDesc")} />
              )}
            </CardContent>
          </Card>
        </>
      ) : (
        <EmptyState icon="▣" title={loading ? t("templatesPage.loadingDetail") : t("templatesPage.notFound")} description={catalogId} />
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
  const { t } = useTranslation();
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
          {template.includedSkills.length ? <span>{t("templatesPage.skillsCount", { count: template.includedSkills.length })}</span> : null}
          {template.sourceId ? <span>{template.sourceId}</span> : null}
        </span>
      </button>
      <div className="template-card-actions">
        {canRemove ? (
          <Button type="button" size="sm" variant="outline" disabled={loading} onClick={() => void onRemove(template.templateId)}>
            {t("templatesPage.remove")}
          </Button>
        ) : null}
      </div>
    </li>
  );
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
