import { memo, useMemo } from "react";
import { renderAgentAvatar, type AgentAvatarExpression } from "../../lib/agent-avatar";

/** Decorative identity cue; the adjacent Agent name remains the accessible label. */
export const AgentAvatar = memo(function AgentAvatar({
  agentId,
  expression = "neutral",
}: { agentId: string; expression?: AgentAvatarExpression }) {
  const src = useMemo(
    () => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(renderAgentAvatar(agentId, expression))}`,
    [agentId, expression],
  );
  return (
    <img
      className="agent-badge agent-avatar"
      data-expression={expression}
      src={src}
      width={36}
      height={36}
      alt=""
      aria-hidden="true"
      draggable={false}
    />
  );
});
