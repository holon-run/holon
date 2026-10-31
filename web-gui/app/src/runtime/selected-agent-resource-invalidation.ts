const listeners = new Map<string, () => void>();

export function registerSelectedAgentRefresh(agentId: string, refresh: () => void): () => void {
  listeners.set(agentId, refresh);
  return () => { if (listeners.get(agentId) === refresh) listeners.delete(agentId); };
}

/** Local writes and explicit retries use the selected scope's scheduler. */
export function refreshSelectedAgentResources(agentId: string): boolean {
  const refresh = listeners.get(agentId);
  refresh?.();
  return refresh !== undefined;
}
