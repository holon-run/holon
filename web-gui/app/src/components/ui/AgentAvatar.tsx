import { memo, useMemo } from "react";
import { renderAgentAvatar } from "../../lib/agent-avatar";

/** Decorative identity cue; the adjacent Agent name remains the accessible label. */
export const AgentAvatar = memo(function AgentAvatar({ agentId }: { agentId: string }) {
  const src = useMemo(
    () => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(renderAgentAvatar(agentId))}`,
    [agentId],
  );
  return (
    <img
      className="agent-badge agent-avatar"
      src={src}
      width={36}
      height={36}
      alt=""
      aria-hidden="true"
      draggable={false}
    />
  );
});
