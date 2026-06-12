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
  const refresh = () => refreshAgentDetail(agentId, displayLevel);

  useEffect(() => {
    void refresh();
  }, [agentId, displayLevel, refreshAgentDetail]);

  return { detail, loading, refresh };
}
