import {
  CircleCheck, Bell, MessageSquare, TriangleAlert, X, type LucideProps,
} from "lucide-react";
import { useEffect, useId, useRef, useState, type ComponentType } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import type { NotificationKind, NotificationTarget, RuntimeNotification } from "../../runtime/notification-sync";
import {
  runtimeNotificationSync,
  useRuntimeNotifications,
  useSystemNotificationPermission,
} from "../../runtime/runtime-notifications";

/** Long enough to read the card and reach for it, short enough not to pile up. */
const TOAST_TTL_MS = 8_000;

interface ToastPresentation {
  tone: "warning" | "accent" | "success";
  Icon: ComponentType<LucideProps>;
}

// Server data is untrusted, so unknown kinds fall back instead of throwing.
const toastPresentation: Partial<Record<NotificationKind, ToastPresentation>> = {
  operator_input_required: { tone: "warning", Icon: TriangleAlert },
  operator_first_brief: { tone: "accent", Icon: MessageSquare },
  work_item_completed: { tone: "success", Icon: CircleCheck },
};
const fallbackPresentation: ToastPresentation = { tone: "accent", Icon: Bell };

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
          <SystemNotificationSettings />
        </div>
      </details>
      {/* The topbar's backdrop-filter would anchor a fixed child, so the stack
          lives on body; the sr-only region stays mounted for announcements. */}
      {typeof document === "undefined" ? null : createPortal(
        <div className="notification-toasts">
          <div role="status" className="sr-only">
            {toasts[0] ? t("notifications.announcement", {
              kind: t(`notifications.kinds.${toasts[0].kind}`), agent: toasts[0].target.agent_id,
            }) : ""}
          </div>
          {toasts.map((item) => (
            <NotificationToast key={item.id} item={item} onDismiss={runtimeNotificationSync.dismiss}
              onNavigate={onNavigate} />
          ))}
        </div>,
        document.body,
      )}
    </div>
  );
}

function NotificationToast({ item, onNavigate, onDismiss }: {
  item: RuntimeNotification;
  onNavigate: (target: NotificationTarget) => void;
  onDismiss: (id: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const [paused, setPaused] = useState(false);
  const { tone, Icon } = toastPresentation[item.kind] ?? fallbackPresentation;
  const timestamp = new Date(item.created_at);

  // Hover and focus both pause, so the card only disappears untouched.
  useEffect(() => {
    if (paused) return;
    const timer = setTimeout(() => onDismiss(item.id), TOAST_TTL_MS);
    return () => clearTimeout(timer);
  }, [item.id, onDismiss, paused]);

  return (
    <div className="notification-toast" data-testid="notification-toast" data-kind={item.kind} data-tone={tone}
      onPointerEnter={() => setPaused(true)} onPointerLeave={() => setPaused(false)}
      onFocusCapture={() => setPaused(true)}
      onBlurCapture={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setPaused(false);
      }}>
      <span className="notification-toast-tone" aria-hidden="true" />
      <button type="button" className="notification-toast-main" onClick={() => {
        onDismiss(item.id);
        onNavigate(item.target);
      }}>
        <span className="notification-toast-title">
          <Icon size={15} aria-hidden="true" />
          {t(`notifications.kinds.${item.kind}`)}
        </span>
        <span className="notification-toast-meta">
          <span className="notification-toast-target">
            {item.target.agent_id}{item.target.work_item_id ? ` · ${item.target.work_item_id}` : ""}
          </span>
          {Number.isFinite(timestamp.getTime()) ? (
            <time dateTime={item.created_at}>
              {timestamp.toLocaleTimeString(i18n.resolvedLanguage, { hour: "2-digit", minute: "2-digit" })}
            </time>
          ) : null}
        </span>
      </button>
      <button type="button" className="notification-toast-dismiss" aria-label={t("notifications.dismiss")}
        onClick={() => onDismiss(item.id)}>
        <X size={14} aria-hidden="true" />
      </button>
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

/** Hidden tabs can only reach the user through the OS notification surface. */
function SystemNotificationSettings() {
  const { t } = useTranslation();
  const permission = useSystemNotificationPermission((state) => state.permission);
  const refresh = useSystemNotificationPermission((state) => state.refresh);
  const request = useSystemNotificationPermission((state) => state.request);
  // The browser can change permission outside this page (site settings, other
  // tabs), so re-read it whenever the panel opens.
  useEffect(() => { refresh(); }, [refresh]);
  if (permission === "unsupported") return null;
  return (
    // No landmark role here: the panel is already the region, and a second
    // named region (it would contain "Notifications") confuses region queries.
    <section className="notification-system">
      <h3>{t("notifications.system.title")}</h3>
      {permission === "granted" ? <p>{t("notifications.system.enabled")}</p> : null}
      {permission === "denied" ? <p>{t("notifications.system.blocked")}</p> : null}
      {permission === "default" ? (
        <>
          <p>{t("notifications.system.hint")}</p>
          <Button variant="secondary" size="sm" onClick={() => { void request(); }}>
            {t("notifications.system.enable")}
          </Button>
        </>
      ) : null}
    </section>
  );
}
