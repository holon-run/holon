import { useTranslation } from "react-i18next";

import { isPlainLeftClick } from "../../app/routes";
import { taskConversationPath } from "../../runtime/task-target";
import type { TaskSummary, TaskTargetRef } from "../../runtime/types";

export function TaskConversationLink({ task, target = task.target, fromAgentId, onOpenConversation }: {
  task: TaskSummary;
  target?: TaskTargetRef | null;
  fromAgentId?: string;
  onOpenConversation?: (target: TaskTargetRef) => void;
}) {
  const { t } = useTranslation();
  if (!target) return null;
  const href = taskConversationPath(target, fromAgentId);
  const label = target.kind === "child_agent" ? t("inspector.childAgent") : t("inspector.targetAgent");
  if (!href) {
    return (
      <p className="inspector-muted task-conversation">
        {label}: <code>{target.agent_id}</code> · {t("inspector.unknownTargetIdentity")}
      </p>
    );
  }
  return (
    <p className="inspector-muted task-conversation">
      {label}: <code>{target.agent_id}</code>
      {" "}
      <a
        className="task-conversation-open"
        href={href}
        aria-label={t("inspector.openAgentConversation")}
        onClick={(event) => {
          if (!onOpenConversation || !isPlainLeftClick(event)) return;
          event.preventDefault();
          onOpenConversation(target);
        }}
      >
        {t("inspector.openConversation")}
      </a>
    </p>
  );
}
