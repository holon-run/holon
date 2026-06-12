import { useCallback, useEffect, useMemo, useState } from "react";

import { createRuntimeClient } from "./client";
import type { AgentDetail, DisplayLevel } from "./types";

interface AgentDetailState {
  detail: AgentDetail | null;
  loading: boolean;
  refresh: () => Promise<void>;
}

export function useAgentDetail(agentId: string | undefined, displayLevel: DisplayLevel): AgentDetailState {
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
      setDetail(await client.getAgentDetail(agentId, displayLevel));
    } finally {
      setLoading(false);
    }
  }, [agentId, client, displayLevel]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return { detail, loading, refresh };
}
