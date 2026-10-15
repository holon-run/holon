import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { createRuntimeClient, RuntimeHttpError } from "./client";
import { getRuntimeConnectionConfig, useRuntimeStore } from "./runtime-store";
import type { AgentSummary } from "./types";

export type AgentAccessStatus = "loading" | "available" | "unauthorized" | "missing" | "deleting" | "deleted" | "replaced" | "error";
interface AgentAccess {
  status: AgentAccessStatus;
  agent?: AgentSummary;
  parentAgentId?: string;
  parentTaskId?: string;
  error?: string;
}

/** Resolve a route independently of the public roster. Reads never start an agent. */
export function useAgentAccess(agentId: string | undefined, expectedIncarnation?: number) {
  const connection = useRuntimeStore((state) => state.bootstrap.connection);
  const config = getRuntimeConnectionConfig();
  const client = useMemo(() => createRuntimeClient(config), [connection.mode, connection.baseUrl, config.token]);
  const key = `${connection.mode}:${connection.baseUrl}:${agentId}:${expectedIncarnation}`;
  const [result, setResult] = useState<{ key: string; access: AgentAccess }>();
  const [revision, setRevision] = useState(0);
  const pinnedIdentity = useRef<{ key: string; incarnation: number } | undefined>(undefined);
  const retry = useCallback(() => setRevision((value) => value + 1), []);

  useEffect(() => {
    if (!agentId) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const commit = (access: AgentAccess) => {
      if (!cancelled) setResult({ key, access });
    };
    const load = async () => {
      try {
        const detail = await client.getAgentIdentity(agentId);
        const identity = detail.identity;
        const relation = {
          parentAgentId: identity.parent_agent_id ?? identity.lineage_parent_agent_id ?? undefined,
          parentTaskId: identity.delegated_from_task_id ?? undefined,
        };
        const pinnedIncarnation = expectedIncarnation
          ?? (pinnedIdentity.current?.key === key ? pinnedIdentity.current.incarnation : undefined);
        if (pinnedIncarnation !== undefined && identity.incarnation !== pinnedIncarnation) {
          commit({ status: "replaced", ...relation });
          return;
        }
        if (!cancelled) pinnedIdentity.current = { key, incarnation: identity.incarnation };
        if (identity.status === "deleting" || identity.status === "deleted") {
          commit({ status: identity.status, ...relation });
          return;
        }
        const agent = await client.getAgentState(agentId);
        if (agent.incarnation !== identity.incarnation) {
          commit({ status: "replaced", ...relation });
          return;
        }
        commit({ status: "available", agent, ...relation });
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

  return { ...(result?.key === key ? result.access : { status: "loading" as const }), retry };
}
