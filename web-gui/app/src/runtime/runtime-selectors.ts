import type { AgentSummary } from "./types";
import type { RuntimeStoreState } from "./runtime-store";

/**
 * Cache for the enriched agent. refreshBootstrap replaces bootstrap agents
 * every ~1s with fresh /agents/list data (which carries no work items).
 * Without enrichment, the panel flickers to "no work items" whenever the
 * cached merge momentarily loses them. The cache avoids creating a new object
 * on every store update when the underlying references haven't changed.
 */
let enrichedAgent: AgentSummary | undefined;
let enrichedBootstrapRef: AgentSummary | undefined;
let enrichedSessionRef: AgentSummary | undefined;

export function selectSelectedAgent(state: RuntimeStoreState): AgentSummary | undefined {
  const bootstrapAgent = state.selectedAgentId
    ? state.bootstrap.agents.find((agent) => agent.id === state.selectedAgentId)
    : state.bootstrap.agents[0];
  if (!bootstrapAgent) {
    enrichedAgent = undefined;
    return undefined;
  }
  // Bootstrap carries work items only when merged from cached state.
  // When missing, enrich from session detail (which always has accurate data).
  const sessionAgent = state.sessionsByAgentId[bootstrapAgent.id]?.detail?.agent;
  if (!sessionAgent || bootstrapAgent.workItems?.length) {
    enrichedAgent = undefined;
    return bootstrapAgent;
  }
  // Return cached enriched agent when underlying references are unchanged.
  if (enrichedBootstrapRef === bootstrapAgent && enrichedSessionRef === sessionAgent && enrichedAgent) {
    return enrichedAgent;
  }
  enrichedBootstrapRef = bootstrapAgent;
  enrichedSessionRef = sessionAgent;
  enrichedAgent = {
    ...bootstrapAgent,
    workItems: sessionAgent.workItems?.length ? sessionAgent.workItems : bootstrapAgent.workItems,
    currentWork: bootstrapAgent.currentWork ?? sessionAgent.currentWork,
  };
  return enrichedAgent;
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
