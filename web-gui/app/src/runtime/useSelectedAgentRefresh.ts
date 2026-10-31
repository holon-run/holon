import { useEffect, useRef } from "react";

import { agentAccessRejection } from "./agent-access-records";
import { conversationScopeSnapshot, subscribeConversationScope, useConversationScopeStore } from "./conversation-scope-store";
import { createSelectedAgentRefreshScheduler } from "./selected-agent-refresh";
import { getRuntimeConnectionConfig, useRuntimeStore } from "./runtime-store";
import { registerSelectedAgentRefresh } from "./selected-agent-resource-invalidation";
import type { AgentSummary } from "./types";
export { refreshSelectedAgentResources } from "./selected-agent-resource-invalidation";

/** Only the admitted, selected route owns automatic HTTP resource reads. */
export function useSelectedAgentRefresh(
  agent: AgentSummary | undefined,
  scopeKey: string | null,
  enabled: boolean,
  onAccessError: (error: unknown) => void,
): void {
  const connection = useRuntimeStore((state) => state.bootstrap.connection);
  const user = useRuntimeStore((state) => state.currentUser);
  const userLoaded = useRuntimeStore((state) => state.currentUserLoaded);
  const identity = useRuntimeStore((state) => state.discovery.identity);
  const unauthorized = useRuntimeStore((state) => state.discovery.freshness === "unauthorized");
  const conversationIdentity = useConversationScopeStore((state) =>
    JSON.stringify(scopeKey ? state.scopes[scopeKey]?.view?.scope : null),
  );
  const config = getRuntimeConnectionConfig();
  const contextKey = JSON.stringify([
    connection.mode, connection.baseUrl, config.token, connection.authRequired,
    user, userLoaded, identity, conversationIdentity, unauthorized,
  ]);
  const errorHandler = useRef(onAccessError);
  errorHandler.current = onAccessError;
  const agentId = agent?.id;
  const incarnation = agent?.incarnation;

  useEffect(() => {
    if (!enabled || !agentId || incarnation === undefined || !scopeKey
      || !userLoaded || unauthorized || connection.authRequired) return;
    const abort = new AbortController();
    const options = { incarnation, signal: abort.signal, throwOnError: true };
    const scheduler = createSelectedAgentRefreshScheduler({
      resources: {
        state: {
          refresh: () => useRuntimeStore.getState().refreshAgentState(agentId, options),
          intervalMs: () => {
            const state = useRuntimeStore.getState();
            const current = state.sessionsByAgentId[agentId]?.detail?.agent
              ?? state.bootstrap.agents.find((entry) => entry.id === agentId);
            return current?.currentRunId || (current?.activeTaskCount ?? 0) > 0 ? 5000 : 30000;
          },
        },
        workItems: {
          refresh: () => useRuntimeStore.getState().refreshAgentWorkItems(agentId, options),
          intervalMs: () => 30000,
        },
      },
      onError: (_resource, error) => {
        if (agentAccessRejection(error) || (error as { status?: number })?.status === 410) {
          scheduler.setActive(false);
          errorHandler.current(error);
        }
      },
    });
    const active = () => document.visibilityState !== "hidden" && navigator.onLine !== false;
    const restore = () => {
      scheduler.setActive(active());
      if (active()) scheduler.resume();
    };
    let previousHead: string | undefined;
    let wasReady = false;
    const observe = () => {
      const snapshot = conversationScopeSnapshot(scopeKey);
      const ready = snapshot.status.kind === "ready" && snapshot.view?.reset_reason === null;
      if (ready) {
        const head = JSON.stringify([snapshot.view?.scope, snapshot.view?.through_seq]);
        if (!wasReady) scheduler.resume();
        else if (head !== previousHead) scheduler.hint();
        previousHead = head;
      }
      wasReady = ready;
    };
    const refresh = () => scheduler.refreshNow();
    const unregisterRefresh = registerSelectedAgentRefresh(agentId, refresh);
    const unsubscribe = subscribeConversationScope(scopeKey, observe);
    restore();
    observe();
    document.addEventListener("visibilitychange", restore);
    window.addEventListener("online", restore);
    window.addEventListener("offline", restore);
    window.addEventListener("pageshow", restore);
    window.addEventListener("focus", restore);
    return () => {
      abort.abort();
      scheduler.dispose();
      unsubscribe();
      unregisterRefresh();
      document.removeEventListener("visibilitychange", restore);
      window.removeEventListener("online", restore);
      window.removeEventListener("offline", restore);
      window.removeEventListener("pageshow", restore);
      window.removeEventListener("focus", restore);
    };
  }, [agentId, incarnation, scopeKey, enabled, contextKey]);
}
