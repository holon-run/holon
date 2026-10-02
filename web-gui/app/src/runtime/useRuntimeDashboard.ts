import { useEffect, useRef } from "react";

import { backfillRetryDelayMs } from "./global-sync-coordinator";
import { type BootstrapRefreshOptions, useRuntimeStore } from "./runtime-store";
import type { RuntimeBootstrap } from "./types";

const DASHBOARD_SAFETY_REFRESH_MS = 5 * 60_000;

interface BootstrapRetryChain {
  cancelled: boolean;
  timer: number | undefined;
  attempt: number;
}

function stopRetryChain(chain: BootstrapRetryChain | null): void {
  if (chain == null) {
    return;
  }
  chain.cancelled = true;
  if (chain.timer !== undefined) {
    window.clearTimeout(chain.timer);
  }
}

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
  // error persists and reset once it clears. The attempt count lives on the
  // chain, not the effect: during a sustained outage every refreshBootstrap
  // response replaces the bootstrap object (another disconnected
  // connection), re-running this effect, which would otherwise reset the
  // backoff to its floor and retry a long outage at 1s intervals. The chain
  // survives those re-runs, self-schedules after every refresh settles (the
  // 429 projection_busy path replaces nothing), and stops itself once the
  // connection recovers; unmount cancels it.
  const retryChainRef = useRef<BootstrapRetryChain | null>(null);

  const connection = bootstrap.connection;
  useEffect(() => {
    if (connection.error == null || connection.baseUrl == null) {
      // Recovered or not retryable: stop the chain and reset the ladder.
      stopRetryChain(retryChainRef.current);
      retryChainRef.current = null;
      return;
    }
    if (retryChainRef.current) {
      // A refreshed disconnected bootstrap replaced the previous one; the
      // running chain keeps its attempt count so the backoff keeps growing.
      return;
    }

    const chain: BootstrapRetryChain = {
      cancelled: false,
      timer: undefined,
      attempt: 0,
    };
    retryChainRef.current = chain;

    const scheduleRetry = () => {
      if (chain.cancelled) {
        return;
      }
      const live = useRuntimeStore.getState().bootstrap.connection;
      if (live.error == null || live.baseUrl == null) {
        // The connection recovered under this chain: stop instead of
        // scheduling another attempt.
        chain.cancelled = true;
        retryChainRef.current = null;
        return;
      }
      chain.attempt += 1;
      chain.timer = window.setTimeout(runRetry, backfillRetryDelayMs(chain.attempt));
    };

    const runRetry = () => {
      if (chain.cancelled) {
        return;
      }
      void useRuntimeStore
        .getState()
        .refreshBootstrap({ background: true, trigger: "connect.retry" })
        .catch(() => undefined)
        .finally(scheduleRetry);
    };

    scheduleRetry();
  }, [bootstrap, connection]);

  // Effect cleanups re-run on dependency changes, so unmount cleanup lives in
  // its own effect: only a real unmount cancels the retry chain.
  useEffect(() => {
    return () => {
      stopRetryChain(retryChainRef.current);
      retryChainRef.current = null;
    };
  }, []);

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
