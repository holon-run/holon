import { memo, useCallback, useMemo, useState, useSyncExternalStore } from "react";
import { renderAgentAvatar, type AgentAvatarExpression } from "../../lib/agent-avatar";
import { agentAvatarResources, avatarResourceRequest } from "../../runtime/agent-avatar-resources";

/** Decorative identity cue; the adjacent Agent name remains the accessible label. */
export const AgentAvatar = memo(function AgentAvatar({
  agentId,
  expression = "neutral",
}: { agentId: string; expression?: AgentAvatarExpression }) {
  const fallback = useMemo(
    () => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(renderAgentAvatar(agentId, expression))}`,
    [agentId, expression],
  );
  const epoch = useSyncExternalStore(
    agentAvatarResources.subscribe, agentAvatarResources.getEpoch, agentAvatarResources.getEpoch,
  );
  const request = useMemo(() => avatarResourceRequest(agentId, expression), [agentId, expression]);
  const subscribe = useCallback(
    (listener: () => void) => agentAvatarResources.retain(request, listener, epoch),
    [request, epoch],
  );
  const snapshot = useCallback(() => agentAvatarResources.peek(request, epoch), [request, epoch]);
  const remote = useSyncExternalStore(subscribe, snapshot, () => undefined);
  const [failedUrl, setFailedUrl] = useState<string>();
  const src = remote && remote !== failedUrl ? remote : fallback;
  return (
    <img
      className="agent-badge agent-avatar"
      data-expression={expression}
      src={src}
      onError={() => { if (remote) setFailedUrl(remote); }}
      width={36}
      height={36}
      alt=""
      aria-hidden="true"
      draggable={false}
    />
  );
});
