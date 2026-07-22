import { useEffect, useRef } from "react";

import { useRuntimeStore } from "./runtime-store";
import type { AgentDetail, DisplayLevel } from "./types";

interface AgentDetailState {
  detail: AgentDetail | null;
  loading: boolean;
  contentStatus: "unknown" | "available" | "confirmed-empty";
  syncStatus: "idle" | "refreshing" | "streaming" | "recovering" | "stale" | "error";
  refresh: () => Promise<void>;
}

export function useAgentDetail(agentId: string | undefined, displayLevel: DisplayLevel): AgentDetailState {
  const detail = useRuntimeStore((state) => (agentId ? state.sessionsByAgentId[agentId]?.detail ?? null : null));
  const loading = useRuntimeStore((state) => (agentId ? state.sessionsByAgentId[agentId]?.loading ?? false : false));
  const contentStatus = useRuntimeStore((state) =>
    agentId ? state.sessionsByAgentId[agentId]?.contentStatus ?? "unknown" : "unknown",
  );
  const syncStatus = useRuntimeStore((state) =>
    agentId ? state.sessionsByAgentId[agentId]?.syncStatus ?? "idle" : "idle",
  );
  const ensureAgentSession = useRuntimeStore((state) => state.ensureAgentSession);
  const refreshAgentDetail = useRuntimeStore((state) => state.refreshAgentDetail);
  const registerAgentForEvents = useRuntimeStore((state) => state.registerAgentForEvents);
  const refresh = async () => {
    await refreshAgentDetail(agentId, displayLevel, { force: true, trigger: "manual.refresh" });
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
      void ensureAgentSession(agentId, displayLevel);
    } else if (levelIncreased) {
      void refreshAgentDetail(agentId, displayLevel, { trigger: "display_level.increased" });
    }
    prevDisplayLevelRef.current = displayLevel;
  }, [agentId, displayLevel, ensureAgentSession, refreshAgentDetail, detail]);

  return { detail, loading, contentStatus, syncStatus, refresh };
}
