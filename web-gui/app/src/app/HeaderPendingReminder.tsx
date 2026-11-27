import { ListTodo, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { RuntimeNotification } from "../runtime/notification-sync";
import { runtimeNotificationSync } from "../runtime/runtime-notifications";

export interface HeaderPendingReminderProps {
  reminders: RuntimeNotification[];
  onOpenWorkItem: (workItemId: string) => void;
}

/**
 * Header affordance for the active agent's pending reminders.
 *
 * `Ignore` is always available so a reminder without a work item can still be
 * dismissed. The "Open work item" action only renders when the first reminder
 * targets a work item.
 */
export function HeaderPendingReminder({ reminders, onOpenWorkItem }: HeaderPendingReminderProps) {
  const { t } = useTranslation();
  if (reminders.length === 0) return null;
  const primary = reminders[0];
  const workItemId = primary.target.work_item_id;
  return (
    <span className="header-pending-reminder">
      {workItemId ? (
        <button
          type="button"
          className="header-pending-open"
          title={primary.title ?? primary.summary ?? undefined}
          onClick={() => onOpenWorkItem(workItemId)}
        >
          <ListTodo size={12} aria-hidden="true" />
          <span>{t("dashboard.triageOpenWorkItem")}</span>
        </button>
      ) : null}
      <button
        type="button"
        className="header-pending-ignore"
        aria-label={t("notifications.ignoreItem")}
        title={t("notifications.ignore")}
        onClick={() => {
          for (const item of reminders) {
            runtimeNotificationSync.ignorePending(item.id);
          }
        }}
      >
        <X size={12} aria-hidden="true" />
      </button>
    </span>
  );
}
