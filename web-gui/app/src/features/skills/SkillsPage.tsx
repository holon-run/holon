import { useMemo, useState, type FormEvent } from "react";

import { Button } from "../../components/ui/Button";
import { Card, CardContent, CardHeader } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type { AddSkillInput, SkillCatalogEntry, SkillCatalogState, SkillInstallMode } from "../../runtime/types";

interface SkillsPageProps {
  catalog: SkillCatalogState;
  loading: boolean;
  error?: string;
  onRefresh: () => void;
  onAddSkill: (input: AddSkillInput) => Promise<boolean>;
  onRemoveSkill: (name: string) => Promise<boolean>;
  onUpdateSkill: (name?: string) => Promise<boolean>;
  onCheckSkill: (name?: string) => Promise<boolean>;
}

type AddSourceType = Extract<AddSkillInput["kind"], "local" | "remote">;

export function SkillsPage({
  catalog,
  loading,
  error,
  onRefresh,
  onAddSkill,
  onRemoveSkill,
  onUpdateSkill,
  onCheckSkill,
}: SkillsPageProps) {
  const skills = catalog.catalog;
  const [query, setQuery] = useState("");
  const [scopeFilter, setScopeFilter] = useState<"all" | SkillCatalogEntry["scope"]>("all");
  const [addSourceType, setAddSourceType] = useState<AddSourceType>("remote");
  const [addSource, setAddSource] = useState("");
  const [addSkillName, setAddSkillName] = useState("");
  const [addMode, setAddMode] = useState<SkillInstallMode>("linked");
  const [message, setMessage] = useState<string>();
  const stats = useMemo(() => skillStats(skills), [skills]);
  const libraryRoots = useMemo(() => summarizeLibraryRoots(skills), [skills]);
  const visibleSkills = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return skills.filter((skill) => {
      const matchesScope = scopeFilter === "all" || skill.scope === scopeFilter;
      if (!matchesScope) return false;
      if (!normalizedQuery) return true;
      return [skill.name, skill.description, skill.skillId, skill.path]
        .filter(Boolean)
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
      setMessage(`Installed ${source} to the User Skill Library.`);
    }
  }

  async function runCatalogAction(action: "update" | "check", name?: string) {
    const ok = action === "update" ? await onUpdateSkill(name) : await onCheckSkill(name);
    if (ok) setMessage(`${action === "update" ? "Updated" : "Checked"} ${name ?? "the Skill Library"}.`);
  }

  async function removeSkill(name: string) {
    const ok = await onRemoveSkill(name);
    if (ok) setMessage(`Removed ${name} from the User Skill Library.`);
  }

  return (
    <div className="skills-inner scroll-surface">
      <section className="skills-hero context-card">
        <div className="skills-hero-copy">
          <span className="eyebrow">Skill Library</span>
          <h1>User Skill Library</h1>
          <p>
            Install reusable skills into the User Skill Library through the daemon API. Skills are stored under{" "}
            <code>{libraryRoots.user}</code>; workspace and agent-scoped skills may also appear in the effective catalog.
          </p>
        </div>
        <div className="skills-actions" aria-label="Skill library actions">
          <Button type="button" variant="outline" disabled={loading} onClick={() => void runCatalogAction("check")}>
            Check
          </Button>
          <Button type="button" variant="outline" disabled={loading} onClick={() => void runCatalogAction("update")}>
            Update
          </Button>
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
      {message && !error ? (
        <div className="skills-message" role="status">
          {message}
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
              <span>Install skill to User Library</span>
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
                  placeholder="optional package skill name"
                  onChange={(event) => setAddSkillName(event.target.value)}
                  disabled={loading}
                />
              </label>
            ) : null}
            <label>
              <span>Install mode</span>
              <select value={addMode} onChange={(event) => setAddMode(event.target.value as SkillInstallMode)} disabled={loading}>
                <option value="linked">Linked local ref</option>
                <option value="copied">Copied snapshot</option>
              </select>
            </label>
            <Button type="submit" variant="accent" disabled={loading || !addSource.trim()}>
              Install
            </Button>
          </form>
          <p className="skills-add-help">
            Remote packages are downloaded into the user skill cache first; Linked then points the User Library at that local
            folder, while Copied stores a snapshot in the User Library. Local folders use the same link-or-copy behavior directly.
          </p>

          <div className="skills-toolbar" role="search">
            <label className="skills-search">
              <span>Search skills</span>
              <input
                id="skills-search"
                name="skills-search"
                type="search"
                value={query}
                placeholder="Name, description, or id"
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
                <option value="user">User/global</option>
                <option value="workspace">Workspace</option>
                <option value="agent">Agent</option>
              </select>
            </label>
          </div>

          {visibleSkills.length ? (
            <ul className="skills-list">
              {visibleSkills.map((skill) => (
                <SkillRow
                  key={`${skill.scope}:${skill.skillId}:${skill.path}`}
                  skill={skill}
                  loading={loading}
                  onRemove={removeSkill}
                  onUpdate={(name) => runCatalogAction("update", name)}
                  onCheck={(name) => runCatalogAction("check", name)}
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
  onUpdate,
  onCheck,
}: {
  skill: SkillCatalogEntry;
  loading: boolean;
  onRemove: (name: string) => void;
  onUpdate: (name: string) => void;
  onCheck: (name: string) => void;
}) {
  return (
    <li className="skills-row">
      <div className="skills-row-main">
        <div>
          <strong>{skill.name}</strong>
          <StatusBadge className="state-chip" kind="connection" value={skill.scope} />
        </div>
        <p>{skill.description || "No description provided."}</p>
        <span className="skills-row-id">{skill.skillId}</span>
      </div>
      <div className="skills-row-actions">
        <Button type="button" size="sm" variant="ghost" disabled={loading} onClick={() => onCheck(skill.name)}>
          Check
        </Button>
        <Button type="button" size="sm" variant="ghost" disabled={loading} onClick={() => onUpdate(skill.name)}>
          Update
        </Button>
        <Button type="button" size="sm" variant="outline" disabled={loading || skill.scope !== "user"} onClick={() => onRemove(skill.name)}>
          Remove
        </Button>
      </div>
    </li>
  );
}

function buildAddSkillInput(type: AddSourceType, source: string, skill: string, mode: SkillInstallMode): AddSkillInput {
  if (type === "local") return { kind: "local", path: source, mode };
  return { kind: "remote", package: source, skill: skill || undefined, mode };
}

function sourcePlaceholder(type: AddSourceType) {
  if (type === "local") return "/path/to/skill";
  return "owner/repo or package";
}

function summarizeLibraryRoots(skills: SkillCatalogEntry[]) {
  const userPath = skills.find((skill) => skill.scope === "user")?.path;
  return { user: collapseHome(skillRoot(userPath) ?? "~/.agents/skills") };
}

function skillRoot(path?: string) {
  if (!path) return undefined;
  const marker = "/.agents/skills/";
  const index = path.indexOf(marker);
  if (index >= 0) return path.slice(0, index + marker.length - 1);
  return path.replace(/\/[^/]+$/, "");
}

function collapseHome(path: string) {
  return path.replace(/^\/Users\/[^/]+/, "~");
}

function skillStats(skills: SkillCatalogEntry[]) {
  const byScope = skills.reduce<Record<string, number>>((counts, skill) => {
    counts[skill.scope] = (counts[skill.scope] ?? 0) + 1;
    return counts;
  }, {});
  return [
    { label: "total skills", value: String(skills.length) },
    { label: "user/global", value: String(byScope.user ?? 0) },
    { label: "workspace", value: String(byScope.workspace ?? 0) },
    { label: "agent scoped", value: String(byScope.agent ?? 0) },
  ];
}
