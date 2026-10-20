import type { CurrentUser } from "./client";
import type { AgentSummary } from "./types";
import {
  AGENT_PRESENTATION_CACHE_SCHEMA_VERSION,
  cacheGetAgentPresentations,
  cachePutAgentPresentations,
  cacheRemoveAgentPresentation,
} from "./idb-cache";

export const AGENT_PRESENTATION_CACHE_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;
export const AGENT_PRESENTATION_CACHE_MAX_AGENTS = 500;
export const AGENT_PRESENTATION_CACHE_MAX_BYTES = 2 * 1024 * 1024;

const requiredStrings = [
  "id", "badge", "profile", "lifecycle", "focusSummary", "workspace", "attention",
  "model", "footer", "subtitle", "lastBrief", "lastTurnTime", "posture", "postureReason",
] as const;
const optionalStrings = [
  "name", "inputDisabledReason", "modelSelection", "runtimeDefaultModel",
  "modelReasoningEffort", "modelServiceTier", "modelServiceTierOverride", "waitingReason",
] as const;
const counts = ["pending", "activeTaskCount", "waitingCount"] as const;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** This is a presentation namespace, never an authorization credential. */
export function agentPresentationCacheKey(
  remoteKey: string, user: CurrentUser | undefined, agentId?: string,
): string | undefined {
  if (!user || !remoteKey || typeof user.authMethod !== "string" || !user.authMethod
    || typeof user.userId !== "string" || !user.userId) return undefined;
  const namespace = [user.authMethod, user.userId];
  if (agentId !== undefined) namespace.push(agentId);
  return `${remoteKey}#${JSON.stringify(namespace)}`;
}

function presentation(value: unknown): AgentSummary | undefined {
  if (!isRecord(value)) return undefined;
  const result: Record<string, unknown> = {};
  for (const field of requiredStrings) {
    if (typeof value[field] !== "string") return undefined;
    result[field] = value[field];
  }
  if (!(value.id as string).trim() || !(value.lifecycle as string).trim()
    || !Number.isSafeInteger(value.incarnation) || (value.incarnation as number) <= 0) return undefined;
  result.incarnation = value.incarnation;
  for (const field of counts) {
    if (!Number.isSafeInteger(value[field]) || (value[field] as number) < 0) return undefined;
    result[field] = value[field];
  }
  for (const field of optionalStrings) {
    if (value[field] === undefined) continue;
    if (typeof value[field] !== "string") return undefined;
    result[field] = value[field];
  }
  if (value.badgeHue !== undefined) {
    if (typeof value.badgeHue !== "number" || !Number.isFinite(value.badgeHue)) return undefined;
    result.badgeHue = value.badgeHue;
  }
  if (value.badgeTone !== undefined) {
    if (value.badgeTone !== "muted") return undefined;
    result.badgeTone = value.badgeTone;
  }
  if (value.modelSource !== undefined) {
    if (value.modelSource !== "runtime_default" && value.modelSource !== "agent_override") return undefined;
    result.modelSource = value.modelSource;
  }
  if (value.acceptsInput !== undefined) {
    if (typeof value.acceptsInput !== "boolean") return undefined;
    result.acceptsInput = value.acceptsInput;
  }
  // Lifecycle hints gate explicit attempts, never server-side authorization.
  return { ...result, canRename: false } as unknown as AgentSummary;
}

function retired(value: unknown): boolean {
  return isRecord(value) && (value.lifecycle === "deleting" || value.lifecycle === "deleted");
}

export async function readCachedAgents(key: string): Promise<AgentSummary[] | undefined> {
  if (!key) return undefined;
  try {
    const entry = await cacheGetAgentPresentations(key);
    if (!isRecord(entry) || entry.remoteKey !== key
      || entry.schemaVersion !== AGENT_PRESENTATION_CACHE_SCHEMA_VERSION
      || typeof entry.cachedAt !== "number" || !Number.isFinite(entry.cachedAt)
      || entry.cachedAt <= 0 || entry.cachedAt > Date.now()
      || Date.now() - entry.cachedAt > AGENT_PRESENTATION_CACHE_MAX_AGE_MS
      || !Array.isArray(entry.agents) || entry.agents.length > AGENT_PRESENTATION_CACHE_MAX_AGENTS
      || JSON.stringify(entry).length * 2 > AGENT_PRESENTATION_CACHE_MAX_BYTES) return undefined;
    const agents: AgentSummary[] = [];
    const ids = new Set<string>();
    for (const value of entry.agents) {
      if (retired(value)) continue;
      const agent = presentation(value);
      if (!agent || ids.has(agent.id)) return undefined;
      ids.add(agent.id);
      agents.push(agent);
    }
    return agents;
  } catch { return undefined; }
}

export async function writeCachedAgents(key: string, agents: AgentSummary[]): Promise<void> {
  if (!key) return;
  try {
    const entry = {
      remoteKey: key,
      schemaVersion: AGENT_PRESENTATION_CACHE_SCHEMA_VERSION,
      cachedAt: Date.now(),
      agents: [] as AgentSummary[],
    };
    let bytes = JSON.stringify(entry).length * 2;
    const ids = new Set<string>();
    for (const value of agents) {
      if (entry.agents.length >= AGENT_PRESENTATION_CACHE_MAX_AGENTS) break;
      if (retired(value)) continue;
      const agent = presentation(value);
      if (!agent || ids.has(agent.id)) continue;
      const size = JSON.stringify(agent).length * 2 + 2;
      if (bytes + size > AGENT_PRESENTATION_CACHE_MAX_BYTES) continue;
      bytes += size;
      ids.add(agent.id);
      entry.agents.push(agent);
    }
    await cachePutAgentPresentations(entry);
  } catch { /* Presentation caching is best-effort. */ }
}

export async function removeCachedAgent(key: string, agentId: string): Promise<void> {
  if (key && agentId) await cacheRemoveAgentPresentation(key, agentId);
}
