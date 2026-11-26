import {
  Bell, CircleCheck, MessageSquare, TriangleAlert, X, type LucideProps,
} from "lucide-react";
import { useEffect, useMemo, useState, type ComponentType } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import type { NotificationKind, NotificationTarget, RuntimeNotification } from "../../runtime/notification-sync";
import {
  runtimeNotificationSync,
  useRuntimeNotifications,
  useSystemNotificationPermission,
} from "../../runtime/runtime-notifications";
import { useRuntimeStore } from "../../runtime/runtime-store";

/** Long enough to read the card and reach for it, short enough not to pile up. */
const TOAST_TTL_MS = 8_000;
const SYSTEM_PROMPT_DISMISSED_KEY = "holon.webGui.systemNotificationPromptDismissed.v1";

function readPromptDismissed(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(SYSTEM_PROMPT_DISMISSED_KEY) === "1";
  } catch {
    return false;
  }
}

function writePromptDismissed(): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(SYSTEM_PROMPT_DISMISSED_KEY, "1");
  } catch {
    // Ignore storage errors; session state still hides the prompt.
  }
}

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
  onOpenSettings?: () => void;
}

/**
 * Headless global overlay for live notification toasts and a one-time proactive
 * browser notification opt-in card. Persistent pending/recent lists live on
 * Dashboard, and permanent system notification settings live on Settings.
 */
export function NotificationCenter({ onNavigate }: Props) {
  const { t } = useTranslation();
  const { status, toasts } = useRuntimeNotifications();
  const permission = useSystemNotificationPermission((state) => state.permission);
  const refreshPermission = useSystemNotificationPermission((state) => state.refresh);
  const requestPermission = useSystemNotificationPermission((state) => state.request);
  const [promptDismissed, setPromptDismissed] = useState<boolean>(readPromptDismissed);

  const agents = useRuntimeStore((state) => state.bootstrap?.agents ?? []);
  const agentNamesById = useMemo(() => {
    const map = new Map<string, string>();
    for (const agent of agents) {
      if (agent.name && agent.name !== agent.id) {
        map.set(agent.id, agent.name);
      }
    }
    return map;
  }, [agents]);

  useEffect(() => {
    refreshPermission();
  }, [refreshPermission]);

  if (status === "loading" || status === "unavailable" || typeof document === "undefined") {
    return null;
  }

  const showPermissionPrompt = permission === "default" && !promptDismissed;

  const dismissPrompt = () => {
    writePromptDismissed();
    setPromptDismissed(true);
  };

  const acceptPrompt = () => {
    writePromptDismissed();
    setPromptDismissed(true);
    void requestPermission();
  };

  return createPortal(
    <div className="notification-toasts">
      <div role="status" className="sr-only">
        {toasts[0] ? t("notifications.announcement", {
          kind: t(`notifications.kinds.${toasts[0].kind}`),
          agent: agentNamesById.get(toasts[0].target.agent_id) ?? toasts[0].target.agent_id,
        }) : ""}
      </div>
      {showPermissionPrompt ? (
        <div
          className="notification-toast notification-permission-prompt"
          data-testid="system-notification-prompt"
          data-tone="accent"
          role="region"
          aria-label={t("notifications.system.title")}
        >
          <span className="notification-toast-tone" aria-hidden="true" />
          <div className="notification-prompt-body">
            <span className="notification-toast-title">
              <Bell size={15} aria-hidden="true" />
              <strong>{t("notifications.system.promptTitle")}</strong>
            </span>
            <p className="notification-prompt-copy">{t("notifications.system.promptBody")}</p>
            <div className="notification-prompt-actions">
              <Button type="button" variant="accent" size="sm" onClick={acceptPrompt}>
                {t("notifications.system.enable")}
              </Button>
              <Button type="button" variant="ghost" size="sm" onClick={dismissPrompt}>
                {t("notifications.system.notNow")}
              </Button>
            </div>
          </div>
          <button
            type="button"
            className="notification-toast-dismiss"
            aria-label={t("notifications.system.notNow")}
            onClick={dismissPrompt}
          >
            <X size={14} aria-hidden="true" />
          </button>
        </div>
      ) : null}
      {toasts.map((item) => (
        <NotificationToast
          key={item.id}
          item={item}
          agentName={agentNamesById.get(item.target.agent_id)}
          onDismiss={runtimeNotificationSync.dismiss}
          onNavigate={onNavigate}
        />
      ))}
    </div>,
    document.body,
  );
}

function NotificationToast({ item, agentName, onNavigate, onDismiss }: {
  item: RuntimeNotification;
  agentName?: string;
  onNavigate: (target: NotificationTarget) => void;
  onDismiss: (id: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const [paused, setPaused] = useState(false);
  const { tone, Icon } = toastPresentation[item.kind] ?? fallbackPresentation;
  const timestamp = new Date(item.created_at);
  const agentLabel = agentName ?? item.target.agent_id;
  const detailText = item.title ?? item.summary;

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
          <strong>{agentLabel}</strong>
        </span>
        {detailText ? (
          <span className="notification-toast-summary">{detailText}</span>
        ) : null}
        <span className="notification-toast-meta">
          <span className="notification-toast-target">
            {detailText
              ? agentLabel
              : `${agentLabel}${item.target.work_item_id ? ` · ${item.target.work_item_id}` : ""}`}
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

/** Settings card for managing browser OS notifications on hidden tabs. */
export function SystemNotificationSettingsCard() {
  const { t } = useTranslation();
  const permission = useSystemNotificationPermission((state) => state.permission);
  const refresh = useSystemNotificationPermission((state) => state.refresh);
  const request = useSystemNotificationPermission((state) => state.request);

  useEffect(() => {
    refresh();
  }, [refresh]);

  if (permission === "unsupported") return null;

  return (
    <Card className="settings-card settings-primary-card notification-system">
      <div className="settings-card-head">
        <div>
          <span className="eyebrow">{t("notifications.title")}</span>
          <h2>{t("notifications.system.title")}</h2>
        </div>
      </div>
      <p className="settings-muted">{t("notifications.system.hint")}</p>
      {permission === "granted" ? (
        <p className="settings-status-note is-enabled">{t("notifications.system.enabled")}</p>
      ) : null}
      {permission === "denied" ? (
        <p className="settings-status-note is-blocked">{t("notifications.system.blocked")}</p>
      ) : null}
      {permission === "default" ? (
        <div className="settings-actions-row">
          <Button
            type="button"
            variant="secondary"
            onClick={() => {
              writePromptDismissed();
              void request();
            }}
          >
            {t("notifications.system.enable")}
          </Button>
        </div>
      ) : null}
    </Card>
  );
}
