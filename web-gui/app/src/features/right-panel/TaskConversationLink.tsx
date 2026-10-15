import { useTranslation } from "react-i18next";

import { taskConversationPath } from "../../runtime/task-target";
import type { TaskSummary, TaskTargetRef } from "../../runtime/types";

export function TaskConversationLink({ task, target = task.target }: {
  task: TaskSummary;
  target?: TaskTargetRef | null;
}) {
  const { t } = useTranslation();
  if (!target) return null;
  const href = taskConversationPath(target);
  const label = target.kind === "child_agent" ? t("inspector.childAgent") : t("inspector.targetAgent");
  return (
    <p className="inspector-muted">
      {label}: {href ? (
        <a href={href} aria-label={t("inspector.openAgentConversation")}>{target.agent_id}</a>
      ) : (
        <span>{target.agent_id} · {t("inspector.unknownTargetIdentity")}</span>
      )}
    </p>
  );
}
