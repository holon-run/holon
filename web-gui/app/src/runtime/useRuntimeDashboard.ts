import { useCallback, useEffect, useMemo, useState } from "react";

import { createRuntimeClient } from "./client";
import type { RuntimeBootstrap } from "./types";

interface RuntimeDashboardState {
  bootstrap: RuntimeBootstrap;
  loading: boolean;
  refresh: () => Promise<void>;
}

export function useRuntimeDashboard(): RuntimeDashboardState {
  const client = useMemo(() => createRuntimeClient(), []);
  const [bootstrap, setBootstrap] = useState<RuntimeBootstrap | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setBootstrap(await client.getBootstrap());
    } finally {
      setLoading(false);
    }
  }, [client]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return {
    bootstrap: bootstrap ?? {
      attentionCount: 0,
      connection: {
        mode: "local",
        source: "fixture",
        summary: "loading runtime dashboard",
      },
      metrics: [],
      agents: [],
    },
    loading,
    refresh,
  };
}
