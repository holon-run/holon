import { useMemo, useState, type FormEvent } from "react";

import { parseSkillMarkdown } from "./parseSkillMarkdown";
import { Button } from "../../components/ui/Button";
import { Card, CardContent, CardHeader } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { MarkdownContent } from "../../components/MarkdownContent";
import { StatusBadge } from "../../components/ui/StatusChip";
import type { SkillInstallJob } from "../../runtime/runtime-store";
import type { AddSkillInput, SkillCatalogEntry, SkillCatalogState, SkillDetailState, SkillInstallMode } from "../../runtime/types";

interface SkillsPageProps {
  catalog: SkillCatalogState;
  loading: boolean;
  error?: string;
  installJobs: SkillInstallJob[];
  onRefresh: () => void;
  onAddSkill: (input: AddSkillInput) => Promise<boolean>;
  onRemoveSkill: (name: string) => Promise<boolean>;
  onOpenSkill: (skillId: string) => void;
}

type AddSourceType = Extract<AddSkillInput["kind"], "local" | "remote">;

export function SkillsPage({
  catalog,
  loading,
  error,
  installJobs,
  onRefresh,
  onAddSkill,
  onRemoveSkill,
  onOpenSkill,
}: SkillsPageProps) {
  const skills = catalog.catalog;
  const [query, setQuery] = useState("");
  const [scopeFilter, setScopeFilter] = useState<"all" | SkillCatalogEntry["scope"]>("all");
  const [addSourceType, setAddSourceType] = useState<AddSourceType>("remote");
  const [addSource, setAddSource] = useState("");
  const [addSkillName, setAddSkillName] = useState("");
  const [addMode, setAddMode] = useState<SkillInstallMode>("linked");
  const stats = useMemo(() => skillStats(skills), [skills]);
  const libraryRoots = useMemo(() => summarizeLibraryRoots(skills), [skills]);
  const visibleSkills = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return skills.filter((skill) => {
      const matchesScope = scopeFilter === "all" || normalizedSkillScope(skill.scope) === scopeFilter;
      if (!matchesScope) return false;
      if (!normalizedQuery) return true;
      return [skill.name, skill.description, skill.skillId, skill.rootId, skill.skillDir]
        .filter((value): value is string => Boolean(value))
        .some((value) => value.toLowerCase().includes(normalizedQuery));
    });
  }, [query, scopeFilter, skills]);

  async function handleAddSkill(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const source = addSource.trim();
    if (!source) return;
    const input = buildAddSkillInput(addSourceType, source, addSkillName.trim(), addMode);
    const ok = await onAddSkill(input);
    if (ok) {
      setAddSource("");
      setAddSkillName("");
    }
  }

  async function removeSkill(name: string) {
    await onRemoveSkill(name);
  }

  return (
    <div className="skills-inner scroll-surface">
      <section className="skills-hero context-card">
        <div className="skills-hero-copy">
          <span className="eyebrow">Skill Library</span>
          <h1>Global Skill Library</h1>
          <p>
            Install reusable skills into the Global Skill Library through the daemon API. Skills are stored under{" "}
            <code>{libraryRoots.user}</code>; workspace and agent-scoped skills may also appear in the effective catalog.
          </p>
        </div>
        <div className="skills-actions" aria-label="Skill library actions">
          <Button type="button" variant="outline" disabled={loading} onClick={onRefresh}>
            {loading ? "Refreshing…" : "Refresh"}
          </Button>
        </div>
      </section>

      <section className="skills-summary" aria-label="Skill library summary">
        {stats.map((stat) => (
          <Card className="skills-stat" key={stat.label}>
            <strong>{stat.value}</strong>
            <span>{stat.label}</span>
          </Card>
        ))}
      </section>

      {error ? (
        <div className="skills-error" role="alert">
          <strong>Skill operation failed</strong>
          <span>{error}</span>
        </div>
      ) : null}
      {installJobs.length > 0 ? (
        <div className="skills-install-jobs" role="status" aria-label="Skill install jobs">
          {installJobs.map((job) => (
            <div key={job.jobId} className="skills-install-job">
              {(job.status === "queued" || job.status === "running") ? (
                <span className="spinner" aria-hidden="true" />
              ) : null}
              <span className="skills-install-job-source">{job.source}</span>
              <span className={`skills-install-job-status status-${job.status}`}>
                {job.status === "failed" ? `failed: ${job.error ?? "unknown error"}` : job.status}
              </span>
            </div>
          ))}
        </div>
      ) : null}

      <Card className="skills-library-card">
        <CardHeader className="skills-library-head">
          <div>
            <p>
              Showing {visibleSkills.length} of {skills.length} skills
            </p>
          </div>
          <StatusBadge className="state-chip" kind="connection" value={catalog.source} />
        </CardHeader>
        <CardContent>
          <form className="skills-add-form" onSubmit={(event) => void handleAddSkill(event)}>
            <label>
              <span>Install skill to Global Library</span>
              <select
                value={addSourceType}
                onChange={(event) => setAddSourceType(event.target.value as AddSourceType)}
                disabled={loading}
              >
                <option value="remote">Remote package</option>
                <option value="local">Local folder</option>
              </select>
            </label>
            <label className="skills-add-source">
              <span>Source</span>
              <input
                value={addSource}
                placeholder={sourcePlaceholder(addSourceType)}
                onChange={(event) => setAddSource(event.target.value)}
                disabled={loading}
              />
            </label>
            {addSourceType === "remote" ? (
              <label>
                <span>Skill</span>
                <input
                  value={addSkillName}
                  placeholder="optional; leave empty to install all skills"
                  onChange={(event) => setAddSkillName(event.target.value)}
                  disabled={loading}
                />
              </label>
            ) : null}
            {addSourceType === "local" ? (
              <label>
                <span>Install mode</span>
                <select value={addMode} onChange={(event) => setAddMode(event.target.value as SkillInstallMode)} disabled={loading}>
                  <option value="linked">Linked local ref</option>
                  <option value="copied">Copied snapshot</option>
                </select>
              </label>
            ) : null}
            <Button type="submit" variant="accent" disabled={loading || !addSource.trim()}>
              Install
            </Button>
          </form>
          <p className="skills-add-help">
            Remote packages are imported into the Global Library by the skill manager and do not need a link/copy choice here.
            Leave Skill empty to import every skill under a GitHub repository's <code>skills/</code> directory,
            or provide one concrete skill such as <code>docx</code>.
            Local folders can be linked in place or copied as a snapshot.
          </p>

          <div className="skills-toolbar" role="search">
            <label className="skills-search">
              <span>Search skills</span>
              <input
                id="skills-search"
                name="skills-search"
                type="search"
                value={query}
                placeholder="Name, description, or skill id"
                onChange={(event) => setQuery(event.target.value)}
              />
            </label>
            <label className="skills-scope-filter">
              <span>Scope</span>
              <select
                id="skills-scope"
                name="skills-scope"
                value={scopeFilter}
                onChange={(event) => setScopeFilter(event.target.value as typeof scopeFilter)}
              >
                <option value="all">All scopes</option>
                <option value="user">Global</option>
                <option value="workspace">Workspace</option>
                <option value="agent">Agent</option>
              </select>
            </label>
          </div>

          {visibleSkills.length ? (
            <ul className="skills-list">
              {visibleSkills.map((skill) => (
                <SkillRow
                  key={skill.skillId}
                  skill={skill}
                  loading={loading}
                  onRemove={removeSkill}
                  onOpen={onOpenSkill}
                />
              ))}
            </ul>
          ) : (
            <EmptyState
              icon="◇"
              title={loading ? "Loading skills…" : skills.length ? "No skills match the current filters" : "No skills in the global catalog"}
              description={
                skills.length
                  ? "Try a different query or scope filter."
                  : "Refresh after adding skills through the daemon API or CLI."
              }
            />
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function SkillRow({
  skill,
  loading,
  onRemove,
  onOpen,
}: {
  skill: SkillCatalogEntry;
  loading: boolean;
  onRemove: (name: string) => void;
  onOpen: (skillId: string) => void;
}) {
  return (
    <li className="skills-row">
      <button type="button" className="skills-row-open" onClick={() => onOpen(skill.skillId)}>
        <div>
          <strong>{skill.name}</strong>
          <StatusBadge className="state-chip" kind="connection" value={skill.scope}>
            {skillScopeLabel(skill.scope)}
          </StatusBadge>
        </div>
        <p>{skill.description || "No description provided."}</p>
      </button>
      <div className="skills-row-actions">
        <Button type="button" size="sm" variant="outline" disabled={loading || normalizedSkillScope(skill.scope) !== "user"} onClick={() => onRemove(skill.name)}>
          Remove
        </Button>
      </div>
    </li>
  );
}

export function SkillDetailPage({
  skillId,
  detail,
  loading,
  error,
  onBack,
  onRefresh,
}: {
  skillId: string;
  detail?: SkillDetailState;
  loading: boolean;
  error?: string;
  onBack: () => void;
  onRefresh: () => void;
}) {
  const skill = detail?.skill;
  return (
    <section className="page skill-detail-route" aria-label="Skill detail">
      <nav className="skill-detail-breadcrumb" aria-label="Breadcrumb">
        <button type="button" className="breadcrumb-back" onClick={onBack}>
          ← Skills
        </button>
        <span className="breadcrumb-sep" aria-hidden="true">/</span>
        <span className="breadcrumb-current">{skill?.name ?? skillId}</span>
      </nav>
      <div className="skills-inner skill-detail-page scroll-surface">
      <section className="skills-hero context-card">
        <div className="skills-hero-copy">
          <span className="eyebrow">Skill detail</span>
          <h1>{skill?.name ?? skillId}</h1>
          {skill?.description ? <p>{skill.description}</p> : null}
        </div>
        <div className="skills-actions" aria-label="Skill detail actions">
          <Button type="button" variant="outline" disabled={loading} onClick={onRefresh}>
            {loading ? "Refreshing…" : "Refresh"}
          </Button>
        </div>
      </section>

      {skill ? (
        <dl className="skills-detail-meta">
          <div className="skills-detail-meta-item">
            <dt>Scope</dt>
            <dd><code>{skill.scope}</code></dd>
          </div>
          <div className="skills-detail-meta-item">
            <dt>Skill directory</dt>
            <dd><code>{skill.skillDir}</code></dd>
          </div>
          <div className="skills-detail-meta-item">
            <dt>Path</dt>
            <dd><code>{collapseHome(skill.path)}</code></dd>
          </div>
        </dl>
      ) : null}

      {error || detail?.error ? (
        <div className="skills-error" role="alert">
          <strong>Skill detail failed</strong>
          <span>{error ?? detail?.error}</span>
        </div>
      ) : null}
      {skill ? (
        <div className="skill-detail-card">
          <MarkdownContent text={parseSkillMarkdown(detail?.content ?? "")} />
        </div>
      ) : (
        <EmptyState
          icon="◇"
          title={loading ? "Loading skill…" : "Skill not found"}
          description={
            loading
              ? "Resolving the skill through the catalog."
              : "The requested skill id was not found in the Global Skill Library."
          }
        />
      )}
      </div>
    </section>
  );
}

function buildAddSkillInput(type: AddSourceType, source: string, skill: string, mode: SkillInstallMode): AddSkillInput {
  if (type === "local") return { kind: "local", path: source, mode };
  return { kind: "remote", package: source, skill: skill || undefined };
}

function sourcePlaceholder(type: AddSourceType) {
  if (type === "local") return "/path/to/skill";
  return "owner/repo, owner/repo@skill, or GitHub tree URL";
}

function skillScopeLabel(scope: SkillCatalogEntry["scope"]) {
  if (normalizedSkillScope(scope) === "user") return "Global";
  if (scope === "workspace") return "Workspace";
  return "Agent";
}

function normalizedSkillScope(scope: SkillCatalogEntry["scope"]) {
  return scope === "user_global" ? "user" : scope;
}

export function summarizeLibraryRoots(skills: SkillCatalogEntry[]) {
  void skills;
  return { user: "~/.agents/skills" };
}

export function skillRoot(path?: string) {
  if (!path) return undefined;
  for (const marker of ["/skills/", "/.agents/skills/", "/.codex/skills/", "/.claude/skills/"]) {
    const index = path.indexOf(marker);
    if (index >= 0) return path.slice(0, index + marker.length - 1);
  }
  return path.replace(/\/[^/]+$/, "");
}

function collapseHome(path: string) {
  return path.replace(/^\/Users\/[^/]+/, "~");
}

function skillStats(skills: SkillCatalogEntry[]) {
  const byScope = skills.reduce<Record<string, number>>((counts, skill) => {
    const scope = normalizedSkillScope(skill.scope);
    counts[scope] = (counts[scope] ?? 0) + 1;
    return counts;
  }, {});
  return [
    { label: "total skills", value: String(skills.length) },
    { label: "global", value: String(byScope.user ?? 0) },
    { label: "workspace", value: String(byScope.workspace ?? 0) },
    { label: "agent scoped", value: String(byScope.agent ?? 0) },
  ];
}
