import { useCallback } from "react";

import { useRuntimeStore } from "./runtime-store";
import type { AgentSyncStatus } from "./runtime-store-helpers";
import type { AgentDetail, DisplayLevel } from "./types";

interface AgentDetailState {
  detail: AgentDetail | null;
  loading: boolean;
  contentStatus: "unknown" | "available" | "confirmed-empty";
  syncStatus: AgentSyncStatus;
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
  const refreshAgentDetail = useRuntimeStore((state) => state.refreshAgentDetail);
  const refresh = useCallback(async () => {
    if (agentId === undefined) return;
    await refreshAgentDetail(agentId, displayLevel, { trigger: "manual.refresh" });
  }, [agentId, displayLevel, refreshAgentDetail]);

  // Conversation content now flows from the conversation read model
  // (useConversationSession). Mounting a page must not start the legacy
  // event-session hydration or catch-up; refresh stays available for the
  // manual action only.
  return { detail, loading, contentStatus, syncStatus, refresh };
}
