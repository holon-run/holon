import type { AgentSummary } from "../runtime/types";

export const ALL_WORKSPACES_FILTER = "all";
export const STANDALONE_WORKSPACE_KEY = "__standalone__";

export interface ExternalWorkspaceRef {
  /** Canonical deduplication key (normalized anchor path when available, else workspace ID). */
  key: string;
  /** Human-readable short workspace name (alias, repo name, or folder basename). */
  label: string;
  /** Full path or anchor for tooltip context. */
  anchor: string;
  workspaceId: string;
}

export interface WorkspaceFilterOption {
  key: string;
  label: string;
  anchor?: string;
  count: number;
}

export interface PrimaryWorkspaceAssignment {
  key: string;
  label: string;
  anchor?: string;
  isStandalone: boolean;
  /** Count of additional external workspaces beyond the primary one. */
  extraCount: number;
  allExternal: ExternalWorkspaceRef[];
}

const AGENT_HOME_PATH_RE = /(?:^|\/)\.holon\/agents\/[^/]+\/?$/;

function normalizePath(value?: string): string {
  if (!value) return "";
  return value.trim().replace(/\/+$/, "");
}

function pathBasename(path: string): string | undefined {
  const normalized = normalizePath(path);
  if (!normalized || normalized === "—" || normalized === "not bound") return undefined;
  return normalized.split("/").pop();
}

export function isAgentHomeWorkspace(workspaceId?: string, anchor?: string): boolean {
  const id = workspaceId?.trim() ?? "";
  const normAnchor = normalizePath(anchor);
  if ((!id || id === "not bound") && (!normAnchor || normAnchor === "—" || normAnchor === "not bound")) {
    return true;
  }
  if (id.startsWith("agent_home:")) return true;
  if (normAnchor && AGENT_HOME_PATH_RE.test(normAnchor)) return true;
  return false;
}

function canonicalWorkspaceKey(workspaceId: string, anchor: string): string {
  const normAnchor = normalizePath(anchor);
  if (normAnchor && normAnchor !== "—" && normAnchor !== "not bound" && normAnchor !== workspaceId) {
    return `path:${normAnchor}`;
  }
  return `id:${workspaceId}`;
}

/**
 * Extract deduplicated external (non-`agent_home`) workspaces mounted on an agent,
 * keeping the active external workspace first when present.
 */
export function extractExternalWorkspaces(agent: AgentSummary): ExternalWorkspaceRef[] {
  const result: ExternalWorkspaceRef[] = [];
  const seenKeys = new Set<string>();

  const pushCandidate = (workspaceId?: string, name?: string, anchor?: string, repoName?: string) => {
    const id = workspaceId?.trim() ?? "";
    const normAnchor = normalizePath(anchor);
    if (isAgentHomeWorkspace(id, normAnchor)) return;
    const effectiveId = id || normAnchor;
    if (!effectiveId) return;
    const key = canonicalWorkspaceKey(effectiveId, normAnchor || effectiveId);
    if (seenKeys.has(key)) return;
    seenKeys.add(key);
    const label =
      (name && name !== "not bound" && name !== "—" ? name.trim() : undefined)
      ?? repoName?.trim()
      ?? pathBasename(normAnchor)
      ?? effectiveId;
    result.push({
      key,
      label,
      anchor: normAnchor || label,
      workspaceId: effectiveId,
    });
  };

  if (agent.workspaceSummary) {
    pushCandidate(
      agent.workspaceSummary.id,
      agent.workspaceSummary.name,
      agent.workspaceSummary.anchor,
    );
  } else if (agent.workspace && agent.workspace !== "not bound" && agent.workspace !== "—") {
    pushCandidate(agent.workspace, agent.workspace, agent.workspace);
  }

  if (agent.attachedWorkspaces) {
    for (const attached of agent.attachedWorkspaces) {
      pushCandidate(attached.workspaceId, attached.name, attached.anchor, attached.repoName);
    }
  }

  return result;
}

/**
 * Assign an agent to a single deterministic workspace group (never duplicating an agent across groups):
 * 1. Active external workspace if the agent's current workspace is external;
 * 2. Otherwise the first attached external workspace (e.g. when `is_active` temporarily points at `agent_home`);
 * 3. Otherwise the `STANDALONE_WORKSPACE_KEY` bucket.
 */
export function resolvePrimaryWorkspace(
  agent: AgentSummary,
  standaloneLabel: string,
): PrimaryWorkspaceAssignment {
  const external = extractExternalWorkspaces(agent);
  if (external.length === 0) {
    return {
      key: STANDALONE_WORKSPACE_KEY,
      label: standaloneLabel,
      isStandalone: true,
      extraCount: 0,
      allExternal: [],
    };
  }
  const primary = external[0];
  return {
    key: primary.key,
    label: primary.label,
    anchor: primary.anchor,
    isStandalone: false,
    extraCount: external.length - 1,
    allExternal: external,
  };
}

/**
 * Multi-workspace filter predicate (M:N): an agent mounting both `holon` and `agentinbox`
 * matches when filtering by either workspace, while appearing only once in `all`.
 */
export function agentMatchesWorkspaceFilter(agent: AgentSummary, filterKey: string): boolean {
  if (!filterKey || filterKey === ALL_WORKSPACES_FILTER) return true;
  const external = extractExternalWorkspaces(agent);
  if (filterKey === STANDALONE_WORKSPACE_KEY) {
    return external.length === 0;
  }
  return external.some((ws) => ws.key === filterKey);
}

/**
 * Build workspace filter options sorted by agent count (descending) then label,
 * with the Standalone option placed at the end when standalone agents exist.
 */
export function buildWorkspaceFilterOptions(
  agents: readonly AgentSummary[],
  standaloneLabel: string,
): WorkspaceFilterOption[] {
  const byKey = new Map<string, WorkspaceFilterOption>();
  let standaloneCount = 0;

  for (const agent of agents) {
    const external = extractExternalWorkspaces(agent);
    if (external.length === 0) {
      standaloneCount += 1;
      continue;
    }
    for (const ws of external) {
      const existing = byKey.get(ws.key);
      if (existing) {
        existing.count += 1;
      } else {
        byKey.set(ws.key, {
          key: ws.key,
          label: ws.label,
          anchor: ws.anchor,
          count: 1,
        });
      }
    }
  }

  const options = [...byKey.values()].sort((left, right) => {
    if (left.count !== right.count) return right.count - left.count;
    return left.label.localeCompare(right.label);
  });

  if (standaloneCount > 0) {
    options.push({
      key: STANDALONE_WORKSPACE_KEY,
      label: standaloneLabel,
      count: standaloneCount,
    });
  }

  return options;
}
