import { useCallback } from "react";

import { useRuntimeStore } from "./runtime-store";
import { refreshSelectedAgentResources } from "./useSelectedAgentRefresh";
import type { AgentSyncStatus } from "./runtime-store-helpers";
import type { AgentDetail } from "./types";

interface AgentDetailState {
  detail: AgentDetail | null;
  loading: boolean;
  contentStatus: "unknown" | "available" | "confirmed-empty";
  syncStatus: AgentSyncStatus;
  refresh: () => Promise<void>;
}

export function useAgentDetail(agentId: string | undefined): AgentDetailState {
  const detail = useRuntimeStore((state) => (agentId ? state.sessionsByAgentId[agentId]?.detail ?? null : null));
  const loading = useRuntimeStore((state) => {
    const session = agentId ? state.sessionsByAgentId[agentId] : undefined;
    return Boolean(session?.loading || session?.resourceRefresh?.state?.loading || session?.resourceRefresh?.workItems?.loading);
  });
  const contentStatus = useRuntimeStore((state) =>
    agentId ? state.sessionsByAgentId[agentId]?.contentStatus ?? "unknown" : "unknown",
  );
  const syncStatus = useRuntimeStore((state) =>
    agentId ? state.sessionsByAgentId[agentId]?.syncStatus ?? "idle" : "idle",
  );
  const refresh = useCallback(async () => {
    if (agentId === undefined) return;
    refreshSelectedAgentResources(agentId);
  }, [agentId]);

  // Conversation content now flows from the conversation read model
  // (useConversationSession). Mounting a page must not start the legacy
  // event-session hydration or catch-up. Manual resource refresh also uses
  // the shared scheduler, not the full-detail composite HTTP reader.
  return { detail, loading, contentStatus, syncStatus, refresh };
}
