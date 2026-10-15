import { pathForRoute } from "../app/routes";
import type { TaskTargetRef } from "./types";

export function taskConversationPath(target?: TaskTargetRef | null): string | undefined {
  if (!target?.agent_id || !["child_agent", "agent_message_wait"].includes(target.kind)) return undefined;
  if (target.incarnation == null || !Number.isSafeInteger(target.incarnation) || target.incarnation <= 0) return undefined;
  return pathForRoute("agent", target.agent_id, undefined, { incarnation: target.incarnation });
}
