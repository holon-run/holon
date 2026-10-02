import { useEffect, useRef } from "react";

import { type BootstrapRefreshOptions, useRuntimeStore } from "./runtime-store";
import type { RuntimeBootstrap } from "./types";

const DASHBOARD_SAFETY_REFRESH_MS = 5 * 60_000;
const CONNECT_RETRY_BASE_MS = 1_000;
const CONNECT_RETRY_MAX_MS = 15_000;

interface RuntimeDashboardState {
  bootstrap: RuntimeBootstrap;
  loading: boolean;
  refresh: (options?: BootstrapRefreshOptions) => Promise<void>;
}

export function bootstrapConnectRetryDelayMs(attempt: number): number {
  return Math.min(
    CONNECT_RETRY_MAX_MS,
    CONNECT_RETRY_BASE_MS * 2 ** Math.max(0, attempt - 1),
  );
}

export function useRuntimeDashboard(): RuntimeDashboardState {
  const bootstrap = useRuntimeStore((state) => state.bootstrap);
  const loading = useRuntimeStore((state) => state.bootstrapLoading);
  const refresh = useRuntimeStore((state) => state.refreshBootstrap);
  const connectRetryAttemptRef = useRef(0);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // A page that loads while the runtime is unreachable gets a disconnected
  // bootstrap; without an active retry the dashboard stays in the error state
  // until the safety interval fires (~5 minutes), long after the runtime
  // returns. Retry with bounded exponential backoff while the connection
  // error persists and reset once it clears.
  const connection = bootstrap.connection;
  useEffect(() => {
    if (connection.error == null || connection.baseUrl == null) {
      connectRetryAttemptRef.current = 0;
      return;
    }
    connectRetryAttemptRef.current += 1;
    const delayMs = bootstrapConnectRetryDelayMs(connectRetryAttemptRef.current);
    const timer = window.setTimeout(() => {
      void refresh({ background: true, trigger: "connect.retry" });
    }, delayMs);
    return () => {
      window.clearTimeout(timer);
    };
  }, [bootstrap, connection, refresh]);

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
