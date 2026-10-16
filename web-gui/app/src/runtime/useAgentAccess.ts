import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { createRuntimeClient, RuntimeHttpError } from "./client";
import { getRuntimeConnectionConfig, useRuntimeStore } from "./runtime-store";
import type { AgentSummary } from "./types";

export type AgentAccessStatus = "loading" | "available" | "unauthorized" | "missing" | "deleting" | "deleted" | "replaced" | "error";
interface AgentAccess {
  status: AgentAccessStatus;
  agent?: AgentSummary;
  error?: string;
}
interface ParentCandidate {
  agentId: string;
  supervisionId: string;
  taskId?: string;
}

/** Resolve a route independently of the public roster. Reads never start an agent. */
export function useAgentAccess(agentId: string | undefined, expectedIncarnation?: number) {
  const connection = useRuntimeStore((state) => state.bootstrap.connection);
  const config = getRuntimeConnectionConfig();
  const client = useMemo(() => createRuntimeClient(config), [connection.mode, connection.baseUrl, config.token]);
  const key = `${connection.mode}:${connection.baseUrl}:${agentId}:${expectedIncarnation}`;
  const [result, setResult] = useState<{ key: string; access: AgentAccess; parent?: ParentCandidate }>();
  const [verifiedParent, setVerifiedParent] = useState<{ key: string; agentId?: string }>();
  const [revision, setRevision] = useState(0);
  const pinnedIdentity = useRef<{ key: string; incarnation: number } | undefined>(undefined);
  const retry = useCallback(() => setRevision((value) => value + 1), []);

  useEffect(() => {
    if (!agentId) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const commit = (access: AgentAccess, parent?: ParentCandidate) => {
      if (!cancelled) setResult({ key, access, parent });
    };
    const load = async () => {
      try {
        const detail = await client.getAgentIdentity(agentId);
        const identity = detail.identity;
        // Creation lineage and capability presets are not supervision.
        const relations = detail.canonical_relations;
        const supervision = relations?.resolution === "resolved" && relations.agent_id === agentId
          ? relations.supervision : undefined;
        const supervisorId = supervision?.supervisor_agent_id;
        // Never remap a historical "default" parent to today's unrelated default route.
        const parent = supervision && (supervision.state === "active" || supervision.state === "cleanup_required") && supervision.child_agent_id === agentId
          && supervisorId && supervisorId !== agentId && supervisorId !== "default"
          ? { agentId: supervisorId, supervisionId: supervision.supervision_id, taskId: supervision.delegated_from_task_id ?? undefined } : undefined;
        const pinnedIncarnation = expectedIncarnation
          ?? (pinnedIdentity.current?.key === key ? pinnedIdentity.current.incarnation : undefined);
        if (pinnedIncarnation !== undefined && identity.incarnation !== pinnedIncarnation) {
          commit({ status: "replaced" });
          return;
        }
        if (!cancelled) pinnedIdentity.current = { key, incarnation: identity.incarnation };
        if (identity.status === "deleting" || identity.status === "deleted") {
          commit({ status: identity.status }, parent);
          return;
        }
        const agent = await client.getAgentState(agentId);
        if (agent.incarnation !== identity.incarnation) {
          commit({ status: "replaced" });
          return;
        }
        commit({ status: "available", agent }, parent);
      } catch (error) {
        const status = error instanceof RuntimeHttpError
          ? error.status === 401 || error.status === 403 ? "unauthorized" : error.status === 404 ? "missing" : "error"
          : "error";
        commit({ status, error: error instanceof Error ? error.message : String(error) });
      } finally {
        if (!cancelled) timer = setTimeout(() => void load(), 5000);
      }
    };
    void load();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [agentId, expectedIncarnation, client, key, revision]);

  const access = result?.key === key ? result.access : { status: "loading" as const };
  const parent = result?.key === key ? result.parent : undefined;
  const parentKey = `${key}:${parent?.agentId}:${parent?.supervisionId}`;
  useEffect(() => {
    if (!parent) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const load = async () => {
      let agentId: string | undefined;
      try {
        const detail = await client.getAgentIdentity(parent.agentId);
        if (detail.identity.agent_id === parent.agentId && detail.identity.status === "active") {
          agentId = parent.agentId;
        }
      } catch {
        // An unavailable parent must not block the child's conversation or input.
      } finally {
        if (!cancelled) {
          setVerifiedParent({ key: parentKey, agentId });
          timer = setTimeout(() => void load(), 5000);
        }
      }
    };
    void load();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [client, parent?.agentId, parentKey, revision]);
  const parentAgentId = verifiedParent?.key === parentKey ? verifiedParent.agentId : undefined;
  return { ...access, parentAgentId, parentTaskId: parentAgentId ? parent?.taskId : undefined, retry };
}
