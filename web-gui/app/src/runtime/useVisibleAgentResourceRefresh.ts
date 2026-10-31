import { useEffect, useRef, useState } from "react";

import { agentAccessRejection } from "./agent-access-records";
import { conversationScopeSnapshot, subscribeConversationScope } from "./conversation-scope-store";
import { useResumeAdmission, useSelectedAgentRefreshContext } from "./selected-agent-refresh-context";
import { createSelectedAgentRefreshScheduler } from "./selected-agent-refresh";
import { registerSelectedAgentRefresh } from "./selected-agent-resource-invalidation";

export interface AgentResourceReadOptions {
  incarnation?: number;
  signal?: AbortSignal;
}

interface ResourceState<T> {
  key: string;
  data?: T;
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  lastSuccessAt?: number;
  error?: string;
}

/** A visible inspector section has one cancellable, identity-scoped budget. */
export function useVisibleAgentResourceRefresh<T>(
  agentId: string,
  resource: string,
  visible: boolean,
  read: (agentId: string, options: AgentResourceReadOptions) => Promise<T>,
) {
  const context = useSelectedAgentRefreshContext((state) =>
    state.current?.agentId === agentId ? state.current : null);
  const key = context?.key;
  const [record, setRecord] = useState<ResourceState<T>>();
  const currentKey = useRef(key);
  currentKey.current = key;
  const reader = useRef(read);
  reader.current = read;
  const retry = useRef<() => void>(() => {});

  useEffect(() => {
    if (!context || !visible) return;
    const abort = new AbortController();
    const current = () => !abort.signal.aborted && currentKey.current === context.key;
    const scheduler = createSelectedAgentRefreshScheduler({
      resources: {
        [resource]: {
          intervalMs: () => 30_000,
          refresh: async () => {
            if (!current()) return;
            setRecord((old) => ({
              ...(old?.key === context.key ? old : { key: context.key }),
              loading: old?.key !== context.key || old.lastSuccessAt === undefined,
              refreshing: old?.key === context.key && old.lastSuccessAt !== undefined,
              stale: old?.key === context.key && old.lastSuccessAt !== undefined,
              error: undefined,
            }));
            try {
              const data = await reader.current(agentId, { incarnation: context.incarnation, signal: abort.signal });
              if (!current()) return;
              setRecord({ key: context.key, data, loading: false, refreshing: false, stale: false, lastSuccessAt: Date.now() });
            } catch (error) {
              if (!current()) return;
              setRecord((old) => ({
                ...(old?.key === context.key ? old : { key: context.key }),
                loading: false, refreshing: false, stale: true,
                error: error instanceof Error ? error.message : String(error),
              }));
              throw error;
            }
          },
        },
      },
      onError: (_resource, error) => {
        const status = (error as { status?: number })?.status;
        if (status !== 404 && status !== 410 && agentAccessRejection(error)) context.onAccessError(error);
      },
    });
    const active = () => document.visibilityState !== "hidden" && navigator.onLine !== false;
    let resumeHeld = false;
    const resumePending = () => useResumeAdmission.getState().status.pending;
    const resumeNow = () => {
      if (resumePending()) {
        resumeHeld = true;
        return;
      }
      scheduler.resume();
    };
    const hintNow = () => {
      if (resumePending()) {
        resumeHeld = true;
        return;
      }
      scheduler.hint();
    };
    const restore = () => {
      if (!active()) {
        scheduler.setActive(false);
        return;
      }
      if (resumePending()) {
        resumeHeld = true;
        return;
      }
      scheduler.setActive(true);
      scheduler.resume();
    };
    let previousHead: string | undefined;
    let wasReady = false;
    const observe = () => {
      const snapshot = conversationScopeSnapshot(context.scopeKey);
      const ready = snapshot.status.kind === "ready" && snapshot.view?.reset_reason === null;
      if (ready) {
        const head = JSON.stringify([snapshot.view?.scope, snapshot.view?.through_seq]);
        if (!wasReady) resumeNow();
        else if (head !== previousHead) hintNow();
        previousHead = head;
      }
      wasReady = ready;
    };
    retry.current = () => scheduler.refreshNow();
    const unregister = registerSelectedAgentRefresh(agentId, retry.current);
    const unsubscribe = subscribeConversationScope(context.scopeKey, observe);
    // A pending global resume reconciliation will replace the client scope;
    // release the held automatic resume once it settles.
    const unsubscribeResume = useResumeAdmission.subscribe(() => {
      if (!resumeHeld || resumePending()) return;
      resumeHeld = false;
      if (active()) {
        scheduler.setActive(true);
        scheduler.resume();
      }
    });
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
      retry.current = () => {};
      unregister();
      unsubscribe();
      unsubscribeResume();
      document.removeEventListener("visibilitychange", restore);
      window.removeEventListener("online", restore);
      window.removeEventListener("offline", restore);
      window.removeEventListener("pageshow", restore);
      window.removeEventListener("focus", restore);
    };
  }, [agentId, context, resource, visible]);

  return {
    state: record?.key === key ? record : undefined,
    refresh: () => retry.current(),
    updateData: (update: (data: T) => T) => {
      if (currentKey.current !== key) return;
      setRecord((old) => old && old.key === key && old.data !== undefined ? { ...old, data: update(old.data) } : old);
    },
  };
}
