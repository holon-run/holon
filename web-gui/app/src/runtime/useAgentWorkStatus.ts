import { useEffect, useRef, useState } from "react";
import { createRuntimeClient } from "./client";
import { getRuntimeConnectionConfig } from "./runtime-store";
import type { AgentSummary } from "./types";

/** Visible-conversation state only: no transcript hydration or background-tab polling. */
export function useAgentWorkStatus(agentId: string, changeKey: string) {
  const [snapshot, setSnapshot] = useState<{ agent?: AgentSummary; stale: boolean }>({ stale: false });
  const refreshRef = useRef<() => void>(() => {});
  const config = getRuntimeConnectionConfig();
  useEffect(() => {
    const client = createRuntimeClient(config);
    let disposed = false;
    let inFlight = false;
    let requested = false;
    const refresh = async () => {
      if (disposed || document.visibilityState === "hidden") return;
      if (inFlight) { requested = true; return; }
      inFlight = true;
      try {
        const agent = await client.getAgentState(agentId);
        if (!disposed) setSnapshot({ agent, stale: false });
      } catch {
        if (!disposed) setSnapshot((previous) => ({ ...previous, stale: true }));
      } finally {
        inFlight = false;
        if (requested && !disposed) { requested = false; void refresh(); }
      }
    };
    refreshRef.current = () => { void refresh(); };
    void refresh();
    const timer = window.setInterval(() => { void refresh(); }, 5000);
    document.addEventListener("visibilitychange", refreshRef.current);
    const listener = refreshRef.current;
    return () => {
      disposed = true;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", listener);
    };
  }, [agentId, config.mode, config.baseUrl, config.token]);
  const lastChange = useRef(changeKey);
  useEffect(() => {
    if (lastChange.current !== changeKey) refreshRef.current();
    lastChange.current = changeKey;
  }, [changeKey]);
  return { ...snapshot, refresh: () => refreshRef.current() };
}
