import { useEffect } from "react";

import { useRuntimeStore } from "./runtime-store";
import type { RuntimeBootstrap } from "./types";

interface RuntimeDashboardState {
  bootstrap: RuntimeBootstrap;
  loading: boolean;
  refresh: () => Promise<void>;
}

export function useRuntimeDashboard(): RuntimeDashboardState {
  const bootstrap = useRuntimeStore((state) => state.bootstrap);
  const loading = useRuntimeStore((state) => state.bootstrapLoading);
  const refresh = useRuntimeStore((state) => state.refreshBootstrap);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return {
    bootstrap,
    loading,
    refresh,
  };
}
