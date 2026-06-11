import { useCallback, useEffect, useMemo, useState } from "react";

import { createRuntimeClient } from "./client";
import type { AgentDetail } from "./types";

interface AgentDetailState {
  detail: AgentDetail | null;
  loading: boolean;
  refresh: () => Promise<void>;
}

export function useAgentDetail(agentId: string | undefined): AgentDetailState {
  const client = useMemo(() => createRuntimeClient(), []);
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    if (!agentId) {
      setDetail(null);
      return;
    }
    setLoading(true);
    try {
      setDetail(await client.getAgentDetail(agentId));
    } finally {
      setLoading(false);
    }
  }, [agentId, client]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return { detail, loading, refresh };
}
