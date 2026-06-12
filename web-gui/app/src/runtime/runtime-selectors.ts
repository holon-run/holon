import type { AgentSummary } from "./types";
import type { RuntimeStoreState } from "./runtime-store";

export function selectSelectedAgent(state: RuntimeStoreState): AgentSummary | undefined {
  if (state.selectedAgentId) {
    return state.bootstrap.agents.find((agent) => agent.id === state.selectedAgentId);
  }
  return state.bootstrap.agents[0];
}

export function selectSelectedAgentDetail(state: RuntimeStoreState) {
  const selectedAgent = selectSelectedAgent(state);
  if (!selectedAgent) return null;
  return state.sessionsByAgentId[selectedAgent.id]?.detail ?? null;
}

export function selectSelectedAgentDetailLoading(state: RuntimeStoreState): boolean {
  const selectedAgent = selectSelectedAgent(state);
  if (!selectedAgent) return false;
  return state.sessionsByAgentId[selectedAgent.id]?.loading ?? false;
}
