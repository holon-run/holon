import { useMemo, useState } from "react";

import { Button } from "../../components/ui/Button";
import { Card, CardContent, CardHeader } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type { SkillCatalogEntry, SkillCatalogState } from "../../runtime/types";

interface SkillsPageProps {
  catalog: SkillCatalogState;
  loading: boolean;
  error?: string;
  onRefresh: () => void;
}

export function SkillsPage({ catalog, loading, error, onRefresh }: SkillsPageProps) {
  const skills = catalog.catalog;
  const [query, setQuery] = useState("");
  const [scopeFilter, setScopeFilter] = useState<"all" | SkillCatalogEntry["scope"]>("all");
  const stats = useMemo(() => skillStats(skills), [skills]);
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

  return (
    <div className="skills-inner scroll-surface">
      <section className="skills-hero context-card">
        <div className="skills-hero-copy">
          <span className="eyebrow">Skill Library</span>
          <h1>Global skills</h1>
          <p>
            Browse the daemon catalog for the global Holon skill library. Mutating actions such as add, remove, update,
            and check are intentionally read-only here until the daemon skills control API lands.
          </p>
        </div>
        <div className="skills-actions" aria-label="Skill library actions">
          <span className="skills-readonly-note" title="The current daemon only exposes the catalog read API.">
            Read-only catalog
          </span>
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
          <strong>Skill catalog unavailable</strong>
          <span>{error}</span>
        </div>
      ) : null}

      <Card className="skills-library-card">
        <CardHeader className="skills-library-head">
          <div>
            <span className="eyebrow">Installed catalog</span>
            <h2>Global library</h2>
            <p>
              Showing {visibleSkills.length} of {skills.length} skills
            </p>
          </div>
          <StatusBadge className="state-chip" kind="connection" value={catalog.source} />
        </CardHeader>
        <CardContent>
          <div className="skills-toolbar" role="search">
            <label className="skills-search">
              <span>Search skills</span>
              <input
                id="skills-search"
                name="skills-search"
                type="search"
                value={query}
                placeholder="Name, description, id, or path"
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
                <SkillRow key={`${skill.scope}:${skill.skillId}:${skill.path}`} skill={skill} />
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

function SkillRow({ skill }: { skill: SkillCatalogEntry }) {
  return (
    <li className="skills-row">
      <div className="skills-row-main">
        <div>
          <strong>{skill.name}</strong>
          <StatusBadge className="state-chip" kind="connection" value={skill.scope} />
        </div>
        <p>{skill.description || "No description provided."}</p>
        <code>{skill.path || skill.skillId}</code>
      </div>
      <span className="skills-row-status" title="Update and remove require the daemon skills control API.">
        catalog only
      </span>
    </li>
  );
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
