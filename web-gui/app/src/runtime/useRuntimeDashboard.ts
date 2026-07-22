import { useEffect } from "react";

import { type BootstrapRefreshOptions, useRuntimeStore } from "./runtime-store";
import type { RuntimeBootstrap } from "./types";

const DASHBOARD_SAFETY_REFRESH_MS = 5 * 60_000;

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
    const refreshIfNeeded = () => {
      if (document.visibilityState === "visible") {
        void refresh({ background: true, trigger: "safety.refresh" });
      }
    };

    const jitter = Math.floor(Math.random() * 30_000);
    const interval = window.setInterval(
      refreshIfNeeded,
      DASHBOARD_SAFETY_REFRESH_MS + jitter,
    );
    return () => {
      window.clearInterval(interval);
    };
  }, [refresh]);

  return {
    bootstrap,
    loading,
    refresh,
  };
}
