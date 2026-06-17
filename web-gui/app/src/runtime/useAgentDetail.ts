import { useEffect } from "react";

import { useRuntimeStore } from "./runtime-store";
import type { AgentDetail, DisplayLevel } from "./types";

interface AgentDetailState {
  detail: AgentDetail | null;
  loading: boolean;
  refresh: () => Promise<void>;
}

export function useAgentDetail(agentId: string | undefined, displayLevel: DisplayLevel): AgentDetailState {
  const detail = useRuntimeStore((state) => (agentId ? state.sessionsByAgentId[agentId]?.detail ?? null : null));
  const loading = useRuntimeStore((state) => (agentId ? state.sessionsByAgentId[agentId]?.loading ?? false : false));
  const refreshAgentDetail = useRuntimeStore((state) => state.refreshAgentDetail);
  const registerAgentForEvents = useRuntimeStore((state) => state.registerAgentForEvents);
  const unregisterAgentForEvents = useRuntimeStore((state) => state.unregisterAgentForEvents);
  const refresh = async () => {
    await refreshAgentDetail(agentId, displayLevel);
  };

  useEffect(() => {
    if (!agentId) return;
    registerAgentForEvents(agentId);
    void refreshAgentDetail(agentId, displayLevel);
    return () => {
      unregisterAgentForEvents(agentId);
    };
  }, [agentId, displayLevel, refreshAgentDetail, registerAgentForEvents, unregisterAgentForEvents]);

  return { detail, loading, refresh };
}
