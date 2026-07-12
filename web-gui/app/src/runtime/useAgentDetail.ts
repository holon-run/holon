import { useEffect, useRef } from "react";

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
  const refresh = async () => {
    await refreshAgentDetail(agentId, displayLevel);
  };

  useEffect(() => {
    if (!agentId) return;
    registerAgentForEvents(agentId);
  }, [agentId, registerAgentForEvents]);

  const prevDisplayLevelRef = useRef<DisplayLevel | undefined>(undefined);
  useEffect(() => {
    if (!agentId) return;
    const prevLevel = prevDisplayLevelRef.current;
    const levelRank: Record<DisplayLevel, number> = { info: 0, verbose: 1, debug: 2 };
    const levelIncreased = prevLevel != null && levelRank[displayLevel] > levelRank[prevLevel];
    if (!detail || detail.error) {
      void refreshAgentDetail(agentId, displayLevel);
    } else if (levelIncreased) {
      void refreshAgentDetail(agentId, displayLevel);
    }
    prevDisplayLevelRef.current = displayLevel;
  }, [agentId, displayLevel, refreshAgentDetail, detail]);

  return { detail, loading, refresh };
}
