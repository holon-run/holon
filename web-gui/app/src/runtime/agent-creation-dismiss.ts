import type { AgentCreationProgress } from "./agent-creation";
import type { ConversationTurnGroup } from "./conversation-view-model";

/**
 * Runtime-only receipts settle successfully without the agent ever running a
 * turn (control commands, task results that never reached a model turn).
 * They are not evidence that a created agent works, so they must not retire
 * the creation banner.
 */
function isRuntimeOnlyReceipt(turn: ConversationTurnGroup): boolean {
  return turn.result.kind === "none" && turn.result.reason.kind === "reducer_only";
}

/**
 * True when the agent produced at least one settled, successful turn the
 * operator can see in the conversation.
 */
export function hasSettledSuccessfulTurn(turns: readonly ConversationTurnGroup[]): boolean {
  return turns.some((turn) =>
    turn.settled
    && turn.execution.kind === "terminal"
    && turn.execution.outcome === "completed"
    && !isRuntimeOnlyReceipt(turn));
}

/**
 * The creation banner states that the agent was created, not that its first
 * turn succeeded (#3737). It may retire only once a successful turn proves
 * the agent runs; a failed, uncertain, or degraded creation keeps it visible
 * so "created" never reads as "run failed".
 */
export function shouldDismissAgentCreation(
  progress: AgentCreationProgress | undefined,
  turns: readonly ConversationTurnGroup[],
): boolean {
  if (!progress || progress.status !== "ready") return false;
  return hasSettledSuccessfulTurn(turns);
}
