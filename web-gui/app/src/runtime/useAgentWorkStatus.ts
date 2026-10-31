import { useRuntimeStore } from "./runtime-store";
import { refreshSelectedAgentResources } from "./useSelectedAgentRefresh";

/** Work bar and inspector render the same state; neither owns another poller. */
export function useAgentWorkStatus(agentId: string, incarnation?: number) {
  const agent = useRuntimeStore((state) => {
    const current = state.sessionsByAgentId[agentId]?.detail?.agent
      ?? state.bootstrap.agents.find((entry) => entry.id === agentId);
    return current?.incarnation === incarnation ? current : undefined;
  });
  const stale = useRuntimeStore((state) => {
    const resources = state.sessionsByAgentId[agentId]?.resourceRefresh;
    return Boolean(resources?.state?.stale || resources?.workItems?.stale);
  });
  return { agent, stale, refresh: () => refreshSelectedAgentResources(agentId) };
}
