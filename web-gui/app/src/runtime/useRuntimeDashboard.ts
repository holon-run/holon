import { useEffect } from "react";

import { backfillRetryDelayMs } from "./global-sync-coordinator";
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

  // A page that loads while the runtime is unreachable gets a disconnected
  // bootstrap; without an active retry the dashboard stays in the error state
  // until the safety interval fires (~5 minutes), long after the runtime
  // returns. Retry with bounded exponential backoff while the connection
  // error persists and reset once it clears.
  // The loop self-schedules after every refresh settles: a refresh can
  // complete without replacing the bootstrap object (429 projection_busy is
  // skipped in the store), which would otherwise leave the chain without a
  // next timer. Recovery replaces the bootstrap, re-runs this effect, and
  // cancels the in-flight loop.
  const connection = bootstrap.connection;
  useEffect(() => {
    if (connection.error == null || connection.baseUrl == null) {
      return;
    }
    let cancelled = false;
    let attempt = 0;
    let timer: number | undefined;

    const scheduleRetry = () => {
      if (cancelled) {
        return;
      }
      attempt += 1;
      timer = window.setTimeout(runRetry, backfillRetryDelayMs(attempt));
    };

    const runRetry = () => {
      if (cancelled) {
        return;
      }
      void refresh({ background: true, trigger: "connect.retry" })
        .catch(() => undefined)
        .finally(() => {
          scheduleRetry();
        });
    };

    scheduleRetry();
    return () => {
      cancelled = true;
      if (timer !== undefined) {
        window.clearTimeout(timer);
      }
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
