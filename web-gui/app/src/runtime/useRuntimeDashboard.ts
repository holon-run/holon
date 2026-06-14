import { useEffect } from "react";

import { type BootstrapRefreshOptions, useRuntimeStore } from "./runtime-store";
import type { RuntimeBootstrap } from "./types";

const DASHBOARD_AUTO_REFRESH_MS = 30_000;

interface RuntimeDashboardState {
  bootstrap: RuntimeBootstrap;
  loading: boolean;
  refresh: (options?: BootstrapRefreshOptions) => Promise<void>;
}

export function useRuntimeDashboard(): RuntimeDashboardState {
  const bootstrap = useRuntimeStore((state) => state.bootstrap);
  const loading = useRuntimeStore((state) => state.bootstrapLoading);
  const refresh = useRuntimeStore((state) => state.refreshBootstrap);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const refreshIfVisible = () => {
      if (document.visibilityState === "visible") {
        void refresh({ background: true });
      }
    };

    const interval = window.setInterval(refreshIfVisible, DASHBOARD_AUTO_REFRESH_MS);
    document.addEventListener("visibilitychange", refreshIfVisible);
    return () => {
      window.clearInterval(interval);
      document.removeEventListener("visibilitychange", refreshIfVisible);
    };
  }, [refresh]);

  return {
    bootstrap,
    loading,
    refresh,
  };
}
