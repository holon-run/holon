import { useMemo } from "react";

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
  const stats = useMemo(() => skillStats(skills), [skills]);

  return (
    <div className="skills-inner scroll-surface">
      <section className="skills-hero context-card">
        <span className="eyebrow">Skill Library</span>
        <h1>Manage global skills</h1>
        <p>
          This page represents the global Holon skill library. It reads the daemon catalog now; add, remove, update,
          and check actions are reserved for the daemon-backed skills management API.
        </p>
        <div className="skills-actions">
          <Button type="button" variant="accent" disabled title="Waiting for daemon add API">
            Add skill
          </Button>
          <Button type="button" variant="secondary" disabled title="Waiting for daemon update/check API">
            Check updates
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
          <strong>Skill catalog unavailable</strong>
          <span>{error}</span>
        </div>
      ) : null}

      <Card className="skills-library-card">
        <CardHeader className="skills-library-head">
          <div>
            <span className="eyebrow">Installed catalog</span>
            <h2>Global library</h2>
          </div>
          <StatusBadge className="state-chip" kind="connection" value={catalog.source} />
        </CardHeader>
        <CardContent>
          {skills.length ? (
            <ul className="skills-list">
              {skills.map((skill) => (
                <SkillRow key={`${skill.scope}:${skill.skillId}:${skill.path}`} skill={skill} />
              ))}
            </ul>
          ) : (
            <EmptyState
              icon="◇"
              title={loading ? "Loading skills…" : "No skills in the global catalog"}
              description="Refresh after adding skills through the daemon API or CLI."
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
      <div className="skills-row-actions">
        <Button type="button" size="sm" variant="ghost" disabled title="Waiting for daemon update API">
          Update
        </Button>
        <Button type="button" size="sm" variant="ghost" disabled title="Waiting for daemon remove API">
          Remove
        </Button>
      </div>
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
