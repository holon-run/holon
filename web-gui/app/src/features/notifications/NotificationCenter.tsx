import { Bell, X } from "lucide-react";
import { useId, useRef } from "react";
import { useTranslation } from "react-i18next";

import type { NotificationTarget, RuntimeNotification } from "../../runtime/notification-sync";
import { runtimeNotificationSync, useRuntimeNotifications } from "../../runtime/runtime-notifications";

interface Props {
  onNavigate: (target: NotificationTarget) => void;
}

export function NotificationCenter({ onNavigate }: Props) {
  const { t } = useTranslation();
  const { pending, recent, status, toasts } = useRuntimeNotifications();
  const disclosure = useRef<HTMLDetailsElement>(null);
  const trigger = useRef<HTMLElement>(null);
  const title = useId();
  if (status === "loading" || status === "unavailable") return null;

  function close() {
    if (disclosure.current?.open) {
      disclosure.current.open = false;
      trigger.current?.focus();
    }
  }

  return (
    <div className="notification-center">
      <details ref={disclosure} onKeyDown={(event) => {
        if (event.key !== "Escape" || !disclosure.current?.open) return;
        event.stopPropagation();
        close();
      }}>
        <summary className="notification-trigger" ref={trigger} title={t("notifications.title")}
          aria-label={t("notifications.trigger", { count: pending.length })}>
          <Bell size={16} aria-hidden="true" />
          {pending.length > 0 ? <span aria-hidden="true">{pending.length > 99 ? "99+" : pending.length}</span> : null}
        </summary>
        <div className="notification-panel" role="region" aria-labelledby={title}>
          <h2 id={title}>{t("notifications.title")}</h2>
          {status === "stale" ? <p role="status">{t("notifications.stale")}</p> : null}
          <NotificationList heading={t("notifications.pending")} items={pending} onNavigate={(target) => {
            close();
            onNavigate(target);
          }} />
          <NotificationList heading={t("notifications.recent")} items={recent} onNavigate={(target) => {
            close();
            onNavigate(target);
          }} />
        </div>
      </details>
      <div className="notification-toasts">
        <div role="status" className="sr-only">
          {toasts[0] ? t("notifications.announcement", {
            kind: t(`notifications.kinds.${toasts[0].kind}`), agent: toasts[0].target.agent_id,
          }) : ""}
        </div>
        {toasts.map((item) => (
          <div className="notification-toast" key={item.id} data-testid="notification-toast">
            <NotificationLink item={item} onNavigate={(target) => {
              runtimeNotificationSync.dismiss(item.id);
              onNavigate(target);
            }} />
            <button className="notification-dismiss" type="button" aria-label={t("notifications.dismiss")}
              onClick={() => {
                runtimeNotificationSync.dismiss(item.id);
                trigger.current?.focus();
              }}>
              <X size={16} aria-hidden="true" />
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

function NotificationList({ heading, items, onNavigate }: Props & {
  heading: string;
  items: RuntimeNotification[];
}) {
  const { t } = useTranslation();
  return (
    <section aria-label={heading}>
      <h3>{heading}</h3>
      {items.length ? <ul>{items.map((item) => (
        <li key={item.id}><NotificationLink item={item} onNavigate={onNavigate} /></li>
      ))}</ul> : <p>{t("notifications.empty")}</p>}
    </section>
  );
}

function NotificationLink({ item, onNavigate }: Props & { item: RuntimeNotification }) {
  const { t, i18n } = useTranslation();
  const timestamp = new Date(item.created_at);
  return (
    <button type="button" className="notification-link" onClick={() => onNavigate(item.target)}>
      <strong>{t(`notifications.kinds.${item.kind}`)}</strong>
      <span>{item.target.agent_id}{item.target.work_item_id ? ` · ${item.target.work_item_id}` : ""}</span>
      {Number.isFinite(timestamp.getTime()) ? (
        <time dateTime={item.created_at}>
          {timestamp.toLocaleString(i18n.resolvedLanguage, { dateStyle: "medium", timeStyle: "short" })}
        </time>
      ) : null}
    </button>
  );
}
