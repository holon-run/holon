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
  const startAgentEventStream = useRuntimeStore((state) => state.startAgentEventStream);
  const stopAgentEventStream = useRuntimeStore((state) => state.stopAgentEventStream);
  const refresh = async () => {
    await refreshAgentDetail(agentId, displayLevel);
  };

  useEffect(() => {
    startAgentEventStream(agentId, displayLevel);
    void refreshAgentDetail(agentId, displayLevel);
    return () => {
      stopAgentEventStream(agentId);
    };
  }, [agentId, displayLevel, refreshAgentDetail, startAgentEventStream, stopAgentEventStream]);

  return { detail, loading, refresh };
}
