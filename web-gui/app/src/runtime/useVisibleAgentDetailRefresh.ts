import { useEffect, useRef } from "react";

import { agentAccessRejection } from "./agent-access-records";
import { conversationScopeSnapshot, subscribeConversationScope } from "./conversation-scope-store";
import { useRuntimeStore } from "./runtime-store";
import { createSelectedAgentRefreshScheduler } from "./selected-agent-refresh";
import { registerSelectedAgentRefresh } from "./selected-agent-resource-invalidation";

const TERMINAL = new Set(["completed", "failed", "cancelled", "interrupted", "success", "error"]);

/** Only the currently displayed object owns a detail refresh budget. */
export function useVisibleAgentDetailRefresh(
  agentId: string | undefined,
  incarnation: number | undefined,
  scopeKey: string | null,
  enabled: boolean,
  contextKey: string,
  onAccessError: (error: unknown) => void,
): void {
  const errorHandler = useRef(onAccessError);
  errorHandler.current = onAccessError;
  const target = useRuntimeStore((state) => {
    const view = state.rightPanelView;
    if (!state.rightPanelOpen || !view || view.agentId !== agentId) return null;
    if (view.kind === "work_item_detail") return JSON.stringify(["workItem", view.workItem.id]);
    if (view.kind === "task_detail") return JSON.stringify(["task", view.task.id]);
    if (view.kind === "tool_execution_detail") return JSON.stringify(["toolExecution", view.toolExecutionId]);
    return null;
  });

  useEffect(() => {
    if (!enabled || !agentId || incarnation === undefined || !scopeKey || !target) return;
    const [kind, id] = JSON.parse(target) as [string, string];
    const abort = new AbortController();
    const options = { incarnation, signal: abort.signal, force: true, throwOnError: true };
    const refresh = () => {
      const store = useRuntimeStore.getState();
      if (kind === "workItem") return store.loadAgentWorkItemDetail(agentId, id, options);
      if (kind === "task") return store.loadAgentTaskDetail(agentId, id, true, options);
      const selection = store.rightPanelView;
      return store.loadAgentToolExecutionDetail(agentId, id,
        selection?.kind === "tool_execution_detail" ? selection.fallbackActivity : undefined, undefined, options);
    };
    const scheduler = createSelectedAgentRefreshScheduler({
      resources: {
        [target]: {
          refresh,
          intervalMs: () => {
            if (kind === "workItem") return 30_000;
            const session = useRuntimeStore.getState().sessionsByAgentId[agentId];
            if (kind === "task") {
              const detail = session?.taskDetailsById[id];
              // A terminal status alone cannot prove that final output was read.
              return !detail?.stale && TERMINAL.has(detail?.status?.status ?? "")
                && TERMINAL.has(detail?.output?.task?.status ?? detail?.output?.status ?? "") ? Infinity : 5_000;
            }
            const detail = session?.toolExecutionDetailsById[id];
            return !detail?.stale && TERMINAL.has(detail?.toolExecution?.status ?? "") ? Infinity : 5_000;
          },
        },
      },
      onError: (_resource, error) => {
        // Object 404/410 is local to this inspector, not proof the Agent is gone.
        const status = (error as { status?: number })?.status;
        if (status !== 404 && status !== 410 && agentAccessRejection(error)) errorHandler.current(error);
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
    const listVersion = () => {
      const item = useRuntimeStore.getState().bootstrap.agents.find((agent) => agent.id === agentId)
        ?.workItems?.find((item) => item.id === id);
      return JSON.stringify([item?.revision, item?.updatedAt, item?.planArtifact]);
    };
    let previousListVersion = listVersion();
    const unsubscribeList = useRuntimeStore.subscribe(() => {
      if (kind !== "workItem") return;
      const next = listVersion();
      if (next !== previousListVersion) {
        previousListVersion = next;
        scheduler.hint();
      }
    });
    const unregister = registerSelectedAgentRefresh(agentId, () => scheduler.refreshNow());
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
      unsubscribeList();
      unregister();
      document.removeEventListener("visibilitychange", restore);
      window.removeEventListener("online", restore);
      window.removeEventListener("offline", restore);
      window.removeEventListener("pageshow", restore);
      window.removeEventListener("focus", restore);
    };
  }, [agentId, incarnation, scopeKey, enabled, contextKey, target]);
}
