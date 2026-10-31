const listeners = new Map<string, Set<() => void>>();

export function registerSelectedAgentRefresh(agentId: string, refresh: () => void): () => void {
  const callbacks = listeners.get(agentId) ?? new Set();
  callbacks.add(refresh);
  listeners.set(agentId, callbacks);
  return () => {
    callbacks.delete(refresh);
    if (callbacks.size === 0 && listeners.get(agentId) === callbacks) listeners.delete(agentId);
  };
}

/** Local writes and explicit retries use the selected scope's scheduler. */
export function refreshSelectedAgentResources(agentId: string): boolean {
  const callbacks = listeners.get(agentId);
  callbacks?.forEach((refresh) => refresh());
  return Boolean(callbacks?.size);
}
