import { RuntimeHttpError, isTimeoutAbortError, type createRuntimeClient } from "./client";
import { ConversationHttpError } from "@holon/conversation-sdk";
import { disposeAllConversationScopes, disposeConversationScope } from "./conversation-scope-store";
import { cacheClearConversationScope } from "./idb-cache";
import { readCachedAgents, removeCachedAgent, writeCachedAgents } from "./agent-presentation-cache";
import type { AgentSummary } from "./types";

export type AgentAccessStatus = "loading" | "revalidating" | "available" | "unauthorized" | "missing" | "deleting" | "deleted" | "replaced" | "error";
export interface ParentCandidate {
  agentId: string;
  supervisionId: string;
  taskId?: string;
}
export interface AgentAccess {
  status: AgentAccessStatus;
  agent?: AgentSummary;
  error?: string;
  parent?: ParentCandidate;
  parentAgentId?: string;
}
interface AccessRecord {
  readonly key: string;
  readonly options: AccessOptions;
  access: AgentAccess;
  idleAccess: AgentAccess;
  pinnedIncarnation?: number;
  lastAgent?: AgentSummary;
  users: number;
  generation: number;
  abort?: AbortController;
  timer?: ReturnType<typeof setTimeout>;
  inFlight?: Promise<void>;
  parentAbort?: AbortController;
  parentTimer?: ReturnType<typeof setTimeout>;
  parentGeneration: number;
  retryAttempt: number;
}
export interface AccessOptions {
  key: string;
  identityKey: string;
  agentId: string;
  expectedIncarnation?: number;
  client: Pick<ReturnType<typeof createRuntimeClient>, "getAgentIdentity" | "getAgentState">;
  scopeKey: (incarnation: number) => string;
  cacheKey: (incarnation: number) => string | undefined;
  presentationCacheKey?: string;
  rosterCacheKey?: string;
}

// Only idle records count toward the bound; mounted readers are never evicted.
export const AGENT_ACCESS_KEEP_ALIVE = 16;
const records = new Map<string, AccessRecord>();
const listeners = new Set<() => void>();
let revision = 0;
let context: string | undefined;
const LOADING: AgentAccess = { status: "loading" };

export function setAgentAccessContext(next: string): void {
  if (context !== undefined && context !== next) {
    clearAgentAccessRecords();
    disposeAllConversationScopes(false);
  }
  context = next;
}

function publish(): void {
  revision += 1;
  for (const listener of listeners) listener();
}
export const subscribeAgentAccess = (listener: () => void) => {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
};
export const agentAccessRevision = () => revision;
export function agentAccessSnapshot(key: string): AgentAccess {
  const record = records.get(key);
  return record ? record.users > 0 ? record.access : record.idleAccess : LOADING;
}
export function agentAccessRecordCount(): number { return records.size; }

/** An explicit link to an already pinned scope joins the same validation. */
export function agentAccessKey(key: string, identityKey: string, incarnation?: number): string {
  if (records.has(key) || incarnation === undefined) return key;
  for (const record of records.values()) {
    if (record.options.identityKey === identityKey && record.pinnedIncarnation === incarnation) return record.key;
  }
  return key;
}

function stop(record: AccessRecord): void {
  record.generation += 1;
  record.abort?.abort();
  clearTimeout(record.timer);
  record.timer = undefined;
  record.inFlight = undefined;
  stopParent(record);
}
export function clearAgentAccessRecords(): void {
  for (const record of records.values()) stop(record);
  records.clear();
  context = undefined;
  publish();
}
function terminal(status: AgentAccessStatus): boolean {
  return !["loading", "revalidating", "available", "error"].includes(status);
}
function retryable(status: AgentAccessStatus): boolean {
  return !terminal(status) || status === "missing" || status === "unauthorized";
}
function transientRefreshFailure(error: unknown): boolean {
  if (error instanceof RuntimeHttpError) {
    return [408, 425, 429].includes(error.status) || error.status >= 500 && error.status <= 599;
  }
  return isTimeoutAbortError(error)
    || error instanceof TypeError && /fetch|network|load failed/i.test(error.message);
}
function commit(record: AccessRecord, access: AgentAccess): void {
  const previous = record.access.parent;
  const sameParent = previous?.agentId === access.parent?.agentId && previous?.supervisionId === access.parent?.supervisionId;
  record.access = { ...access, parentAgentId: sameParent ? record.access.parentAgentId : undefined };
  record.idleAccess = terminal(access.status) ? access
    : { status: "revalidating", agent: record.lastAgent };
  if (!sameParent) {
    stopParent(record);
    if (access.parent) loadParent(record, access.parent);
  }
  publish();
}

function stopParent(record: AccessRecord): void {
  record.parentGeneration += 1;
  record.parentAbort?.abort();
  clearTimeout(record.parentTimer);
}

/** Shared by all readers, independent of (and never awaited by) child admission. */
function loadParent(record: AccessRecord, parent: ParentCandidate): void {
  const generation = record.parentGeneration;
  const abort = new AbortController();
  record.parentAbort = abort;
  const load = async () => {
    let parentAgentId: string | undefined;
    let repeat = true;
    try {
      const detail = await record.options.client.getAgentIdentity(parent.agentId, abort.signal);
      if (detail.identity.agent_id === parent.agentId && detail.identity.status === "active") parentAgentId = parent.agentId;
      else repeat = false;
    } catch (error) {
      if (error instanceof RuntimeHttpError && [401, 403, 404].includes(error.status)) repeat = false;
    } finally {
      if (record.users > 0 && record.parentGeneration === generation && records.get(record.key) === record) {
        record.access = { ...record.access, parentAgentId };
        publish();
        if (repeat) record.parentTimer = setTimeout(() => { void load(); }, 5000);
      }
    }
  };
  void load();
}
function revoke(record: AccessRecord): void {
  const incarnation = record.pinnedIncarnation;
  record.lastAgent = undefined;
  for (const key of [record.options.presentationCacheKey, record.options.rosterCacheKey]) {
    if (key !== undefined) void removeCachedAgent(key, record.options.agentId);
  }
  if (incarnation === undefined) return;
  disposeConversationScope(record.options.scopeKey(incarnation), false);
  const cacheKey = record.options.cacheKey(incarnation);
  if (cacheKey !== undefined) void cacheClearConversationScope(cacheKey, record.options.agentId);
}

function load(record: AccessRecord, preview: boolean): Promise<void> {
  if (record.inFlight) return record.inFlight;
  // Only a continuously mounted, online-admitted route retains permission
  // through transient background refresh failures. Cache is never admission.
  const retainAdmission = !preview && record.access.status === "available";
  let retryDelay = 5000;
  if (preview) commit(record, {
    status: record.lastAgent ? "revalidating" : "loading", agent: record.lastAgent,
    parent: record.access.parent,
  });
  const generation = ++record.generation;
  const abort = new AbortController();
  record.abort = abort;
  const live = () => record.users > 0 && record.generation === generation && records.get(record.key) === record;
  const { client, agentId } = record.options;
  // Identity owns continuous admission; the selected-resource scheduler owns
  // ongoing state reads. Route entry and recovery still require online state.
  const admittedAgent = retainAdmission && !record.access.error ? record.lastAgent : undefined;
  const stateResult = admittedAgent
    ? Promise.resolve({ agent: admittedAgent })
    : client.getAgentState(agentId, abort.signal)
      .then((agent) => ({ agent }), (error: unknown) => ({ error }));
  const run = async () => {
    try {
      const identityRequest = client.getAgentIdentity(agentId, abort.signal);
      const detail = await Promise.race([identityRequest, stateResult.then((result) => {
        // Only definitive access denials may preempt identity; state 409/410
        // cannot distinguish lifecycle tombstones from an incarnation change.
        if ("error" in result && result.error instanceof RuntimeHttpError
          && [401, 403, 404].includes(result.error.status)) throw result.error;
        return identityRequest;
      })]);
      if (!live()) return;
      const identity = detail.identity;
      if (!Number.isSafeInteger(identity.incarnation) || identity.incarnation < 1) {
        throw new Error("Agent identity did not provide a valid incarnation.");
      }
      const relations = detail.canonical_relations;
      const supervision = relations?.resolution === "resolved" && relations.agent_id === agentId
        ? relations.supervision : undefined;
      const supervisorId = supervision?.supervisor_agent_id;
      const parent = supervision && (supervision.state === "active" || supervision.state === "cleanup_required")
        && supervision.child_agent_id === agentId && supervisorId && supervisorId !== agentId && supervisorId !== "default"
        ? { agentId: supervisorId, supervisionId: supervision.supervision_id, taskId: supervision.delegated_from_task_id ?? undefined } : undefined;
      if (record.pinnedIncarnation !== undefined && identity.incarnation !== record.pinnedIncarnation) {
        revoke(record);
        commit(record, { status: "replaced" });
        return;
      }
      record.pinnedIncarnation = identity.incarnation;
      if (identity.status === "deleting" || identity.status === "deleted") {
        revoke(record);
        commit(record, { status: identity.status, parent });
        return;
      }
      const result = await stateResult;
      if (!live()) return;
      if ("error" in result) throw result.error;
      const agent = result.agent;
      if (identity.agent_id !== agentId || agent.id !== agentId || agent.incarnation !== identity.incarnation) {
        revoke(record);
        commit(record, { status: "replaced" });
        return;
      }
      if (agent.lifecycle === "deleting" || agent.lifecycle === "deleted") {
        revoke(record);
        commit(record, { status: agent.lifecycle, parent });
        return;
      }
      record.lastAgent = agent;
      record.retryAttempt = 0;
      commit(record, { status: "available", agent, parent });
      if (record.options.presentationCacheKey) {
        void writeCachedAgents(record.options.presentationCacheKey, [agent]);
      }
    } catch (error) {
      if (!live()) return;
      const status = error instanceof RuntimeHttpError
        ? error.status === 401 || error.status === 403 ? "unauthorized" : error.status === 404 ? "missing" : "error"
        : "error";
      if (terminal(status)) revoke(record);
      const keepAvailable = status === "error" && retainAdmission && transientRefreshFailure(error);
      record.retryAttempt += 1;
      retryDelay = Math.max(
        Math.min(30_000, 5000 * 2 ** Math.min(record.retryAttempt - 1, 3)),
        error instanceof RuntimeHttpError ? (error.retryAfterSeconds ?? 0) * 1000 : 0,
      );
      commit(record, { status: keepAvailable ? "available" : status,
        agent: status === "error" ? record.lastAgent : undefined,
        parent: record.access.parent,
        error: error instanceof Error ? error.message : String(error) });
    } finally {
      if (live()) {
        abort.abort();
        record.inFlight = undefined;
        if (!terminal(record.access.status)) record.timer = setTimeout(() => { void load(record, false); }, retryDelay);
      }
    }
  };
  record.inFlight = run();
  return record.inFlight;
}

/** Presentation may arrive before online admission, never after its revocation. */
async function hydratePresentation(record: AccessRecord): Promise<void> {
  const generation = record.generation;
  const keys = [record.options.presentationCacheKey, record.options.rosterCacheKey]
    .filter((key): key is string => key !== undefined);
  if (!keys.length) return;
  const cached = (await Promise.all(keys.map(readCachedAgents))).flatMap((agents) => agents ?? []);
  if (record.users === 0 || records.get(record.key) !== record || record.generation !== generation
    || record.lastAgent || terminal(record.access.status)) return;
  const agent = cached.filter((candidate) => candidate.id === record.options.agentId
    && (record.pinnedIncarnation === undefined || candidate.incarnation === record.pinnedIncarnation))
    .sort((a, b) => (b.incarnation ?? 0) - (a.incarnation ?? 0))[0];
  if (!agent) return;
  record.pinnedIncarnation = agent.incarnation;
  record.lastAgent = agent;
  commit(record, { ...record.access, status: record.access.status === "error" ? "error" : "revalidating", agent });
}

export function acquireAgentAccess(options: AccessOptions): () => void {
  let record = records.get(options.key);
  if (!record) {
    record = { key: options.key, options, access: LOADING, idleAccess: LOADING,
      pinnedIncarnation: options.expectedIncarnation, users: 0, generation: 0, parentGeneration: 0, retryAttempt: 0 };
  }
  // Map order is the idle-record LRU.
  records.delete(options.key);
  records.set(options.key, record);
  record.users += 1;
  if (record.users === 1 && record.access.parent) loadParent(record, record.access.parent);
  if (record.users === 1 && retryable(record.access.status)) void load(record, true);
  if (record.users === 1 && !record.lastAgent && !terminal(record.access.status)) void hydratePresentation(record);
  publish();
  const retained = record;
  return () => {
    if (records.get(options.key) !== retained) return;
    retained.users -= 1;
    if (retained.users !== 0) return;
    stop(retained);
    records.delete(options.key);
    records.set(options.key, retained);
    const idle = [...records.values()].filter((entry) => entry.users === 0);
    for (const entry of idle.slice(0, Math.max(0, idle.length - AGENT_ACCESS_KEEP_ALIVE))) records.delete(entry.key);
    publish();
  };
}

export function retryAgentAccess(key: string): void {
  const record = records.get(key);
  if (!record || !retryable(record.access.status)) return;
  clearTimeout(record.timer);
  void load(record, true);
}

export function agentAccessRejection(error: unknown): AgentAccessStatus | undefined {
  if (error instanceof RuntimeHttpError) {
    if (error.status === 409 && error.code === "agent_incarnation_mismatch") return "replaced";
    return error.status === 401 || error.status === 403 ? "unauthorized"
      : error.status === 404 ? "missing" : undefined;
  }
  if (!(error instanceof ConversationHttpError)) return undefined;
  if (error.body.code === "conversation_identity_changed") return "replaced";
  return error.status === 401 || error.status === 403 ? "unauthorized"
    : error.status === 404 ? "missing" : undefined;
}

/** A decoded conversation rejection revokes admission without waiting for a poll. */
export function rejectAgentAccess(key: string, error: unknown): void {
  const record = records.get(key);
  const status = agentAccessRejection(error);
  if (!record || !status || terminal(record.access.status)) return;
  stop(record);
  revoke(record);
  commit(record, { status });
}
